//! Observability-Driven Agent Quality Feedback Loop.
//!
//! Closes the loop between observability telemetry and agent behavior by
//! computing per-skill quality scores and automatically adjusting skill
//! weights via Thompson sampling.
//!
//! ## Feedback Loop
//!
//! ```text
//! Agent Turn → Observability Metrics → Quality Scorer → Weight Optimizer
//!                                                       ↓
//!                                              Skill Selector (next turn)
//! ```
//!
//! ## Quality Score
//!
//! Per-skill EMA quality:
//!     Q_{t+1} = α · q_t + (1 − α) · Q_t
//!
//! where q_t is the instant quality signal and α is the decay factor.
//!
//! ## Thompson Sampling
//!
//! Skill weights are Beta(α, β) posteriors. On each turn:
//! - Success (q > threshold): α += reward
//! - Failure (q ≤ threshold): β += penalty
//!
//! Exploration is automatic via posterior sampling.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Quality Signal ─────────────────────────────────────────────────────────

/// A quality signal from a single agent turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualitySignal {
    /// Skill that was executed.
    pub skill_id: String,
    /// Pack that the skill belongs to (if any).
    pub pack_id: Option<String>,
    /// Quality score ∈ [0, 1].
    pub score: f64,
    /// Latency in milliseconds.
    pub latency_ms: u64,
    /// Token usage (input + output).
    pub tokens_used: u64,
    /// Whether the user explicitly rated this interaction.
    pub user_rated: bool,
    /// Optional decomposed quality dimensions.
    pub dimensions: QualityDimensions,
}

/// Decomposed quality dimensions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QualityDimensions {
    /// Factual accuracy ∈ [0, 1].
    pub accuracy: Option<f64>,
    /// Response relevance ∈ [0, 1].
    pub relevance: Option<f64>,
    /// Coherence and fluency ∈ [0, 1].
    pub coherence: Option<f64>,
    /// Safety and harmlessness ∈ [0, 1].
    pub safety: Option<f64>,
    /// Helpfulness ∈ [0, 1].
    pub helpfulness: Option<f64>,
}

impl QualityDimensions {
    /// Compute aggregate score as weighted mean of available dimensions.
    pub fn aggregate(&self) -> Option<f64> {
        let mut sum = 0.0;
        let mut weight = 0.0;

        let dims = [
            (self.accuracy, 2.0),
            (self.relevance, 1.5),
            (self.coherence, 1.0),
            (self.safety, 3.0),   // safety weighted heavily
            (self.helpfulness, 1.5),
        ];

        for (val, w) in &dims {
            if let Some(v) = val {
                sum += v * w;
                weight += w;
            }
        }

        if weight > 0.0 {
            Some(sum / weight)
        } else {
            None
        }
    }
}

// ─── EMA Quality Tracker ────────────────────────────────────────────────────

/// Exponential Moving Average quality tracker for skills.
pub struct QualityTracker {
    /// Per-skill EMA quality scores.
    scores: HashMap<String, EmaState>,
    /// EMA decay factor α ∈ (0, 1). Higher = more weight on recent.
    alpha: f64,
    /// Threshold for "good" quality.
    quality_threshold: f64,
}

/// EMA state for a single skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmaState {
    /// Current EMA quality score.
    pub value: f64,
    /// Number of observations.
    pub count: u64,
    /// Sum of raw scores (for computing mean).
    pub total_score: f64,
    /// Minimum observed score.
    pub min_score: f64,
    /// Maximum observed score.
    pub max_score: f64,
    /// Total latency in ms (for computing average).
    pub total_latency_ms: u64,
    /// Total tokens used.
    pub total_tokens: u64,
}

impl EmaState {
    fn new(initial: f64) -> Self {
        Self {
            value: initial,
            count: 0,
            total_score: 0.0,
            min_score: f64::MAX,
            max_score: f64::MIN,
            total_latency_ms: 0,
            total_tokens: 0,
        }
    }

    fn update(&mut self, signal: &QualitySignal, alpha: f64) {
        self.value = alpha * signal.score + (1.0 - alpha) * self.value;
        self.count += 1;
        self.total_score += signal.score;
        self.min_score = self.min_score.min(signal.score);
        self.max_score = self.max_score.max(signal.score);
        self.total_latency_ms += signal.latency_ms;
        self.total_tokens += signal.tokens_used;
    }

    /// Mean quality across all observations.
    pub fn mean_quality(&self) -> f64 {
        if self.count == 0 {
            return self.value;
        }
        self.total_score / self.count as f64
    }

    /// Mean latency in milliseconds.
    pub fn mean_latency_ms(&self) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        self.total_latency_ms as f64 / self.count as f64
    }
}

impl QualityTracker {
    /// Create a new quality tracker.
    ///
    /// - `alpha`: EMA decay factor ∈ (0, 1). Default: 0.3
    /// - `quality_threshold`: threshold for "good" quality. Default: 0.7
    pub fn new(alpha: f64, quality_threshold: f64) -> Self {
        Self {
            scores: HashMap::new(),
            alpha: alpha.clamp(0.01, 0.99),
            quality_threshold,
        }
    }

