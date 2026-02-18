//! Context window guard — predictive compaction with running token estimation.
//!
//! Maintains running token estimate T_est.
//! Triggers compaction when T_est > α × C (α = 0.80, C = context limit).
//! Tiered compaction: Level 1 (drop metadata, ~15% savings) →
//! Level 2 (summarize old turns, ~40% savings) →
//! Level 3 (aggressive truncation to last n turns).
//! Circuit breaker: closed → open (after 3 failures in 60s) → half-open.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Configuration for the context window guard.
#[derive(Debug, Clone)]
pub struct ContextGuardConfig {
    /// Total context window size in tokens.
    pub context_limit: usize,
    /// Trigger compaction at this fraction of the limit.
    pub trigger_threshold: f64, // α = 0.80
    /// Reserve tokens for the model's response.
    pub response_reserve: usize,
    /// Maximum compaction failures before circuit breaks.
    pub circuit_breaker_threshold: u32,
    /// Circuit breaker cooldown.
    pub circuit_breaker_cooldown: Duration,
}

impl Default for ContextGuardConfig {
    fn default() -> Self {
        Self {
            context_limit: 128_000,
            trigger_threshold: 0.80,
            response_reserve: 8_192,
            circuit_breaker_threshold: 3,
            circuit_breaker_cooldown: Duration::from_secs(60),
        }
    }
}

/// Running token counter for the context window.
pub struct ContextGuard {
    config: ContextGuardConfig,
    /// Current estimated token count.
    estimated_tokens: usize,
    /// Circuit breaker state.
    breaker: CircuitBreaker,
}

/// Circuit breaker states for compaction failures.
#[derive(Debug)]
enum CircuitBreakerState {
    Closed,
    Open { opened_at: Instant },
    HalfOpen,
}

struct CircuitBreaker {
    state: CircuitBreakerState,
    failure_count: u32,
    last_failure: Option<Instant>,
    threshold: u32,
    cooldown: Duration,
}

impl CircuitBreaker {
    fn new(threshold: u32, cooldown: Duration) -> Self {
        Self {
            state: CircuitBreakerState::Closed,
            failure_count: 0,
            last_failure: None,
            threshold,
            cooldown,
        }
    }

    fn record_failure(&mut self) {
        let now = Instant::now();

        // Reset counter if last failure was long ago
        if let Some(last) = self.last_failure {
            if now.duration_since(last) > self.cooldown {
                self.failure_count = 0;
            }
        }

        self.failure_count += 1;
        self.last_failure = Some(now);

        if self.failure_count >= self.threshold {
            self.state = CircuitBreakerState::Open { opened_at: now };
        }
    }

    fn record_success(&mut self) {
        self.failure_count = 0;
        self.state = CircuitBreakerState::Closed;
    }

    fn is_allowed(&mut self) -> bool {
        match &self.state {
            CircuitBreakerState::Closed => true,
            CircuitBreakerState::Open { opened_at } => {
                if Instant::now().duration_since(*opened_at) >= self.cooldown {
                    self.state = CircuitBreakerState::HalfOpen;
                    true // Allow one attempt
                } else {
                    false
                }
            }
            CircuitBreakerState::HalfOpen => true,
        }
    }
}

/// Compaction level — progressively more aggressive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CompactionLevel {
    /// Drop tool-result metadata, keep text summaries. Saves ~15%.
    DropMetadata = 1,
    /// Summarize turns older than k. Saves ~40%.
    SummarizeOld = 2,
    /// Aggressive truncation to last n turns.
    Truncate = 3,
}

/// Result of a compaction operation.
#[derive(Debug)]
pub struct CompactionResult {
    pub level: CompactionLevel,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub turns_removed: usize,
    pub turns_summarized: usize,
}

/// Action recommended by the context guard.
#[derive(Debug, PartialEq)]
pub enum GuardAction {
    /// Context is within budget, proceed normally.
    Ok,
    /// Compaction needed at specified level.
    Compact(CompactionLevel),
    /// Context is critically over budget, must truncate immediately.
    ForceTruncate { keep_last_n: usize },
    /// Circuit breaker is open, compaction disabled.
    CircuitBroken,
}

impl ContextGuard {
    pub fn new(config: ContextGuardConfig) -> Self {
        let breaker = CircuitBreaker::new(
            config.circuit_breaker_threshold,
            config.circuit_breaker_cooldown,
        );
        Self {
            config,
            estimated_tokens: 0,
            breaker,
        }
    }

