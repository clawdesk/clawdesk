//! # Streaming Response — Stream-first architecture for message responses.
//!
//! Replaces the dual-path pattern (stream events + final response) with a
//! single ordered event stream per message. The frontend receives typed
//! events via Tauri's `app.emit()`, with the final `Done` event carrying
//! all metadata (tokens, cost, model, etc.).
//!
//! ## Event Flow
//!
//! ```text
//! Frontend: invoke("send_message", {...})
//!              ↓
//! Backend:  spawn message handler
//!              ↓
//!           emit("message-stream", StreamStart { ... })
//!           emit("message-stream", Chunk { text, done: false })
//!           emit("message-stream", ToolStart { ... })
//!           emit("message-stream", ToolEnd { ... })
//!           emit("message-stream", Chunk { text, done: false })
//!           emit("message-stream", StreamEnd { ... })
//!              ↓
//! Frontend: receives single ordered event stream
//! ```
//!
//! ## Advantages over current dual-path
//!
//! - Single event stream per message (no coordination between stream + response)
//! - Partial results handled naturally (stream stops on error)
//! - Error events terminate the stream consistently
//! - Backpressure via bounded channel capacity

use crate::state::TauriAgentEvent;
use serde::{Deserialize, Serialize};

/// The Tauri event name for message stream events.
pub const MESSAGE_STREAM_EVENT: &str = "message-stream";

/// A unified stream event for a single message lifecycle.
///
/// Sent to the Tauri frontend via `app.emit()`. The frontend subscribes
/// to `message-stream` events filtered by `chat_id` + `message_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageStreamEvent {
    /// Message processing has started. Contains the message ID for correlation.
    StreamStart {
        chat_id: String,
        message_id: String,
        agent_id: String,
        model: String,
    },

    /// An incremental text chunk from the LLM.
    Chunk {
        chat_id: String,
        message_id: String,
        text: String,
        /// Whether this is the final text chunk.
        done: bool,
    },

    /// Reasoning/thinking token chunk (separate from visible content).
    ThinkingChunk {
        chat_id: String,
        message_id: String,
        text: String,
    },

    /// A new tool execution round has started.
    RoundStart {
        chat_id: String,
        message_id: String,
        round: usize,
    },

    /// A tool execution has started.
    ToolStart {
        chat_id: String,
        message_id: String,
        tool_name: String,
        tool_args: String,
    },

    /// A tool execution has completed.
    ToolEnd {
        chat_id: String,
        message_id: String,
        tool_name: String,
        success: bool,
        duration_ms: u64,
        preview: String,
    },

    /// A tool was blocked by policy.
    ToolBlocked {
        chat_id: String,
        message_id: String,
        tool_name: String,
        reason: String,
    },

    /// Context compaction was applied.
    Compaction {
        chat_id: String,
        message_id: String,
        level: String,
        tokens_before: usize,
        tokens_after: usize,
    },

    /// Model fallback triggered.
    FallbackTriggered {
        chat_id: String,
        message_id: String,
        from_model: String,
        to_model: String,
        reason: String,
    },

    /// An error occurred during processing.
    Error {
        chat_id: String,
        message_id: String,
        error: String,
    },

    /// Message processing is complete. Carries all metadata.
    StreamEnd {
        chat_id: String,
        message_id: String,
        /// The complete response content.
        content: String,
        /// Total tool rounds executed.
        total_rounds: usize,
        /// Input tokens consumed.
        input_tokens: u64,
        /// Output tokens generated.
        output_tokens: u64,
        /// Model that generated the response.
        model: String,
        /// Duration in milliseconds.
        duration_ms: u64,
        /// Skills that were active for this message.
        active_skills: Vec<String>,
    },
}

impl MessageStreamEvent {
    /// Get the chat_id from any event variant.
    pub fn chat_id(&self) -> &str {
        match self {
            Self::StreamStart { chat_id, .. }
            | Self::Chunk { chat_id, .. }
            | Self::ThinkingChunk { chat_id, .. }
            | Self::RoundStart { chat_id, .. }
            | Self::ToolStart { chat_id, .. }
            | Self::ToolEnd { chat_id, .. }
            | Self::ToolBlocked { chat_id, .. }
            | Self::Compaction { chat_id, .. }
            | Self::FallbackTriggered { chat_id, .. }
            | Self::Error { chat_id, .. }
            | Self::StreamEnd { chat_id, .. } => chat_id,
        }
    }

    /// Get the message_id from any event variant.
    pub fn message_id(&self) -> &str {
        match self {
            Self::StreamStart { message_id, .. }
            | Self::Chunk { message_id, .. }
            | Self::ThinkingChunk { message_id, .. }
            | Self::RoundStart { message_id, .. }
            | Self::ToolStart { message_id, .. }
            | Self::ToolEnd { message_id, .. }
            | Self::ToolBlocked { message_id, .. }
            | Self::Compaction { message_id, .. }
            | Self::FallbackTriggered { message_id, .. }
            | Self::Error { message_id, .. }
            | Self::StreamEnd { message_id, .. } => message_id,
        }
    }
}

