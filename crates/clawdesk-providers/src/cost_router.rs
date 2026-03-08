//! Cost-Aware Model Routing — constrained optimization for model selection.
//!
//! Implements the recommendation from §4.3 of the multi-agent analysis:
//!
//! ```text
//! minimize  cost(m)
//!   s.t.    quality(m, t) ≥ q_min
//!           latency(m, t) ≤ l_max
//! ```
//!
//! Each model-task pair has EWMA estimates for quality, cost, and latency
//! that converge after ~10 observations. Model routing selects the cheapest
//! model meeting quality and latency constraints.
//!
//! ## Algorithm
//!
//! 1. For each candidate model, compute EWMA estimates of quality, cost, latency.
//! 2. Filter models that violate constraints (quality < q_min or latency > l_max).
//! 3. Among feasible models, select the one with minimum cost.
//! 4. If no model is feasible, relax constraints and warn.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

// ───────────────────────────────────────────────────────────────
// Configuration
// ───────────────────────────────────────────────────────────────

/// Configuration for cost-aware model routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRouterConfig {
    /// Minimum quality score ∈ [0, 1] (q_min).
    #[serde(default = "default_min_quality")]
    pub min_quality: f64,
    /// Maximum acceptable latency in milliseconds (l_max).
    #[serde(default = "default_max_latency_ms")]
    pub max_latency_ms: u64,
    /// EWMA smoothing factor α ∈ (0, 1). Higher = more responsive.
    #[serde(default = "default_ewma_alpha")]
    pub ewma_alpha: f64,
    /// Minimum observations before trusting EWMA estimates.
    #[serde(default = "default_min_observations")]
    pub min_observations: u32,
    /// Whether to allow constraint relaxation when no model is feasible.
    #[serde(default = "default_allow_relaxation")]
    pub allow_relaxation: bool,
    /// Cost weight for tie-breaking (higher = stronger cost preference).
    #[serde(default = "default_cost_weight")]
    pub cost_weight: f64,
}

fn default_min_quality() -> f64 { 0.7 }
fn default_max_latency_ms() -> u64 { 30_000 }
fn default_ewma_alpha() -> f64 { 0.2 }
fn default_min_observations() -> u32 { 10 }
fn default_allow_relaxation() -> bool { true }
fn default_cost_weight() -> f64 { 1.0 }

impl Default for CostRouterConfig {
    fn default() -> Self {
        Self {
            min_quality: default_min_quality(),
            max_latency_ms: default_max_latency_ms(),
            ewma_alpha: default_ewma_alpha(),
            min_observations: default_min_observations(),
            allow_relaxation: default_allow_relaxation(),
            cost_weight: default_cost_weight(),
        }
    }
}

// ───────────────────────────────────────────────────────────────
// EWMA tracker
// ───────────────────────────────────────────────────────────────

/// EWMA (Exponentially Weighted Moving Average) tracker for a single metric.
///
/// `x̂_{n} = α · x_n + (1 - α) · x̂_{n-1}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EwmaEstimate {
    /// Current smoothed estimate.
    pub value: f64,
    /// Smoothing factor.
    pub alpha: f64,
    /// Number of observations.
    pub observations: u32,
}

impl EwmaEstimate {
    pub fn new(alpha: f64) -> Self {
        Self {
            value: 0.0,
            alpha,
            observations: 0,
        }
    }

    /// Incorporate a new observation.
    pub fn update(&mut self, sample: f64) {
        if self.observations == 0 {
            self.value = sample;
        } else {
            self.value = self.alpha * sample + (1.0 - self.alpha) * self.value;
        }
        self.observations += 1;
    }

    /// Whether we have enough data to trust this estimate.
    pub fn is_warm(&self, min_observations: u32) -> bool {
        self.observations >= min_observations
    }
}

// ───────────────────────────────────────────────────────────────
// Model statistics
// ───────────────────────────────────────────────────────────────

/// Per-model, per-task-type statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTaskStats {
    /// Quality metric EWMA ∈ [0, 1] (e.g., task success rate).
    pub quality: EwmaEstimate,
    /// Cost per request EWMA in USD.
    pub cost: EwmaEstimate,
    /// Latency EWMA in milliseconds.
    pub latency_ms: EwmaEstimate,
    /// Total requests routed to this model-task pair.
    pub total_requests: u64,
    /// Total cost accumulated.
    pub total_cost: f64,
}

