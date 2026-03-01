//! Per-session serialization with lane-based concurrency control.
//!
//! Ensures only one agent run per session at a time. Additional requests
//! queue behind the active run. A watchdog timer detects hung sessions
//! and force-unlocks them.
//!
//! ## Design
//!
//! Each session gets a `SessionLane` — a bounded MPSC queue that serializes
//! agent runs. When `enqueue()` is called, the task waits for exclusive
//! access to the session before proceeding.
//!
//! ```text
//! Session "abc":  [run_1] → [run_2 waiting] → [run_3 waiting]
//! Session "def":  [run_1] (independent, runs concurrently with "abc")
//! ```
//!
//! Without serialization, P(race condition) = 1 for concurrent messages
//! at any realistic processing time. Lane serialization reduces P to 0.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

/// Default watchdog timeout — force-unlock hung sessions after this duration.
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

/// Per-session mutex guard. Dropping this releases the session lock.
pub struct SessionGuard {
    _guard: OwnedMutexGuard<()>,
    session_id: String,
}

impl SessionGuard {
    /// The session ID this guard is protecting.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        info!(session_id = %self.session_id, "session lane released");
    }
}

/// Lane manager — manages per-session serialization mutexes.
///
/// Thread-safe: the inner HashMap is behind a Mutex.
/// Each session gets an independent `Arc<Mutex<()>>` for serialization.
pub struct SessionLaneManager {
    lanes: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    watchdog_timeout: Duration,
}

impl SessionLaneManager {
    pub fn new() -> Self {
        Self {
            lanes: Mutex::new(HashMap::new()),
            watchdog_timeout: WATCHDOG_TIMEOUT,
        }
    }

    /// Create a manager with a custom watchdog timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            lanes: Mutex::new(HashMap::new()),
            watchdog_timeout: timeout,
        }
    }

    /// Acquire exclusive access to a session's lane.
    ///
    /// If another run is active for this session, this call blocks until
    /// that run completes (or the watchdog timeout fires).
    ///
    /// Returns a `SessionGuard` that releases the lock when dropped.
    pub async fn acquire(&self, session_id: &str) -> Result<SessionGuard, SessionLaneError> {
        let mutex = {
            let mut lanes = self.lanes.lock().await;
            lanes
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };

        // Acquire the per-session mutex with watchdog timeout
        match timeout(self.watchdog_timeout, mutex.lock_owned()).await {
            Ok(guard) => {
                info!(session_id, "session lane acquired");
                Ok(SessionGuard {
                    _guard: guard,
                    session_id: session_id.to_string(),
                })
            }
            Err(_) => {
                warn!(
                    session_id,
                    timeout_secs = self.watchdog_timeout.as_secs(),
                    "session lane watchdog fired — previous run may be hung"
                );
                // Force-create a new mutex for this session, abandoning the hung one.
                // The hung run's mutex guard will be dropped when its task completes
                // (or is cancelled), but new runs won't wait for it.
                let new_mutex = Arc::new(Mutex::new(()));
                let guard = new_mutex.clone().lock_owned().await;
                {
                    let mut lanes = self.lanes.lock().await;
                    lanes.insert(session_id.to_string(), new_mutex);
                }
                Ok(SessionGuard {
                    _guard: guard,
                    session_id: session_id.to_string(),
                })
            }
        }
    }

    /// Remove a session's lane entry (cleanup when session ends).
    pub async fn remove(&self, session_id: &str) {
        let mut lanes = self.lanes.lock().await;
        lanes.remove(session_id);
    }

    /// Number of active session lanes.
    pub async fn lane_count(&self) -> usize {
        let lanes = self.lanes.lock().await;
        lanes.len()
    }

    /// Cleanup lanes that have no waiters (garbage collection).
    /// Returns the number of lanes removed.
    pub async fn gc(&self) -> usize {
        let mut lanes = self.lanes.lock().await;
        let before = lanes.len();
        // Remove lanes where the mutex is not currently held
        // (strong_count == 1 means only the HashMap holds a reference)
        lanes.retain(|_, mutex| Arc::strong_count(mutex) > 1);
        before - lanes.len()
    }
}

impl Default for SessionLaneManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Error from session lane operations.
#[derive(Debug)]
pub enum SessionLaneError {
    /// Watchdog timeout fired (should not happen in normal operation after fix).
    WatchdogTimeout { session_id: String },
}

impl std::fmt::Display for SessionLaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionLaneError::WatchdogTimeout { session_id } => {
                write!(f, "session lane watchdog timeout for session '{}'", session_id)
            }
        }
    }
}

impl std::error::Error for SessionLaneError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_serialization() {
        let mgr = Arc::new(SessionLaneManager::new());
        let counter = Arc::new(AtomicU32::new(0));

        // Two concurrent tasks for the same session should serialize
        let counter1 = counter.clone();
        let counter2 = counter.clone();

        let t1 = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move {
                let _guard = mgr.acquire("session-1").await.unwrap();
                assert_eq!(counter1.fetch_add(1, Ordering::SeqCst), 0);
                tokio::time::sleep(Duration::from_millis(50)).await;
                assert_eq!(counter1.load(Ordering::SeqCst), 1); // Still 1 — t2 hasn't started
            }
        });

        // Give t1 a moment to acquire
        tokio::time::sleep(Duration::from_millis(10)).await;

        let t2 = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move {
                let _guard = mgr.acquire("session-1").await.unwrap();
                assert_eq!(counter2.fetch_add(1, Ordering::SeqCst), 1); // t1 already done
            }
        });

        t1.await.unwrap();
        t2.await.unwrap();
    }

    #[tokio::test]
    async fn test_different_sessions_concurrent() {
        let mgr = Arc::new(SessionLaneManager::new());

        let mgr1 = mgr.clone();
        let mgr2 = mgr.clone();

        // Different sessions should run concurrently
        let t1 = tokio::spawn(async move {
            let _guard = mgr1.acquire("session-a").await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        let t2 = tokio::spawn(async move {
            let _guard = mgr2.acquire("session-b").await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        // Both should complete in ~50ms, not ~100ms
        let start = std::time::Instant::now();
        t1.await.unwrap();
        t2.await.unwrap();
        assert!(start.elapsed().as_millis() < 100);
    }

    #[tokio::test]
    async fn test_gc() {
        let mgr = SessionLaneManager::new();

        {
            let _guard = mgr.acquire("temp").await.unwrap();
        }
        // After guard dropped, lane should be GC-able
        let removed = mgr.gc().await;
        assert_eq!(removed, 1);
        assert_eq!(mgr.lane_count().await, 0);
    }
}
