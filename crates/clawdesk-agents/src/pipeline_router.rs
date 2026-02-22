//! Pipeline-aware task routing — connects the LinUCB `TaskRouter` to
//! the pipeline executor for per-step model selection.
//!
//! ## Task Router Integration (P3)
//!
//! The `TaskRouter` (task_router.rs) uses a contextual LinUCB bandit for
//! model selection, but it operates at the request level. Pipelines run
//! multi-step DAGs where each step may benefit from a different model.
//!
//! This module provides `PipelineRouter` — an adapter that:
//! 1. Extracts `TaskFeatures` from each pipeline step's context.
//! 2. Selects the optimal `ExecutionPath` for each step via LinUCB.
//! 3. Records feedback after step completion for online learning.
//! 4. Supports model pinning (override LinUCB for specific steps).

use crate::task_router::{
    ExecutionPath, RoutingCandidate, RoutingDecision, RoutingWeights, TaskFeatures, TaskRouter,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use tracing::{debug, info};

/// Configuration for pipeline-aware routing.
#[derive(Debug, Clone)]
pub struct PipelineRoutingConfig {
    /// LinUCB exploration coefficient (higher = more exploration).
    pub alpha: f64,
    /// Scalarization weights for quality/cost/latency tradeoff.
    pub weights: RoutingWeights,
    /// Model pins: force a specific execution path for named steps.
    pub step_pins: HashMap<String, ExecutionPath>,
    /// Default candidates offered to the router when none are specified.
    pub default_candidates: Vec<RoutingCandidate>,
}

impl Default for PipelineRoutingConfig {
    fn default() -> Self {
        Self {
            alpha: 0.2,
            weights: RoutingWeights::default(),
            step_pins: HashMap::new(),
            default_candidates: Vec::new(),
        }
    }
}

/// Record of a routing decision for a pipeline step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRoutingRecord {
    /// Pipeline ID.
    pub pipeline_id: String,
    /// Step name within the pipeline.
    pub step_name: String,
    /// The routing decision.
    pub decision: RoutingDecision,
    /// Whether this was pinned (overridden).
    pub pinned: bool,
    /// When the decision was made.
    pub timestamp: DateTime<Utc>,
}

/// Pipeline-aware task router — wraps `TaskRouter` with pipeline context.
pub struct PipelineRouter {
    router: Mutex<TaskRouter>,
    config: PipelineRoutingConfig,
    /// History of routing decisions for auditing.
    history: Mutex<Vec<StepRoutingRecord>>,
}

impl PipelineRouter {
    /// Create a new pipeline router.
    pub fn new(config: PipelineRoutingConfig) -> Self {
        let router = TaskRouter::new(config.alpha, config.weights);
        Self {
            router: Mutex::new(router),
            config,
            history: Mutex::new(Vec::new()),
        }
    }

    /// Route a pipeline step — returns the best execution path.
    ///
    /// If the step is pinned, returns the pinned path directly.
    /// Otherwise, extracts features from the step context and runs LinUCB.
    pub fn route_step(
        &self,
        pipeline_id: &str,
        step_name: &str,
        step_context: &str,
        candidates: Option<&[RoutingCandidate]>,
    ) -> Option<RoutingDecision> {
        // Check for pinned override
        if let Some(pinned_path) = self.config.step_pins.get(step_name) {
            let decision = RoutingDecision {
                selected_key: format!("pin:{}", step_name),
                selected_path: pinned_path.clone(),
                score: 1.0,
                normalized_quality: 1.0,
                normalized_cost: 0.0,
                normalized_latency: 0.0,
            };

            let record = StepRoutingRecord {
                pipeline_id: pipeline_id.to_string(),
                step_name: step_name.to_string(),
                decision: decision.clone(),
                pinned: true,
                timestamp: Utc::now(),
            };
            self.history.lock().unwrap().push(record);

            debug!(
                pipeline_id,
                step_name,
                path = ?pinned_path,
                "step routed via pin"
            );
            return Some(decision);
        }

        // Extract features from step context
        let features = TaskFeatures::from_task_text(step_context, None);

        let resolved_candidates = candidates.unwrap_or(&self.config.default_candidates);
        if resolved_candidates.is_empty() {
            return None;
        }

        let decision = {
            let mut router = self.router.lock().unwrap();
            router.select(&features, resolved_candidates)?
        };

        let record = StepRoutingRecord {
            pipeline_id: pipeline_id.to_string(),
            step_name: step_name.to_string(),
            decision: decision.clone(),
            pinned: false,
            timestamp: Utc::now(),
        };
        self.history.lock().unwrap().push(record);

        info!(
            pipeline_id,
            step_name,
            selected = %decision.selected_key,
            score = decision.score,
            "step routed via LinUCB"
        );

        Some(decision)
    }

