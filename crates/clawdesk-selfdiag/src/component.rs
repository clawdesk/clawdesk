//! Component health tracking.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Duration;

/// A system component that can be monitored.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Component {
    /// LLM provider (anthropic, openai, etc.).
    Provider(String),
    /// Embedding service.
    EmbeddingService,
    /// Memory/vector store (SochDB).
    MemoryStore,
    /// Tool execution subsystem.
    ToolExecution,
    /// Network connectivity.
    Network,
    /// Local disk/storage.
    Storage,
    /// Browser automation.
    Browser,
    /// Custom component.
    Custom(String),
}

/// Health status of a component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Critical,
    Unknown,
}

/// A single health observation.
#[derive(Debug, Clone)]
pub struct HealthObservation {
    /// Whether this operation succeeded.
    pub success: bool,
    /// Latency of the operation.
    pub latency: Duration,
    /// Optional error category.
    pub error_kind: Option<String>,
    /// When this observation was recorded.
    pub timestamp: DateTime<Utc>,
}

/// Tracked health state for a single component.
pub struct ComponentHealth {
    /// EWMA of error rate (0.0–1.0).
    pub error_rate_ewma: f64,
    /// EWMA of latency (milliseconds).
    pub latency_ewma_ms: f64,
    /// Baseline latency (established during healthy operation).
    pub baseline_latency_ms: f64,
    /// Total observations.
    pub total_observations: u64,
    /// Recent errors for pattern detection.
    pub recent_errors: VecDeque<(DateTime<Utc>, String)>,
    /// When this component was last healthy.
    pub last_healthy: DateTime<Utc>,
    /// Maximum recent errors to keep.
    max_recent_errors: usize,
}

impl ComponentHealth {
    pub fn new() -> Self {
        Self {
            error_rate_ewma: 0.0,
            latency_ewma_ms: 100.0, // 100ms default baseline
            baseline_latency_ms: 100.0,
            total_observations: 0,
            recent_errors: VecDeque::new(),
            last_healthy: Utc::now(),
            max_recent_errors: 20,
        }
    }

    /// Record a new observation.
    pub fn record(&mut self, obs: &HealthObservation) {
        self.total_observations += 1;
        let latency_ms = obs.latency.as_secs_f64() * 1000.0;

        // EWMA updates (α = 0.2)
        const ALPHA: f64 = 0.2;
        let error_val = if obs.success { 0.0 } else { 1.0 };
        self.error_rate_ewma = ALPHA * error_val + (1.0 - ALPHA) * self.error_rate_ewma;
        self.latency_ewma_ms = ALPHA * latency_ms + (1.0 - ALPHA) * self.latency_ewma_ms;

        // Update baseline during healthy periods (slow adaptation)
        if obs.success && self.status() == HealthStatus::Healthy {
            const BASELINE_ALPHA: f64 = 0.01; // very slow
            self.baseline_latency_ms = BASELINE_ALPHA * latency_ms
                + (1.0 - BASELINE_ALPHA) * self.baseline_latency_ms;
            self.last_healthy = Utc::now();
        }

        // Track recent errors
        if !obs.success {
            let kind = obs.error_kind.clone().unwrap_or_else(|| "unknown".into());
            self.recent_errors.push_back((obs.timestamp, kind));
            if self.recent_errors.len() > self.max_recent_errors {
                self.recent_errors.pop_front();
            }
        }
    }

    /// Current health status based on error rate and latency.
    pub fn status(&self) -> HealthStatus {
        if self.total_observations < 5 {
            return HealthStatus::Unknown;
        }

        // Critical: >50% error rate OR latency >10x baseline
        if self.error_rate_ewma > 0.5
            || self.latency_ewma_ms > self.baseline_latency_ms * 10.0
        {
            return HealthStatus::Critical;
        }

        // Degraded: >10% error rate OR latency >3x baseline
        if self.error_rate_ewma > 0.1
            || self.latency_ewma_ms > self.baseline_latency_ms * 3.0
        {
            return HealthStatus::Degraded;
        }

        HealthStatus::Healthy
    }

    /// Latency anomaly ratio (current / baseline). >3.0 is concerning.
    pub fn latency_anomaly_ratio(&self) -> f64 {
        if self.baseline_latency_ms <= 0.0 { return 1.0; }
        self.latency_ewma_ms / self.baseline_latency_ms
    }

    /// How long since the component was last healthy.
    pub fn time_since_healthy(&self) -> Duration {
        let elapsed = (Utc::now() - self.last_healthy).num_seconds().max(0) as u64;
        Duration::from_secs(elapsed)
    }

    /// Most common recent error type.
    pub fn dominant_error(&self) -> Option<String> {
        if self.recent_errors.is_empty() { return None; }
        let mut counts = std::collections::HashMap::new();
        for (_, kind) in &self.recent_errors {
            *counts.entry(kind.as_str()).or_insert(0u32) += 1;
        }
        counts.into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(kind, _)| kind.to_string())
    }
}

impl Default for ComponentHealth {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_obs(latency_ms: u64) -> HealthObservation {
        HealthObservation {
            success: true,
            latency: Duration::from_millis(latency_ms),
            error_kind: None,
            timestamp: Utc::now(),
        }
    }

    fn err_obs(kind: &str) -> HealthObservation {
        HealthObservation {
            success: false,
            latency: Duration::from_millis(5000),
            error_kind: Some(kind.into()),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn healthy_on_good_observations() {
        let mut health = ComponentHealth::new();
        for _ in 0..10 {
            health.record(&ok_obs(50));
        }
        assert_eq!(health.status(), HealthStatus::Healthy);
    }

    #[test]
    fn critical_on_many_errors() {
        let mut health = ComponentHealth::new();
        for _ in 0..5 { health.record(&ok_obs(50)); } // establish baseline
        for _ in 0..20 { health.record(&err_obs("timeout")); }
        assert_eq!(health.status(), HealthStatus::Critical);
    }

    #[test]
    fn degraded_on_latency_spike() {
        let mut health = ComponentHealth::new();
        for _ in 0..20 { health.record(&ok_obs(50)); } // baseline ~50ms
        for _ in 0..10 { health.record(&ok_obs(500)); } // 10x spike
        assert!(health.latency_anomaly_ratio() > 2.0);
    }

    #[test]
    fn dominant_error_tracking() {
        let mut health = ComponentHealth::new();
        health.record(&err_obs("timeout"));
        health.record(&err_obs("timeout"));
        health.record(&err_obs("auth_failure"));
        assert_eq!(health.dominant_error(), Some("timeout".into()));
    }
}