/// Convert a `TauriAgentEvent` to a `MessageStreamEvent` with chat/message context.
///
/// Used to bridge the existing `AgentEvent` → `TauriAgentEvent` path
/// into the new stream-first architecture. This enables incremental migration.
pub fn bridge_agent_event(
    chat_id: &str,
    message_id: &str,
    event: &TauriAgentEvent,
) -> Option<MessageStreamEvent> {
    match event {
        TauriAgentEvent::StreamChunk { text, done } => Some(MessageStreamEvent::Chunk {
            chat_id: chat_id.to_string(),
            message_id: message_id.to_string(),
            text: text.clone(),
            done: *done,
        }),
        TauriAgentEvent::ThinkingChunk { text } => Some(MessageStreamEvent::ThinkingChunk {
            chat_id: chat_id.to_string(),
            message_id: message_id.to_string(),
            text: text.clone(),
        }),
        TauriAgentEvent::RoundStart { round } => Some(MessageStreamEvent::RoundStart {
            chat_id: chat_id.to_string(),
            message_id: message_id.to_string(),
            round: *round,
        }),
        TauriAgentEvent::ToolStart { name, args } => Some(MessageStreamEvent::ToolStart {
            chat_id: chat_id.to_string(),
            message_id: message_id.to_string(),
            tool_name: name.clone(),
            tool_args: args.clone(),
        }),
        TauriAgentEvent::ToolEnd {
            name,
            success,
            duration_ms,
        } => Some(MessageStreamEvent::ToolEnd {
            chat_id: chat_id.to_string(),
            message_id: message_id.to_string(),
            tool_name: name.clone(),
            success: *success,
            duration_ms: *duration_ms,
            preview: String::new(),
        }),
        TauriAgentEvent::ToolBlocked { name, reason } => {
            Some(MessageStreamEvent::ToolBlocked {
                chat_id: chat_id.to_string(),
                message_id: message_id.to_string(),
                tool_name: name.clone(),
                reason: reason.clone(),
            })
        }
        TauriAgentEvent::Compaction {
            level,
            tokens_before,
            tokens_after,
        } => Some(MessageStreamEvent::Compaction {
            chat_id: chat_id.to_string(),
            message_id: message_id.to_string(),
            level: level.clone(),
            tokens_before: *tokens_before,
            tokens_after: *tokens_after,
        }),
        TauriAgentEvent::FallbackTriggered {
            from_model,
            to_model,
            reason,
        } => Some(MessageStreamEvent::FallbackTriggered {
            chat_id: chat_id.to_string(),
            message_id: message_id.to_string(),
            from_model: from_model.clone(),
            to_model: to_model.clone(),
            reason: reason.clone(),
        }),
        TauriAgentEvent::Error { error } => Some(MessageStreamEvent::Error {
            chat_id: chat_id.to_string(),
            message_id: message_id.to_string(),
            error: error.clone(),
        }),
        _ => None, // Other events don't map to stream events
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_event_serialization() {
        let event = MessageStreamEvent::StreamStart {
            chat_id: "c1".into(),
            message_id: "m1".into(),
            agent_id: "agent-1".into(),
            model: "claude-sonnet-4-20250514".into(),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("stream_start"));
        assert!(json.contains("c1"));
    }

    #[test]
    fn test_stream_end_carries_metadata() {
        let event = MessageStreamEvent::StreamEnd {
            chat_id: "c1".into(),
            message_id: "m1".into(),
            content: "Hello world".into(),
            total_rounds: 3,
            input_tokens: 1000,
            output_tokens: 500,
            model: "claude-sonnet-4-20250514".into(),
            duration_ms: 5000,
            active_skills: vec!["coding".into()],
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("stream_end"));
        assert!(json.contains("1000"));
        assert!(json.contains("coding"));
    }

    #[test]
    fn test_chat_id_extraction() {
        let event = MessageStreamEvent::ToolBlocked {
            chat_id: "chat-xyz".into(),
            message_id: "msg-1".into(),
            tool_name: "shell".into(),
            reason: "denied".into(),
        };
        assert_eq!(event.chat_id(), "chat-xyz");
        assert_eq!(event.message_id(), "msg-1");
    }

    #[test]
    fn test_bridge_agent_event() {
        let agent_event = TauriAgentEvent::StreamChunk {
            text: "Hello".into(),
            done: false,
        };

        let stream_event = bridge_agent_event("c1", "m1", &agent_event);
        assert!(stream_event.is_some());

        match stream_event.unwrap() {
            MessageStreamEvent::Chunk { text, done, .. } => {
                assert_eq!(text, "Hello");
                assert!(!done);
            }
            _ => panic!("expected Chunk"),
        }
    }

    #[test]
    fn test_bridge_unmapped_events() {
        let agent_event = TauriAgentEvent::Done { total_rounds: 5 };
        assert!(bridge_agent_event("c1", "m1", &agent_event).is_none());
    }
}
