//! # Loop Stages — Decomposed `execute_loop` types
//!
//! Defines the typed intermediate results flowing between the named stages
//! of `execute_loop`. The actual stage implementations live in `runner.rs`
//! as methods on `AgentRunner` (they need access to private runner fields).
//!
//! ## State Machine
//!
//! ```text
//! ┌──────────────────┐
//! │  CheckCompaction  │ → context guard → compaction/truncation
//! ├──────────────────┤
//! │  StreamFromLLM    │ → provider.stream() → accumulate content + tool calls
//! ├──────────────────┤
//! │  HandleOverflow   │ → tiered recovery on context length exceeded
//! ├──────────────────┤
//! │  RecoverToolCalls │ → extract tool calls from text (Qwen/DeepSeek compat)
//! ├──────────────────┤
//! │  ProcessToolRound │ → loop guard → execute → budget → push results
//! ├──────────────────┤
//! │  BuildResponse    │ → collect tool messages, segment response
//! └──────────────────┘
//! ```

use crate::builtin_tools::MessagingToolTracker;
use crate::loop_guard::LoopGuard;
use clawdesk_providers::{FinishReason, ToolCall};

// ═══════════════════════════════════════════════════════════
// Result types for loop stages
// ═══════════════════════════════════════════════════════════

/// Accumulated state tracking across tool-use rounds.
pub(crate) struct LoopState {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub messaging_tracker: MessagingToolTracker,
    pub loop_guard: LoopGuard,
    pub initial_msg_count: usize,
    pub overflow_retries: u8,
}

/// Result of streaming from the LLM provider.
pub(crate) struct StreamResult {
    pub content: String,
    pub finish_reason: FinishReason,
    pub usage: clawdesk_providers::TokenUsage,
    pub tool_calls: Vec<ToolCall>,
}

/// Outcome of handling a context overflow error.
pub(crate) enum OverflowOutcome {
    /// Retry this round — context has been reduced.
    Retry,
    /// All retries exhausted — return user-friendly error.
    Exhausted,
}

/// Outcome of processing a round's tool calls and LLM response.
pub(crate) enum RoundOutcome {
    /// Tool calls processed — continue to next round.
    Continue,
    /// No tool calls — final response ready.
    Done {
        content: String,
        round: usize,
        finish_reason: FinishReason,
    },
}

// ═══════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_result_default_finish() {
        let result = StreamResult {
            content: "Hello".to_string(),
            finish_reason: FinishReason::Stop,
            usage: clawdesk_providers::TokenUsage::default(),
            tool_calls: vec![],
        };
        assert_eq!(result.finish_reason, FinishReason::Stop);
        assert!(result.tool_calls.is_empty());
    }

    #[test]
    fn overflow_outcome_variants() {
        let retry = OverflowOutcome::Retry;
        assert!(matches!(retry, OverflowOutcome::Retry));
        let exhausted = OverflowOutcome::Exhausted;
        assert!(matches!(exhausted, OverflowOutcome::Exhausted));
    }

    #[test]
    fn round_outcome_done_carries_content() {
        let outcome = RoundOutcome::Done {
            content: "Final answer".to_string(),
            round: 3,
            finish_reason: FinishReason::Stop,
        };
        match outcome {
            RoundOutcome::Done { content, round, finish_reason } => {
                assert_eq!(content, "Final answer");
                assert_eq!(round, 3);
                assert_eq!(finish_reason, FinishReason::Stop);
            }
            RoundOutcome::Continue => panic!("expected Done"),
        }
    }

    #[test]
    fn round_outcome_continue() {
        let outcome = RoundOutcome::Continue;
        assert!(matches!(outcome, RoundOutcome::Continue));
    }
}
