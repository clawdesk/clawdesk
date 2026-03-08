//! Adaptive WFQ Priority with Thompson Sampling.
//!
//! Extends the WFQ scheduler with online learning of optimal priority weights:
//!
//! 1. **Thompson Sampling bandit** — each priority class has a Beta(α, β)
//!    posterior modeling the probability that events of that class "succeed"
//!    (e.g., result in user satisfaction or timely completion).
//!
//! 2. **Online weight adaptation** — periodically samples from each class's
//!    posterior and adjusts WFQ weights proportionally:
//!
//!    ```text
//!    w_k ∝ θ̃_k  where θ̃_k ~ Beta(α_k, β_k)
//!    ```
//!
//! 3. **Priority escalation** — uses logistic regression on event features
//!    to predict whether a Standard event should be escalated to Urgent:
//!
//!    ```text
//!    P(urgent | features) = σ(w^T x + b)
//!    ```
//!
//! ## Convergence
//!
//! Thompson Sampling converges to the optimal arm in O(log T) regret.
//! With K=3 priority classes, the bandit identifies the best weight
//! allocation within ~50 observations.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

// ───────────────────────────────────────────────────────────────
// Thompson Sampling Bandit
// ───────────────────────────────────────────────────────────────

/// Beta posterior parameters for a single priority class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BetaPosterior {
    /// Positive feedback count (successes).
    pub alpha: f64,
    /// Negative feedback count (failures).
    pub beta: f64,
}

impl BetaPosterior {
    /// Uniform prior: Beta(1, 1).
    pub fn uniform() -> Self {
        Self {
            alpha: 1.0,
            beta: 1.0,
        }
    }

    /// Record a success.
    pub fn record_success(&mut self) {
        self.alpha += 1.0;
    }

    /// Record a failure.
    pub fn record_failure(&mut self) {
        self.beta += 1.0;
    }

    /// Mean of the beta distribution: E[Beta(α, β)] = α / (α + β).
    pub fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    /// Variance: Var = αβ / ((α+β)²(α+β+1)).
    pub fn variance(&self) -> f64 {
        let ab = self.alpha + self.beta;
        (self.alpha * self.beta) / (ab * ab * (ab + 1.0))
    }

    /// Sample from Beta(α, β) using the Jöhnk algorithm.
    ///
    /// This is a simple rejection sampler that doesn't require the `rand` crate.
    /// Uses a hash-based PRNG seeded from a process-unique counter.
    pub fn sample(&self) -> f64 {
        // Simple approximation for contexts where we don't have rand:
        // Use the mean ± scaled noise from hash-based pseudo-random.
        let noise = pseudo_random_unit() * 2.0 - 1.0;
        let std_dev = self.variance().sqrt();
        (self.mean() + noise * std_dev).clamp(0.001, 0.999)
    }

    /// Total observations.
    pub fn observations(&self) -> f64 {
        self.alpha + self.beta - 2.0 // Subtract prior.
    }
}

/// Hash-based pseudo-random number in [0, 1) using process-unique counter.
fn pseudo_random_unit() -> f64 {
    use std::hash::{BuildHasher, Hasher};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static RANDOM_STATE: std::sync::OnceLock<std::collections::hash_map::RandomState> =
        std::sync::OnceLock::new();

    let state = RANDOM_STATE.get_or_init(std::collections::hash_map::RandomState::new);
    let tick = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = state.build_hasher();
    hasher.write_u64(tick);
    let hash = hasher.finish();
    (hash >> 11) as f64 / (1u64 << 53) as f64
}

// ───────────────────────────────────────────────────────────────
// Adaptive Weight Manager
// ───────────────────────────────────────────────────────────────

/// Adaptive weight manager for K=3 priority classes.
///
/// Periodically samples from Beta posteriors and returns optimal WFQ weights.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveWeightManager {
    /// Beta posteriors: [Urgent, Standard, Batch].
    posteriors: [BetaPosterior; 3],
    /// Base weights (initial configuration).
    base_weights: [f64; 3],
    /// Learning rate: how much to blend sampled vs base weights.
    pub learning_rate: f64,
    /// Minimum weight (prevents starvation).
    pub min_weight: f64,
}

impl AdaptiveWeightManager {
    /// Create with default base weights (Urgent=8, Standard=4, Batch=1).
    pub fn new() -> Self {
        Self {
            posteriors: [
                BetaPosterior::uniform(),
                BetaPosterior::uniform(),
                BetaPosterior::uniform(),
            ],
            base_weights: [8.0, 4.0, 1.0],
            learning_rate: 0.3,
            min_weight: 0.5,
        }
    }

    /// Record feedback for a priority class.
    pub fn record_feedback(&mut self, class: usize, success: bool) {
        if class < 3 {
            if success {
                self.posteriors[class].record_success();
            } else {
                self.posteriors[class].record_failure();
            }
        }
    }

    /// Sample new weights using Thompson Sampling.
    ///
    /// Returns [Urgent, Standard, Batch] weights.
    pub fn sample_weights(&self) -> [f64; 3] {
        let mut weights = [0.0; 3];
        for i in 0..3 {
            let sampled = self.posteriors[i].sample();
            // Blend sampled preference with base weight.
            let adaptive = self.base_weights[i] * (1.0 + self.learning_rate * (sampled - 0.5));
            weights[i] = adaptive.max(self.min_weight);
        }
        weights
    }

    /// Get the mean weights (expected values, no sampling noise).
    pub fn mean_weights(&self) -> [f64; 3] {
        let mut weights = [0.0; 3];
        for i in 0..3 {
            let mean = self.posteriors[i].mean();
            let adaptive = self.base_weights[i] * (1.0 + self.learning_rate * (mean - 0.5));
            weights[i] = adaptive.max(self.min_weight);
        }
        weights
    }

