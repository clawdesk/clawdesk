//! Channel health monitoring — tracks liveness, latency, and error rates.
//!
//! Uses a fixed-size array indexed by `ChannelId` for true per-channel
//! concurrent locking. Health probes for different channels never contend.

use clawdesk_types::channel::ChannelId;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Health status of a channel.
#[derive(Debug, Clone)]
pub struct ChannelHealth {
    pub channel_id: ChannelId,
    pub status: HealthStatus,
    pub last_check: Option<Instant>,
    pub latency_ms: Option<u64>,
    pub error_count: u64,
    pub success_count: u64,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
}

/// Health check status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// Channel is healthy and responsive.
    Healthy,
    /// Channel is experiencing intermittent issues.
    Degraded,
    /// Channel is unreachable or erroring consistently.
    Unhealthy,
    /// Channel has not been checked yet.
    Unknown,
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Degraded => write!(f, "degraded"),
            Self::Unhealthy => write!(f, "unhealthy"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Threshold configuration for health assessment.
#[derive(Debug, Clone)]
pub struct HealthThresholds {
    /// Consecutive failures before marking as unhealthy.
    pub unhealthy_after: u32,
    /// Consecutive failures before marking as degraded.
    pub degraded_after: u32,
    /// Latency above this (ms) counts as degraded.
    pub latency_degraded_ms: u64,
    /// Successful probes to recover from unhealthy.
    pub recovery_count: u32,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            unhealthy_after: 5,
            degraded_after: 2,
            latency_degraded_ms: 5000,
            recovery_count: 3,
        }
    }
}

/// Number of `ChannelId` variants. Must be kept in sync with the enum.
const NUM_CHANNELS: usize = 9;

/// Map a `ChannelId` to a fixed array index.
fn channel_index(id: ChannelId) -> usize {
    match id {
        ChannelId::Telegram => 0,
        ChannelId::Discord => 1,
        ChannelId::Slack => 2,
        ChannelId::WhatsApp => 3,
        ChannelId::WebChat => 4,
        ChannelId::Email => 5,
        ChannelId::IMessage => 6,
        ChannelId::Irc => 7,
        ChannelId::Internal => 8,
    }
}

/// All channel IDs for iteration.
const ALL_CHANNELS: [ChannelId; NUM_CHANNELS] = [
    ChannelId::Telegram,
    ChannelId::Discord,
    ChannelId::Slack,
    ChannelId::WhatsApp,
    ChannelId::WebChat,
    ChannelId::Email,
    ChannelId::IMessage,
    ChannelId::Irc,
    ChannelId::Internal,
];

/// Monitors health of all registered channels.
///
/// Uses a fixed-size array with per-channel `RwLock` instead of a global
/// `RwLock<HashMap>`. This eliminates cross-channel contention: health probes
/// for Telegram, Discord, and Slack can execute in true parallel.
///
/// Access: O(1) array index, no hash computation.
/// Contention: per-channel only (13 independent locks).
pub struct HealthMonitor {
    /// Per-channel health state. Each element has its own RwLock.
    slots: [RwLock<ChannelHealth>; NUM_CHANNELS],
    thresholds: HealthThresholds,
}

impl HealthMonitor {
    pub fn new(thresholds: HealthThresholds) -> Self {
        Self {
            slots: ALL_CHANNELS.map(|id| {
                RwLock::new(ChannelHealth {
                    channel_id: id,
                    status: HealthStatus::Unknown,
                    last_check: None,
                    latency_ms: None,
                    error_count: 0,
                    success_count: 0,
                    consecutive_failures: 0,
                    last_error: None,
                })
            }),
            thresholds,
        }
    }

    /// Register a channel for monitoring (resets state to Unknown).
    pub async fn register(&self, channel_id: ChannelId) {
        let mut health = self.slots[channel_index(channel_id)].write().await;
        health.status = HealthStatus::Unknown;
        health.last_check = None;
        health.latency_ms = None;
        health.error_count = 0;
        health.success_count = 0;
        health.consecutive_failures = 0;
        health.last_error = None;
    }

    /// Record a successful health check probe.
    /// Only acquires the write lock for the specific channel.
    pub async fn record_success(&self, channel_id: ChannelId, latency: Duration) {
        let mut health = self.slots[channel_index(channel_id)].write().await;
        health.last_check = Some(Instant::now());
        health.latency_ms = Some(latency.as_millis() as u64);
        health.success_count += 1;
        health.consecutive_failures = 0;

        health.status = if latency.as_millis() as u64 > self.thresholds.latency_degraded_ms {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        };

        debug!(%channel_id, latency_ms = latency.as_millis(), "health check passed");
    }

