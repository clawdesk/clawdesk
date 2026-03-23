//! L1: Sentinel — anomaly detection for tool execution patterns.
//!
//! The sentinel monitors tool invocations in real-time and detects anomalies:
//! - **Rate spikes**: tool invocations/minute exceeds EWMA baseline by N sigma
//! - **Cost drift**: cumulative session cost exceeding threshold
//! - **Repetition loops**: same tool+args called N+ times
//! - **Path escapes**: file operations outside workspace root
//! - **Metacognitive alerts**: forwarded from the metacognition subsystem
//!
//! When an anomaly is detected, the sentinel produces an `Escalation` that
//! boosts the risk score for subsequent tool classifications.

use chrono::{DateTime, Utc};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tracing::warn;

use crate::workspace::{CognitiveEvent, GlobalWorkspace};

/// A detected anomaly signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SentinelSignal {
    /// Tool invocation rate spiked above EWMA baseline.
    RateSpike {
        current_rate: f64,
        baseline: f64,
        sigma: f64,
    },
    /// Session cost exceeding budget.
    CostDrift {
        session_cost: f64,
        threshold: f64,
    },
    /// Same tool+args repeated N times.
    RepetitionLoop {
        tool: String,
        count: usize,
    },
    /// File operation targeting path outside workspace.
    PathEscape {
        tool: String,
        path: String,
    },
    /// Unknown tool not in the risk table.
    UnknownTool {
        tool: String,
    },
    /// Metacognition reports agent is stuck.
    MetacognitiveAlert {
        reason: String,
    },
}

/// Escalation recommendation from the sentinel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Escalation {
    /// Risk score boost to add to the next classification.
    pub risk_boost: f64,
    /// Force deliberation (L2) regardless of base classification.
    pub force_deliberation: bool,
    /// Force human veto (L3) regardless of base classification.
    pub force_human_veto: bool,
    /// Human-readable explanation for audit trail.
    pub explanation: String,
    /// The signal(s) that triggered this escalation.
    pub signals: Vec<SentinelSignal>,
}

/// Sentinel configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelConfig {
    /// Standard deviations above EWMA baseline to trigger rate spike alert.
    pub rate_spike_sigma: f64,
    /// EWMA decay factor (0..1). Higher = more responsive, lower = smoother.
    pub ewma_alpha: f64,
    /// Session cost threshold (USD) to trigger cost drift alert.
    pub cost_threshold: f64,
    /// Number of identical tool+args calls to trigger repetition alert.
    pub repetition_threshold: usize,
    /// Sliding window size in seconds for rate calculation.
    pub window_seconds: u64,
}

impl Default for SentinelConfig {
    fn default() -> Self {
        Self {
            rate_spike_sigma: 2.0,
            ewma_alpha: 0.3,
            cost_threshold: 1.0,
            repetition_threshold: 4,
            window_seconds: 60,
        }
    }
}

/// EWMA (Exponentially Weighted Moving Average) state.
#[derive(Debug)]
struct EwmaState {
    mean: f64,
    variance: f64,
    alpha: f64,
    initialized: bool,
}

impl EwmaState {
    fn new(alpha: f64) -> Self {
        Self {
            mean: 0.0,
            variance: 0.0,
            alpha,
            initialized: false,
        }
    }

    fn update(&mut self, value: f64) {
        if !self.initialized {
            self.mean = value;
            self.variance = 0.0;
            self.initialized = true;
            return;
        }
        let diff = value - self.mean;
        self.mean += self.alpha * diff;
        self.variance = (1.0 - self.alpha) * (self.variance + self.alpha * diff * diff);
    }

    fn std_dev(&self) -> f64 {
        self.variance.sqrt()
    }
}

/// The Sentinel — L1 anomaly detector.
pub struct Sentinel {
    config: SentinelConfig,
    /// Per-minute tool invocation rate EWMA.
    rate_ewma: EwmaState,
    /// Recent tool invocations (timestamp, tool_name) for rate calculation.
    recent_calls: VecDeque<(DateTime<Utc>, String)>,
    /// Recent tool+args fingerprints for repetition detection.
    recent_fingerprints: VecDeque<u64>,
    /// Cumulative session cost.
    session_cost: f64,
    /// Workspace root for path escape detection.
    workspace_root: Option<String>,
    /// Global workspace for publishing signals.
    workspace: Option<Arc<GlobalWorkspace>>,
    /// Pending signals injected from external cognitive subsystems.
    /// Drained on the next `observe()` call.
    pending_injected: Vec<SentinelSignal>,
}

impl Sentinel {
    pub fn new(config: SentinelConfig) -> Self {
        let alpha = config.ewma_alpha;
        Self {
            config,
            rate_ewma: EwmaState::new(alpha),
            recent_calls: VecDeque::with_capacity(256),
            recent_fingerprints: VecDeque::with_capacity(64),
            session_cost: 0.0,
            workspace_root: None,
            workspace: None,
            pending_injected: Vec::new(),
        }
    }