impl ModelTaskStats {
    pub fn new(alpha: f64) -> Self {
        Self {
            quality: EwmaEstimate::new(alpha),
            cost: EwmaEstimate::new(alpha),
            latency_ms: EwmaEstimate::new(alpha),
            total_requests: 0,
            total_cost: 0.0,
        }
    }
}

/// Model metadata for cost estimation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCostProfile {
    /// Model identifier (e.g., "claude-sonnet-4-20250514").
    pub model_id: String,
    /// Provider name.
    pub provider: String,
    /// Cost per input token in USD.
    pub input_cost_per_token: f64,
    /// Cost per output token in USD.
    pub output_cost_per_token: f64,
    /// Maximum context window size.
    pub max_tokens: u32,
    /// Known capability tier (higher = more capable).
    pub capability_tier: u8,
    /// Whether this model supports streaming.
    pub supports_streaming: bool,
    /// Whether this model supports tool use.
    pub supports_tools: bool,
}

/// A routing decision from the cost-aware router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRoutingDecision {
    /// Selected model.
    pub model_id: String,
    /// Provider for the selected model.
    pub provider: String,
    /// Estimated quality for this task type.
    pub estimated_quality: f64,
    /// Estimated cost for this request.
    pub estimated_cost: f64,
    /// Estimated latency in milliseconds.
    pub estimated_latency_ms: f64,
    /// Whether constraints were relaxed to find this model.
    pub constraints_relaxed: bool,
    /// Number of candidate models considered.
    pub candidates_considered: u32,
    /// Reason for selection.
    pub reason: String,
}

// ───────────────────────────────────────────────────────────────
// Cost-Aware Router
// ───────────────────────────────────────────────────────────────

/// Key for per-model, per-task-type statistics.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ModelTaskKey {
    pub model_id: String,
    pub task_type: String,
}

/// Cost-aware model router.
///
/// Tracks per-model, per-task-type performance and selects the cheapest model
/// meeting quality and latency constraints.
pub struct CostAwareRouter {
    /// Configuration.
    config: CostRouterConfig,
    /// Registered model cost profiles.
    models: DashMap<String, ModelCostProfile>,
    /// Per (model, task_type) statistics.
    stats: DashMap<ModelTaskKey, ModelTaskStats>,
}

impl CostAwareRouter {
    pub fn new(config: CostRouterConfig) -> Self {
        Self {
            config,
            models: DashMap::new(),
            stats: DashMap::new(),
        }
    }

    /// Register a model's cost profile.
    pub fn register_model(&self, profile: ModelCostProfile) {
        self.models.insert(profile.model_id.clone(), profile);
    }

    /// Remove a model.
    pub fn remove_model(&self, model_id: &str) {
        self.models.remove(model_id);
    }

