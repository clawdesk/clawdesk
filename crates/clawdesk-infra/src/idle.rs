//! Idle detection system — fires callbacks when the gateway is idle.
//!
//! Tracks the timestamp of the last meaningful activity (message processed,
//! tool call, session started). When no activity has occurred for
//! `idle_threshold` seconds, the system enters "idle" state and fires
//! registered callbacks (e.g., WAL checkpoint, conversation compaction).
//!
//! The idle check runs on a `check_interval` timer. Once idle callbacks
//! fire, they won't fire again until new activity occurs and the system
//! re-enters idle after the threshold.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Callback invoked when the system transitions to idle.
pub type IdleCallback = Box<dyn Fn() + Send + Sync + 'static>;

/// Configuration for the idle detection system.
#[derive(Debug, Clone)]
pub struct IdleConfig {
    /// How long (seconds) without activity before considered idle.
    pub idle_threshold_secs: u64,
    /// How often (seconds) to check for idleness.
    pub check_interval_secs: u64,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            idle_threshold_secs: 300, // 5 minutes
            check_interval_secs: 60,  // check every minute
        }
    }
}

/// Tracks system activity and fires callbacks when idle.
///
/// Thread-safe: `record_activity()` can be called from any task. The idle
/// detection loop runs in a spawned task and checks the last-activity
/// timestamp atomically.
pub struct IdleDetector {
    /// Monotonic timestamp (nanos since start) of the last activity.
    last_activity_ns: AtomicU64,
    /// Whether the system is currently in idle state.
    is_idle: AtomicBool,
    /// Reference instant for converting between Instant and u64 nanos.
    epoch: Instant,
    config: IdleConfig,
    /// Registered idle callbacks.
    callbacks: Vec<IdleCallback>,
}

impl IdleDetector {
    /// Create a new idle detector with configuration and callbacks.
    pub fn new(config: IdleConfig, callbacks: Vec<IdleCallback>) -> Self {
        let epoch = Instant::now();
        Self {
            last_activity_ns: AtomicU64::new(0),
            is_idle: AtomicBool::new(false),
            epoch,
            config,
            callbacks,
        }
    }

    /// Record that activity occurred — resets the idle timer.
    ///
    /// This is O(1) and lock-free; safe to call from hot paths.
    pub fn record_activity(&self) {
        let now_ns = self.epoch.elapsed().as_nanos() as u64;
        self.last_activity_ns.store(now_ns, Ordering::Relaxed);
        // If we were idle, clear the flag so callbacks can fire again next time.
        self.is_idle.store(false, Ordering::Relaxed);
    }

    /// Check if the system is currently idle.
    pub fn is_idle(&self) -> bool {
        self.is_idle.load(Ordering::Relaxed)
    }

    /// Duration since last activity.
    pub fn idle_duration(&self) -> Duration {
        let last_ns = self.last_activity_ns.load(Ordering::Relaxed);
        let now_ns = self.epoch.elapsed().as_nanos() as u64;
        Duration::from_nanos(now_ns.saturating_sub(last_ns))
    }

    /// Run the idle detection loop. Call from a spawned task.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        let check_interval = Duration::from_secs(self.config.check_interval_secs);
        let threshold = Duration::from_secs(self.config.idle_threshold_secs);

        info!(
            threshold_secs = self.config.idle_threshold_secs,
            check_secs = self.config.check_interval_secs,
            "idle detector started"
        );

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("idle detector shutting down");
                    break;
                }
                _ = tokio::time::sleep(check_interval) => {
                    let idle_dur = self.idle_duration();
                    if idle_dur >= threshold && !self.is_idle.load(Ordering::Relaxed) {
                        // Transition to idle — fire callbacks once.
                        self.is_idle.store(true, Ordering::Relaxed);
                        info!(
                            idle_secs = idle_dur.as_secs(),
                            callbacks = self.callbacks.len(),
                            "system idle, firing callbacks"
                        );
                        for (i, cb) in self.callbacks.iter().enumerate() {
                            let start = Instant::now();
                            cb();
                            let elapsed = start.elapsed();
                            if elapsed > Duration::from_secs(5) {
                                warn!(
                                    callback_idx = i,
                                    elapsed_ms = elapsed.as_millis() as u64,
                                    "idle callback took >5s"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[tokio::test]
    async fn test_idle_detection() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let detector = Arc::new(IdleDetector::new(
            IdleConfig {
                idle_threshold_secs: 0, // immediate
                check_interval_secs: 1,
            },
            vec![Box::new(move || {
                counter_clone.fetch_add(1, Ordering::Relaxed);
            })],
        ));

        assert!(!detector.is_idle());

        // Don't record activity — after threshold, should go idle.
        let cancel = CancellationToken::new();
        let det = detector.clone();
        let c = cancel.clone();
        let handle = tokio::spawn(async move {
            det.run(c).await;
        });

        // Wait for one check interval.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(detector.is_idle());
        assert!(counter.load(Ordering::Relaxed) >= 1);

        cancel.cancel();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_activity_resets_idle() {
        let detector = Arc::new(IdleDetector::new(
            IdleConfig {
                idle_threshold_secs: 1,
                check_interval_secs: 1,
            },
            vec![],
        ));

        // Record activity → should not be idle.
        detector.record_activity();
        assert!(!detector.is_idle());
        assert!(detector.idle_duration() < Duration::from_secs(1));
    }
}
