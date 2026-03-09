//! SLO monitoring and error-budget alerting.
//!
//! Uses EWMA (Exponentially Weighted Moving Average) burn rate to detect
//! when a service is consuming its error budget faster than expected.
//!
//! ## Error Budget Model
//!
//! Given an SLO target (e.g., 99.9% success rate), the error budget is:
//!
//! $$
//! \text{error\_budget} = 1 - \text{SLO\_target}
//! $$
//!
//! Burn rate measures how fast the budget is being consumed:
//!
//! $$
//! \text{burn\_rate} = \frac{\text{observed\_error\_rate}}{\text{error\_budget}}
//! $$
//!
//! A burn rate of 1.0 means the budget will be exhausted exactly at the end
//! of the SLO window. A burn rate > 1.0 means premature exhaustion.
//!
//! ## EWMA Smoothing
//!
//! Raw error rates are noisy. EWMA smooths the signal:
//!
//! $$
//! \text{EWMA}_n = \alpha \cdot x_n + (1 - \alpha) \cdot \text{EWMA}_{n-1}
//! $$
//!
//! Default $\alpha = 0.1$ (slow-moving average suitable for SLO windows).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{info, warn};

// ─────────────────────────────────────────────────────────────────────────────
// SLO definition
// ─────────────────────────────────────────────────────────────────────────────

/// An SLO definition with target, window, and alert thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SloDefinition {
    /// Human-readable name (e.g., "provider-latency-p99").
    pub name: String,
    /// SLO target as a fraction (e.g., 0.999 for 99.9%).
    pub target: f64,
    /// SLO window duration.
    #[serde(with = "serde_duration_secs")]
    pub window: Duration,
    /// Burn rate threshold for warning alerts.
    pub warn_burn_rate: f64,
    /// Burn rate threshold for critical alerts.
    pub critical_burn_rate: f64,
}

impl SloDefinition {
    /// Error budget = 1 - target.
    pub fn error_budget(&self) -> f64 {
        1.0 - self.target
    }
}

/// Predefined SLOs for ClawDesk services.
pub fn default_slos() -> Vec<SloDefinition> {
    vec![
        SloDefinition {
            name: "provider-success-rate".into(),
            target: 0.999,
            window: Duration::from_secs(30 * 24 * 3600), // 30 days
            warn_burn_rate: 2.0,
            critical_burn_rate: 10.0,
        },
        SloDefinition {
            name: "agent-response-latency-p99".into(),
            target: 0.99,
            window: Duration::from_secs(7 * 24 * 3600), // 7 days
            warn_burn_rate: 3.0,
            critical_burn_rate: 14.4,
        },
        SloDefinition {
            name: "tool-execution-success".into(),
            target: 0.995,
            window: Duration::from_secs(7 * 24 * 3600),
            warn_burn_rate: 2.0,
            critical_burn_rate: 10.0,
        },
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// EWMA tracker
// ─────────────────────────────────────────────────────────────────────────────

/// EWMA tracker for a single error rate signal.
#[derive(Debug, Clone)]
pub struct EwmaTracker {
    /// Smoothing factor (0 < α ≤ 1). Lower = smoother.
    alpha: f64,
    /// Current EWMA value.
    value: f64,
    /// Whether the tracker has been initialized.
    initialized: bool,
    /// Total observations.
    total: u64,
    /// Total errors.
    errors: u64,
}

impl EwmaTracker {
    pub fn new(alpha: f64) -> Self {
        Self {
            alpha: alpha.clamp(0.001, 1.0),
            value: 0.0,
            initialized: false,
            total: 0,
            errors: 0,
        }
    }

    /// Record an observation. `is_error = true` for failures.
    pub fn record(&mut self, is_error: bool) {
        self.total += 1;
        if is_error {
            self.errors += 1;
        }

        let sample = if is_error { 1.0 } else { 0.0 };
        if !self.initialized {
            self.value = sample;
            self.initialized = true;
        } else {
            self.value = self.alpha * sample + (1.0 - self.alpha) * self.value;
        }
    }

    /// Current smoothed error rate.
    pub fn error_rate(&self) -> f64 {
        self.value
    }

    /// Raw (unsmoothed) error rate.
    pub fn raw_error_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.errors as f64 / self.total as f64
        }
    }

    /// Total observations recorded.
    pub fn total(&self) -> u64 {
        self.total
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SLO monitor
// ─────────────────────────────────────────────────────────────────────────────

/// Alert severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlertSeverity {
    Ok,
    Warning,
    Critical,
}

/// An alert emitted when an SLO's error budget is being consumed too fast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SloAlert {
    /// SLO name.
    pub slo_name: String,
    /// Alert severity.
    pub severity: AlertSeverity,
    /// Current burn rate.
    pub burn_rate: f64,
    /// EWMA error rate at alert time.
    pub error_rate: f64,
    /// Error budget remaining (fraction).
    pub budget_remaining: f64,
    /// Human-readable message.
    pub message: String,
}

/// Per-SLO state tracked by the monitor.
struct SloState {
    definition: SloDefinition,
    tracker: EwmaTracker,
    last_alert: Option<(AlertSeverity, Instant)>,
    /// Minimum interval between alerts of the same severity.
    alert_cooldown: Duration,
}

/// SLO monitor that tracks multiple SLOs and emits alerts.
pub struct SloMonitor {
    states: HashMap<String, SloState>,
    /// EWMA alpha for all trackers.
    alpha: f64,
    /// Alert cooldown to prevent alert storms.
    alert_cooldown: Duration,
}