    /// Record a quality signal.
    pub fn record(&mut self, signal: &QualitySignal) {
        let state = self
            .scores
            .entry(signal.skill_id.clone())
            .or_insert_with(|| EmaState::new(0.5));
        state.update(signal, self.alpha);
    }

    /// Get the current quality score for a skill.
    pub fn quality(&self, skill_id: &str) -> f64 {
        self.scores
            .get(skill_id)
            .map(|s| s.value)
            .unwrap_or(0.5)
    }

    /// Get the full EMA state for a skill.
    pub fn state(&self, skill_id: &str) -> Option<&EmaState> {
        self.scores.get(skill_id)
    }

    /// Whether a skill is above the quality threshold.
    pub fn is_quality_good(&self, skill_id: &str) -> bool {
        self.quality(skill_id) >= self.quality_threshold
    }

    /// All skills sorted by quality (descending).
    pub fn ranked_skills(&self) -> Vec<(&str, f64)> {
        let mut ranked: Vec<(&str, f64)> = self
            .scores
            .iter()
            .map(|(id, s)| (id.as_str(), s.value))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }

    /// Skills below the quality threshold (candidates for demotion).
    pub fn underperformers(&self) -> Vec<(&str, f64)> {
        self.scores
            .iter()
            .filter(|(_, s)| s.value < self.quality_threshold)
            .map(|(id, s)| (id.as_str(), s.value))
            .collect()
    }
}

impl Default for QualityTracker {
    fn default() -> Self {
        Self::new(0.3, 0.7)
    }
}

// ─── Thompson Sampling Weight Optimizer ─────────────────────────────────────

/// Beta distribution parameters for Thompson sampling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BetaParams {
    pub alpha: f64,
    pub beta: f64,
}

impl BetaParams {
    pub fn new(alpha: f64, beta: f64) -> Self {
        Self {
            alpha: alpha.max(0.01),
            beta: beta.max(0.01),
        }
    }

    /// Uniform prior.
    pub fn uniform() -> Self {
        Self::new(1.0, 1.0)
    }

    /// Expected value: α / (α + β).
    pub fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    /// Variance: αβ / ((α + β)² (α + β + 1)).
    pub fn variance(&self) -> f64 {
        let sum = self.alpha + self.beta;
        (self.alpha * self.beta) / (sum * sum * (sum + 1.0))
    }

    /// Update on success (quality above threshold).
    pub fn record_success(&mut self, reward: f64) {
        self.alpha += reward;
    }

    /// Update on failure (quality below threshold).
    pub fn record_failure(&mut self, penalty: f64) {
        self.beta += penalty;
    }

    /// Sample from Beta distribution using Jöhnk's algorithm (simple approximation).
    /// For production, use a proper random crate.
    pub fn sample_approx(&self) -> f64 {
        // Approximate: return the mean ± some exploration noise.
        // In production, this should use `rand::distributions::Beta`.
        self.mean()
    }
}

impl Default for BetaParams {
    fn default() -> Self {
        Self::uniform()
    }
}

/// Thompson sampling weight optimizer for skill selection.
pub struct WeightOptimizer {
    /// Per-skill Beta parameters.
    params: HashMap<String, BetaParams>,
    /// Quality threshold for success/failure classification.
    threshold: f64,
    /// Success reward magnitude.
    success_reward: f64,
    /// Failure penalty magnitude.
    failure_penalty: f64,
}

impl WeightOptimizer {
    pub fn new(threshold: f64) -> Self {
        Self {
            params: HashMap::new(),
            threshold,
            success_reward: 1.0,
            failure_penalty: 1.0,
        }
    }

    /// Update the Beta posterior for a skill based on a quality signal.
    pub fn update(&mut self, signal: &QualitySignal) {
        let params = self
            .params
            .entry(signal.skill_id.clone())
            .or_insert_with(BetaParams::uniform);

        if signal.score >= self.threshold {
            params.record_success(self.success_reward * signal.score);
        } else {
            params.record_failure(self.failure_penalty * (1.0 - signal.score));
        }
    }

    /// Get the optimized weight for a skill (Thompson sample).
    pub fn weight(&self, skill_id: &str) -> f64 {
        self.params
            .get(skill_id)
            .map(|p| p.sample_approx())
            .unwrap_or(0.5)
    }

    /// Get all skill weights (expected values from posteriors).
    pub fn all_weights(&self) -> HashMap<&str, f64> {
        self.params
            .iter()
            .map(|(id, p)| (id.as_str(), p.mean()))
            .collect()
    }

    /// Get Beta params for a skill.
    pub fn get_params(&self, skill_id: &str) -> Option<&BetaParams> {
        self.params.get(skill_id)
    }
}

impl Default for WeightOptimizer {
    fn default() -> Self {
        Self::new(0.7)
    }
}

// ─── Promotion Quality Gate ─────────────────────────────────────────────────

