//! # Evaluation Engineering — How to know while doing
//!
//! Automated quality gates, eval harness, outcome auto-labeling, and the
//! autonomous re-entry loop that feeds evaluation results back into lower
//! layers (Context, Prompt, Intent).
//!
//! ## Architecture
//!
//! ```text
//! Agent turn completes
//!   ↓
//! EvalPipeline::evaluate(turn_outcome)
//!   ├── QualityGate::check()      → pass / fail / degrade
//!   ├── OutcomeLabeler::label()   → automatic reward signal
//!   └── LoopDecision::decide()    → continue / reenter(layer) / abort
//!         ↓
//!       feeds back into TurnRouter (LinUCB reward)
//! ```

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

// ─── Quality Gate ────────────────────────────────────────────────────────────

/// A quality gate that decides whether an agent turn's output meets a bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGate {
    /// Human-readable gate name (e.g. "tool_success_rate").
    pub name: String,
    /// The policy that determines pass/fail.
    pub policy: GatePolicy,
    /// Whether this gate blocks continuation (hard) or just warns (soft).
    pub severity: GateSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateSeverity {
    /// Failure blocks the run.
    Hard,
    /// Failure emits a warning but the run continues.
    Soft,
}

/// Policy for evaluating a quality gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GatePolicy {
    /// Tool call success rate must be above threshold.
    ToolSuccessRate { min_rate: f64 },
    /// Response must be under a token budget.
    MaxResponseTokens { limit: usize },
    /// No security findings above a severity level.
    NoSecurityFindings { max_severity: String },
    /// Custom predicate evaluated against turn metadata.
    Custom { expression: String },
}

/// Result of checking a quality gate.
#[derive(Debug, Clone, Serialize)]
pub struct GateResult {
    pub gate_name: String,
    pub passed: bool,
    pub detail: String,
    pub severity: GateSeverity,
}

impl QualityGate {
    pub fn check(&self, outcome: &TurnOutcome) -> GateResult {
        let (passed, detail) = match &self.policy {
            GatePolicy::ToolSuccessRate { min_rate } => {
                if outcome.total_tool_calls == 0 {
                    (true, "no tool calls".into())
                } else {
                    let rate = outcome.successful_tool_calls as f64
                        / outcome.total_tool_calls as f64;
                    (
                        rate >= *min_rate,
                        format!("{:.0}% (min {:.0}%)", rate * 100.0, min_rate * 100.0),
                    )
                }
            }
            GatePolicy::MaxResponseTokens { limit } => {
                let ok = outcome.output_tokens <= *limit;
                (ok, format!("{} tokens (limit {})", outcome.output_tokens, limit))
            }
            GatePolicy::NoSecurityFindings { max_severity } => {
                let dominated = outcome.security_findings.iter().any(|f| {
                    severity_ord(f) >= severity_ord(max_severity)
                });
                (
                    !dominated,
                    if dominated {
                        format!("found findings ≥ {}", max_severity)
                    } else {
                        "clean".into()
                    },
                )
            }
            GatePolicy::Custom { expression } => {
                // Placeholder — real implementation would eval the expression
                // against outcome metadata.
                (true, format!("custom: {} (not evaluated)", expression))
            }
        };
        GateResult {
            gate_name: self.name.clone(),
            passed,
            detail,
            severity: self.severity,
        }
    }
}

