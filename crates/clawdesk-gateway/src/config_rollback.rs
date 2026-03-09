//! Generational Rollback with Canary Health Monitoring.
//!
//! Maintains a ring buffer of recent configuration snapshots and provides
//! automatic rollback when a canary health function detects degradation
//! after a configuration change.
//!
//! ## Canary Health Model
//!
//! After each configuration commit, a canary window opens. During this window:
//! 1. The composite health function `H(t)` is evaluated at regular intervals.
//! 2. If `H(t) < threshold`, an automatic rollback is triggered.
//! 3. If the canary window closes without issues, the generation is promoted.
//!
//! ## Composite Health Function
//!
//! ```text
//! H(t) = w_err × (1 - error_rate) + w_lat × (1 - latency_ratio) + w_sat × saturation
//! ```
//!
//! Where:
//! - `error_rate`: fraction of requests failing (0.0–1.0)
//! - `latency_ratio`: p99_latency / target_latency (clamped to 0.0–1.0)
//! - `saturation`: 1.0 - resource_utilization (higher = more headroom)

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Health metrics
// ---------------------------------------------------------------------------

/// Instantaneous health metrics for canary evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthMetrics {
    /// Fraction of requests failing (0.0 = no errors, 1.0 = all errors).
    pub error_rate: f64,
    /// P99 latency in milliseconds.
    pub p99_latency_ms: f64,
    /// Target latency in milliseconds (for ratio calculation).
    pub target_latency_ms: f64,
    /// Resource utilization (0.0 = idle, 1.0 = fully saturated).
    pub resource_utilization: f64,
}

impl Default for HealthMetrics {
    fn default() -> Self {
        Self {
            error_rate: 0.0,
            p99_latency_ms: 50.0,
            target_latency_ms: 200.0,
            resource_utilization: 0.3,
        }
    }
}

/// Weights for the composite health function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthWeights {
    /// Weight for error rate component.
    pub error: f64,
    /// Weight for latency component.
    pub latency: f64,
    /// Weight for saturation component.
    pub saturation: f64,
}

impl Default for HealthWeights {
    fn default() -> Self {
        Self {
            error: 0.5,
            latency: 0.3,
            saturation: 0.2,
        }
    }
}

/// Compute the composite health score H(t) ∈ [0.0, 1.0].
///
/// Higher is healthier.
pub fn composite_health(metrics: &HealthMetrics, weights: &HealthWeights) -> f64 {
    let error_component = 1.0 - metrics.error_rate.clamp(0.0, 1.0);

    let latency_ratio = if metrics.target_latency_ms > 0.0 {
        (metrics.p99_latency_ms / metrics.target_latency_ms).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let latency_component = 1.0 - latency_ratio;

    let saturation_component = 1.0 - metrics.resource_utilization.clamp(0.0, 1.0);

    let h = weights.error * error_component
        + weights.latency * latency_component
        + weights.saturation * saturation_component;

    h.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Canary monitor
// ---------------------------------------------------------------------------

/// Configuration for the canary health monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryConfig {
    /// Duration of the canary window after a config change.
    pub canary_window: Duration,
    /// Health check interval within the canary window.
    pub check_interval: Duration,
    /// Health score threshold below which rollback is triggered.
    pub rollback_threshold: f64,
    /// Minimum number of health checks before making a rollback decision.
    pub min_checks: usize,
    /// Health function weights.
    pub weights: HealthWeights,
}

impl Default for CanaryConfig {
    fn default() -> Self {
        Self {
            canary_window: Duration::from_secs(60),
            check_interval: Duration::from_secs(5),
            rollback_threshold: 0.6,
            min_checks: 3,
            weights: HealthWeights::default(),
        }
    }
}

/// State of the canary monitor for a specific generation.
#[derive(Debug, Clone)]
pub struct CanaryState {
    /// Generation being monitored.
    pub generation: u64,
    /// When the canary window started.
    pub started_at: Instant,
    /// Health check results during the canary window.
    pub health_checks: Vec<HealthCheck>,
    /// Current verdict.
    pub verdict: CanaryVerdict,
}

/// A single health check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    /// Health score at this check.
    pub score: f64,
    /// Raw metrics used.
    pub metrics: HealthMetrics,
    /// When this check was performed.
    pub check_index: usize,
}

/// Canary monitoring verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CanaryVerdict {
    /// Canary window still open, monitoring in progress.
    Monitoring,
    /// All checks passed, generation promoted.
    Promoted,
    /// Health degradation detected, rollback triggered.
    RollbackTriggered,
}

