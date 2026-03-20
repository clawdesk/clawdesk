//! Daemon sleep/wake optimization for sub-15MB idle RSS.
//!
//! ## Memory Model
//!
//! ```text
//! M_idle = M_binary_mapped (~8 MB) + M_tokio_single (~0.5 MB)
//!        + M_bus (~1 MB) + M_cron (~0.5 MB)
//!        + M_sochdb_wal (~2 MB) + M_tls_cache (~1 MB) ≈ 13 MB
//! ```
//!
//! ## Sleep Transition
//!
//! Leaky bucket: `activity_rate = events/sec`
//! When rate < θ_sleep (0.01 events/sec) for t_debounce (300s): enter sleep.
//! Wake: O(1) via epoll/kqueue → tokio waker → spawn workers.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// Daemon power state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerState {
    /// Full operation — all subsystems active
    Active,
    /// Reduced polling, cloud connections dropped
    Sleep,
    /// Transitioning between states
    Transitioning,
}

/// Configuration for sleep/wake behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SleepWakeConfig {
    /// Activity rate threshold to enter sleep (events/sec)
    pub sleep_threshold: f64,
    /// Duration below threshold before sleeping (seconds)
    pub sleep_debounce_secs: u64,
    /// Polling interval multiplier when sleeping
    pub sleep_poll_multiplier: u32,
    /// Whether to drop cloud WebSocket connections when sleeping
    pub drop_cloud_on_sleep: bool,
    /// Whether to release provider connection pools when sleeping
    pub release_pools_on_sleep: bool,
    /// Whether to compact SochDB after idle period
    pub compact_on_sleep: bool,
    /// Idle time before compaction (seconds)
    pub compact_after_idle_secs: u64,
}

impl Default for SleepWakeConfig {
    fn default() -> Self {
        Self {
            sleep_threshold: 0.01,
            sleep_debounce_secs: 300,
            sleep_poll_multiplier: 10,
            drop_cloud_on_sleep: true,
            release_pools_on_sleep: true,
            compact_on_sleep: true,
            compact_after_idle_secs: 300,
        }
    }
}

/// Activity tracker using a leaky bucket model.
pub struct ActivityTracker {
    /// Number of events in the current window
    event_count: AtomicU64,
    /// Window start time
    window_start: Instant,
    /// Window duration
    window_duration: Duration,
    /// Is currently sleeping
    sleeping: AtomicBool,
    /// Time sleeping started
    sleep_start: Option<Instant>,
    /// Consecutive windows below threshold
    quiet_windows: u32,
    /// Config
    config: SleepWakeConfig,
}

impl ActivityTracker {
    pub fn new(config: SleepWakeConfig) -> Self {
        Self {
            event_count: AtomicU64::new(0),
            window_start: Instant::now(),
            window_duration: Duration::from_secs(60),
            sleeping: AtomicBool::new(false),
            sleep_start: None,
            quiet_windows: 0,
            config,
        }
    }

    /// Record an activity event.
    pub fn record_activity(&self) {
        self.event_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Check whether the daemon should transition state.
    ///
    /// Call this periodically (e.g., every minute).
    pub fn evaluate(&mut self) -> Option<PowerTransition> {
        let elapsed = self.window_start.elapsed();
        if elapsed < self.window_duration {
            return None;
        }

        let count = self.event_count.swap(0, Ordering::Relaxed);
        let rate = count as f64 / elapsed.as_secs_f64();
        self.window_start = Instant::now();

        let is_sleeping = self.sleeping.load(Ordering::Relaxed);

        if !is_sleeping && rate < self.config.sleep_threshold {
            self.quiet_windows += 1;
            let debounce_windows = (self.config.sleep_debounce_secs / self.window_duration.as_secs()).max(1) as u32;

            if self.quiet_windows >= debounce_windows {
                self.sleeping.store(true, Ordering::Relaxed);
                self.sleep_start = Some(Instant::now());
                self.quiet_windows = 0;
                info!(rate, "daemon entering sleep mode");
                return Some(PowerTransition::EnterSleep);
            }
        } else if !is_sleeping {
            self.quiet_windows = 0;
        }

        if is_sleeping && rate >= self.config.sleep_threshold {
            self.sleeping.store(false, Ordering::Relaxed);
            let sleep_duration = self.sleep_start.map(|s| s.elapsed());
            self.sleep_start = None;
            info!(rate, ?sleep_duration, "daemon waking from sleep");
            return Some(PowerTransition::Wake);
        }

        None
    }

    /// Whether the daemon is currently sleeping.
    pub fn is_sleeping(&self) -> bool {
        self.sleeping.load(Ordering::Relaxed)
    }

    /// Current activity rate (approximate).
    pub fn approx_rate(&self) -> f64 {
        let count = self.event_count.load(Ordering::Relaxed);
        let elapsed = self.window_start.elapsed().as_secs_f64().max(1.0);
        count as f64 / elapsed
    }
}

/// Power state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerTransition {
    /// Transition to sleep mode
    EnterSleep,
    /// Wake from sleep mode
    Wake,
}

/// Actions to take on sleep transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SleepActions {
    /// Switch to single-threaded tokio runtime
    pub minimize_threads: bool,
    /// Release provider HTTP connection pools
    pub release_pools: bool,
    /// Drop cloud WebSocket connections
    pub drop_cloud_ws: bool,
    /// Compact SochDB
    pub compact_db: bool,
    /// Reduce cron polling intervals
    pub reduce_polling: bool,
    /// Drop cached model metadata
    pub drop_model_cache: bool,
}

impl SleepActions {
    pub fn from_config(config: &SleepWakeConfig) -> Self {
        Self {
            minimize_threads: true,
            release_pools: config.release_pools_on_sleep,
            drop_cloud_ws: config.drop_cloud_on_sleep,
            compact_db: config.compact_on_sleep,
            reduce_polling: true,
            drop_model_cache: true,
        }
    }
}

/// Actions to take on wake transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeActions {
    /// Spawn worker threads back
    pub restore_threads: bool,
    /// Re-establish cloud connections
    pub reconnect_cloud: bool,
    /// Restore normal polling intervals
    pub restore_polling: bool,
}

impl Default for WakeActions {
    fn default() -> Self {
        Self {
            restore_threads: true,
            reconnect_cloud: true,
            restore_polling: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initially_active() {
        let tracker = ActivityTracker::new(SleepWakeConfig::default());
        assert!(!tracker.is_sleeping());
    }

    #[test]
    fn records_activity() {
        let tracker = ActivityTracker::new(SleepWakeConfig::default());
        tracker.record_activity();
        tracker.record_activity();
        assert!(tracker.approx_rate() > 0.0);
    }

    #[test]
    fn sleep_config_defaults() {
        let config = SleepWakeConfig::default();
        assert_eq!(config.sleep_threshold, 0.01);
        assert_eq!(config.sleep_debounce_secs, 300);
        assert!(config.drop_cloud_on_sleep);
    }
}
