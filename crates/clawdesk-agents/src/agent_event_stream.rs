//! # Agent Event Stream — Composable typed async event stream for agent loops.
//!
//! Exposes the agent loop's internal state transitions as a composable, typed
//! SPMC (single-producer, multiple-consumer) broadcast channel. Any number of
//! receivers (TUI, parent orchestrator, telemetry, test harness) can subscribe.
//!
//! ## Event Model
//!
//! Fine-grained lifecycle events:
//! ```text
//! AgentStart → TurnStart → MessageStart → MessageUpdate* → MessageEnd
//!            → ToolExecutionStart → ToolExecutionUpdate* → ToolExecutionEnd
//!            → TurnEnd → AgentEnd
//! ```
//!
//! ## Performance
//!
//! Uses `tokio::sync::broadcast` with bounded ring buffer (default 256 events).
//! Producer: O(1) per emit. Consumer: O(1) per receive. Total cost for k
//! consumers over n events: O(n·k). Slow consumers see `RecvError::Lagged`
//! and skip ahead — correct for observability (latest state > complete history).

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tracing::debug;

/// Default event stream capacity (ring buffer size).
const DEFAULT_CAPACITY: usize = 256;

// ═══════════════════════════════════════════════════════════════════════════
// Agent lifecycle events
// ═══════════════════════════════════════════════════════════════════════════

/// Unique identifier for an agent execution instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentExecutionId(pub String);

impl AgentExecutionId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

/// Fine-grained lifecycle event emitted during agent execution.
///
/// Modeled after pi-mono's event stream: agent_start, turn_start,
/// message_start, message_update, message_end, tool_execution_start,
/// tool_execution_update, tool_execution_end, turn_end, agent_end.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum AgentLoopEvent {
    /// Agent execution started.
    AgentStart {
        execution_id: String,
        agent_id: String,
        model: String,
        /// Parent execution ID if this is a sub-agent.
        parent_execution_id: Option<String>,
    },

    /// A new turn (round) has started.
    TurnStart {
        execution_id: String,
        turn_number: usize,
        token_count: usize,
        message_count: usize,
    },

    /// LLM response streaming has started.
    MessageStart {
        execution_id: String,
        turn_number: usize,
    },

    /// Incremental LLM content update (streaming chunk).
    MessageUpdate {
        execution_id: String,
        turn_number: usize,
        delta: String,
        /// Accumulated content length so far.
        accumulated_len: usize,
    },

    /// Reasoning/thinking token chunk (separate from visible content).
    ThinkingUpdate {
        execution_id: String,
        turn_number: usize,
        delta: String,
    },

    /// LLM response for this turn is complete.
    MessageEnd {
        execution_id: String,
        turn_number: usize,
        content_length: usize,
        finish_reason: String,
        input_tokens: u64,
        output_tokens: u64,
    },

    /// A tool execution has started.
    ToolExecutionStart {
        execution_id: String,
        turn_number: usize,
        tool_call_id: String,
        tool_name: String,
        /// Index within the current round's batch of tool calls.
        tool_index: usize,
        total_tools: usize,
    },

    /// Progress update from a running tool (for long-running tools).
    ToolExecutionUpdate {
        execution_id: String,
        tool_call_id: String,
        tool_name: String,
        progress: String,
    },

    /// A tool execution has completed.
    ToolExecutionEnd {
        execution_id: String,
        turn_number: usize,
        tool_call_id: String,
        tool_name: String,
        success: bool,
        duration_ms: u64,
        /// Truncated output preview.
        output_preview: String,
    },

    /// A tool execution was blocked (by policy, loop guard, etc.).
    ToolBlocked {
        execution_id: String,
        tool_name: String,
        reason: String,
    },

    /// Context compaction was applied.
    Compaction {
        execution_id: String,
        turn_number: usize,
        level: String,
        tokens_before: usize,
        tokens_after: usize,
    },

    /// A turn (round) has ended.
    TurnEnd {
        execution_id: String,
        turn_number: usize,
        has_tool_calls: bool,
        tool_call_count: usize,
    },

    /// Agent execution has completed.
    AgentEnd {
        execution_id: String,
        total_turns: usize,
        total_input_tokens: u64,
        total_output_tokens: u64,
        duration_ms: u64,
        success: bool,
        error: Option<String>,
    },

    /// Model fallback triggered (for observability).
    FallbackTriggered {
        execution_id: String,
        from_model: String,
        to_model: String,
        reason: String,
        attempt: usize,
    },

    /// Steering message was injected mid-execution (Rec 2).
    SteeringInjected {
        execution_id: String,
        turn_number: usize,
        message_preview: String,
    },
}