    /// Select the best model for a task type.
    ///
    /// Algorithm:
    /// 1. Filter by tool/streaming requirements.
    /// 2. Compute EWMA quality/cost/latency per candidate.
    /// 3. Filter by quality ≥ q_min AND latency ≤ l_max.
    /// 4. Select minimum cost among feasible.
    /// 5. If no feasible model, relax constraints or pick best-quality.
    pub fn select(
        &self,
        task_type: &str,
        requires_tools: bool,
        requires_streaming: bool,
        estimated_input_tokens: u32,
        estimated_output_tokens: u32,
    ) -> Option<CostRoutingDecision> {
        let candidates: Vec<_> = self.models.iter().collect();
        if candidates.is_empty() {
            return None;
        }

        let mut scored: Vec<(String, String, f64, f64, f64, bool)> = Vec::new();

        for entry in &candidates {
            let profile = entry.value();

            // Filter by capability requirements.
            if requires_tools && !profile.supports_tools {
                continue;
            }
            if requires_streaming && !profile.supports_streaming {
                continue;
            }

            let key = ModelTaskKey {
                model_id: profile.model_id.clone(),
                task_type: task_type.to_string(),
            };

            let (quality, cost, latency, warm) = if let Some(stats) = self.stats.get(&key) {
                let s = stats.value();
                let warm = s.quality.is_warm(self.config.min_observations)
                    && s.cost.is_warm(self.config.min_observations);
                (s.quality.value, s.cost.value, s.latency_ms.value, warm)
            } else {
                // No data yet — use cost profile for estimation.
                let estimated_cost = profile.input_cost_per_token
                    * estimated_input_tokens as f64
                    + profile.output_cost_per_token * estimated_output_tokens as f64;
                // Default quality based on capability tier.
                let default_quality = 0.5 + 0.1 * profile.capability_tier as f64;
                (default_quality.min(1.0), estimated_cost, 5000.0, false)
            };

            scored.push((
                profile.model_id.clone(),
                profile.provider.clone(),
                quality,
                cost,
                latency,
                warm,
            ));
        }

        if scored.is_empty() {
            return None;
        }

        // Phase 1: Find feasible models (quality ≥ q_min AND latency ≤ l_max).
        let feasible: Vec<_> = scored
            .iter()
            .filter(|(_, _, q, _, l, _)| {
                *q >= self.config.min_quality && *l <= self.config.max_latency_ms as f64
            })
            .collect();

        if let Some(best) = feasible
            .iter()
            .min_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal))
        {
            return Some(CostRoutingDecision {
                model_id: best.0.clone(),
                provider: best.1.clone(),
                estimated_quality: best.2,
                estimated_cost: best.3,
                estimated_latency_ms: best.4,
                constraints_relaxed: false,
                candidates_considered: scored.len() as u32,
                reason: "minimum cost among feasible models".into(),
            });
        }

        // Phase 2: No feasible model — relax constraints.
        if self.config.allow_relaxation {
            warn!(task_type, "no model meets constraints; relaxing");

            // Pick the model with highest quality (regardless of cost/latency).
            if let Some(best) = scored
                .iter()
                .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
            {
                return Some(CostRoutingDecision {
                    model_id: best.0.clone(),
                    provider: best.1.clone(),
                    estimated_quality: best.2,
                    estimated_cost: best.3,
                    estimated_latency_ms: best.4,
                    constraints_relaxed: true,
                    candidates_considered: scored.len() as u32,
                    reason: "constraints relaxed; selected highest quality".into(),
                });
            }
        }

        None
    }

    /// Record an observation after a model completes a task.
    pub fn record_observation(
        &self,
        model_id: &str,
        task_type: &str,
        quality: f64,
        cost: f64,
        latency: Duration,
    ) {
        let key = ModelTaskKey {
            model_id: model_id.into(),
            task_type: task_type.into(),
        };

        let mut entry = self
            .stats
            .entry(key)
            .or_insert_with(|| ModelTaskStats::new(self.config.ewma_alpha));

        let stats = entry.value_mut();
        stats.quality.update(quality);
        stats.cost.update(cost);
        stats.latency_ms.update(latency.as_millis() as f64);
        stats.total_requests += 1;
        stats.total_cost += cost;

        debug!(
            model = model_id,
            task_type,
            quality,
            cost,
            latency_ms = latency.as_millis(),
            ewma_quality = stats.quality.value,
            ewma_cost = stats.cost.value,
            "model observation recorded"
        );
    }

    /// Get current statistics for a model-task pair.
    pub fn get_stats(&self, model_id: &str, task_type: &str) -> Option<ModelTaskStats> {
        let key = ModelTaskKey {
            model_id: model_id.into(),
            task_type: task_type.into(),
        };
        self.stats.get(&key).map(|e| e.value().clone())
    }

    /// Get a cost summary across all models.
    pub fn cost_summary(&self) -> Vec<ModelCostSummary> {
        let mut by_model: HashMap<String, (f64, u64)> = HashMap::new();
        for entry in self.stats.iter() {
            let stats = entry.value();
            let counter = by_model
                .entry(entry.key().model_id.clone())
                .or_insert((0.0, 0));
            counter.0 += stats.total_cost;
            counter.1 += stats.total_requests;
        }

        by_model
            .into_iter()
            .map(|(model_id, (total_cost, total_requests))| ModelCostSummary {
                model_id,
                total_cost,
                total_requests,
                avg_cost_per_request: if total_requests > 0 {
                    total_cost / total_requests as f64
                } else {
                    0.0
                },
            })
            .collect()
    }

    /// Number of registered models.
    pub fn model_count(&self) -> usize {
        self.models.len()
    }
}

