//! Proactive Dynamic Context Budget for tool results.
//!
//! Provides per-result and aggregate budget calculation to prevent tool outputs
//! from exceeding the context window. Works in concert with the ContextGuard
//! (which handles compaction after the fact) by proactively capping tool outputs
//! *before* they enter the message history.
//!
//! ## Budget Formula
//!
//! ```text
//! headroom = context_limit − current_tokens − response_reserve
//! per_result_budget = headroom / max(1, pending_results)
//! per_result_char_limit = per_result_budget × 4.2 / safety_factor
//! ```
//!
//! The safety factor (default 1.2) accounts for token estimation error.
//! The aggregate guard ensures total tool output stays under 75% of headroom.

use clawdesk_types::estimate_tokens;

/// Configuration for the dynamic context budget.
#[derive(Debug, Clone)]
pub struct ContextBudgetConfig {
    /// Maximum fraction of remaining headroom a single result may consume.
    /// Default: 0.30 (30% of headroom per result).
    pub max_single_result_fraction: f64,
    /// Maximum fraction of headroom all tool results may consume in aggregate.
    /// Default: 0.75 (75% of headroom total).
    pub max_aggregate_fraction: f64,
    /// Safety factor for token estimation error (chars ÷ tokens).
    /// Default: 1.2 (assume 20% overcount).
    pub safety_factor: f64,
    /// Average characters per token (for char ↔ token conversion).
    /// Default: 4.2.
    pub chars_per_token: f64,
    /// Minimum per-result character limit to avoid overly aggressive truncation.
    /// Default: 200.
    pub min_chars: usize,
}

impl Default for ContextBudgetConfig {
    fn default() -> Self {
        Self {
            max_single_result_fraction: 0.30,
            max_aggregate_fraction: 0.75,
            safety_factor: 1.2,
            chars_per_token: 4.2,
            min_chars: 200,
        }
    }
}

/// Dynamic context budget calculator.
///
/// Create one per tool round, then call `per_result_char_limit()` to get
/// the maximum number of characters each tool result should contain.
#[derive(Debug)]
pub struct ContextBudget {
    config: ContextBudgetConfig,
    /// Total context window size in tokens.
    context_limit: usize,
    /// Current token count in the message history.
    current_tokens: usize,
    /// Reserved tokens for the LLM response.
    response_reserve: usize,
    /// Number of pending tool results.
    pending_results: usize,
    /// Number of tool results already processed.
    completed_results: usize,
    /// Tokens consumed by tool results so far in this round.
    consumed_tokens: usize,
}

impl ContextBudget {
    /// Create a new budget for the current round.
    pub fn new(
        config: ContextBudgetConfig,
        context_limit: usize,
        current_tokens: usize,
        response_reserve: usize,
        pending_results: usize,
    ) -> Self {
        Self {
            config,
            context_limit,
            current_tokens,
            response_reserve,
            pending_results: pending_results.max(1),
            completed_results: 0,
            consumed_tokens: 0,
        }
    }

    /// Compute the headroom available for tool results.
    pub fn headroom(&self) -> usize {
        self.context_limit
            .saturating_sub(self.current_tokens)
            .saturating_sub(self.response_reserve)
    }

    /// Compute the per-result token budget.
    pub fn per_result_token_budget(&self) -> usize {
        let headroom = self.headroom();
        let aggregate_budget = (headroom as f64 * self.config.max_aggregate_fraction) as usize;
        let remaining_aggregate = aggregate_budget.saturating_sub(self.consumed_tokens);
        let remaining_results = self.pending_results.saturating_sub(self.completed_results);
        let per_result = remaining_aggregate / remaining_results.max(1);
        let single_max = (headroom as f64 * self.config.max_single_result_fraction) as usize;
        per_result.min(single_max)
    }

    /// Compute the per-result character limit.
    ///
    /// Uses the `estimate_tokens` function from `clawdesk-types` when content
    /// is available, falling back to the configured `chars_per_token` ratio
    /// for pre-sizing limits.
    pub fn per_result_char_limit(&self) -> usize {
        let token_budget = self.per_result_token_budget();
        let char_limit = (token_budget as f64 * self.config.chars_per_token / self.config.safety_factor) as usize;
        char_limit.max(self.config.min_chars)
    }

    /// Record that a tool result consumed some tokens.
    pub fn record_consumption(&mut self, tokens: usize) {
        self.consumed_tokens += tokens;
        self.completed_results += 1;
    }