impl AgentLoopEvent {
    /// Get the execution ID from any event variant.
    pub fn execution_id(&self) -> &str {
        match self {
            Self::AgentStart { execution_id, .. }
            | Self::TurnStart { execution_id, .. }
            | Self::MessageStart { execution_id, .. }
            | Self::MessageUpdate { execution_id, .. }
            | Self::ThinkingUpdate { execution_id, .. }
            | Self::MessageEnd { execution_id, .. }
            | Self::ToolExecutionStart { execution_id, .. }
            | Self::ToolExecutionUpdate { execution_id, .. }
            | Self::ToolExecutionEnd { execution_id, .. }
            | Self::ToolBlocked { execution_id, .. }
            | Self::Compaction { execution_id, .. }
            | Self::TurnEnd { execution_id, .. }
            | Self::AgentEnd { execution_id, .. }
            | Self::FallbackTriggered { execution_id, .. }
            | Self::SteeringInjected { execution_id, .. } => execution_id,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Event stream — SPMC broadcast channel wrapper
// ═══════════════════════════════════════════════════════════════════════════

/// A composable event stream for agent loop lifecycle events.
///
/// Wraps `tokio::sync::broadcast` with typed events. Producers emit events
/// via `emit()`, consumers subscribe via `subscribe()`.
///
/// Multiple event streams can be composed: a parent orchestrator can
/// subscribe to all child agent event streams and correlate events
/// across the agent hierarchy.
#[derive(Clone)]
pub struct AgentEventStream {
    tx: broadcast::Sender<AgentLoopEvent>,
    execution_id: String,
    start_time: Instant,
}

impl AgentEventStream {
    /// Create a new event stream with the default capacity.
    pub fn new(execution_id: String) -> Self {
        let (tx, _) = broadcast::channel(DEFAULT_CAPACITY);
        Self {
            tx,
            execution_id,
            start_time: Instant::now(),
        }
    }

    /// Create a new event stream with a custom capacity.
    pub fn with_capacity(execution_id: String, capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            tx,
            execution_id,
            start_time: Instant::now(),
        }
    }

    /// Get the execution ID for this stream.
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    /// Get the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Elapsed time since stream creation.
    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    /// Subscribe to the event stream. Returns a receiver that can be used
    /// to receive events asynchronously.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentLoopEvent> {
        self.tx.subscribe()
    }

    /// Get a reference to the sender (for bridging with existing `AgentEvent` system).
    pub fn sender(&self) -> &broadcast::Sender<AgentLoopEvent> {
        &self.tx
    }

    /// Emit an event to all subscribers.
    /// Zero-cost when there are no subscribers (no allocation, no serialization).
    pub fn emit(&self, event: AgentLoopEvent) {
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(event);
        }
    }

    // ═══════════════════════════════════════════════════════════
    // Convenience emitters for common lifecycle events
    // ═══════════════════════════════════════════════════════════

    pub fn emit_agent_start(&self, agent_id: &str, model: &str, parent_id: Option<&str>) {
        self.emit(AgentLoopEvent::AgentStart {
            execution_id: self.execution_id.clone(),
            agent_id: agent_id.to_string(),
            model: model.to_string(),
            parent_execution_id: parent_id.map(String::from),
        });
    }

    pub fn emit_turn_start(&self, turn: usize, tokens: usize, messages: usize) {
        self.emit(AgentLoopEvent::TurnStart {
            execution_id: self.execution_id.clone(),
            turn_number: turn,
            token_count: tokens,
            message_count: messages,
        });
    }

    pub fn emit_message_start(&self, turn: usize) {
        self.emit(AgentLoopEvent::MessageStart {
            execution_id: self.execution_id.clone(),
            turn_number: turn,
        });
    }

    pub fn emit_message_update(&self, turn: usize, delta: &str, accumulated: usize) {
        self.emit(AgentLoopEvent::MessageUpdate {
            execution_id: self.execution_id.clone(),
            turn_number: turn,
            delta: delta.to_string(),
            accumulated_len: accumulated,
        });
    }