impl CanaryState {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            started_at: Instant::now(),
            health_checks: Vec::new(),
            verdict: CanaryVerdict::Monitoring,
        }
    }

    /// Record a health check and evaluate the canary verdict.
    pub fn record_check(
        &mut self,
        metrics: HealthMetrics,
        config: &CanaryConfig,
    ) -> CanaryVerdict {
        let score = composite_health(&metrics, &config.weights);
        let check_index = self.health_checks.len();

        self.health_checks.push(HealthCheck {
            score,
            metrics,
            check_index,
        });

        // Need minimum checks before deciding.
        if self.health_checks.len() < config.min_checks {
            return CanaryVerdict::Monitoring;
        }

        // Check if any recent score is below threshold.
        if score < config.rollback_threshold {
            warn!(
                generation = self.generation,
                score,
                threshold = config.rollback_threshold,
                "canary health BELOW threshold — triggering rollback"
            );
            self.verdict = CanaryVerdict::RollbackTriggered;
            return CanaryVerdict::RollbackTriggered;
        }

        // Check if canary window has closed.
        if self.started_at.elapsed() >= config.canary_window {
            info!(
                generation = self.generation,
                checks = self.health_checks.len(),
                avg_score = self.average_score(),
                "canary window closed — generation promoted"
            );
            self.verdict = CanaryVerdict::Promoted;
            return CanaryVerdict::Promoted;
        }

        CanaryVerdict::Monitoring
    }

    /// Average health score across all checks.
    pub fn average_score(&self) -> f64 {
        if self.health_checks.is_empty() {
            return 1.0;
        }
        let sum: f64 = self.health_checks.iter().map(|c| c.score).sum();
        sum / self.health_checks.len() as f64
    }
}

// ---------------------------------------------------------------------------
// Rollback ring buffer
// ---------------------------------------------------------------------------

/// Entry in the rollback ring buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackEntry {
    /// Generation number.
    pub generation: u64,
    /// When this generation was committed.
    pub committed_at: chrono::DateTime<chrono::Utc>,
    /// Fingerprint of the config at this generation.
    pub fingerprint: String,
    /// Canary verdict (if canary monitoring completed).
    pub canary_verdict: Option<CanaryVerdict>,
    /// Reason for the config change.
    pub reason: Option<String>,
}

/// Ring buffer of recent configuration snapshots for rollback.
pub struct RollbackBuffer {
    entries: Mutex<VecDeque<RollbackEntry>>,
    capacity: usize,
}

