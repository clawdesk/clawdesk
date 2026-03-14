//! # Proactive Compaction Scheduler — Layer 2 gap closure
//!
//! The audit found: "No proactive compaction scheduling (runs on-demand only)".
//! This module adds a background compaction scheduler that estimates when
//! compaction will be needed and triggers it *before* the context window
//! is full, avoiding the latency spike of on-demand compaction mid-turn.
//!
//! ## Algorithm
//!
//! Uses Exponential Weighted Moving Average (EWMA) of per-turn token growth
//! to predict when the context will hit the trigger threshold. If the
//! prediction falls within a configurable lookahead window (default: 2 turns),
//! compaction is triggered proactively during idle time.
//!
//! ```text
//! growth_est(t) = α · Δ_tokens(t) + (1 - α) · growth_est(t-1)
//! turns_until_trigger = (threshold - current_tokens) / growth_est
//! if turns_until_trigger ≤ lookahead → trigger proactive compaction
//! ```

use serde::{Deserialize, Serialize};

/// Configuration for the proactive compaction scheduler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProactiveCompactionConfig {
    /// EWMA smoothing factor α ∈ (0, 1). Higher = more responsive.
    pub alpha: f64,
    /// How many turns ahead to look when deciding to compact.
    pub lookahead_turns: u32,
    /// Minimum number of turns observed before predictions are trusted.
    pub warmup_turns: u32,
    /// Minimum growth rate (tokens/turn) to consider compaction worthwhile.
    pub min_growth_rate: f64,
}

impl Default for ProactiveCompactionConfig {
    fn default() -> Self {
        Self {
            alpha: 0.3,
            lookahead_turns: 2,
            warmup_turns: 3,
            min_growth_rate: 100.0,
        }
    }
}

/// Proactive compaction scheduler state.
pub struct CompactionScheduler {
    config: ProactiveCompactionConfig,
    /// EWMA estimate of tokens added per turn.
    growth_estimate: f64,
    /// Previous token count for computing deltas.
    prev_tokens: usize,
    /// Number of turns observed.
    turns_observed: u32,
    /// Context limit (total budget).
    context_limit: usize,
    /// Trigger threshold as a fraction of context_limit (e.g. 0.8).
    trigger_fraction: f64,
}

/// Recommendation from the scheduler.
#[derive(Debug, Clone, PartialEq)]
pub enum CompactionAdvice {
    /// No action needed.
    None,
    /// Compact proactively — we predict hitting the threshold within the lookahead window.
    CompactNow {
        estimated_turns_until_full: f64,
        current_utilization: f64,
    },
    /// Already past the threshold — compact immediately (fallback to on-demand).
    CompactUrgent {
        current_utilization: f64,
    },
}

impl CompactionScheduler {
    pub fn new(context_limit: usize, trigger_fraction: f64, config: ProactiveCompactionConfig) -> Self {
        Self {
            config,
            growth_estimate: 0.0,
            prev_tokens: 0,
            turns_observed: 0,
            context_limit,
            trigger_fraction,
        }
    }

    /// Call after each turn with the current total token count.
    /// Returns advice on whether to compact proactively.
    pub fn observe(&mut self, current_tokens: usize) -> CompactionAdvice {
        let threshold = (self.context_limit as f64 * self.trigger_fraction) as usize;

        // Already over threshold — urgent.
        if current_tokens >= threshold {
            return CompactionAdvice::CompactUrgent {
                current_utilization: current_tokens as f64 / self.context_limit as f64,
            };
        }

        // Compute delta.
        let delta = if self.turns_observed > 0 && current_tokens > self.prev_tokens {
            (current_tokens - self.prev_tokens) as f64
        } else {
            0.0
        };

        // Update EWMA.
        if self.turns_observed == 0 {
            self.growth_estimate = delta;
        } else {
            self.growth_estimate =
                self.config.alpha * delta + (1.0 - self.config.alpha) * self.growth_estimate;
        }

        self.prev_tokens = current_tokens;
        self.turns_observed += 1;

        // Need warmup before predictions are trustworthy.
        if self.turns_observed < self.config.warmup_turns {
            return CompactionAdvice::None;
        }

        // Growth too low to worry about.
        if self.growth_estimate < self.config.min_growth_rate {
            return CompactionAdvice::None;
        }

        // Predict turns until threshold.
        let remaining = threshold.saturating_sub(current_tokens) as f64;
        let turns_until_full = remaining / self.growth_estimate;

        if turns_until_full <= self.config.lookahead_turns as f64 {
            CompactionAdvice::CompactNow {
                estimated_turns_until_full: turns_until_full,
                current_utilization: current_tokens as f64 / self.context_limit as f64,
            }
        } else {
            CompactionAdvice::None
        }
    }

    /// Reset after compaction occurs (token count drops).
    pub fn reset_after_compaction(&mut self, new_token_count: usize) {
        self.prev_tokens = new_token_count;
        // Don't reset growth_estimate — it's still valid historical info.
    }

    pub fn growth_estimate(&self) -> f64 {
        self.growth_estimate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proactive_compaction_triggers() {
        let config = ProactiveCompactionConfig {
            alpha: 0.5,
            lookahead_turns: 2,
            warmup_turns: 2,
            min_growth_rate: 50.0,
        };
        let mut scheduler = CompactionScheduler::new(10_000, 0.8, config);

        // Turn 1: 1000 tokens (warmup)
        assert_eq!(scheduler.observe(1000), CompactionAdvice::None);
        // Turn 2: 3000 tokens (warmup, delta = 2000)
        assert_eq!(scheduler.observe(3000), CompactionAdvice::None);
        // Turn 3: 5000 tokens (delta = 2000, growth_est ~2000)
        // threshold = 8000, remaining = 3000, turns_until = 1.5 < 2 → compact
        let advice = scheduler.observe(5000);
        assert!(matches!(advice, CompactionAdvice::CompactNow { .. }));
    }

    #[test]
    fn test_urgent_compaction() {
        let mut scheduler = CompactionScheduler::new(10_000, 0.8, Default::default());
        let advice = scheduler.observe(9000); // 90% > 80% threshold
        assert!(matches!(advice, CompactionAdvice::CompactUrgent { .. }));
    }

    #[test]
    fn test_low_growth_no_compaction() {
        let config = ProactiveCompactionConfig {
            alpha: 0.5,
            lookahead_turns: 2,
            warmup_turns: 2,
            min_growth_rate: 100.0,
        };
        let mut scheduler = CompactionScheduler::new(100_000, 0.8, config);
        scheduler.observe(1000);
        scheduler.observe(1010); // delta = 10
        let advice = scheduler.observe(1020); // delta = 10, growth ~10 < 100
        assert_eq!(advice, CompactionAdvice::None);
    }
}