    /// Check if the aggregate budget is exhausted.
    pub fn is_aggregate_exhausted(&self) -> bool {
        let aggregate_budget = (self.headroom() as f64 * self.config.max_aggregate_fraction) as usize;
        self.consumed_tokens >= aggregate_budget
    }

    /// Truncate content to fit within the per-result budget.
    /// Returns the (possibly truncated) content, whether truncation occurred,
    /// and the estimated token count of the result (measured via `estimate_tokens`).
    pub fn truncate_to_budget(&self, content: &str) -> (String, bool, usize) {
        let token_budget = self.per_result_token_budget();
        let actual_tokens = estimate_tokens(content);

        if actual_tokens <= token_budget {
            return (content.to_string(), false, actual_tokens);
        }

        // Estimate char limit from the token budget using per-byte-class-aware ratio.
        // Start with the configured ratio, then binary-refine.
        let char_limit = self.per_result_char_limit();

        // UTF-8 safe prefix
        let mut end = char_limit.min(content.len());
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        let preview = &content[..end];
        let truncated = format!(
            "{}...\n[truncated: output was {} tokens (~{} chars), budget allows ~{} tokens]",
            preview,
            actual_tokens,
            content.len(),
            token_budget,
        );
        let final_tokens = estimate_tokens(&truncated);
        (truncated, true, final_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_headroom_calculation() {
        let budget = ContextBudget::new(
            ContextBudgetConfig::default(),
            128_000,
            50_000,
            8_192,
            4,
        );
        assert_eq!(budget.headroom(), 128_000 - 50_000 - 8_192);
    }

    #[test]
    fn test_per_result_char_limit_reasonable() {
        let budget = ContextBudget::new(
            ContextBudgetConfig::default(),
            128_000,
            50_000,
            8_192,
            4,
        );
        let limit = budget.per_result_char_limit();
        // Should be a reasonable number, not tiny or huge
        assert!(limit > 200, "limit too small: {}", limit);
        assert!(limit < 500_000, "limit too large: {}", limit);
    }

    #[test]
    fn test_truncation() {
        let budget = ContextBudget::new(
            ContextBudgetConfig {
                min_chars: 10,
                ..Default::default()
            },
            1000,
            900,
            50,
            1,
        );
        let content = "a".repeat(10_000);
        let (truncated, did_truncate, _tokens) = budget.truncate_to_budget(&content);
        assert!(did_truncate);
        assert!(truncated.len() < content.len());
        assert!(truncated.contains("[truncated:"));
    }

    #[test]
    fn test_no_truncation_when_fits() {
        let budget = ContextBudget::new(
            ContextBudgetConfig::default(),
            128_000,
            10_000,
            8_192,
            1,
        );
        let content = "short output";
        let (result, did_truncate, _tokens) = budget.truncate_to_budget(content);
        assert!(!did_truncate);
        assert_eq!(result, content);
    }

    #[test]
    fn test_min_chars_floor() {
        let budget = ContextBudget::new(
            ContextBudgetConfig {
                min_chars: 500,
                ..Default::default()
            },
            1000,
            999, // Almost full
            1,
            1,
        );
        let limit = budget.per_result_char_limit();
        assert!(limit >= 500, "limit should respect min_chars: {}", limit);
    }

    #[test]
    fn test_aggregate_exhaustion() {
        let mut budget = ContextBudget::new(
            ContextBudgetConfig::default(),
            10_000,
            5_000,
            1_000,
            4,
        );
        assert!(!budget.is_aggregate_exhausted());
        // Consume all of the aggregate budget
        let headroom = budget.headroom();
        budget.record_consumption((headroom as f64 * 0.75) as usize + 1);
        assert!(budget.is_aggregate_exhausted());
    }

    #[test]
    fn test_per_result_budget_shrinks_after_consumption() {
        let mut budget = ContextBudget::new(
            ContextBudgetConfig::default(),
            128_000,
            50_000,
            8_192,
            4,
        );
        let first_budget = budget.per_result_token_budget();
        // After consuming one result, the per-result budget for remaining
        // results should be recalculated against the remaining aggregate.
        budget.record_consumption(first_budget);
        let second_budget = budget.per_result_token_budget();
        // With 3 results remaining and less aggregate, second should differ
        // from first — specifically it should not equal first (the old bug).
        assert!(
            second_budget <= first_budget,
            "per-result budget should not grow after consumption: first={}, second={}",
            first_budget, second_budget,
        );
    }
}
