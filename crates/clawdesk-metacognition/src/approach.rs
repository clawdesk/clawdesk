//! Approach evaluation — scoring the current strategy and identifying alternatives.
//!
//! The evaluator answers: "Given recent turns, is the current approach
//! converging toward a solution, or should we pivot?"
//!
//! It uses a simple exponential progress estimator: if the ratio of
//! successful tool calls is declining and output variation is narrowing,
//! the approach is losing momentum.

use serde::{Deserialize, Serialize};

/// Score for the current approach's viability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApproachScore {
    /// Confidence that the current approach will succeed (0.0–1.0).
    pub confidence: f64,
    /// Momentum — is progress accelerating or decelerating?
    /// Positive = accelerating, negative = decelerating.
    pub momentum: f64,
    /// Estimated remaining turns to completion (None = unknown).
    pub estimated_remaining_turns: Option<usize>,
}

/// A concrete alternative approach the agent could try.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlternativeApproach {
    /// Human-readable description of the alternative.
    pub description: String,
    /// Estimated confidence if we switch to this approach.
    pub estimated_confidence: f64,
    /// Source of the suggestion.
    pub source: ApproachSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApproachSource {
    /// Derived from procedural memory (past successes).
    ProceduralMemory,
    /// Derived from the metacognitive monitor's heuristics.
    Heuristic,
    /// Suggested by the LLM itself in previous reasoning.
    SelfSuggested,
}

/// Evaluates how well the current approach is working.
pub struct ApproachEvaluator {
    /// EWMA of tool success rate (α = 0.3).
    success_ewma: f64,
    /// Previous success rate for momentum computation.
    prev_success_rate: f64,
    /// Total turns observed.
    turns_observed: usize,
    /// Turns since last successful tool completion.
    turns_since_success: usize,
    /// Maximum turns without success before confidence drops to zero.
    max_stall_turns: usize,
}

impl ApproachEvaluator {
    pub fn new() -> Self {
        Self {
            success_ewma: 0.5, // prior: neutral
            prev_success_rate: 0.5,
            turns_observed: 0,
            turns_since_success: 0,
            max_stall_turns: 8,
        }
    }

    /// Update with a new turn's outcome data.
    pub fn observe(
        &mut self,
        successful_tools: usize,
        total_tools: usize,
        output_length: usize,
    ) -> ApproachScore {
        self.turns_observed += 1;

        let current_rate = if total_tools == 0 {
            // No tool calls — treat as neutral (exploration turn).
            0.5
        } else {
            successful_tools as f64 / total_tools as f64
        };

        // Presence of substantial output is itself a weak progress signal.
        let output_bonus = if output_length > 100 { 0.05 } else { 0.0 };

        let adjusted_rate = (current_rate + output_bonus).min(1.0);

        // EWMA update (α = 0.3 — responsive but not jittery)
        const ALPHA: f64 = 0.3;
        self.prev_success_rate = self.success_ewma;
        self.success_ewma = ALPHA * adjusted_rate + (1.0 - ALPHA) * self.success_ewma;

        // Momentum = delta of EWMA
        let momentum = self.success_ewma - self.prev_success_rate;

        // Track stall
        if successful_tools > 0 {
            self.turns_since_success = 0;
        } else if total_tools > 0 {
            self.turns_since_success += 1;
        }

        // Confidence decays with stall and low EWMA
        let stall_penalty = if self.max_stall_turns > 0 {
            1.0 - (self.turns_since_success as f64 / self.max_stall_turns as f64).min(1.0)
        } else {
            1.0
        };
        let confidence = (self.success_ewma * stall_penalty).clamp(0.0, 1.0);

        // Remaining turns estimate:
        // If momentum is positive and we have enough data, extrapolate.
        let estimated_remaining_turns = if self.turns_observed >= 3 && momentum > 0.01 {
            let gap = 1.0 - confidence;
            Some((gap / momentum).ceil() as usize)
        } else {
            None
        };

        ApproachScore {
            confidence,
            momentum,
            estimated_remaining_turns,
        }
    }

    pub fn current_confidence(&self) -> f64 {
        self.success_ewma
    }

    /// Reset state for a new approach (after strategy switch).
    pub fn reset(&mut self) {
        self.success_ewma = 0.5;
        self.prev_success_rate = 0.5;
        self.turns_observed = 0;
        self.turns_since_success = 0;
    }
}

impl Default for ApproachEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_increases_with_success() {
        let mut eval = ApproachEvaluator::new();
        let s1 = eval.observe(3, 3, 200);
        let s2 = eval.observe(2, 2, 150);
        assert!(s2.confidence >= s1.confidence);
    }

    #[test]
    fn confidence_drops_on_total_failure() {
        let mut eval = ApproachEvaluator::new();
        for _ in 0..6 {
            eval.observe(0, 3, 10);
        }
        let score = eval.observe(0, 3, 10);
        assert!(score.confidence < 0.2, "confidence should be low after repeated failure: {}", score.confidence);
    }

    #[test]
    fn momentum_is_negative_on_decline() {
        let mut eval = ApproachEvaluator::new();
        eval.observe(5, 5, 300); // good
        eval.observe(5, 5, 300); // good
        eval.observe(0, 5, 20);  // bad
        let score = eval.observe(0, 5, 20); // bad
        assert!(score.momentum < 0.0, "momentum should be negative: {}", score.momentum);
    }
}