impl RollbackBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    /// Push a new generation into the rollback buffer.
    pub fn push(&self, entry: RollbackEntry) {
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() >= self.capacity {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
    }

    /// Get the most recent N entries.
    pub fn recent(&self, count: usize) -> Vec<RollbackEntry> {
        self.entries
            .lock()
            .map(|e| e.iter().rev().take(count).cloned().collect())
            .unwrap_or_default()
    }

    /// Find a specific generation.
    pub fn find(&self, generation: u64) -> Option<RollbackEntry> {
        self.entries
            .lock()
            .ok()?
            .iter()
            .find(|e| e.generation == generation)
            .cloned()
    }

    /// Get the last promoted generation (most recent with Promoted verdict).
    pub fn last_promoted(&self) -> Option<RollbackEntry> {
        self.entries
            .lock()
            .ok()?
            .iter()
            .rev()
            .find(|e| e.canary_verdict == Some(CanaryVerdict::Promoted))
            .cloned()
    }

    /// Number of entries in the buffer.
    pub fn len(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or(0)
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------------------------
// Rollback decision
// ---------------------------------------------------------------------------

/// Decision about what to roll back to.
#[derive(Debug, Clone)]
pub enum RollbackDecision {
    /// Roll back to a specific generation.
    RollbackTo(u64),
    /// Roll back to the last promoted generation.
    RollbackToLastPromoted,
    /// No rollback needed.
    NoAction,
}

/// Determine rollback target based on canary verdict.
pub fn decide_rollback(
    canary: &CanaryState,
    buffer: &RollbackBuffer,
) -> RollbackDecision {
    match canary.verdict {
        CanaryVerdict::RollbackTriggered => {
            if let Some(promoted) = buffer.last_promoted() {
                info!(
                    current = canary.generation,
                    target = promoted.generation,
                    "rolling back to last promoted generation"
                );
                RollbackDecision::RollbackTo(promoted.generation)
            } else {
                // No promoted generation found — roll back to any previous.
                let recent = buffer.recent(2);
                if recent.len() >= 2 {
                    RollbackDecision::RollbackTo(recent[1].generation)
                } else {
                    warn!("no rollback target available");
                    RollbackDecision::NoAction
                }
            }
        }
        _ => RollbackDecision::NoAction,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_health_perfect() {
        let metrics = HealthMetrics {
            error_rate: 0.0,
            p99_latency_ms: 0.0,
            target_latency_ms: 200.0,
            resource_utilization: 0.0,
        };
        let weights = HealthWeights::default();
        let h = composite_health(&metrics, &weights);
        assert!((h - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn composite_health_degraded() {
        let metrics = HealthMetrics {
            error_rate: 0.5,
            p99_latency_ms: 300.0,
            target_latency_ms: 200.0,
            resource_utilization: 0.9,
        };
        let weights = HealthWeights::default();
        let h = composite_health(&metrics, &weights);
        assert!(h < 0.5);
    }

    #[test]
    fn canary_promotes_healthy() {
        let config = CanaryConfig {
            canary_window: Duration::from_millis(10),
            check_interval: Duration::from_millis(1),
            rollback_threshold: 0.5,
            min_checks: 2,
            ..Default::default()
        };
        let mut canary = CanaryState::new(1);
        let good = HealthMetrics::default();

        canary.record_check(good.clone(), &config);
        assert_eq!(canary.verdict, CanaryVerdict::Monitoring);

        canary.record_check(good.clone(), &config);
        std::thread::sleep(Duration::from_millis(15));

        let verdict = canary.record_check(good, &config);
        assert_eq!(verdict, CanaryVerdict::Promoted);
    }

    #[test]
    fn canary_triggers_rollback() {
        let config = CanaryConfig {
            canary_window: Duration::from_secs(60),
            rollback_threshold: 0.6,
            min_checks: 2,
            ..Default::default()
        };
        let mut canary = CanaryState::new(1);
        let good = HealthMetrics::default();
        let bad = HealthMetrics {
            error_rate: 0.8,
            p99_latency_ms: 500.0,
            target_latency_ms: 200.0,
            resource_utilization: 0.95,
        };

        canary.record_check(good, &config);
        canary.record_check(bad.clone(), &config);
        let verdict = canary.record_check(bad, &config);
        assert_eq!(verdict, CanaryVerdict::RollbackTriggered);
    }

    #[test]
    fn rollback_buffer_capacity() {
        let buffer = RollbackBuffer::new(3);
        for i in 0..5 {
            buffer.push(RollbackEntry {
                generation: i,
                committed_at: chrono::Utc::now(),
                fingerprint: format!("fp-{i}"),
                canary_verdict: None,
                reason: None,
            });
        }
        assert_eq!(buffer.len(), 3);
        // Oldest should be generation 2 (0 and 1 evicted).
        let recent = buffer.recent(3);
        assert_eq!(recent[0].generation, 4);
    }

    #[test]
    fn rollback_buffer_find() {
        let buffer = RollbackBuffer::new(10);
        buffer.push(RollbackEntry {
            generation: 42,
            committed_at: chrono::Utc::now(),
            fingerprint: "fp-42".into(),
            canary_verdict: Some(CanaryVerdict::Promoted),
            reason: None,
        });

        assert!(buffer.find(42).is_some());
        assert!(buffer.find(99).is_none());
    }

    #[test]
    fn last_promoted() {
        let buffer = RollbackBuffer::new(10);
        buffer.push(RollbackEntry {
            generation: 1,
            committed_at: chrono::Utc::now(),
            fingerprint: "".into(),
            canary_verdict: Some(CanaryVerdict::Promoted),
            reason: None,
        });
        buffer.push(RollbackEntry {
            generation: 2,
            committed_at: chrono::Utc::now(),
            fingerprint: "".into(),
            canary_verdict: Some(CanaryVerdict::Monitoring),
            reason: None,
        });

        let last = buffer.last_promoted().unwrap();
        assert_eq!(last.generation, 1);
    }

    #[test]
    fn decide_rollback_to_promoted() {
        let buffer = RollbackBuffer::new(10);
        buffer.push(RollbackEntry {
            generation: 5,
            committed_at: chrono::Utc::now(),
            fingerprint: "".into(),
            canary_verdict: Some(CanaryVerdict::Promoted),
            reason: None,
        });

        let mut canary = CanaryState::new(6);
        canary.verdict = CanaryVerdict::RollbackTriggered;

        match decide_rollback(&canary, &buffer) {
            RollbackDecision::RollbackTo(gen) => assert_eq!(gen, 5),
            other => panic!("expected RollbackTo, got {:?}", other),
        }
    }

    #[test]
    fn average_score() {
        let mut canary = CanaryState::new(1);
        canary.health_checks.push(HealthCheck {
            score: 0.8,
            metrics: HealthMetrics::default(),
            check_index: 0,
        });
        canary.health_checks.push(HealthCheck {
            score: 0.6,
            metrics: HealthMetrics::default(),
            check_index: 1,
        });
        assert!((canary.average_score() - 0.7).abs() < f64::EPSILON);
    }
}