    /// Update the token estimate after appending a message.
    /// Token estimation: ~4 chars per token (fast O(1) estimate).
    pub fn record_tokens(&mut self, text: &str) {
        self.estimated_tokens += estimate_tokens(text);
    }

    /// Set the token count directly (e.g., from a tokenizer).
    pub fn set_token_count(&mut self, count: usize) {
        self.estimated_tokens = count;
    }

    /// Subtract tokens after compaction.
    pub fn subtract_tokens(&mut self, count: usize) {
        self.estimated_tokens = self.estimated_tokens.saturating_sub(count);
    }

    /// Get current estimated token count.
    pub fn current_tokens(&self) -> usize {
        self.estimated_tokens
    }

    /// Available token budget for the next response.
    pub fn available_budget(&self) -> usize {
        let used = self.estimated_tokens + self.config.response_reserve;
        self.config.context_limit.saturating_sub(used)
    }

    /// Check what action should be taken given current token usage.
    pub fn check(&mut self) -> GuardAction {
        let effective_limit = self.config.context_limit - self.config.response_reserve;
        let threshold = (effective_limit as f64 * self.config.trigger_threshold) as usize;

        if self.estimated_tokens <= threshold {
            return GuardAction::Ok;
        }

        if !self.breaker.is_allowed() {
            return GuardAction::CircuitBroken;
        }

        // Determine compaction level based on how far over we are
        let ratio = self.estimated_tokens as f64 / effective_limit as f64;
        if ratio > 0.95 {
            GuardAction::ForceTruncate { keep_last_n: 10 }
        } else if ratio > 0.90 {
            GuardAction::Compact(CompactionLevel::SummarizeOld)
        } else {
            GuardAction::Compact(CompactionLevel::DropMetadata)
        }
    }

    /// Report compaction success.
    pub fn compaction_succeeded(&mut self, result: &CompactionResult) {
        self.estimated_tokens = result.tokens_after;
        self.breaker.record_success();
    }

    /// Report compaction failure.
    pub fn compaction_failed(&mut self) {
        self.breaker.record_failure();
    }

    /// Utilization as a fraction (0.0 - 1.0).
    pub fn utilization(&self) -> f64 {
        let effective = self.config.context_limit - self.config.response_reserve;
        self.estimated_tokens as f64 / effective as f64
    }
}

// Re-export the canonical tokenizer from clawdesk-types.
// This was previously defined inline here; now consolidated in one place.
pub use clawdesk_types::tokenizer::estimate_tokens;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guard_ok_when_under_threshold() {
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 1000,
            trigger_threshold: 0.80,
            response_reserve: 100,
            ..Default::default()
        });
        guard.set_token_count(500); // 500 / 900 = 55% < 80%
        assert_eq!(guard.check(), GuardAction::Ok);
    }

    #[test]
    fn test_guard_triggers_compaction() {
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 1000,
            trigger_threshold: 0.80,
            response_reserve: 100,
            ..Default::default()
        });
        guard.set_token_count(750); // 750 / 900 = 83% > 80%
        let action = guard.check();
        assert!(matches!(action, GuardAction::Compact(CompactionLevel::DropMetadata)));
    }

    #[test]
    fn test_guard_force_truncate() {
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 1000,
            trigger_threshold: 0.80,
            response_reserve: 100,
            ..Default::default()
        });
        guard.set_token_count(860); // 860 / 900 = 95.5% > 95%
        let action = guard.check();
        assert!(matches!(action, GuardAction::ForceTruncate { .. }));
    }

    #[test]
    fn test_token_estimation() {
        // Empty string
        assert_eq!(estimate_tokens(""), 0);
        // Single char (alnum: 1/4.2 ≈ 0.24, ceil = 1)
        assert_eq!(estimate_tokens("a"), 1);
        // "hello" = 5 alnum: 5/4.2 ≈ 1.19, ceil = 2
        assert_eq!(estimate_tokens("hello"), 2);
        // Punctuation-heavy JSON: {"a": 1} = 4 punct + 1 alnum + 2 ws
        let json_est = estimate_tokens("{\"a\": 1}");
        assert!(json_est >= 3, "JSON should estimate more tokens due to punctuation");
    }

    #[test]
    fn test_utilization() {
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 1000,
            trigger_threshold: 0.80,
            response_reserve: 0,
            ..Default::default()
        });
        guard.set_token_count(500);
        assert!((guard.utilization() - 0.5).abs() < 0.01);
    }
}
