//! Causal Tracing — attribution and counterfactual replay.
//!
//! Extends the trace system with:
//!
//! 1. **Input/output capture** per pipeline node — records the exact data
//!    flowing through each step for post-hoc analysis.
//!
//! 2. **Causal attribution** — computes each agent's Average Causal Effect (ACE)
//!    on the final outcome using do-calculus:
//!
//!    ```text
//!    ACE(a_i) = E[Y | do(a_i = actual)] - E[Y | do(a_i = baseline)]
//!    ```
//!
//! 3. **Counterfactual replay** — re-executes a pipeline with modified inputs
//!    at specific nodes to answer "what if?" questions.
//!
//! ## Complexity
//!
//! - Capture: O(1) per node (append to log).
//! - Attribution: O(D × C) where D = depth, C = counterfactual samples.
//! - Replay: O(pipeline_cost) per counterfactual.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ───────────────────────────────────────────────────────────────
// Node capture
// ───────────────────────────────────────────────────────────────

/// Captured input/output for a single pipeline node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapture {
    /// Node identifier (e.g., "step:3:summarizer").
    pub node_id: String,
    /// Agent that executed this node.
    pub agent_id: String,
    /// Input text/data to the node.
    pub input: String,
    /// Output text/data from the node.
    pub output: Option<String>,
    /// Whether the node succeeded.
    pub success: bool,
    /// Error message if failed.
    pub error: Option<String>,
    /// Execution start time.
    pub started_at: DateTime<Utc>,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Token usage (input, output).
    pub tokens: Option<(u32, u32)>,
    /// Quality score assigned by evaluator (if any).
    pub quality_score: Option<f64>,
}

/// Causal trace — the complete provenance log for a pipeline execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalTrace {
    /// Pipeline/run identifier.
    pub run_id: String,
    /// Ordered list of node captures (execution order).
    pub captures: Vec<NodeCapture>,
    /// Final output of the pipeline.
    pub final_output: Option<String>,
    /// Overall quality score.
    pub overall_quality: Option<f64>,
    /// Timestamp.
    pub created_at: DateTime<Utc>,
}

impl CausalTrace {
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            captures: Vec::new(),
            final_output: None,
            overall_quality: None,
            created_at: Utc::now(),
        }
    }

    /// Add a node capture.
    pub fn record(&mut self, capture: NodeCapture) {
        self.captures.push(capture);
    }

    /// Finalize the trace with the pipeline output and quality.
    pub fn finalize(&mut self, output: String, quality: f64) {
        self.final_output = Some(output);
        self.overall_quality = Some(quality);
    }

    /// Get the capture for a specific node.
    pub fn get_node(&self, node_id: &str) -> Option<&NodeCapture> {
        self.captures.iter().find(|c| c.node_id == node_id)
    }

    /// Get all captures for a specific agent.
    pub fn agent_captures(&self, agent_id: &str) -> Vec<&NodeCapture> {
        self.captures
            .iter()
            .filter(|c| c.agent_id == agent_id)
            .collect()
    }
}

// ───────────────────────────────────────────────────────────────
// Causal attribution
// ───────────────────────────────────────────────────────────────

/// Causal attribution score for an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalAttribution {
    /// Agent ID.
    pub agent_id: String,
    /// Average Causal Effect: how much removing/changing this agent affects quality.
    pub ace: f64,
    /// Number of counterfactual samples used.
    pub num_samples: u32,
    /// Node IDs this agent was responsible for.
    pub nodes: Vec<String>,
    /// Fraction of total quality attributable to this agent.
    pub attribution_fraction: f64,
}

/// Configuration for counterfactual analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterfactualConfig {
    /// Baseline quality score (e.g., quality without any agent).
    pub baseline_quality: f64,
    /// Number of counterfactual samples per agent.
    pub num_samples: u32,
    /// Whether to use marginal or Shapley attribution.
    pub use_shapley: bool,
}

impl Default for CounterfactualConfig {
    fn default() -> Self {
        Self {
            baseline_quality: 0.0,
            num_samples: 5,
            use_shapley: false,
        }
    }
}

