//! GAP-G: Per-turn dynamic model routing.
//!
//! Bridges `TaskRouter` (LinUCB bandit) with `ModelCatalog` (capability-indexed
//! model registry) to select the optimal model on every turn.
//!
//! ## Flow
//!
//! 1. `TaskFeatures` extracted from the user message (AhoCorasick keyword sets).
//! 2. `ModelCatalog` provides capability-matching candidates.
//! 3. `TaskRouter` (LinUCB bandit) scores candidates using contextual features.
//! 4. Winner is returned as a model ID for the caller to resolve.
//! 5. After execution, caller reports reward for online learning.

use crate::task_router::{
    ExecutionPath, RoutingCandidate, RoutingDecision, TaskFeatures, TaskRouter,
};
use clawdesk_domain::model_catalog::{ModelCapabilities, ModelCatalog, ModelEntry};
use std::sync::Mutex;
use tracing::{debug, info};

/// Per-turn model routing decision.
#[derive(Debug, Clone)]
pub struct TurnRoutingResult {
    /// The selected model ID (e.g. "claude-sonnet-4-20250514").
    pub model_id: String,
    /// The provider name (e.g. "anthropic").
    pub provider: String,
    /// Combined bandit + cost/quality score.
    pub score: f64,
    /// The features extracted from the user message (for feedback).
    pub features: TaskFeatures,
    /// The routing key (for feedback).
    pub selected_key: String,
}

/// Per-turn dynamic model router.
///
/// Wraps `TaskRouter` (online learning) + `ModelCatalog` (capability index)
/// to select the best model for each user turn. Thread-safe via `Mutex`.
pub struct TurnRouter {
    /// LinUCB bandit for online learning.
    router: Mutex<TaskRouter>,
    /// Capability-indexed model catalog.
    catalog: ModelCatalog,
    /// Minimum capabilities required for all routing decisions.
    /// Default: TEXT + TOOLS (agents need tool calling).
    required_caps: ModelCapabilities,
}

impl TurnRouter {
    /// Create a new TurnRouter with default parameters.
    pub fn new(catalog: ModelCatalog) -> Self {
        use crate::task_router::RoutingWeights;
        Self {
            router: Mutex::new(TaskRouter::new(
                0.5, // exploration coefficient
                RoutingWeights {
                    quality: 0.6,
                    cost: 0.2,
                    latency: 0.2,
                },
            )),
            catalog,
            required_caps: ModelCapabilities::TEXT.union(ModelCapabilities::TOOLS),
        }
    }

    /// Create with custom capability requirements.
    pub fn with_required_caps(mut self, caps: ModelCapabilities) -> Self {
        self.required_caps = caps;
        self
    }

    /// Route a turn: extract features from user message, select best model.
    ///
    /// Returns `None` if no models match required capabilities or the catalog
    /// is empty. The caller should fall back to the user-configured model.
    pub fn route_turn(
        &self,
        user_message: &str,
        workspace_size: Option<usize>,
    ) -> Option<TurnRoutingResult> {
        // Step 1: Extract features from user message
        let features = TaskFeatures::from_task_text(user_message, workspace_size);

        // Step 2: Get capability-matching models from catalog
        let selection = self.catalog.select(self.required_caps, 0.3)?;

        // Step 3: Convert ModelEntry → RoutingCandidate
        let mut candidates: Vec<RoutingCandidate> = Vec::new();
        // Primary model
        candidates.push(model_to_candidate(&selection.primary));
        // Fallback models
        for fallback in &selection.fallbacks {
            candidates.push(model_to_candidate(fallback));
        }

        if candidates.is_empty() {
            return None;
        }

        // Step 4: Bandit selection
        let decision = {
            let mut router = self.router.lock().ok()?;
            router.select(&features, &candidates)?
        };

        // Extract the winning model's provider from the path
        let (model_id, provider) = match &decision.selected_path {
            ExecutionPath::ApiProvider { provider, model } => {
                (model.clone(), provider.clone())
            }
            _ => return None, // Only API providers are routed
        };

        info!(
            model = %model_id,
            provider = %provider,
            score = decision.score,
            is_coding = features.is_coding,
            is_research = features.is_research,
            tokens = features.estimated_tokens,
            "GAP-G: Turn router selected model"
        );

        Some(TurnRoutingResult {
            model_id,
            provider,
            score: decision.score,
            features,
            selected_key: decision.selected_key,
        })
    }

    /// Record reward feedback for a previous routing decision.
    ///
    /// Called after execution completes with a reward signal:
    /// - 1.0 = excellent (fast, correct, cheap)
    /// - 0.5 = acceptable
    /// - 0.0 = poor (slow, wrong, expensive)
    pub fn record_feedback(
        &self,
        selected_key: &str,
        features: &TaskFeatures,
        reward: f64,
    ) {
        if let Ok(mut router) = self.router.lock() {
            if let Err(e) = router.record_feedback(selected_key, features, reward) {
                debug!(error = %e, "Failed to record routing feedback");
            }
        }
    }
}

/// Convert a `ModelEntry` from the catalog into a `RoutingCandidate`
/// that the `TaskRouter` bandit can score.
fn model_to_candidate(entry: &ModelEntry) -> RoutingCandidate {
    // Quality heuristic: inverse cost as proxy (expensive models tend to be better).
    // This is a bootstrap prior — the bandit will learn the real quality.
    let quality = match entry.cost_per_m_input {
        c if c >= 10.0 => 0.95,  // Opus-tier
        c if c >= 2.0 => 0.85,   // Sonnet/GPT-4o tier
        c if c >= 0.5 => 0.70,   // Flash/Mini tier
        _ => 0.55,               // Very cheap / local
    };

    RoutingCandidate {
        key: format!("{}:{}", entry.provider, entry.id),
        path: ExecutionPath::ApiProvider {
            provider: entry.provider.clone(),
            model: entry.id.clone(),
        },
        estimated_quality: quality,
        estimated_cost_usd: entry.cost_per_m_input,
        estimated_latency_ms: entry.avg_latency_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_turn_router_routes() {
        let catalog = ModelCatalog::default_catalog();
        let router = TurnRouter::new(catalog);
        let result = router.route_turn("Write a function that sorts an array", None);
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(!r.model_id.is_empty());
        assert!(!r.provider.is_empty());
    }

    #[test]
    fn test_turn_router_empty_catalog() {
        let catalog = ModelCatalog::new();
        let router = TurnRouter::new(catalog);
        assert!(router.route_turn("hello", None).is_none());
    }

    #[test]
    fn test_feedback_loop() {
        let catalog = ModelCatalog::default_catalog();
        let router = TurnRouter::new(catalog);
        let result = router.route_turn("Explain quantum computing", None).unwrap();
        // Record positive feedback
        router.record_feedback(&result.selected_key, &result.features, 0.9);
        // Route again — bandit should learn
        let result2 = router.route_turn("Explain quantum computing", None).unwrap();
        assert!(!result2.model_id.is_empty());
    }
}