fn severity_ord(s: &str) -> u8 {
    match s.to_lowercase().as_str() {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

// ─── Turn Outcome ────────────────────────────────────────────────────────────

/// Structured outcome of a single agent turn, used for evaluation.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TurnOutcome {
    pub turn_number: u64,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub total_tool_calls: usize,
    pub successful_tool_calls: usize,
    pub failed_tool_calls: usize,
    pub tool_names: Vec<String>,
    pub security_findings: Vec<String>,
    pub duration: Duration,
    pub model: String,
    pub error: Option<String>,
}

// ─── Outcome Auto-Labeling ───────────────────────────────────────────────────

/// Automatically derives a reward signal from a `TurnOutcome` without
/// human intervention. This feeds into the LinUCB bandit in `TurnRouter`.
pub struct OutcomeLabeler {
    /// Weight for tool success rate in the reward signal.
    pub tool_success_weight: f64,
    /// Weight for latency (lower is better, normalized).
    pub latency_weight: f64,
    /// Weight for token efficiency.
    pub efficiency_weight: f64,
    /// Maximum expected latency for normalization (seconds).
    pub max_expected_latency_secs: f64,
}

impl Default for OutcomeLabeler {
    fn default() -> Self {
        Self {
            tool_success_weight: 0.5,
            latency_weight: 0.2,
            efficiency_weight: 0.3,
            max_expected_latency_secs: 60.0,
        }
    }
}

impl OutcomeLabeler {
    /// Compute a reward ∈ [0, 1] from the turn outcome.
    pub fn label(&self, outcome: &TurnOutcome) -> f64 {
        // Tool success component.
        let tool_score = if outcome.total_tool_calls == 0 {
            1.0
        } else {
            outcome.successful_tool_calls as f64 / outcome.total_tool_calls as f64
        };

        // Latency component (lower = better).
        let latency_secs = outcome.duration.as_secs_f64();
        let latency_score = 1.0
            - (latency_secs / self.max_expected_latency_secs).clamp(0.0, 1.0);

        // Efficiency: output_tokens / input_tokens ratio (lower = more efficient).
        let efficiency_score = if outcome.input_tokens == 0 {
            1.0
        } else {
            let ratio = outcome.output_tokens as f64 / outcome.input_tokens as f64;
            // Ideal ratio ~0.1-0.3, penalize bloat.
            1.0 - (ratio - 0.2).abs().min(1.0)
        };

        // Error penalty.
        let error_penalty = if outcome.error.is_some() { 0.5 } else { 1.0 };

        let raw = self.tool_success_weight * tool_score
            + self.latency_weight * latency_score
            + self.efficiency_weight * efficiency_score;

        (raw * error_penalty).clamp(0.0, 1.0)
    }
}

// ─── Eval Loop Decision ──────────────────────────────────────────────────────

/// After evaluation, what should the agent do?
#[derive(Debug, Clone, Serialize)]
pub enum LoopDecision {
    /// Continue normally to the next turn.
    Continue,
    /// Re-enter a specific layer to correct a problem.
    Reenter {
        layer: ReentryLayer,
        reason: String,
    },
    /// Abort the run — quality is too low or unrecoverable.
    Abort {
        reason: String,
    },
}

/// Which layer to re-enter when evaluation detects a problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReentryLayer {
    /// Re-enter Prompt Engineering — rephrase or restructure the prompt.
    Prompt,
    /// Re-enter Context Engineering — compact, recall different memory.
    Context,
    /// Re-enter Intent Engineering — re-decompose the goal.
    Intent,
    /// Re-enter Judgment Engineering — escalate to human.
    Judgment,
}

// ─── Eval Pipeline ───────────────────────────────────────────────────────────

/// The full evaluation pipeline that runs after each agent turn.
pub struct EvalPipeline {
    pub gates: Vec<QualityGate>,
    pub labeler: OutcomeLabeler,
    /// Consecutive failed gate count for escalation.
    consecutive_failures: u32,
    /// Max consecutive failures before abort.
    max_consecutive_failures: u32,
}

/// Result of running the eval pipeline.
#[derive(Debug, Clone, Serialize)]
pub struct EvalResult {
    pub gate_results: Vec<GateResult>,
    pub reward: f64,
    pub decision: LoopDecision,
    pub all_gates_passed: bool,
}

impl EvalPipeline {
    pub fn new(gates: Vec<QualityGate>) -> Self {
        Self {
            gates,
            labeler: OutcomeLabeler::default(),
            consecutive_failures: 0,
            max_consecutive_failures: 3,
        }
    }

    /// Create a pipeline with sensible production defaults.
    pub fn production_defaults() -> Self {
        Self::new(vec![
            QualityGate {
                name: "tool_success_rate".into(),
                policy: GatePolicy::ToolSuccessRate { min_rate: 0.7 },
                severity: GateSeverity::Hard,
            },
            QualityGate {
                name: "response_budget".into(),
                policy: GatePolicy::MaxResponseTokens { limit: 16_000 },
                severity: GateSeverity::Soft,
            },
            QualityGate {
                name: "security_clean".into(),
                policy: GatePolicy::NoSecurityFindings {
                    max_severity: "high".into(),
                },
                severity: GateSeverity::Hard,
            },
        ])
    }

    /// Evaluate a turn outcome through the full pipeline.
    pub fn evaluate(&mut self, outcome: &TurnOutcome) -> EvalResult {
        // 1. Run quality gates.
        let gate_results: Vec<GateResult> = self
            .gates
            .iter()
            .map(|g| g.check(outcome))
            .collect();

        let hard_failures: Vec<&GateResult> = gate_results
            .iter()
            .filter(|r| !r.passed && r.severity == GateSeverity::Hard)
            .collect();

        let all_passed = hard_failures.is_empty();

        // 2. Auto-label for reward.
        let reward = self.labeler.label(outcome);

        // 3. Decide loop action.
        let decision = if !all_passed {
            self.consecutive_failures += 1;
            if self.consecutive_failures >= self.max_consecutive_failures {
                LoopDecision::Abort {
                    reason: format!(
                        "{} consecutive gate failures: {}",
                        self.consecutive_failures,
                        hard_failures
                            .iter()
                            .map(|r| format!("{}: {}", r.gate_name, r.detail))
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                }
            } else {
                // Decide which layer to re-enter based on the failure type.
                let layer = if hard_failures.iter().any(|r| r.gate_name == "security_clean") {
                    ReentryLayer::Judgment
                } else if hard_failures.iter().any(|r| r.gate_name == "tool_success_rate") {
                    ReentryLayer::Intent
                } else {
                    ReentryLayer::Context
                };
                LoopDecision::Reenter {
                    layer,
                    reason: hard_failures
                        .iter()
                        .map(|r| format!("{}: {}", r.gate_name, r.detail))
                        .collect::<Vec<_>>()
                        .join("; "),
                }
            }
        } else {
            self.consecutive_failures = 0;
            LoopDecision::Continue
        };

        EvalResult {
            gate_results,
            reward,
            decision,
            all_gates_passed: all_passed,
        }
    }
}

// ─── Eval Harness (Systematic Testing) ───────────────────────────────────────

/// A test case for the eval harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCase {
    pub id: String,
    pub prompt: String,
    pub expected_tools: Vec<String>,
    pub expected_files: Vec<String>,
    pub timeout_secs: u64,
    pub tags: Vec<String>,
}