    /// Record step completion feedback for online learning.
    ///
    /// The reward signal (0.0 = failure, 1.0 = perfect) is fed back
    /// to the LinUCB arm to improve future routing decisions.
    pub fn record_step_feedback(
        &self,
        step_name: &str,
        step_context: &str,
        selected_key: &str,
        reward: f64,
    ) -> Result<(), String> {
        let features = TaskFeatures::from_task_text(step_context, None);
        let mut router = self.router.lock().unwrap();
        router.record_feedback(selected_key, &features, reward)
    }

    /// Get routing history for a pipeline.
    pub fn history_for_pipeline(&self, pipeline_id: &str) -> Vec<StepRoutingRecord> {
        let history = self.history.lock().unwrap();
        history
            .iter()
            .filter(|r| r.pipeline_id == pipeline_id)
            .cloned()
            .collect()
    }

    /// Get all routing history.
    pub fn all_history(&self) -> Vec<StepRoutingRecord> {
        self.history.lock().unwrap().clone()
    }

    /// Clear routing history.
    pub fn clear_history(&self) {
        self.history.lock().unwrap().clear();
    }

    /// Add a step pin (model override for a specific step name).
    pub fn pin_step(&mut self, step_name: impl Into<String>, path: ExecutionPath) {
        self.config.step_pins.insert(step_name.into(), path);
    }

    /// Remove a step pin.
    pub fn unpin_step(&mut self, step_name: &str) {
        self.config.step_pins.remove(step_name);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessKind;

    fn test_candidates() -> Vec<RoutingCandidate> {
        vec![
            RoutingCandidate {
                key: "sonnet".into(),
                path: ExecutionPath::ApiProvider {
                    provider: "anthropic".into(),
                    model: "sonnet".into(),
                },
                estimated_quality: 0.8,
                estimated_cost_usd: 0.02,
                estimated_latency_ms: 2000.0,
            },
            RoutingCandidate {
                key: "haiku".into(),
                path: ExecutionPath::ApiProvider {
                    provider: "anthropic".into(),
                    model: "haiku".into(),
                },
                estimated_quality: 0.5,
                estimated_cost_usd: 0.005,
                estimated_latency_ms: 500.0,
            },
            RoutingCandidate {
                key: "claude_code".into(),
                path: ExecutionPath::Harness(HarnessKind::ClaudeCode),
                estimated_quality: 0.9,
                estimated_cost_usd: 0.05,
                estimated_latency_ms: 5000.0,
            },
        ]
    }

    #[test]
    fn routes_step_with_linucb() {
        let config = PipelineRoutingConfig {
            default_candidates: test_candidates(),
            ..Default::default()
        };
        let router = PipelineRouter::new(config);

        let decision = router
            .route_step("pipeline-1", "step-a", "refactor the auth module", None)
            .expect("should produce decision");

        assert!(!decision.selected_key.is_empty());
        assert!(decision.score > 0.0);
    }

    #[test]
    fn pinned_step_overrides_linucb() {
        let mut config = PipelineRoutingConfig {
            default_candidates: test_candidates(),
            ..Default::default()
        };
        config.step_pins.insert(
            "critical-step".into(),
            ExecutionPath::Harness(HarnessKind::ClaudeCode),
        );

        let router = PipelineRouter::new(config);

        let decision = router
            .route_step("pipeline-1", "critical-step", "any context", None)
            .expect("should produce decision");

        assert!(decision.selected_key.starts_with("pin:"));
        assert_eq!(
            decision.selected_path,
            ExecutionPath::Harness(HarnessKind::ClaudeCode)
        );

        // Check history records the pin
        let hist = router.history_for_pipeline("pipeline-1");
        assert_eq!(hist.len(), 1);
        assert!(hist[0].pinned);
    }

    #[test]
    fn feedback_improves_routing() {
        let config = PipelineRoutingConfig {
            default_candidates: test_candidates(),
            alpha: 0.1,
            ..Default::default()
        };
        let router = PipelineRouter::new(config);

        // Train the router to prefer "claude_code" for coding tasks
        for _ in 0..20 {
            router
                .record_step_feedback(
                    "code-step",
                    "implement new rust module",
                    "claude_code",
                    1.0,
                )
                .unwrap();
            router
                .record_step_feedback("code-step", "implement new rust module", "haiku", 0.1)
                .unwrap();
        }

        let decision = router
            .route_step("p1", "code-step", "implement new rust module", None)
            .unwrap();
        assert_eq!(decision.selected_key, "claude_code");
    }

    #[test]
    fn history_tracking() {
        let config = PipelineRoutingConfig {
            default_candidates: test_candidates(),
            ..Default::default()
        };
        let router = PipelineRouter::new(config);

        router.route_step("p1", "s1", "search docs", None);
        router.route_step("p1", "s2", "implement feature", None);
        router.route_step("p2", "s1", "review code", None);

        assert_eq!(router.history_for_pipeline("p1").len(), 2);
        assert_eq!(router.history_for_pipeline("p2").len(), 1);
        assert_eq!(router.all_history().len(), 3);

        router.clear_history();
        assert_eq!(router.all_history().len(), 0);
    }
}