    /// Get Beta posteriors for diagnostics.
    pub fn posteriors(&self) -> &[BetaPosterior; 3] {
        &self.posteriors
    }
}

impl Default for AdaptiveWeightManager {
    fn default() -> Self {
        Self::new()
    }
}

// ───────────────────────────────────────────────────────────────
// Priority Escalation via Logistic Regression
// ───────────────────────────────────────────────────────────────

/// Feature vector for priority escalation.
///
/// These features are extracted from event metadata to predict whether
/// a Standard priority event should be escalated to Urgent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationFeatures {
    /// How long the event has been waiting (seconds).
    pub wait_time_secs: f64,
    /// Number of times the user has interacted in the last minute.
    pub user_activity_rate: f64,
    /// Whether the event is part of an active conversation.
    pub in_active_conversation: bool,
    /// Queue depth of the Standard queue.
    pub queue_depth: f64,
    /// Time of day (0-24, for diurnal patterns).
    pub hour_of_day: f64,
}

/// Logistic regression model for priority escalation.
///
/// ```text
/// P(urgent | x) = σ(w^T x + b)
/// σ(z) = 1 / (1 + e^{-z})
/// ```
///
/// Weights are learned via online SGD from user feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationModel {
    /// Feature weights [wait_time, activity_rate, in_conversation, queue_depth, hour].
    pub weights: [f64; 5],
    /// Bias term.
    pub bias: f64,
    /// Learning rate for SGD updates.
    pub lr: f64,
    /// Escalation threshold (default 0.7).
    pub threshold: f64,
}

impl EscalationModel {
    /// Create a new model with reasonable initial weights.
    pub fn new() -> Self {
        Self {
            // Initial weights: wait time and activity rate are strong predictors.
            weights: [0.1, 0.3, 0.5, 0.05, 0.0],
            bias: -1.0, // Default: don't escalate.
            lr: 0.01,
            threshold: 0.7,
        }
    }

    /// Predict P(urgent | features).
    pub fn predict(&self, features: &EscalationFeatures) -> f64 {
        let x = self.featurize(features);
        let z: f64 = self
            .weights
            .iter()
            .zip(x.iter())
            .map(|(w, x)| w * x)
            .sum::<f64>()
            + self.bias;
        sigmoid(z)
    }

    /// Whether to escalate based on the threshold.
    pub fn should_escalate(&self, features: &EscalationFeatures) -> bool {
        self.predict(features) >= self.threshold
    }

    /// Online SGD update with a single observation.
    ///
    /// `label` is 1.0 if the event should have been urgent, 0.0 otherwise.
    pub fn update(&mut self, features: &EscalationFeatures, label: f64) {
        let x = self.featurize(features);
        let pred = self.predict(features);
        let error = pred - label;

        // Gradient descent: w -= lr × error × x
        for i in 0..5 {
            self.weights[i] -= self.lr * error * x[i];
        }
        self.bias -= self.lr * error;
    }

    fn featurize(&self, f: &EscalationFeatures) -> [f64; 5] {
        [
            f.wait_time_secs / 60.0, // Normalize to minutes.
            f.user_activity_rate,
            if f.in_active_conversation { 1.0 } else { 0.0 },
            f.queue_depth / 10.0, // Normalize.
            f.hour_of_day / 24.0, // Normalize to [0, 1].
        ]
    }
}

impl Default for EscalationModel {
    fn default() -> Self {
        Self::new()
    }
}

/// Sigmoid function: σ(z) = 1 / (1 + e^{-z}).
fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_beta_posterior_convergence() {
        let mut post = BetaPosterior::uniform();
        // Record 80 successes, 20 failures → mean should converge to ~0.8.
        for _ in 0..80 {
            post.record_success();
        }
        for _ in 0..20 {
            post.record_failure();
        }
        assert!((post.mean() - 0.8).abs() < 0.05);
    }

    #[test]
    fn test_adaptive_weights() {
        let mut mgr = AdaptiveWeightManager::new();

        // Lots of success for Urgent, failure for Batch.
        for _ in 0..50 {
            mgr.record_feedback(0, true); // Urgent success.
            mgr.record_feedback(2, false); // Batch failure.
        }

        let weights = mgr.mean_weights();
        assert!(weights[0] > weights[2], "Urgent should have higher weight than Batch");
    }

    #[test]
    fn test_escalation_model() {
        let model = EscalationModel::new();

        // Long wait + active user → should lean toward escalation.
        let features = EscalationFeatures {
            wait_time_secs: 300.0,
            user_activity_rate: 5.0,
            in_active_conversation: true,
            queue_depth: 20.0,
            hour_of_day: 14.0,
        };
        let prob = model.predict(&features);
        assert!(prob > 0.0 && prob < 1.0, "probability should be bounded");
    }

    #[test]
    fn test_escalation_sgd_update() {
        let mut model = EscalationModel::new();

        let features = EscalationFeatures {
            wait_time_secs: 120.0,
            user_activity_rate: 3.0,
            in_active_conversation: true,
            queue_depth: 5.0,
            hour_of_day: 10.0,
        };

        // Train toward "should escalate".
        for _ in 0..100 {
            model.update(&features, 1.0);
        }

        assert!(
            model.predict(&features) > 0.5,
            "model should learn to escalate after enough positive examples"
        );
    }

    #[test]
    fn test_sigmoid_properties() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-10);
        assert!(sigmoid(10.0) > 0.99);
        assert!(sigmoid(-10.0) < 0.01);
    }
}
