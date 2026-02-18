//! Agent execution trace — structured post-mortem for every agent run.
//!
//! ## Design rationale
//!
//! OpenClaw debugging means `/context list` (text dump after the fact),
//! reading session JSON files, and checking unified logs (`clawlog.sh`).
//! There's no structured tracing across tool calls, skill activations,
//! or context decisions.
//!
//! `AgentTrace` provides a **complete structured post-mortem** for any
//! agent run: every event timestamped, prompt manifest included, token
//! accounting per round, and explicit decision explanations.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let collector = TraceCollector::new();
//! collector.record(event);
//! // ... agent run completes ...
//! let trace = collector.finalize(prompt_manifest);
//! // Store in SochDB, expose via GET /api/v1/admin/traces/:run_id
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use clawdesk_domain::prompt_builder::PromptManifest;

// ---------------------------------------------------------------------------
// Extended agent events (decision-explaining)
// ---------------------------------------------------------------------------

/// Extended events that explain agent decisions — emitted alongside
/// the existing `AgentEvent` variants for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TraceEvent {
    /// Emitted after prompt assembly — explains what's in the prompt.
    PromptAssembled {
        manifest: PromptManifest,
    },

    /// Emitted when a skill is selected or excluded by the prompt builder.
    SkillDecision {
        skill_id: String,
        included: bool,
        /// Human-readable reason (e.g., "trigger matched: Keywords(code, review)").
        reason: String,
        token_cost: usize,
        budget_remaining: usize,
    },

    /// Emitted when the context guard intervenes (compact, truncate, circuit break).
    ContextGuardAction {
        action: String,
        token_count: usize,
        threshold: f64,
        circuit_breaker_state: String,
    },

    /// Emitted on model fallback (provider error → switch to backup model).
    FallbackTriggered {
        from_model: String,
        to_model: String,
        reason: String,
        attempt: usize,
    },

    /// Emitted when identity is verified before prompt assembly.
    IdentityVerified {
        hash_match: bool,
        source: String,
        version: u64,
    },

    /// Tool invocation with timing and result summary.
    ToolInvocation {
        name: String,
        args_summary: String,
        success: bool,
        duration_ms: u64,
        result_size_bytes: usize,
    },

    /// Compaction applied during the agent run.
    CompactionApplied {
        level: String,
        tokens_before: usize,
        tokens_after: usize,
        turns_removed: usize,
    },

    /// Round start with context snapshot.
    RoundSnapshot {
        round: usize,
        message_count: usize,
        estimated_tokens: usize,
    },

    /// Agent response (non-streaming summary).
    ResponseSummary {
        round: usize,
        content_length: usize,
        finish_reason: String,
        input_tokens: u64,
        output_tokens: u64,
    },

    /// Error during agent execution.
    ExecutionError {
        error: String,
        round: Option<usize>,
        recoverable: bool,
    },
}

/// A timestamped trace event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimestampedEvent {
    pub timestamp: DateTime<Utc>,
    pub event: TraceEvent,
}

// ---------------------------------------------------------------------------
// Agent trace
// ---------------------------------------------------------------------------

/// Complete structured trace for a single agent run.
///
/// This is the answer to "why did the agent do that?" — store in SochDB,
/// expose via the admin API, or return alongside the agent response for
/// debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTrace {
    /// Unique identifier for this trace.
    pub run_id: Uuid,
    /// When the agent run started.
    pub started_at: DateTime<Utc>,
    /// When the agent run completed.
    pub completed_at: Option<DateTime<Utc>>,
    /// All events in chronological order.
    pub events: Vec<TimestampedEvent>,
    /// Prompt manifest from the initial prompt assembly.
    pub prompt_manifest: Option<PromptManifest>,
    /// Total rounds executed.
    pub total_rounds: usize,
    /// Cumulative input tokens across all rounds.
    pub total_input_tokens: u64,
    /// Cumulative output tokens across all rounds.
    pub total_output_tokens: u64,
    /// Tools invoked during this run (summary).
    pub tools_invoked: Vec<ToolInvocationSummary>,
    /// Fallback events that occurred.
    pub fallbacks: Vec<FallbackRecord>,
    /// Compaction events that occurred.
    pub compactions: Vec<CompactionRecord>,
    /// Whether the run completed successfully.
    pub success: bool,
    /// Error message if the run failed.
    pub error: Option<String>,
    /// Model used for this run.
    pub model: String,
    /// Channel that initiated this run (if applicable).
    pub channel: Option<String>,
}

/// Summary of a tool invocation for the trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocationSummary {
    pub name: String,
    pub round: usize,
    pub success: bool,
    pub duration_ms: u64,
}

/// Record of a model fallback event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackRecord {
    pub from_model: String,
    pub to_model: String,
    pub reason: String,
    pub attempt: usize,
    pub timestamp: DateTime<Utc>,
}

/// Record of a compaction event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionRecord {
    pub level: String,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub tokens_saved: usize,
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Trace collector
// ---------------------------------------------------------------------------

/// Collects events during an agent run and produces an `AgentTrace`.
///
/// Create one per agent run. Thread-safe for use with `Arc<Mutex<TraceCollector>>`
/// or pass by `&mut` in the single-threaded agent loop.
pub struct TraceCollector {
    run_id: Uuid,
    started_at: DateTime<Utc>,
    events: Vec<TimestampedEvent>,
    tools: Vec<ToolInvocationSummary>,
    fallbacks: Vec<FallbackRecord>,
    compactions: Vec<CompactionRecord>,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_rounds: usize,
    model: String,
    channel: Option<String>,
}

