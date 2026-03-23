//! Homeostatic Controller — PID-like regulation of system vital signs.
//!
//! The hypothalamus of the cognitive architecture. Monitors five vital signs
//! and takes corrective actions to maintain system health within bounds.
//!
//! | Vital Sign       | Metric          | Default Setpoint | Action if Exceeded        |
//! |------------------|-----------------|------------------|---------------------------|
//! | Token Burn Rate  | tokens/minute   | 5,000            | Downgrade model           |
//! | Cost Rate        | $/hour          | $2.00            | Suppress curiosity        |
//! | Error Rate       | errors/total    | 20%              | Tighten consciousness     |
//! | Latency P95      | milliseconds    | 5,000            | Compact context           |
//! | Memory Pressure  | context window% | 80%              | Summarize old turns       |

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tracing::info;

use crate::workspace::{CognitiveEvent, GlobalWorkspace};

/// The five vital signs monitored by the homeostatic controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VitalSign {
    /// Tokens consumed per minute.
    TokenBurnRate,
    /// USD spent per hour.
    CostRate,
    /// Error ratio in a sliding window (0.0–1.0).
    ErrorRate,
    /// 95th percentile latency in milliseconds.
    LatencyP95,
    /// Context window utilization (0.0–1.0).
    MemoryPressure,
}

/// Setpoints for vital signs — the "comfortable" operating range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VitalSetpoints {
    /// Tokens per minute ceiling. Default: 5000.
    pub token_burn_rate: f64,
    /// USD per hour ceiling. Default: 2.0.
    pub cost_rate: f64,
    /// Error rate ceiling. Default: 0.20 (20%).
    pub error_rate: f64,
    /// Latency P95 ceiling in ms. Default: 5000.
    pub latency_p95: f64,
    /// Context window utilization ceiling. Default: 0.80 (80%).
    pub memory_pressure: f64,
}

impl Default for VitalSetpoints {
    fn default() -> Self {
        Self {
            token_burn_rate: 5000.0,
            cost_rate: 2.0,
            error_rate: 0.20,
            latency_p95: 5000.0,
            memory_pressure: 0.80,
        }
    }
}

/// Corrective actions the controller can take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HomeostaticAction {
    /// Switch to a cheaper/faster model.
    DowngradeModel { from: String, to: String },
    /// Stop proactive exploration to save tokens.
    SuppressCuriosity,
    /// Lower consciousness thresholds (more human oversight).
    TightenConsciousness,
    /// Raise consciousness thresholds (less human oversight).
    RelaxConsciousness,
    /// Summarize older conversation turns to free context window.
    CompactContext,
    /// Pause background tasks (cron, exploration, consolidation).
    PauseBackground,
    /// Alert the human about a critical vital sign violation.
    AlertHuman { message: String },
}

impl std::fmt::Display for HomeostaticAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DowngradeModel { from, to } => write!(f, "downgrade model {from} → {to}"),
            Self::SuppressCuriosity => write!(f, "suppress curiosity"),
            Self::TightenConsciousness => write!(f, "tighten consciousness thresholds"),
            Self::RelaxConsciousness => write!(f, "relax consciousness thresholds"),
            Self::CompactContext => write!(f, "compact context window"),
            Self::PauseBackground => write!(f, "pause background tasks"),
            Self::AlertHuman { message } => write!(f, "alert human: {message}"),
        }
    }
}

/// Current system vital signs — updated by external subsystems.
#[derive(Debug, Clone, Default)]
pub struct SystemVitals {
    /// Current token consumption rate (tokens/minute).
    pub token_burn_rate: f64,
    /// Current cost rate (USD/hour).
    pub cost_rate: f64,
    /// Current error rate (0.0–1.0).
    pub error_rate: f64,
    /// Current P95 latency (ms).
    pub latency_p95: f64,
    /// Current context window utilization (0.0–1.0).
    pub memory_pressure: f64,
}

