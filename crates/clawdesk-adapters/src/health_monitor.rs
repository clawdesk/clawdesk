//! Channel health monitoring — automatic reconnection with priority-queue scheduling.
//!
//! Single BinaryHeap timer for all channels. Next check dequeued in O(1),
//! re-enqueued in O(log c). Total CPU: O(Σ(1/h_i) × log c) per second.

use serde::{Deserialize, Serialize};
use std::collections::{BinaryHeap, HashMap};
use std::time::{Duration, Instant};
use tracing::warn;

/// Health check result for a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelHealth {
    pub channel_id: String,
    pub healthy: bool,
    pub last_check: String,
    pub consecutive_failures: u32,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

/// Configuration for health monitoring.
#[derive(Debug, Clone)]
pub struct HealthMonitorConfig {
    /// Default check interval.
    pub default_interval: Duration,
    /// Per-channel interval overrides.
    pub channel_intervals: HashMap<String, Duration>,
    /// Max consecutive failures before marking unhealthy.
    pub max_failures: u32,
    /// Backoff multiplier for failed channels.
    pub backoff_factor: f64,
    /// Maximum backoff interval.
    pub max_backoff: Duration,
}

impl Default for HealthMonitorConfig {
    fn default() -> Self {
        Self {
            default_interval: Duration::from_secs(30),
            channel_intervals: HashMap::new(),
            max_failures: 3,
            backoff_factor: 2.0,
            max_backoff: Duration::from_secs(300),
        }
    }
}

/// Scheduled health check entry.
#[derive(Debug, Clone)]
struct ScheduledCheck {
    channel_id: String,
    due_at: Instant,
    #[allow(dead_code)]
    interval: Duration,
}

impl PartialEq for ScheduledCheck {
    fn eq(&self, other: &Self) -> bool { self.due_at == other.due_at }
}
impl Eq for ScheduledCheck {}
impl PartialOrd for ScheduledCheck {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ScheduledCheck {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse for min-heap behavior.
        other.due_at.cmp(&self.due_at)
    }
}

/// Channel health monitor with priority-queue scheduling.
pub struct HealthMonitor {
    config: HealthMonitorConfig,
    schedule: BinaryHeap<ScheduledCheck>,
    status: HashMap<String, ChannelHealth>,
}

impl HealthMonitor {
    pub fn new(config: HealthMonitorConfig) -> Self {
        Self {
            config,
            schedule: BinaryHeap::new(),
            status: HashMap::new(),
        }
    }

    /// Register a channel for health monitoring.
    pub fn register(&mut self, channel_id: &str) {
        let interval = self.config.channel_intervals
            .get(channel_id)
            .copied()
            .unwrap_or(self.config.default_interval);

        self.schedule.push(ScheduledCheck {
            channel_id: channel_id.to_string(),
            due_at: Instant::now(),
            interval,
        });

        self.status.insert(channel_id.to_string(), ChannelHealth {
            channel_id: channel_id.to_string(),
            healthy: true,
            last_check: String::new(),
            consecutive_failures: 0,
            latency_ms: None,
            error: None,
        });
    }

    /// Get the next channel to check (O(1) dequeue).
    pub fn next_due(&mut self) -> Option<String> {
        if let Some(check) = self.schedule.peek() {
            if check.due_at <= Instant::now() {
                let check = self.schedule.pop().unwrap();
                return Some(check.channel_id);
            }
        }
        None
    }

    /// Record a health check result and reschedule.
    pub fn record_result(&mut self, channel_id: &str, healthy: bool, latency_ms: Option<u64>, error: Option<String>) {
        let interval = self.config.channel_intervals
            .get(channel_id)
            .copied()
            .unwrap_or(self.config.default_interval);

        if let Some(status) = self.status.get_mut(channel_id) {
            status.healthy = healthy;
            status.latency_ms = latency_ms;
            status.error = error;
            status.last_check = chrono::Utc::now().to_rfc3339();

            if healthy {
                status.consecutive_failures = 0;
                // Normal interval.
                self.schedule.push(ScheduledCheck {
                    channel_id: channel_id.to_string(),
                    due_at: Instant::now() + interval,
                    interval,
                });
            } else {
                status.consecutive_failures += 1;
                // Backoff: interval × backoff_factor^failures, capped.
                let backoff = interval.mul_f64(
                    self.config.backoff_factor.powi(status.consecutive_failures as i32)
                );
                let capped = backoff.min(self.config.max_backoff);
                warn!(
                    channel = channel_id,
                    failures = status.consecutive_failures,
                    next_check_secs = capped.as_secs(),
                    "channel health check failed — backing off"
                );
                self.schedule.push(ScheduledCheck {
                    channel_id: channel_id.to_string(),
                    due_at: Instant::now() + capped,
                    interval,
                });
            }
        }
    }

    /// Get all channel health statuses.
    pub fn all_status(&self) -> Vec<&ChannelHealth> {
        self.status.values().collect()
    }

    /// Get health for a specific channel.
    pub fn get_status(&self, channel_id: &str) -> Option<&ChannelHealth> {
        self.status.get(channel_id)
    }

    /// Number of monitored channels.
    pub fn channel_count(&self) -> usize {
        self.status.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_check() {
        let mut monitor = HealthMonitor::new(HealthMonitorConfig::default());
        monitor.register("telegram");
        monitor.register("discord");
        assert_eq!(monitor.channel_count(), 2);

        // Both should be due immediately.
        assert!(monitor.next_due().is_some());
    }

    #[test]
    fn failure_backoff() {
        let mut monitor = HealthMonitor::new(HealthMonitorConfig {
            default_interval: Duration::from_secs(10),
            ..Default::default()
        });
        monitor.register("slack");
        let _ = monitor.next_due(); // drain the initial check

        monitor.record_result("slack", false, None, Some("timeout".into()));
        let status = monitor.get_status("slack").unwrap();
        assert_eq!(status.consecutive_failures, 1);
        assert!(!status.healthy);
    }

    #[test]
    fn recovery_resets_failures() {
        let mut monitor = HealthMonitor::new(HealthMonitorConfig::default());
        monitor.register("irc");
        let _ = monitor.next_due();

        monitor.record_result("irc", false, None, Some("err".into()));
        assert_eq!(monitor.get_status("irc").unwrap().consecutive_failures, 1);

        monitor.record_result("irc", true, Some(50), None);
        assert_eq!(monitor.get_status("irc").unwrap().consecutive_failures, 0);
    }
}