impl SloMonitor {
    /// Create a new monitor with the given EWMA alpha and alert cooldown.
    pub fn new(alpha: f64, alert_cooldown: Duration) -> Self {
        Self {
            states: HashMap::new(),
            alpha,
            alert_cooldown,
        }
    }

    /// Create a monitor with defaults (α=0.1, 5-minute cooldown).
    pub fn with_defaults() -> Self {
        Self::new(0.1, Duration::from_secs(300))
    }

    /// Register an SLO for monitoring.
    pub fn register(&mut self, slo: SloDefinition) {
        let name = slo.name.clone();
        self.states.insert(
            name,
            SloState {
                definition: slo,
                tracker: EwmaTracker::new(self.alpha),
                last_alert: None,
                alert_cooldown: self.alert_cooldown,
            },
        );
    }

    /// Record an observation for a named SLO.
    ///
    /// Returns an alert if the burn rate exceeds thresholds and the
    /// cooldown period has elapsed since the last alert.
    pub fn record(&mut self, slo_name: &str, is_error: bool) -> Option<SloAlert> {
        let state = self.states.get_mut(slo_name)?;
        state.tracker.record(is_error);

        // Need enough observations for meaningful statistics
        if state.tracker.total() < 10 {
            return None;
        }

        let error_rate = state.tracker.error_rate();
        let error_budget = state.definition.error_budget();

        if error_budget <= 0.0 {
            return None;
        }

        let burn_rate = error_rate / error_budget;

        let severity = if burn_rate >= state.definition.critical_burn_rate {
            AlertSeverity::Critical
        } else if burn_rate >= state.definition.warn_burn_rate {
            AlertSeverity::Warning
        } else {
            AlertSeverity::Ok
        };

        if severity == AlertSeverity::Ok {
            return None;
        }

        // Check cooldown
        if let Some((last_sev, last_time)) = &state.last_alert {
            if *last_sev == severity && last_time.elapsed() < state.alert_cooldown {
                return None;
            }
        }

        // Budget remaining estimate (linear projection over window)
        let window_fraction = 1.0; // Simplified: assume we're at the start of the window
        let budget_remaining = (1.0 - burn_rate * window_fraction).max(0.0);

        let message = format!(
            "SLO '{}' burn rate {:.1}x (error rate: {:.4}, budget remaining: {:.1}%)",
            slo_name,
            burn_rate,
            error_rate,
            budget_remaining * 100.0
        );

        match severity {
            AlertSeverity::Warning => warn!("{}", message),
            AlertSeverity::Critical => warn!(critical = true, "{}", message),
            AlertSeverity::Ok => {}
        }

        state.last_alert = Some((severity, Instant::now()));

        Some(SloAlert {
            slo_name: slo_name.to_string(),
            severity,
            burn_rate,
            error_rate,
            budget_remaining,
            message,
        })
    }

    /// Get the current status of all monitored SLOs.
    pub fn status(&self) -> Vec<SloStatus> {
        self.states
            .values()
            .map(|s| {
                let error_rate = s.tracker.error_rate();
                let error_budget = s.definition.error_budget();
                let burn_rate = if error_budget > 0.0 {
                    error_rate / error_budget
                } else {
                    0.0
                };
                SloStatus {
                    name: s.definition.name.clone(),
                    target: s.definition.target,
                    current_error_rate: error_rate,
                    burn_rate,
                    observations: s.tracker.total(),
                    healthy: burn_rate < s.definition.warn_burn_rate,
                }
            })
            .collect()
    }
}

/// Snapshot of an SLO's current status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SloStatus {
    pub name: String,
    pub target: f64,
    pub current_error_rate: f64,
    pub burn_rate: f64,
    pub observations: u64,
    pub healthy: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Serde helper for Duration as seconds
// ─────────────────────────────────────────────────────────────────────────────

mod serde_duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_converges() {
        let mut tracker = EwmaTracker::new(0.1);

        // 100 successes
        for _ in 0..100 {
            tracker.record(false);
        }
        assert!(tracker.error_rate() < 0.01);

        // Sudden error spike
        for _ in 0..10 {
            tracker.record(true);
        }
        assert!(tracker.error_rate() > 0.05);
    }

    #[test]
    fn slo_monitor_alerts_on_high_burn_rate() {
        let mut monitor = SloMonitor::new(0.5, Duration::from_secs(0)); // Fast alpha, no cooldown

        monitor.register(SloDefinition {
            name: "test-slo".into(),
            target: 0.99,
            window: Duration::from_secs(86400),
            warn_burn_rate: 2.0,
            critical_burn_rate: 10.0,
        });

        // Record 10 successes to prime
        for _ in 0..10 {
            monitor.record("test-slo", false);
        }

        // Spike errors to trigger alert
        let mut alert = None;
        for _ in 0..20 {
            if let Some(a) = monitor.record("test-slo", true) {
                alert = Some(a);
            }
        }

        assert!(alert.is_some());
        let alert = alert.expect("should have alerted");
        assert!(alert.burn_rate > 2.0);
    }

    #[test]
    fn slo_monitor_no_alert_when_healthy() {
        let mut monitor = SloMonitor::with_defaults();
        monitor.register(SloDefinition {
            name: "healthy".into(),
            target: 0.99,
            window: Duration::from_secs(86400),
            warn_burn_rate: 2.0,
            critical_burn_rate: 10.0,
        });

        // All successes
        for _ in 0..100 {
            assert!(monitor.record("healthy", false).is_none());
        }

        let status = monitor.status();
        assert_eq!(status.len(), 1);
        assert!(status[0].healthy);
    }
}
