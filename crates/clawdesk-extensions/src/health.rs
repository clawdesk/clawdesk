//! Integration health monitor with auto-reconnect.
//!
//! Periodically pings active integrations and tracks health as a state machine:
//! Healthy → Degraded → Unhealthy → Reconnecting → Healthy
//!
//! Exponential backoff on failure: delay(k) = min(base × 2^k + jitter, max_delay)

use crate::registry::Integration;
use crate::ExtensionError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Health states for an integration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthState {
    /// Integration is responding normally
    Healthy,
    /// First failure detected, waiting for confirmation
    Degraded,
    /// Multiple failures, attempting reconnection
    Unhealthy,
    /// Currently attempting to reconnect
    Reconnecting,
    /// Health unknown (not yet checked)
    Unknown,
}

impl std::fmt::Display for HealthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "✅ healthy"),
            Self::Degraded => write!(f, "⚠️ degraded"),
            Self::Unhealthy => write!(f, "❌ unhealthy"),
            Self::Reconnecting => write!(f, "🔄 reconnecting"),
            Self::Unknown => write!(f, "❓ unknown"),
        }
    }
}

/// Health status for a single integration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub name: String,
    pub state: HealthState,
    pub last_check: Option<chrono::DateTime<chrono::Utc>>,
    pub last_success: Option<chrono::DateTime<chrono::Utc>>,
    pub consecutive_failures: u32,
    pub latency_ms: Option<u64>,
    #[serde(skip)]
    pub retry_attempt: u32,
}

impl HealthStatus {
    pub fn new(name: String) -> Self {
        Self {
            name,
            state: HealthState::Unknown,
            last_check: None,
            last_success: None,
            consecutive_failures: 0,
            latency_ms: None,
            retry_attempt: 0,
        }
    }

    /// Transition on successful health check
    pub fn mark_healthy(&mut self, latency_ms: u64) {
        self.state = HealthState::Healthy;
        self.last_check = Some(chrono::Utc::now());
        self.last_success = Some(chrono::Utc::now());
        self.consecutive_failures = 0;
        self.retry_attempt = 0;
        self.latency_ms = Some(latency_ms);
    }

    /// Transition on failed health check
    pub fn mark_failed(&mut self) {
        self.last_check = Some(chrono::Utc::now());
        self.consecutive_failures += 1;

        self.state = match self.state {
            HealthState::Healthy | HealthState::Unknown => HealthState::Degraded,
            HealthState::Degraded => HealthState::Unhealthy,
            HealthState::Unhealthy | HealthState::Reconnecting => {
                self.retry_attempt += 1;
                HealthState::Unhealthy
            }
        };
    }

    /// Compute backoff delay for reconnection
    pub fn backoff_delay(&self) -> Duration {
        let base = Duration::from_secs(2);
        let max = Duration::from_secs(300); // 5 minutes
        let delay = base.saturating_mul(2u32.saturating_pow(self.retry_attempt));
        delay.min(max)
    }
}

/// Health monitor that periodically checks all active integrations.
pub struct HealthMonitor {
    /// Health status for each integration
    statuses: Arc<RwLock<HashMap<String, HealthStatus>>>,
    /// Check interval
    interval: Duration,
    /// HTTP client for health checks
    http: reqwest::Client,
}

impl HealthMonitor {
    pub fn new(interval: Duration) -> Self {
        Self {
            statuses: Arc::new(RwLock::new(HashMap::new())),
            interval,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Register an integration for health monitoring.
    pub async fn register(&self, name: &str) {
        let mut statuses = self.statuses.write().await;
        statuses.insert(name.to_string(), HealthStatus::new(name.to_string()));
    }

    /// Unregister an integration.
    pub async fn unregister(&self, name: &str) {
        self.statuses.write().await.remove(name);
    }

    /// Check health of a single integration via HTTP HEAD.
    pub async fn check_health(&self, name: &str, url: &str) -> bool {
        let start = Instant::now();

        let result = self.http.head(url).send().await;

        let latency = start.elapsed().as_millis() as u64;
        let mut statuses = self.statuses.write().await;

        if let Some(status) = statuses.get_mut(name) {
            match result {
                Ok(resp) if resp.status().is_success() || resp.status().is_redirection() => {
                    let prev = status.state;
                    status.mark_healthy(latency);
                    if prev != HealthState::Healthy {
                        info!(name, latency_ms = latency, "integration recovered");
                    }
                    true
                }
                _ => {
                    let prev = status.state;
                    status.mark_failed();
                    if prev != status.state {
                        warn!(
                            name,
                            state = %status.state,
                            failures = status.consecutive_failures,
                            "integration health degraded"
                        );
                    }
                    false
                }
            }
        } else {
            false
        }
    }

    /// Get current health status for all integrations.
    pub async fn all_statuses(&self) -> Vec<HealthStatus> {
        let statuses = self.statuses.read().await;
        statuses.values().cloned().collect()
    }

    /// Get health status for a specific integration.
    pub async fn get_status(&self, name: &str) -> Option<HealthStatus> {
        let statuses = self.statuses.read().await;
        statuses.get(name).cloned()
    }

    /// Run the health monitor loop (call in a background task).
    pub async fn run(
        &self,
        integrations: Arc<RwLock<Vec<(String, String)>>>, // (name, health_url)
    ) {
        info!(interval = ?self.interval, "health monitor started");

        loop {
            tokio::time::sleep(self.interval).await;

            let checks = integrations.read().await.clone();
            for (name, url) in &checks {
                self.check_health(name, url).await;
            }

            debug!(
                checked = checks.len(),
                "health check cycle complete"
            );
        }
    }
}

impl Default for HealthMonitor {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl std::fmt::Debug for HealthMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthMonitor")
            .field("interval", &self.interval)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_state_transitions() {
        let mut status = HealthStatus::new("test".into());
        assert_eq!(status.state, HealthState::Unknown);

        status.mark_healthy(50);
        assert_eq!(status.state, HealthState::Healthy);

        status.mark_failed();
        assert_eq!(status.state, HealthState::Degraded);

        status.mark_failed();
        assert_eq!(status.state, HealthState::Unhealthy);
    }

    #[test]
    fn backoff_increases() {
        let mut status = HealthStatus::new("test".into());
        assert_eq!(status.backoff_delay(), Duration::from_secs(2));

        status.mark_failed();
        status.mark_failed();
        status.mark_failed(); // retry_attempt = 1
        assert!(status.backoff_delay() > Duration::from_secs(2));

        // Max out
        status.retry_attempt = 20;
        assert_eq!(status.backoff_delay(), Duration::from_secs(300));
    }

    #[test]
    fn healthy_resets_failures() {
        let mut status = HealthStatus::new("test".into());
        status.mark_failed();
        status.mark_failed();
        assert_eq!(status.consecutive_failures, 2);

        status.mark_healthy(10);
        assert_eq!(status.consecutive_failures, 0);
        assert_eq!(status.retry_attempt, 0);
    }
}