    /// Record a failed health check probe.
    /// Only acquires the write lock for the specific channel.
    pub async fn record_failure(&self, channel_id: ChannelId, error: String) {
        let mut health = self.slots[channel_index(channel_id)].write().await;
        health.last_check = Some(Instant::now());
        health.error_count += 1;
        health.consecutive_failures += 1;
        health.last_error = Some(error.clone());

        health.status = if health.consecutive_failures >= self.thresholds.unhealthy_after {
            HealthStatus::Unhealthy
        } else if health.consecutive_failures >= self.thresholds.degraded_after {
            HealthStatus::Degraded
        } else {
            health.status
        };

        warn!(
            %channel_id,
            consecutive_failures = health.consecutive_failures,
            %error,
            "health check failed"
        );
    }

    /// Get health status for a specific channel. O(1) array access.
    pub async fn get(&self, channel_id: &ChannelId) -> Option<ChannelHealth> {
        let health = self.slots[channel_index(*channel_id)].read().await;
        Some(health.clone())
    }

    /// Get health status for all channels.
    /// Acquires each channel's read lock independently — no global lock.
    pub async fn all(&self) -> Vec<ChannelHealth> {
        let mut result = Vec::with_capacity(NUM_CHANNELS);
        for slot in &self.slots {
            let health = slot.read().await;
            result.push(health.clone());
        }
        result
    }

    /// Get all unhealthy or degraded channels.
    pub async fn unhealthy(&self) -> Vec<ChannelHealth> {
        let mut result = Vec::new();
        for slot in &self.slots {
            let health = slot.read().await;
            if matches!(health.status, HealthStatus::Unhealthy | HealthStatus::Degraded) {
                result.push(health.clone());
            }
        }
        result
    }
}

impl Default for HealthMonitor {
    fn default() -> Self {
        Self::new(HealthThresholds::default())
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_starts_unknown() {
        let monitor = HealthMonitor::default();
        monitor.register(ChannelId::Telegram).await;
        let health = monitor.get(&ChannelId::Telegram).await.unwrap();
        assert_eq!(health.status, HealthStatus::Unknown);
    }

    #[tokio::test]
    async fn success_marks_healthy() {
        let monitor = HealthMonitor::default();
        monitor.register(ChannelId::Telegram).await;
        monitor
            .record_success(ChannelId::Telegram, Duration::from_millis(50))
            .await;
        let health = monitor.get(&ChannelId::Telegram).await.unwrap();
        assert_eq!(health.status, HealthStatus::Healthy);
        assert_eq!(health.success_count, 1);
    }

    #[tokio::test]
    async fn repeated_failures_mark_degraded_then_unhealthy() {
        let monitor = HealthMonitor::new(HealthThresholds {
            degraded_after: 2,
            unhealthy_after: 4,
            ..Default::default()
        });
        monitor.register(ChannelId::Discord).await;

        // First failure — still unknown/same
        monitor
            .record_failure(ChannelId::Discord, "timeout".into())
            .await;
        let h = monitor.get(&ChannelId::Discord).await.unwrap();
        assert_eq!(h.consecutive_failures, 1);

        // Second failure → degraded
        monitor
            .record_failure(ChannelId::Discord, "timeout".into())
            .await;
        let h = monitor.get(&ChannelId::Discord).await.unwrap();
        assert_eq!(h.status, HealthStatus::Degraded);

        // Fourth failure → unhealthy
        monitor
            .record_failure(ChannelId::Discord, "timeout".into())
            .await;
        monitor
            .record_failure(ChannelId::Discord, "timeout".into())
            .await;
        let h = monitor.get(&ChannelId::Discord).await.unwrap();
        assert_eq!(h.status, HealthStatus::Unhealthy);
    }

    #[tokio::test]
    async fn success_resets_consecutive_failures() {
        let monitor = HealthMonitor::new(HealthThresholds {
            degraded_after: 2,
            unhealthy_after: 4,
            ..Default::default()
        });
        monitor.register(ChannelId::Slack).await;

        // Two failures → degraded
        monitor.record_failure(ChannelId::Slack, "err".into()).await;
        monitor.record_failure(ChannelId::Slack, "err".into()).await;
        assert_eq!(
            monitor.get(&ChannelId::Slack).await.unwrap().status,
            HealthStatus::Degraded
        );

        // Success → healthy, resets counter
        monitor
            .record_success(ChannelId::Slack, Duration::from_millis(10))
            .await;
        let h = monitor.get(&ChannelId::Slack).await.unwrap();
        assert_eq!(h.status, HealthStatus::Healthy);
        assert_eq!(h.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn high_latency_marks_degraded() {
        let monitor = HealthMonitor::new(HealthThresholds {
            latency_degraded_ms: 100,
            ..Default::default()
        });
        monitor.register(ChannelId::WhatsApp).await;
        monitor
            .record_success(ChannelId::WhatsApp, Duration::from_millis(200))
            .await;
        let h = monitor.get(&ChannelId::WhatsApp).await.unwrap();
        assert_eq!(h.status, HealthStatus::Degraded);
    }
}