    pub fn emit_message_end(
        &self,
        turn: usize,
        content_len: usize,
        finish_reason: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        self.emit(AgentLoopEvent::MessageEnd {
            execution_id: self.execution_id.clone(),
            turn_number: turn,
            content_length: content_len,
            finish_reason: finish_reason.to_string(),
            input_tokens,
            output_tokens,
        });
    }

    pub fn emit_tool_start(
        &self,
        turn: usize,
        tool_call_id: &str,
        tool_name: &str,
        index: usize,
        total: usize,
    ) {
        self.emit(AgentLoopEvent::ToolExecutionStart {
            execution_id: self.execution_id.clone(),
            turn_number: turn,
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            tool_index: index,
            total_tools: total,
        });
    }

    pub fn emit_tool_end(
        &self,
        turn: usize,
        tool_call_id: &str,
        tool_name: &str,
        success: bool,
        duration_ms: u64,
        preview: &str,
    ) {
        self.emit(AgentLoopEvent::ToolExecutionEnd {
            execution_id: self.execution_id.clone(),
            turn_number: turn,
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            success,
            duration_ms,
            output_preview: preview.to_string(),
        });
    }

    pub fn emit_agent_end(
        &self,
        total_turns: usize,
        input_tokens: u64,
        output_tokens: u64,
        success: bool,
        error: Option<&str>,
    ) {
        self.emit(AgentLoopEvent::AgentEnd {
            execution_id: self.execution_id.clone(),
            total_turns,
            total_input_tokens: input_tokens,
            total_output_tokens: output_tokens,
            duration_ms: self.elapsed_ms(),
            success,
            error: error.map(String::from),
        });
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Event stream combiner — for multi-agent correlation
// ═══════════════════════════════════════════════════════════════════════════

/// Combines multiple agent event streams into a single receiver.
///
/// Used by parent orchestrators to monitor all child agents from a single
/// async loop. Events are tagged with their execution IDs for correlation.
pub struct EventStreamCombiner {
    tx: broadcast::Sender<AgentLoopEvent>,
    _handles: Vec<tokio::task::JoinHandle<()>>,
}

impl EventStreamCombiner {
    /// Create a combiner that forwards events from multiple streams.
    pub fn new(streams: Vec<&AgentEventStream>) -> Self {
        let (tx, _) = broadcast::channel(DEFAULT_CAPACITY * streams.len().max(1));
        let mut handles = Vec::with_capacity(streams.len());

        for stream in streams {
            let mut rx = stream.subscribe();
            let tx_clone = tx.clone();
            handles.push(tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            let _ = tx_clone.send(event);
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            debug!(skipped = n, "event combiner: subscriber lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }));
        }

        Self {
            tx,
            _handles: handles,
        }
    }

    /// Subscribe to the combined event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentLoopEvent> {
        self.tx.subscribe()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_stream_basic() {
        let stream = AgentEventStream::new("exec-1".into());
        let mut rx = stream.subscribe();

        stream.emit_agent_start("agent-1", "claude-sonnet-4-20250514", None);

        let event = rx.recv().await.unwrap();
        match event {
            AgentLoopEvent::AgentStart {
                execution_id,
                agent_id,
                ..
            } => {
                assert_eq!(execution_id, "exec-1");
                assert_eq!(agent_id, "agent-1");
            }
            _ => panic!("expected AgentStart"),
        }
    }

    #[tokio::test]
    async fn test_event_stream_no_subscribers_no_cost() {
        let stream = AgentEventStream::new("exec-2".into());
        assert_eq!(stream.subscriber_count(), 0);

        // This should be a no-op (no subscribers)
        stream.emit_agent_start("agent-1", "model", None);
    }

    #[tokio::test]
    async fn test_event_stream_multiple_subscribers() {
        let stream = AgentEventStream::new("exec-3".into());
        let mut rx1 = stream.subscribe();
        let mut rx2 = stream.subscribe();

        assert_eq!(stream.subscriber_count(), 2);

        stream.emit_turn_start(0, 1000, 5);

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert_eq!(e1.execution_id(), "exec-3");
        assert_eq!(e2.execution_id(), "exec-3");
    }

    #[tokio::test]
    async fn test_execution_id_extraction() {
        let event = AgentLoopEvent::ToolBlocked {
            execution_id: "ex-1".into(),
            tool_name: "shell".into(),
            reason: "denied".into(),
        };
        assert_eq!(event.execution_id(), "ex-1");
    }
}