/// Sliding window for error rate calculation.
struct ErrorWindow {
    /// Recent results: true = success, false = error.
    results: VecDeque<(DateTime<Utc>, bool)>,
    /// Window size in seconds.
    window_secs: u64,
}

impl ErrorWindow {
    fn new(window_secs: u64) -> Self {
        Self {
            results: VecDeque::with_capacity(256),
            window_secs,
        }
    }

    fn record(&mut self, success: bool) {
        let now = Utc::now();
        self.results.push_back((now, success));
        self.prune(now);
    }

    fn error_rate(&mut self) -> f64 {
        let now = Utc::now();
        self.prune(now);
        if self.results.is_empty() {
            return 0.0;
        }
        let errors = self.results.iter().filter(|(_, ok)| !ok).count();
        errors as f64 / self.results.len() as f64
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        let cutoff = now - chrono::Duration::seconds(self.window_secs as i64);
        while let Some((ts, _)) = self.results.front() {
            if *ts < cutoff {
                self.results.pop_front();
            } else {
                break;
            }
        }
    }
}

/// Sliding window for latency P95 calculation.
struct LatencyWindow {
    /// Recent latency measurements (ms).
    latencies: VecDeque<(DateTime<Utc>, f64)>,
    window_secs: u64,
}

impl LatencyWindow {
    fn new(window_secs: u64) -> Self {
        Self {
            latencies: VecDeque::with_capacity(256),
            window_secs,
        }
    }

    fn record(&mut self, latency_ms: f64) {
        let now = Utc::now();
        self.latencies.push_back((now, latency_ms));
        self.prune(now);
    }

    fn p95(&mut self) -> f64 {
        let now = Utc::now();
        self.prune(now);
        if self.latencies.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<f64> = self.latencies.iter().map(|(_, l)| *l).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((sorted.len() as f64 * 0.95) as usize).min(sorted.len() - 1);
        sorted[idx]
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        let cutoff = now - chrono::Duration::seconds(self.window_secs as i64);
        while let Some((ts, _)) = self.latencies.front() {
            if *ts < cutoff {
                self.latencies.pop_front();
            } else {
                break;
            }
        }
    }
}

/// The Homeostatic Controller — maintains system health within bounds.
pub struct HomeostaticController {
    setpoints: VitalSetpoints,
    /// Accumulated token count this session.
    total_tokens: u64,
    /// Session start time for rate calculations.
    session_start: DateTime<Utc>,
    /// Accumulated cost this session (USD).
    total_cost: f64,
    /// Error rate sliding window.
    error_window: ErrorWindow,
    /// Latency P95 sliding window.
    latency_window: LatencyWindow,
    /// Current context window utilization (set externally).
    memory_pressure: f64,
    /// Current active model name (for downgrade decisions).
    active_model: String,
    /// Global workspace for publishing events.
    workspace: Option<Arc<GlobalWorkspace>>,
    /// Whether curiosity is currently suppressed.
    curiosity_suppressed: bool,
    /// Whether consciousness is tightened.
    consciousness_tightened: bool,
}

impl HomeostaticController {
    pub fn new(setpoints: VitalSetpoints) -> Self {
        Self {
            setpoints,
            total_tokens: 0,
            session_start: Utc::now(),
            total_cost: 0.0,
            error_window: ErrorWindow::new(60),
            latency_window: LatencyWindow::new(120),
            memory_pressure: 0.0,
            active_model: "default".to_string(),
            workspace: None,
            curiosity_suppressed: false,
            consciousness_tightened: false,
        }
    }

    /// Connect to the global workspace.
    pub fn with_global_workspace(mut self, ws: Arc<GlobalWorkspace>) -> Self {
        self.workspace = Some(ws);
        self
    }

    /// Set the active model name.
    pub fn set_active_model(&mut self, model: String) {
        self.active_model = model;
    }

    /// Record token usage from an LLM call.
    pub fn record_tokens(&mut self, tokens: u64) {
        self.total_tokens += tokens;
    }