    /// Set the workspace root for path escape detection.
    pub fn with_workspace_root(mut self, root: String) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Connect to the global workspace for broadcasting signals.
    pub fn with_global_workspace(mut self, ws: Arc<GlobalWorkspace>) -> Self {
        self.workspace = Some(ws);
        self
    }

    /// Record a tool invocation and check for anomalies.
    ///
    /// Returns the accumulated risk boost from all detected signals.
    /// Called BEFORE tool execution for proactive gating.
    pub fn observe(&mut self, tool: &str, args: &serde_json::Value) -> Escalation {
        let now = Utc::now();
        let mut signals = Vec::new();

        // Drain any externally injected signals (from cognitive event loop)
        signals.append(&mut self.pending_injected);

        // --- Rate spike detection ---
        self.recent_calls.push_back((now, tool.to_string()));
        self.prune_old_calls(now);
        let current_rate = self.recent_calls.len() as f64;
        self.rate_ewma.update(current_rate);

        if self.rate_ewma.initialized && self.rate_ewma.std_dev() > 0.01 {
            let z_score = (current_rate - self.rate_ewma.mean) / self.rate_ewma.std_dev();
            if z_score > self.config.rate_spike_sigma {
                signals.push(SentinelSignal::RateSpike {
                    current_rate,
                    baseline: self.rate_ewma.mean,
                    sigma: z_score,
                });
            }
        }

        // --- Repetition loop detection ---
        let fingerprint = hash_tool_args(tool, args);
        self.recent_fingerprints.push_back(fingerprint);
        if self.recent_fingerprints.len() > 64 {
            self.recent_fingerprints.pop_front();
        }
        let repeat_count = self.recent_fingerprints.iter()
            .filter(|&&fp| fp == fingerprint)
            .count();
        if repeat_count >= self.config.repetition_threshold {
            signals.push(SentinelSignal::RepetitionLoop {
                tool: tool.to_string(),
                count: repeat_count,
            });
        }

        // --- Path escape detection ---
        if let Some(ref ws_root) = self.workspace_root {
            if let Some(path) = args.get("path").or(args.get("file_path")).and_then(|v| v.as_str()) {
                if let Ok(canonical) = std::fs::canonicalize(path) {
                    if !canonical.starts_with(ws_root) && !path.starts_with("/tmp") {
                        signals.push(SentinelSignal::PathEscape {
                            tool: tool.to_string(),
                            path: path.to_string(),
                        });
                    }
                }
            }
        }

        // --- Cost drift detection ---
        if self.session_cost > self.config.cost_threshold {
            signals.push(SentinelSignal::CostDrift {
                session_cost: self.session_cost,
                threshold: self.config.cost_threshold,
            });
        }

        // Build escalation from accumulated signals
        let escalation = self.build_escalation(&signals);

        // Publish to global workspace
        if !signals.is_empty() {
            if let Some(ref ws) = self.workspace {
                for signal in &signals {
                    let (label, severity) = match signal {
                        SentinelSignal::RateSpike { sigma, .. } => ("rate_spike", *sigma / 5.0),
                        SentinelSignal::CostDrift { session_cost, threshold } => {
                            ("cost_drift", session_cost / threshold)
                        }
                        SentinelSignal::RepetitionLoop { count, .. } => {
                            ("repetition_loop", *count as f64 / 10.0)
                        }
                        SentinelSignal::PathEscape { .. } => ("path_escape", 0.8),
                        SentinelSignal::UnknownTool { .. } => ("unknown_tool", 0.3),
                        SentinelSignal::MetacognitiveAlert { .. } => ("metacog_alert", 0.5),
                    };
                    ws.publish(CognitiveEvent::AnomalyDetected {
                        signal: label.to_string(),
                        severity: severity.clamp(0.0, 1.0),
                    });
                }
            }
        }

        escalation
    }

    /// Record cost from a completed tool execution.
    pub fn record_cost(&mut self, cost_usd: f64) {
        self.session_cost += cost_usd;
    }

    /// Reset session state (for new sessions).
    pub fn reset(&mut self) {
        self.recent_calls.clear();
        self.recent_fingerprints.clear();
        self.session_cost = 0.0;
        self.rate_ewma = EwmaState::new(self.config.ewma_alpha);
    }

    /// Inject a signal from an external cognitive subsystem.
    ///
    /// Used by the cognitive event loop to forward metacognition alerts,
    /// user frustration signals, etc. into the sentinel's anomaly pipeline.
    /// The injected signal will boost risk on the NEXT tool classification.
    pub fn inject_signal(&mut self, signal: SentinelSignal) {
        // Store the signal so it's included in the next `observe()` call's
        // escalation. We push it onto a pending queue.
        self.pending_injected.push(signal);
    }