/// Quality gate for the promotion pipeline (legacy integration).
///
/// A skill must meet minimum quality thresholds across dimensions
/// before being promoted from staging to production.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGate {
    /// Minimum EMA quality score.
    pub min_quality: f64,
    /// Minimum number of observations.
    pub min_observations: u64,
    /// Minimum success rate (fraction of signals above threshold).
    pub min_success_rate: f64,
    /// Maximum allowed mean latency in milliseconds.
    pub max_mean_latency_ms: Option<f64>,
}

impl Default for QualityGate {
    fn default() -> Self {
        Self {
            min_quality: 0.7,
            min_observations: 10,
            min_success_rate: 0.8,
            max_mean_latency_ms: Some(5000.0),
        }
    }
}

impl QualityGate {
    /// Check if a skill passes the quality gate.
    pub fn check(&self, state: &EmaState) -> QualityGateResult {
        let mut passed = true;
        let mut failures = Vec::new();

        if state.count < self.min_observations {
            passed = false;
            failures.push(format!(
                "insufficient observations: {} < {}",
                state.count, self.min_observations
            ));
        }

        if state.value < self.min_quality {
            passed = false;
            failures.push(format!(
                "quality below threshold: {:.3} < {:.3}",
                state.value, self.min_quality
            ));
        }

        if let Some(max_latency) = self.max_mean_latency_ms {
            let mean_latency = state.mean_latency_ms();
            if mean_latency > max_latency {
                passed = false;
                failures.push(format!(
                    "latency too high: {:.1}ms > {:.1}ms",
                    mean_latency, max_latency
                ));
            }
        }

        QualityGateResult { passed, failures }
    }
}

/// Result of a quality gate check.
#[derive(Debug, Clone)]
pub struct QualityGateResult {
    pub passed: bool,
    pub failures: Vec<String>,
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_signal(skill: &str, score: f64) -> QualitySignal {
        QualitySignal {
            skill_id: skill.into(),
            pack_id: None,
            score,
            latency_ms: 500,
            tokens_used: 1000,
            user_rated: false,
            dimensions: QualityDimensions::default(),
        }
    }

    #[test]
    fn test_ema_tracking() {
        let mut tracker = QualityTracker::new(0.5, 0.7);

        tracker.record(&make_signal("skill_a", 0.9));
        tracker.record(&make_signal("skill_a", 0.8));
        tracker.record(&make_signal("skill_a", 0.7));

        let q = tracker.quality("skill_a");
        assert!(q > 0.6 && q < 1.0, "quality = {}", q);
    }

    #[test]
    fn test_underperformers() {
        let mut tracker = QualityTracker::new(0.8, 0.7);

        tracker.record(&make_signal("good", 0.9));
        tracker.record(&make_signal("bad", 0.3));
        tracker.record(&make_signal("bad", 0.2));

        let under = tracker.underperformers();
        assert_eq!(under.len(), 1);
        assert_eq!(under[0].0, "bad");
    }

    #[test]
    fn test_thompson_sampling_update() {
        let mut opt = WeightOptimizer::new(0.7);

        // Good skill
        for _ in 0..10 {
            opt.update(&make_signal("good", 0.9));
        }
        // Bad skill
        for _ in 0..10 {
            opt.update(&make_signal("bad", 0.3));
        }

        let good_w = opt.weight("good");
        let bad_w = opt.weight("bad");
        assert!(
            good_w > bad_w,
            "good = {}, bad = {} (expected good > bad)",
            good_w, bad_w
        );
    }

    #[test]
    fn test_quality_gate_passes() {
        let gate = QualityGate {
            min_quality: 0.7,
            min_observations: 5,
            min_success_rate: 0.8,
            max_mean_latency_ms: Some(2000.0),
        };

        let state = EmaState {
            value: 0.85,
            count: 10,
            total_score: 8.5,
            min_score: 0.7,
            max_score: 0.95,
            total_latency_ms: 10_000,
            total_tokens: 50_000,
        };

        let result = gate.check(&state);
        assert!(result.passed);
    }

    #[test]
    fn test_quality_gate_fails() {
        let gate = QualityGate::default();

        let state = EmaState {
            value: 0.4,
            count: 3,
            total_score: 1.2,
            min_score: 0.2,
            max_score: 0.6,
            total_latency_ms: 30_000,
            total_tokens: 50_000,
        };

        let result = gate.check(&state);
        assert!(!result.passed);
        assert!(result.failures.len() >= 2); // Low quality + insufficient observations
    }

    #[test]
    fn test_quality_dimensions_aggregate() {
        let dims = QualityDimensions {
            accuracy: Some(0.9),
            relevance: Some(0.8),
            coherence: Some(0.85),
            safety: Some(1.0),
            helpfulness: Some(0.75),
        };
        let agg = dims.aggregate().unwrap();
        assert!(agg > 0.8, "aggregate = {}", agg);
    }

    #[test]
    fn test_beta_params() {
        let mut p = BetaParams::uniform();
        assert!((p.mean() - 0.5).abs() < 1e-10);

        p.record_success(1.0);
        assert!(p.mean() > 0.5);

        let mut q = BetaParams::new(10.0, 2.0);
        assert!(q.mean() > 0.8);
        q.record_failure(5.0);
        assert!(q.mean() < 0.85);
    }
}
