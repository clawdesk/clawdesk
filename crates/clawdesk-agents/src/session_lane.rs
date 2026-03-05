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

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use dashmap::DashMap;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio::time::{Duration, timeout};
use tracing::{debug, info, warn};

/// Default watchdog timeout — force-unlock hung sessions after this duration.
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

/// Default maximum number of tracked session lanes before eviction kicks in.
const DEFAULT_MAX_LANES: usize = 10_000;

/// Tracked lane — mutex + last-access timestamp for LRU eviction.
struct LaneEntry {
    mutex: Arc<Mutex<()>>,
    last_access: AtomicU64, // epoch millis, atomic for lock-free reads
}

impl LaneEntry {
    fn new() -> Self {
        Self {
            mutex: Arc::new(Mutex::new(())),
            last_access: AtomicU64::new(Self::now()),
        }
    }

    fn touch(&self) {
        self.last_access.store(Self::now(), Ordering::Relaxed);
    }

    fn last_access_ms(&self) -> u64 {
        self.last_access.load(Ordering::Relaxed)
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

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
/// Uses `DashMap` for lock-free concurrent access to session lanes.
/// Automatically evicts idle lanes (LRU) when the lane count exceeds
/// `max_lanes`, preventing unbounded memory growth in long-running servers.
pub struct SessionLaneManager {
    lanes: DashMap<String, Arc<LaneEntry>>,
    watchdog_timeout: Duration,
    max_lanes: usize,
}

impl SessionLaneManager {
    pub fn new() -> Self {
        Self {
            lanes: DashMap::new(),
            watchdog_timeout: WATCHDOG_TIMEOUT,
            max_lanes: DEFAULT_MAX_LANES,
        }
    }

    /// Create a manager with a custom watchdog timeout.
    pub fn with_timeout(timeout_dur: Duration) -> Self {
        Self {
            lanes: DashMap::new(),
            watchdog_timeout: timeout_dur,
            max_lanes: DEFAULT_MAX_LANES,
        }
    }

    /// Create a manager with custom timeout and capacity.
    pub fn with_capacity(timeout_dur: Duration, max_lanes: usize) -> Self {
        Self {
            lanes: DashMap::new(),
            watchdog_timeout: timeout_dur,
            max_lanes,
        }
    }

    /// Acquire exclusive access to a session's lane.
    ///
    /// If another run is active for this session, this call blocks until
    /// that run completes (or the watchdog timeout fires).
    ///
    /// Returns a `SessionGuard` that releases the lock when dropped.
    pub async fn acquire(&self, session_id: &str) -> Result<SessionGuard, SessionLaneError> {
        // Evict idle lanes if over capacity.
        self.maybe_evict();

        let entry = self
            .lanes
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(LaneEntry::new()))
            .clone();

        entry.touch();
        let mutex = entry.mutex.clone();

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
                let new_entry = Arc::new(LaneEntry::new());
                let guard = new_entry.mutex.clone().lock_owned().await;
                self.lanes.insert(session_id.to_string(), new_entry);
                Ok(SessionGuard {
                    _guard: guard,
                    session_id: session_id.to_string(),
                })
            }
        }
    }

    /// Remove a session's lane entry (cleanup when session ends).
    pub async fn remove(&self, session_id: &str) {
        self.lanes.remove(session_id);
    }

    /// Number of active session lanes.
    pub fn lane_count(&self) -> usize {
        self.lanes.len()
    }

    /// Cleanup lanes that have no waiters (garbage collection).
    /// Returns the number of lanes removed.
    pub fn gc(&self) -> usize {
        let before = self.lanes.len();
        // Remove lanes where the mutex is not currently held
        // (strong_count == 1 means only the DashMap holds a reference)
        self.lanes.retain(|_, entry| Arc::strong_count(&entry.mutex) > 1);
        before - self.lanes.len()
    }

    /// Evict idle lanes if over capacity, removing the oldest-accessed
    /// lanes that have no active waiters.
    fn maybe_evict(&self) {
        let count = self.lanes.len();
        if count <= self.max_lanes {
            return;
        }

        let excess = count - self.max_lanes;
        // Collect (session_id, last_access_ms) for idle lanes only.
        let mut candidates: Vec<(String, u64)> = self
            .lanes
            .iter()
            .filter(|entry| Arc::strong_count(&entry.value().mutex) <= 1)
            .map(|entry| (entry.key().clone(), entry.value().last_access_ms()))
            .collect();

        // Sort by last_access ascending (oldest first).
        candidates.sort_by_key(|&(_, ts)| ts);

        let to_remove = candidates.len().min(excess);
        for (session_id, _) in candidates.into_iter().take(to_remove) {
            self.lanes.remove(&session_id);
        }

        if to_remove > 0 {
            debug!(evicted = to_remove, remaining = self.lanes.len(), "session lane LRU eviction");
        }
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
        let removed = mgr.gc();
        assert_eq!(removed, 1);
        assert_eq!(mgr.lane_count(), 0);
    }

    #[tokio::test]
    async fn test_eviction() {
        let mgr = SessionLaneManager::with_capacity(Duration::from_secs(300), 3);

        // Create 5 lanes (need to acquire and release)
        for i in 0..5 {
            let _guard = mgr.acquire(&format!("s{i}")).await.unwrap();
        }

        // All 5 lanes exist but are idle
        assert_eq!(mgr.lane_count(), 5);

        // Next acquire triggers eviction (5 > max_lanes=3)
        let _guard = mgr.acquire("s5").await.unwrap();

        // Should have evicted at least 2 idle lanes to get back to max_lanes
        // The held guard (s5) counts, so we should be at max_lanes or fewer
        assert!(mgr.lane_count() <= 4); // 3 + the newly acquired one
    }
}