    /// Process a cognitive event from the global workspace bus.
    ///
    /// Routes relevant events into sentinel signals for risk escalation.
    pub fn handle_cognitive_event(&mut self, event: &CognitiveEvent) {
        match event {
            CognitiveEvent::AgentStuck { reason, streak } => {
                self.inject_signal(SentinelSignal::MetacognitiveAlert {
                    reason: format!("stuck (streak={}): {}", streak, reason),
                });
            }
            CognitiveEvent::ApproachFailing { confidence, .. } if *confidence < 0.3 => {
                self.inject_signal(SentinelSignal::MetacognitiveAlert {
                    reason: format!("approach confidence critically low: {:.2}", confidence),
                });
            }
            CognitiveEvent::UserFrustrationRising { level } => {
                self.inject_signal(SentinelSignal::MetacognitiveAlert {
                    reason: format!("user frustration level: {}", level),
                });
            }
            _ => {}
        }
    }

    /// Prune calls older than the sliding window.
    fn prune_old_calls(&mut self, now: DateTime<Utc>) {
        let cutoff = now - chrono::Duration::seconds(self.config.window_seconds as i64);
        while let Some((ts, _)) = self.recent_calls.front() {
            if *ts < cutoff {
                self.recent_calls.pop_front();
            } else {
                break;
            }
        }
    }

    /// Build an escalation from accumulated signals.
    fn build_escalation(&self, signals: &[SentinelSignal]) -> Escalation {
        if signals.is_empty() {
            return Escalation {
                risk_boost: 0.0,
                force_deliberation: false,
                force_human_veto: false,
                explanation: String::new(),
                signals: vec![],
            };
        }

        let mut risk_boost: f64 = 0.0;
        let mut force_deliberation = false;
        let mut force_human_veto = false;
        let mut explanations = Vec::new();

        for signal in signals {
            match signal {
                SentinelSignal::RateSpike { sigma, .. } => {
                    risk_boost += 0.15;
                    if *sigma > 3.0 {
                        force_deliberation = true;
                    }
                    explanations.push(format!("rate spike ({sigma:.1}σ above baseline)"));
                }
                SentinelSignal::CostDrift { session_cost, threshold } => {
                    risk_boost += 0.1;
                    if *session_cost > threshold * 2.0 {
                        force_deliberation = true;
                    }
                    explanations.push(format!("cost ${session_cost:.2} > ${threshold:.2} threshold"));
                }
                SentinelSignal::RepetitionLoop { tool, count } => {
                    risk_boost += 0.2;
                    force_deliberation = true;
                    if *count > 6 {
                        force_human_veto = true;
                    }
                    explanations.push(format!("{tool} called {count}× with same args"));
                }
                SentinelSignal::PathEscape { tool, path } => {
                    risk_boost += 0.3;
                    force_deliberation = true;
                    explanations.push(format!("{tool} targeting {path} outside workspace"));
                    warn!(tool, path, "sentinel: path escape detected");
                }
                SentinelSignal::UnknownTool { tool } => {
                    risk_boost += 0.1;
                    explanations.push(format!("unknown tool: {tool}"));
                }
                SentinelSignal::MetacognitiveAlert { reason } => {
                    risk_boost += 0.15;
                    force_deliberation = true;
                    explanations.push(format!("metacognition: {reason}"));
                }
            }
        }

        Escalation {
            risk_boost: risk_boost.clamp(0.0, 0.8),
            force_deliberation,
            force_human_veto,
            explanation: explanations.join("; "),
            signals: signals.to_vec(),
        }
    }
}

impl Default for Sentinel {
    fn default() -> Self {
        Self::new(SentinelConfig::default())
    }
}

/// Simple hash of tool name + args for repetition detection.
fn hash_tool_args(tool: &str, args: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    tool.hash(&mut hasher);
    let args_str = args.to_string();
    args_str.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_anomaly_returns_zero_boost() {
        let mut s = Sentinel::default();
        let esc = s.observe("file_read", &serde_json::json!({"path": "/tmp/test.rs"}));
        assert!(esc.risk_boost < f64::EPSILON);
        assert!(!esc.force_deliberation);
        assert!(!esc.force_human_veto);
    }

    #[test]
    fn repetition_triggers_escalation() {
        let mut s = Sentinel::new(SentinelConfig {
            repetition_threshold: 3,
            ..Default::default()
        });
        let args = serde_json::json!({"command": "ls -la"});
        s.observe("shell_exec", &args);
        s.observe("shell_exec", &args);
        let esc = s.observe("shell_exec", &args);
        assert!(esc.risk_boost > 0.1);
        assert!(esc.force_deliberation);
    }

    #[test]
    fn cost_drift_triggers_alert() {
        let mut s = Sentinel::new(SentinelConfig {
            cost_threshold: 0.5,
            ..Default::default()
        });
        s.record_cost(0.6);
        let esc = s.observe("shell_exec", &serde_json::json!({}));
        assert!(esc.risk_boost > 0.0);
        assert!(esc.explanation.contains("cost"));
    }
}