/// Result of running a single eval case.
#[derive(Debug, Clone, Serialize)]
pub struct EvalCaseResult {
    pub case_id: String,
    pub passed: bool,
    pub tools_used: Vec<String>,
    pub files_created: Vec<String>,
    pub duration: Duration,
    pub reward: f64,
    pub error: Option<String>,
}

/// Aggregate metrics from an eval run.
#[derive(Debug, Clone, Serialize)]
pub struct EvalRunSummary {
    pub total_cases: usize,
    pub passed: usize,
    pub failed: usize,
    pub pass_rate: f64,
    /// pass@k: probability that at least 1 of k attempts succeeds.
    pub pass_at_k: f64,
    pub mean_reward: f64,
    pub mean_duration: Duration,
    pub by_tag: std::collections::HashMap<String, TagMetrics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TagMetrics {
    pub total: usize,
    pub passed: usize,
    pub pass_rate: f64,
}

impl EvalRunSummary {
    /// Compute summary from a set of case results.
    pub fn from_results(results: &[EvalCaseResult], k: usize) -> Self {
        let total = results.len();
        let passed = results.iter().filter(|r| r.passed).count();
        let pass_rate = if total == 0 { 0.0 } else { passed as f64 / total as f64 };

        // pass@k = 1 - (1 - pass_rate)^k
        let pass_at_k = if k == 0 {
            0.0
        } else {
            1.0 - (1.0 - pass_rate).powi(k as i32)
        };

        let mean_reward = if total == 0 {
            0.0
        } else {
            results.iter().map(|r| r.reward).sum::<f64>() / total as f64
        };

        let total_duration: Duration = results.iter().map(|r| r.duration).sum();
        let mean_duration = if total == 0 {
            Duration::ZERO
        } else {
            total_duration / total as u32
        };

        // Per-tag breakdown.
        let mut tag_map: std::collections::HashMap<String, (usize, usize)> =
            std::collections::HashMap::new();
        for r in results {
            // EvalCase tags aren't on results, so we look at tool names as proxy.
            // In a real implementation this would join with the original cases.
        }

        Self {
            total_cases: total,
            passed,
            failed: total - passed,
            pass_rate,
            pass_at_k,
            mean_reward,
            mean_duration,
            by_tag: std::collections::HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_outcome(tool_ok: usize, tool_fail: usize) -> TurnOutcome {
        TurnOutcome {
            turn_number: 1,
            input_tokens: 1000,
            output_tokens: 200,
            total_tool_calls: tool_ok + tool_fail,
            successful_tool_calls: tool_ok,
            failed_tool_calls: tool_fail,
            tool_names: vec![],
            security_findings: vec![],
            duration: Duration::from_secs(5),
            model: "test".into(),
            error: None,
        }
    }

    #[test]
    fn test_quality_gate_pass() {
        let gate = QualityGate {
            name: "tool_success".into(),
            policy: GatePolicy::ToolSuccessRate { min_rate: 0.7 },
            severity: GateSeverity::Hard,
        };
        let outcome = mock_outcome(8, 2);
        let result = gate.check(&outcome);
        assert!(result.passed);
    }

    #[test]
    fn test_quality_gate_fail() {
        let gate = QualityGate {
            name: "tool_success".into(),
            policy: GatePolicy::ToolSuccessRate { min_rate: 0.9 },
            severity: GateSeverity::Hard,
        };
        let outcome = mock_outcome(5, 5);
        let result = gate.check(&outcome);
        assert!(!result.passed);
    }

    #[test]
    fn test_auto_labeling() {
        let labeler = OutcomeLabeler::default();
        let good = mock_outcome(10, 0);
        let bad = mock_outcome(2, 8);
        assert!(labeler.label(&good) > labeler.label(&bad));
    }

    #[test]
    fn test_eval_pipeline_abort_after_streak() {
        let mut pipeline = EvalPipeline::production_defaults();
        pipeline.max_consecutive_failures = 2;

        let bad = mock_outcome(1, 9); // 10% success → fails the 70% gate
        let r1 = pipeline.evaluate(&bad);
        assert!(!r1.all_gates_passed);
        assert!(matches!(r1.decision, LoopDecision::Reenter { .. }));

        let r2 = pipeline.evaluate(&bad);
        assert!(matches!(r2.decision, LoopDecision::Abort { .. }));
    }
}