/// Cost summary for a single model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCostSummary {
    pub model_id: String,
    pub total_cost: f64,
    pub total_requests: u64,
    pub avg_cost_per_request: f64,
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_profile(id: &str, provider: &str, input_cost: f64, output_cost: f64, tier: u8) -> ModelCostProfile {
        ModelCostProfile {
            model_id: id.into(),
            provider: provider.into(),
            input_cost_per_token: input_cost,
            output_cost_per_token: output_cost,
            max_tokens: 128_000,
            capability_tier: tier,
            supports_streaming: true,
            supports_tools: true,
        }
    }

    #[test]
    fn test_selects_cheapest_feasible() {
        let router = CostAwareRouter::new(CostRouterConfig {
            min_quality: 0.5,
            ..Default::default()
        });

        // Expensive but high quality.
        router.register_model(make_profile("opus", "anthropic", 0.015, 0.075, 5));
        // Mid-range.
        router.register_model(make_profile("sonnet", "anthropic", 0.003, 0.015, 4));
        // Cheap.
        router.register_model(make_profile("haiku", "anthropic", 0.00025, 0.00125, 2));

        let decision = router.select("coding", false, false, 1000, 500).unwrap();
        // Should pick haiku (cheapest) since default quality for tier 2 = 0.7 (≥ 0.5).
        assert_eq!(decision.model_id, "haiku");
        assert!(!decision.constraints_relaxed);
    }

    #[test]
    fn test_ewma_convergence() {
        let mut ewma = EwmaEstimate::new(0.2);
        for _ in 0..50 {
            ewma.update(0.8);
        }
        assert!((ewma.value - 0.8).abs() < 0.01, "EWMA should converge to 0.8");
    }

    #[test]
    fn test_record_observation_updates_stats() {
        let router = CostAwareRouter::new(CostRouterConfig::default());
        router.register_model(make_profile("sonnet", "anthropic", 0.003, 0.015, 4));

        for i in 0..15 {
            router.record_observation(
                "sonnet",
                "coding",
                0.9,
                0.05,
                Duration::from_millis(2000 + i * 100),
            );
        }

        let stats = router.get_stats("sonnet", "coding").unwrap();
        assert!(stats.quality.is_warm(10));
        assert!((stats.quality.value - 0.9).abs() < 0.1);
    }

    #[test]
    fn test_constraint_relaxation() {
        let router = CostAwareRouter::new(CostRouterConfig {
            min_quality: 0.99, // Unreachably high.
            max_latency_ms: 1, // Unreachably low.
            allow_relaxation: true,
            ..Default::default()
        });

        router.register_model(make_profile("sonnet", "anthropic", 0.003, 0.015, 4));

        let decision = router.select("coding", false, false, 1000, 500).unwrap();
        assert!(decision.constraints_relaxed);
    }

    #[test]
    fn test_cost_summary() {
        let router = CostAwareRouter::new(CostRouterConfig::default());
        router.register_model(make_profile("sonnet", "anthropic", 0.003, 0.015, 4));
        router.register_model(make_profile("haiku", "anthropic", 0.00025, 0.00125, 2));

        router.record_observation("sonnet", "coding", 0.9, 0.10, Duration::from_millis(2000));
        router.record_observation("sonnet", "coding", 0.8, 0.08, Duration::from_millis(1800));
        router.record_observation("haiku", "chat", 0.7, 0.01, Duration::from_millis(500));

        let summary = router.cost_summary();
        assert_eq!(summary.len(), 2);

        let sonnet = summary.iter().find(|s| s.model_id == "sonnet").unwrap();
        assert_eq!(sonnet.total_requests, 2);
        assert!((sonnet.total_cost - 0.18).abs() < 0.001);
    }

    #[test]
    fn test_tool_requirement_filter() {
        let router = CostAwareRouter::new(CostRouterConfig {
            min_quality: 0.5,
            ..Default::default()
        });

        let mut no_tools = make_profile("cheap", "provider", 0.0001, 0.0005, 1);
        no_tools.supports_tools = false;
        router.register_model(no_tools);
        router.register_model(make_profile("capable", "provider", 0.003, 0.015, 3));

        let decision = router.select("coding", true, false, 1000, 500).unwrap();
        assert_eq!(decision.model_id, "capable"); // cheap filtered out.
    }
}