    /// Record cost from an LLM call.
    pub fn record_cost(&mut self, cost_usd: f64) {
        self.total_cost += cost_usd;
    }

    /// Record a tool execution result (success/failure + latency).
    pub fn record_execution(&mut self, success: bool, latency_ms: f64) {
        self.error_window.record(success);
        self.latency_window.record(latency_ms);
    }

    /// Set current memory pressure (0.0–1.0).
    pub fn set_memory_pressure(&mut self, pressure: f64) {
        self.memory_pressure = pressure.clamp(0.0, 1.0);
    }

    /// Compute current vital signs.
    pub fn vitals(&mut self) -> SystemVitals {
        let elapsed_minutes = {
            let secs = (Utc::now() - self.session_start).num_seconds().max(1);
            secs as f64 / 60.0
        };
        let elapsed_hours = elapsed_minutes / 60.0;

        SystemVitals {
            token_burn_rate: self.total_tokens as f64 / elapsed_minutes,
            cost_rate: if elapsed_hours > 0.001 {
                self.total_cost / elapsed_hours
            } else {
                0.0
            },
            error_rate: self.error_window.error_rate(),
            latency_p95: self.latency_window.p95(),
            memory_pressure: self.memory_pressure,
        }
    }

    /// Run one tick of the homeostatic control loop.
    ///
    /// Returns corrective actions that should be applied by the caller.
    /// Publishes vital sign violations to the global workspace.
    pub fn tick(&mut self) -> Vec<HomeostaticAction> {
        let vitals = self.vitals();
        let mut actions = Vec::new();

        // Token burn rate check
        if vitals.token_burn_rate > self.setpoints.token_burn_rate {
            let ratio = vitals.token_burn_rate / self.setpoints.token_burn_rate;
            if ratio > 2.0 {
                actions.push(HomeostaticAction::DowngradeModel {
                    from: self.active_model.clone(),
                    to: suggest_cheaper_model(&self.active_model),
                });
            }
            if !self.curiosity_suppressed {
                actions.push(HomeostaticAction::SuppressCuriosity);
                self.curiosity_suppressed = true;
            }
            self.publish_warning("token_burn_rate", ratio);
        } else if self.curiosity_suppressed
            && vitals.token_burn_rate < self.setpoints.token_burn_rate * 0.5
        {
            self.curiosity_suppressed = false; // restore when safe
        }

        // Cost rate check
        if vitals.cost_rate > self.setpoints.cost_rate {
            let ratio = vitals.cost_rate / self.setpoints.cost_rate;
            if ratio > 1.5 {
                actions.push(HomeostaticAction::DowngradeModel {
                    from: self.active_model.clone(),
                    to: suggest_cheaper_model(&self.active_model),
                });
            }
            actions.push(HomeostaticAction::PauseBackground);
            self.publish_warning("cost_rate", ratio);
        }

        // Error rate check
        if vitals.error_rate > self.setpoints.error_rate {
            if !self.consciousness_tightened {
                actions.push(HomeostaticAction::TightenConsciousness);
                self.consciousness_tightened = true;
            }
            if vitals.error_rate > 0.5 {
                actions.push(HomeostaticAction::AlertHuman {
                    message: format!(
                        "Error rate {:.0}% exceeds 50% — agent may be stuck",
                        vitals.error_rate * 100.0
                    ),
                });
            }
            self.publish_warning("error_rate", vitals.error_rate / self.setpoints.error_rate);
        } else if self.consciousness_tightened
            && vitals.error_rate < self.setpoints.error_rate * 0.5
        {
            actions.push(HomeostaticAction::RelaxConsciousness);
            self.consciousness_tightened = false;
        }

        // Latency P95 check
        if vitals.latency_p95 > self.setpoints.latency_p95 {
            actions.push(HomeostaticAction::CompactContext);
            self.publish_warning(
                "latency_p95",
                vitals.latency_p95 / self.setpoints.latency_p95,
            );
        }

        // Memory pressure check
        if vitals.memory_pressure > self.setpoints.memory_pressure {
            actions.push(HomeostaticAction::CompactContext);
            self.publish_warning(
                "memory_pressure",
                vitals.memory_pressure / self.setpoints.memory_pressure,
            );
        }

        // Log actions taken
        for action in &actions {
            info!(%action, "homeostatic corrective action");
            if let Some(ref ws) = self.workspace {
                ws.publish(CognitiveEvent::HomeostaticAction {
                    action: action.to_string(),
                });
            }
        }

        actions
    }