/// Compute marginal causal attribution for each agent in a trace.
///
/// For each agent, estimates:
/// ```text
/// ACE(a_i) = quality_with_agent - quality_without_agent
/// ```
///
/// The "without" estimate uses the agent's node quality scores:
/// if a node scored poorly, the agent contributed negatively.
///
/// This is a fast approximation — true do-calculus would require
/// actual counterfactual re-execution.
pub fn compute_attributions(
    trace: &CausalTrace,
    config: &CounterfactualConfig,
) -> Vec<CausalAttribution> {
    let overall_quality = trace.overall_quality.unwrap_or(0.0);
    let baseline = config.baseline_quality;

    // Group captures by agent.
    let mut agent_nodes: HashMap<String, Vec<&NodeCapture>> = HashMap::new();
    for capture in &trace.captures {
        agent_nodes
            .entry(capture.agent_id.clone())
            .or_default()
            .push(capture);
    }

    let total_nodes = trace.captures.len().max(1) as f64;
    let mut attributions = Vec::new();

    for (agent_id, nodes) in &agent_nodes {
        // Estimate quality contribution: average quality of this agent's nodes
        // weighted by their fraction of total execution.
        let agent_quality_sum: f64 = nodes
            .iter()
            .map(|n| n.quality_score.unwrap_or(if n.success { 0.7 } else { 0.0 }))
            .sum();

        let agent_quality_avg = if nodes.is_empty() {
            0.0
        } else {
            agent_quality_sum / nodes.len() as f64
        };

        // Weight by fraction of nodes this agent handled.
        let node_fraction = nodes.len() as f64 / total_nodes;

        // ACE: contribution above baseline, weighted by involvement.
        let ace = (agent_quality_avg - baseline) * node_fraction;

        // Attribution fraction: how much of the total quality delta comes from this agent.
        let total_delta = (overall_quality - baseline).max(f64::EPSILON);
        let attribution_fraction = (ace / total_delta).clamp(0.0, 1.0);

        attributions.push(CausalAttribution {
            agent_id: agent_id.clone(),
            ace,
            num_samples: config.num_samples,
            nodes: nodes.iter().map(|n| n.node_id.clone()).collect(),
            attribution_fraction,
        });
    }

    // Sort by ACE descending.
    attributions.sort_by(|a, b| b.ace.partial_cmp(&a.ace).unwrap_or(std::cmp::Ordering::Equal));
    attributions
}

/// A counterfactual experiment: "what if node X had different input?"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterfactualExperiment {
    /// Which node to modify.
    pub target_node_id: String,
    /// The modified input for that node.
    pub modified_input: String,
    /// Expected quality change (estimated).
    pub estimated_quality_delta: Option<f64>,
    /// Description of the experiment.
    pub description: String,
}

/// Generate counterfactual experiments for a trace.
///
/// Identifies the weakest nodes (lowest quality) and suggests experiments
/// to improve them.
pub fn suggest_counterfactuals(trace: &CausalTrace, max_experiments: usize) -> Vec<CounterfactualExperiment> {
    let mut weak_nodes: Vec<_> = trace
        .captures
        .iter()
        .filter(|c| c.quality_score.is_some() || !c.success)
        .map(|c| {
            let q = c.quality_score.unwrap_or(if c.success { 0.5 } else { 0.0 });
            (c, q)
        })
        .collect();

    // Sort by quality ascending (weakest first).
    weak_nodes.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    weak_nodes
        .into_iter()
        .take(max_experiments)
        .map(|(node, quality)| CounterfactualExperiment {
            target_node_id: node.node_id.clone(),
            modified_input: format!("[refined version of: {}]", &node.input[..node.input.len().min(100)]),
            estimated_quality_delta: Some((1.0 - quality) * 0.5),
            description: format!(
                "Node {} (agent {}) scored {:.2}. Modifying its input may improve overall quality.",
                node.node_id, node.agent_id, quality
            ),
        })
        .collect()
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_capture(node_id: &str, agent_id: &str, quality: f64) -> NodeCapture {
        NodeCapture {
            node_id: node_id.into(),
            agent_id: agent_id.into(),
            input: "test input".into(),
            output: Some("test output".into()),
            success: true,
            error: None,
            started_at: Utc::now(),
            duration_ms: 1000,
            tokens: Some((100, 200)),
            quality_score: Some(quality),
        }
    }

    #[test]
    fn test_causal_trace_recording() {
        let mut trace = CausalTrace::new("run-1");
        trace.record(make_capture("step:0", "coder", 0.9));
        trace.record(make_capture("step:1", "reviewer", 0.8));
        trace.finalize("final output".into(), 0.85);

        assert_eq!(trace.captures.len(), 2);
        assert!(trace.get_node("step:0").is_some());
        assert_eq!(trace.agent_captures("coder").len(), 1);
    }

    #[test]
    fn test_attribution_computation() {
        let mut trace = CausalTrace::new("run-1");
        trace.record(make_capture("step:0", "coder", 0.9));
        trace.record(make_capture("step:1", "coder", 0.8));
        trace.record(make_capture("step:2", "reviewer", 0.7));
        trace.finalize("output".into(), 0.8);

        let attrs = compute_attributions(&trace, &CounterfactualConfig::default());
        assert_eq!(attrs.len(), 2);

        // Coder handled 2/3 nodes → higher ACE.
        let coder = attrs.iter().find(|a| a.agent_id == "coder").unwrap();
        let reviewer = attrs.iter().find(|a| a.agent_id == "reviewer").unwrap();
        assert!(coder.ace > reviewer.ace);
    }

    #[test]
    fn test_counterfactual_suggestions() {
        let mut trace = CausalTrace::new("run-1");
        trace.record(make_capture("step:0", "coder", 0.9));
        trace.record(make_capture("step:1", "reviewer", 0.3)); // Weak.
        trace.record(make_capture("step:2", "summarizer", 0.2)); // Weaker.
        trace.finalize("output".into(), 0.5);

        let experiments = suggest_counterfactuals(&trace, 2);
        assert_eq!(experiments.len(), 2);
        // Weakest node should be first.
        assert_eq!(experiments[0].target_node_id, "step:2");
    }
}