impl TraceCollector {
    /// Create a new trace collector for an agent run.
    pub fn new(model: String, channel: Option<String>) -> Self {
        Self {
            run_id: Uuid::new_v4(),
            started_at: Utc::now(),
            events: Vec::with_capacity(64),
            tools: Vec::new(),
            fallbacks: Vec::new(),
            compactions: Vec::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_rounds: 0,
            model,
            channel,
        }
    }

    /// The run ID for this trace.
    pub fn run_id(&self) -> Uuid {
        self.run_id
    }

    /// Record a trace event.
    pub fn record(&mut self, event: TraceEvent) {
        // Extract summaries from specific event types.
        match &event {
            TraceEvent::ToolInvocation {
                name,
                success,
                duration_ms,
                ..
            } => {
                self.tools.push(ToolInvocationSummary {
                    name: name.clone(),
                    round: self.total_rounds,
                    success: *success,
                    duration_ms: *duration_ms,
                });
            }
            TraceEvent::FallbackTriggered {
                from_model,
                to_model,
                reason,
                attempt,
            } => {
                self.fallbacks.push(FallbackRecord {
                    from_model: from_model.clone(),
                    to_model: to_model.clone(),
                    reason: reason.clone(),
                    attempt: *attempt,
                    timestamp: Utc::now(),
                });
            }
            TraceEvent::CompactionApplied {
                level,
                tokens_before,
                tokens_after,
                ..
            } => {
                self.compactions.push(CompactionRecord {
                    level: level.clone(),
                    tokens_before: *tokens_before,
                    tokens_after: *tokens_after,
                    tokens_saved: tokens_before.saturating_sub(*tokens_after),
                    timestamp: Utc::now(),
                });
            }
            TraceEvent::ResponseSummary {
                input_tokens,
                output_tokens,
                ..
            } => {
                self.total_input_tokens += input_tokens;
                self.total_output_tokens += output_tokens;
            }
            TraceEvent::RoundSnapshot { round, .. } => {
                self.total_rounds = *round + 1;
            }
            _ => {}
        }

        self.events.push(TimestampedEvent {
            timestamp: Utc::now(),
            event,
        });
    }

    /// Record a prompt manifest (called once after prompt assembly).
    pub fn record_prompt_assembled(&mut self, manifest: PromptManifest) {
        self.record(TraceEvent::PromptAssembled {
            manifest: manifest.clone(),
        });
    }

    /// Record a skill decision.
    pub fn record_skill_decision(
        &mut self,
        skill_id: &str,
        included: bool,
        reason: &str,
        token_cost: usize,
        budget_remaining: usize,
    ) {
        self.record(TraceEvent::SkillDecision {
            skill_id: skill_id.to_string(),
            included,
            reason: reason.to_string(),
            token_cost,
            budget_remaining,
        });
    }

    /// Finalize the trace — call when the agent run is complete.
    pub fn finalize(
        self,
        prompt_manifest: Option<PromptManifest>,
        success: bool,
        error: Option<String>,
    ) -> AgentTrace {
        AgentTrace {
            run_id: self.run_id,
            started_at: self.started_at,
            completed_at: Some(Utc::now()),
            events: self.events,
            prompt_manifest,
            total_rounds: self.total_rounds,
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
            tools_invoked: self.tools,
            fallbacks: self.fallbacks,
            compactions: self.compactions,
            success,
            error,
            model: self.model,
            channel: self.channel,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_records_events() {
        let mut collector = TraceCollector::new("claude-sonnet-4-20250514".into(), Some("telegram".into()));

        collector.record(TraceEvent::RoundSnapshot {
            round: 0,
            message_count: 5,
            estimated_tokens: 1000,
        });

        collector.record(TraceEvent::ToolInvocation {
            name: "web_search".into(),
            args_summary: "query=rust".into(),
            success: true,
            duration_ms: 250,
            result_size_bytes: 1024,
        });

        collector.record(TraceEvent::ResponseSummary {
            round: 0,
            content_length: 500,
            finish_reason: "stop".into(),
            input_tokens: 1000,
            output_tokens: 200,
        });

        let trace = collector.finalize(None, true, None);

        assert_eq!(trace.total_rounds, 1);
        assert_eq!(trace.total_input_tokens, 1000);
        assert_eq!(trace.total_output_tokens, 200);
        assert_eq!(trace.tools_invoked.len(), 1);
        assert_eq!(trace.tools_invoked[0].name, "web_search");
        assert!(trace.success);
        assert_eq!(trace.events.len(), 3);
    }

    #[test]
    fn collector_tracks_compactions() {
        let mut collector = TraceCollector::new("test-model".into(), None);

        collector.record(TraceEvent::CompactionApplied {
            level: "SummarizeOld".into(),
            tokens_before: 50000,
            tokens_after: 20000,
            turns_removed: 15,
        });

        let trace = collector.finalize(None, true, None);
        assert_eq!(trace.compactions.len(), 1);
        assert_eq!(trace.compactions[0].tokens_saved, 30000);
    }

    #[test]
    fn collector_tracks_fallbacks() {
        let mut collector = TraceCollector::new("claude-sonnet".into(), None);

        collector.record(TraceEvent::FallbackTriggered {
            from_model: "claude-sonnet".into(),
            to_model: "gpt-4o".into(),
            reason: "rate_limited".into(),
            attempt: 2,
        });

        let trace = collector.finalize(None, true, None);
        assert_eq!(trace.fallbacks.len(), 1);
        assert_eq!(trace.fallbacks[0].to_model, "gpt-4o");
    }

    #[test]
    fn trace_serializable() {
        let collector = TraceCollector::new("test".into(), None);
        let trace = collector.finalize(None, true, None);
        let json = serde_json::to_string(&trace).unwrap();
        let restored: AgentTrace = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.run_id, trace.run_id);
    }
}