    /// Reset session state.
    pub fn reset(&mut self) {
        self.total_tokens = 0;
        self.total_cost = 0.0;
        self.session_start = Utc::now();
        self.error_window = ErrorWindow::new(60);
        self.latency_window = LatencyWindow::new(120);
        self.memory_pressure = 0.0;
        self.curiosity_suppressed = false;
        self.consciousness_tightened = false;
    }

    fn publish_warning(&self, resource: &str, ratio: f64) {
        if let Some(ref ws) = self.workspace {
            ws.publish(CognitiveEvent::BudgetWarning {
                resource: resource.to_string(),
                usage_pct: ratio.clamp(0.0, 10.0),
            });
        }
    }
}

impl Default for HomeostaticController {
    fn default() -> Self {
        Self::new(VitalSetpoints::default())
    }
}

/// Suggest a cheaper model for downgrade.
fn suggest_cheaper_model(current: &str) -> String {
    let lower = current.to_lowercase();
    if lower.contains("opus") || lower.contains("sonnet") {
        "claude-3-5-haiku-20241022".to_string()
    } else if lower.contains("gpt-4o") && !lower.contains("mini") {
        "gpt-4o-mini".to_string()
    } else if lower.contains("gemini") && lower.contains("pro") {
        "gemini-2.0-flash".to_string()
    } else {
        // Already cheap or unknown — no downgrade
        current.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_operation_no_actions() {
        let mut ctrl = HomeostaticController::new(VitalSetpoints {
            token_burn_rate: 100000.0, // very high ceiling
            cost_rate: 100.0,
            error_rate: 0.9,
            latency_p95: 50000.0,
            memory_pressure: 0.99,
        });
        ctrl.record_tokens(100);
        ctrl.record_execution(true, 50.0);
        let actions = ctrl.tick();
        assert!(actions.is_empty(), "no actions under normal load");
    }

    #[test]
    fn high_error_rate_tightens_consciousness() {
        let mut ctrl = HomeostaticController::default();
        // Record 80% failures
        for _ in 0..8 {
            ctrl.record_execution(false, 100.0);
        }
        for _ in 0..2 {
            ctrl.record_execution(true, 100.0);
        }
        let actions = ctrl.tick();
        assert!(
            actions.iter().any(|a| matches!(a, HomeostaticAction::TightenConsciousness)),
            "should tighten consciousness on high error rate"
        );
    }

    #[test]
    fn high_memory_pressure_compacts_context() {
        let mut ctrl = HomeostaticController::default();
        ctrl.set_memory_pressure(0.95);
        let actions = ctrl.tick();
        assert!(
            actions.iter().any(|a| matches!(a, HomeostaticAction::CompactContext)),
            "should compact context on high memory pressure"
        );
    }

    #[test]
    fn model_downgrade_suggestion() {
        assert_eq!(suggest_cheaper_model("claude-sonnet-4-20250514"), "claude-3-5-haiku-20241022");
        assert_eq!(suggest_cheaper_model("gpt-4o"), "gpt-4o-mini");
        assert_eq!(suggest_cheaper_model("gemini-2.5-pro"), "gemini-2.0-flash");
        assert_eq!(suggest_cheaper_model("llama3.2"), "llama3.2"); // no downgrade
    }
}
