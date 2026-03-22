//! Exploration budget — token-limited spending on curiosity.
//!
//! Implements Principle 4: ≤10% of available compute on proactive tasks.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Token budget for curiosity exploration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationBudget {
    /// Maximum tokens per exploration cycle (default: 10% of context).
    pub max_tokens_per_cycle: u64,
    /// Maximum concurrent exploration tasks.
    pub max_concurrent_tasks: usize,
    /// Tokens spent in the current cycle.
    pub tokens_spent: u64,
    /// Tasks currently running.
    pub active_tasks: usize,
    /// When the current cycle started.
    pub cycle_start: DateTime<Utc>,
    /// Cycle duration before reset.
    pub cycle_duration_secs: u64,
}

impl ExplorationBudget {
    pub fn new(context_window: u64) -> Self {
        Self {
            max_tokens_per_cycle: context_window / 10, // 10% of context
            max_concurrent_tasks: 2,
            tokens_spent: 0,
            active_tasks: 0,
            cycle_start: Utc::now(),
            cycle_duration_secs: 3600, // 1 hour cycles
        }
    }

    /// Check if we can afford to explore.
    pub fn can_explore(&self, estimated_cost: u64) -> bool {
        if self.active_tasks >= self.max_concurrent_tasks {
            return false;
        }
        if self.tokens_spent + estimated_cost > self.max_tokens_per_cycle {
            return false;
        }
        true
    }

    /// Reserve tokens for an exploration task.
    pub fn reserve(&mut self, tokens: u64) -> bool {
        if !self.can_explore(tokens) {
            return false;
        }
        self.tokens_spent += tokens;
        self.active_tasks += 1;
        debug!(tokens, spent = self.tokens_spent, budget = self.max_tokens_per_cycle, "exploration budget: reserved");
        true
    }

    /// Release tokens after an exploration completes.
    pub fn release(&mut self, actual_tokens_used: u64) {
        self.active_tasks = self.active_tasks.saturating_sub(1);
        // Adjust spent count if actual was less than estimated
        // (but don't go below 0)
        if actual_tokens_used < self.tokens_spent {
            // We already counted the estimate; just leave it
        }
    }

    /// Check if the cycle has expired and reset if so.
    pub fn maybe_reset_cycle(&mut self) {
        let elapsed = (Utc::now() - self.cycle_start).num_seconds().max(0) as u64;
        if elapsed >= self.cycle_duration_secs {
            self.tokens_spent = 0;
            self.cycle_start = Utc::now();
            debug!("exploration budget: cycle reset");
        }
    }

    /// Remaining budget in the current cycle.
    pub fn remaining(&self) -> u64 {
        self.max_tokens_per_cycle.saturating_sub(self.tokens_spent)
    }

    /// Fraction of budget used in the current cycle.
    pub fn utilization(&self) -> f64 {
        if self.max_tokens_per_cycle == 0 {
            return 1.0;
        }
        self.tokens_spent as f64 / self.max_tokens_per_cycle as f64
    }
}

impl Default for ExplorationBudget {
    fn default() -> Self {
        // Default for a 128K context window model
        Self::new(128_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_limits_spending() {
        let mut budget = ExplorationBudget::new(10_000); // 1000 token budget
        assert!(budget.can_explore(500));
        assert!(budget.reserve(500));
        assert!(budget.can_explore(400));
        assert!(!budget.can_explore(600)); // over budget
    }

    #[test]
    fn concurrent_task_limit() {
        let mut budget = ExplorationBudget {
            max_concurrent_tasks: 1,
            ..ExplorationBudget::new(100_000)
        };
        budget.reserve(100);
        assert!(!budget.can_explore(100)); // at max concurrent
    }

    #[test]
    fn utilization_tracking() {
        let mut budget = ExplorationBudget::new(10_000);
        assert!((budget.utilization() - 0.0).abs() < f64::EPSILON);
        budget.reserve(500);
        assert!((budget.utilization() - 0.5).abs() < 0.01);
    }
}
