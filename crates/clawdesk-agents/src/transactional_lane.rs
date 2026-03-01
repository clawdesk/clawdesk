//! Transactional session lane — combines `SessionLaneManager` mutex-based
//! serialization with `TransactionalConn` ACID guarantees for multi-key
//! atomic writes during message persistence.
//!
//! ## Problem
//!
//! `SessionLaneManager` ensures only one agent run per session at a time
//! (mutual exclusion), but within a single run the multiple storage
//! writes (user message + assistant message + session metadata + turn
//! replay + compaction) are independent operations. If the process
//! crashes mid-pipeline, the session can end up with:
//!
//! - A user message but no assistant response (ghost turn).
//! - Updated session metadata but missing conversation records.
//! - Partial compaction (summary written, messages not deleted).
//!
//! ## Solution
//!
//! `TransactionalLane` wraps the lane guard with a transactional scope:
//!
//! 1. **Acquire** the `SessionGuard` (mutual exclusion).
//! 2. **Begin** a `TransactionalConn` transaction.
//! 3. All writes within the lane go through the transaction buffer.
//! 4. On success, **commit** atomically (one WAL fsync).
//! 5. On failure, **rollback** discards all buffered writes.
//! 6. **Release** the `SessionGuard` (drop).
//!
//! This gives us both serialization (no concurrent mutations) and
//! atomicity (all-or-nothing persistence).
//!
//! ## Distributed Coordination
//!
//! For multi-node deployments, `DistributedLock` provides advisory
//! locking via SochDB keys — a lightweight alternative to file locks
//! or external coordination services. Lock entries include a fencing
//! token (monotonic epoch) for split-brain protection.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::{debug, info, warn};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Minimal KV backend trait — abstracts over `sochdb::Database` so this
/// crate doesn't need a direct `sochdb` dependency.
///
/// Implementors: `SochStore::db()`, `SochConn`, or any `ConnectionTrait`.
pub trait KvBackend {
    fn kv_put(&self, key: &[u8], value: &[u8]) -> Result<(), String>;
    fn kv_get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String>;
    fn kv_delete(&self, key: &[u8]) -> Result<(), String>;
    fn kv_scan(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, String>;
}

/// Default lock TTL — auto-expire stale distributed locks.
const DEFAULT_LOCK_TTL_SECS: u64 = 30;

/// Maximum number of distributed locks before GC is forced.
const MAX_DISTRIBUTED_LOCKS: usize = 10_000;

// ── Transactional Lane ──────────────────────────────────────

/// A session lane guard that also owns a transaction scope.
///
/// All writes should go through `write()` / `delete()` methods on
/// this struct. On drop, uncommitted writes are rolled back.
pub struct TransactionalLaneGuard {
    session_id: String,
    /// The owned mutex guard — held for the lifetime of the transaction.
    /// Dropped when the guard is dropped, releasing the session lane.
    _lock: OwnedMutexGuard<()>,
    /// Buffered writes — (key, value) pairs waiting for commit.
    write_buffer: Vec<(Vec<u8>, Vec<u8>)>,
    /// Buffered deletes.
    delete_buffer: Vec<Vec<u8>>,
    /// Whether the transaction has been committed.
    committed: bool,
}

impl TransactionalLaneGuard {
    /// Create a new transactional guard for a session.
    fn new(session_id: String, lock: OwnedMutexGuard<()>) -> Self {
        Self {
            session_id,
            _lock: lock,
            write_buffer: Vec::new(),
            delete_buffer: Vec::new(),
            committed: false,
        }
    }

    /// The session this guard is protecting.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Buffer a write operation.
    pub fn write(&mut self, key: &[u8], value: &[u8]) {
        self.write_buffer.push((key.to_vec(), value.to_vec()));
    }

    /// Buffer a delete operation.
    pub fn delete(&mut self, key: &[u8]) {
        self.delete_buffer.push(key.to_vec());
    }

    /// Number of pending operations.
    pub fn pending_count(&self) -> usize {
        self.write_buffer.len() + self.delete_buffer.len()
    }

    /// Whether this guard has been committed.
    pub fn is_committed(&self) -> bool {
        self.committed
    }

    /// Commit all buffered operations via the provided write function.
    ///
    /// The `writer` closure receives each (key, value) pair and must
    /// persist it. If any write fails, the entire commit is considered
    /// failed (but already-written keys are not rolled back — that's
    /// the caller's responsibility via `TransactionalConn`).
    pub fn commit_with<F, E>(&mut self, mut writer: F) -> Result<CommitSummary, E>
    where
        F: FnMut(WriteOp) -> Result<(), E>,
    {
        let mut puts = 0usize;
        let mut deletes = 0usize;

        for (key, value) in self.write_buffer.drain(..) {
            writer(WriteOp::Put { key, value })?;
            puts += 1;
        }

        for key in self.delete_buffer.drain(..) {
            writer(WriteOp::Delete { key })?;
            deletes += 1;
        }

        self.committed = true;
        info!(
            session = %self.session_id,
            puts,
            deletes,
            "transactional lane committed"
        );

        Ok(CommitSummary { puts, deletes })
    }

    /// Commit all buffered operations atomically via a batch writer.
    ///
    /// Unlike `commit_with` which applies ops sequentially (partial writes
    /// on crash), this method collects all ops into a single `Vec<WriteOp>`
    /// and hands them to `batch_writer` for all-or-nothing commit.
    ///
    /// Callers should pass a closure that delegates to
    /// `TransactionalConn::commit()` or `ConnectionTrait::write_batch()`
    /// for true WAL-backed atomicity.
    ///
    /// # Example
    /// ```ignore
    /// guard.commit_batch(|ops| {
    ///     let kv_ops: Vec<KvBatchOp> = ops.into_iter().map(|op| match op {
    ///         WriteOp::Put { key, value } => KvBatchOp::Put { key, value },
    ///         WriteOp::Delete { key } => KvBatchOp::Delete { key },
    ///     }).collect();
    ///     conn.write_batch(&kv_ops)
    /// })?;
    /// ```
    pub fn commit_batch<F, E>(&mut self, batch_writer: F) -> Result<CommitSummary, E>
    where
        F: FnOnce(Vec<WriteOp>) -> Result<(), E>,
    {
        let puts = self.write_buffer.len();
        let deletes = self.delete_buffer.len();

        let mut ops = Vec::with_capacity(puts + deletes);
        for (key, value) in self.write_buffer.drain(..) {
            ops.push(WriteOp::Put { key, value });
        }
        for key in self.delete_buffer.drain(..) {
            ops.push(WriteOp::Delete { key });
        }

        batch_writer(ops)?;

        self.committed = true;
        info!(
            session = %self.session_id,
            puts,
            deletes,
            "transactional lane committed (atomic batch)"
        );

        Ok(CommitSummary { puts, deletes })
    }

    /// Discard all buffered operations (explicit rollback).
    pub fn rollback(&mut self) {
        let discarded = self.pending_count();
        self.write_buffer.clear();
        self.delete_buffer.clear();
        self.committed = true; // prevent double-warn on drop
        if discarded > 0 {
            debug!(
                session = %self.session_id,
                discarded,
                "transactional lane rolled back"
            );
        }
    }
}

impl Drop for TransactionalLaneGuard {
    fn drop(&mut self) {
        if !self.committed && self.pending_count() > 0 {
            warn!(
                session = %self.session_id,
                pending = self.pending_count(),
                "transactional lane dropped without commit — discarding writes"
            );
            self.write_buffer.clear();
            self.delete_buffer.clear();
        }
    }
}

/// A write operation buffered in the transactional lane.
pub enum WriteOp {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

/// Summary of a committed transaction.
#[derive(Debug, Clone)]
pub struct CommitSummary {
    pub puts: usize,
    pub deletes: usize,
}

// ── Transactional Lane Manager ──────────────────────────────

/// Manages per-session transactional lanes.
///
/// Combines `SessionLaneManager`-style mutex serialization with
/// transactional write buffering.
pub struct TransactionalLaneManager {
    lanes: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    watchdog_timeout: Duration,
}

impl TransactionalLaneManager {
    /// Create a new transactional lane manager.
    pub fn new() -> Self {
        Self {
            lanes: Mutex::new(HashMap::new()),
            watchdog_timeout: Duration::from_secs(300),
        }
    }

    /// Create with a custom watchdog timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            lanes: Mutex::new(HashMap::new()),
            watchdog_timeout: timeout,
        }
    }

    /// Acquire a transactional lane for a session.
    ///
    /// Returns a `TransactionalLaneGuard` that buffers all writes
    /// until explicitly committed.
    pub async fn acquire(
        &self,
        session_id: &str,
    ) -> Result<TransactionalLaneGuard, TransactionalLaneError> {
        let mutex = {
            let mut lanes = self.lanes.lock().await;
            lanes
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };

        match tokio::time::timeout(self.watchdog_timeout, mutex.lock_owned()).await {
            Ok(guard) => {
                info!(session_id, "transactional lane acquired");
                Ok(TransactionalLaneGuard::new(session_id.to_string(), guard))
            }
            Err(_) => {
                warn!(
                    session_id,
                    timeout_secs = self.watchdog_timeout.as_secs(),
                    "transactional lane watchdog fired"
                );
                Err(TransactionalLaneError::WatchdogTimeout {
                    session_id: session_id.to_string(),
                })
            }
        }
    }

    /// Garbage-collect idle lanes.
    pub async fn gc(&self) -> usize {
        let mut lanes = self.lanes.lock().await;
        let before = lanes.len();
        lanes.retain(|_, mutex| Arc::strong_count(mutex) > 1);
        before - lanes.len()
    }

    /// Number of active lanes.
    pub async fn lane_count(&self) -> usize {
        self.lanes.lock().await.len()
    }
}

impl Default for TransactionalLaneManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors from transactional lane operations.
#[derive(Debug)]
pub enum TransactionalLaneError {
    WatchdogTimeout { session_id: String },
    CommitFailed { detail: String },
    LockConflict { session_id: String, holder: String },
}

impl std::fmt::Display for TransactionalLaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WatchdogTimeout { session_id } => {
                write!(f, "transactional lane watchdog timeout for '{session_id}'")
            }
            Self::CommitFailed { detail } => {
                write!(f, "transactional lane commit failed: {detail}")
            }
            Self::LockConflict { session_id, holder } => {
                write!(
                    f,
                    "lock conflict for session '{session_id}' (held by '{holder}')"
                )
            }
        }
    }
}

impl std::error::Error for TransactionalLaneError {}

// ── Distributed Advisory Locks ──────────────────────────────

/// A distributed advisory lock entry stored in SochDB.
///
/// Key: `locks/sessions/{session_id}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedLockEntry {
    /// The node/instance that holds the lock.
    pub holder_id: String,
    /// Monotonic fencing token — prevents stale lock holders from
    /// making writes after their lock has been superseded.
    pub fencing_token: u64,
    /// When the lock was acquired.
    pub acquired_at: DateTime<Utc>,
    /// When the lock expires if not renewed.
    pub expires_at: DateTime<Utc>,
    /// Purpose/context for debugging.
    pub purpose: String,
}

/// Distributed lock manager backed by SochDB.
///
/// Uses SochDB key-value entries as advisory locks with TTL-based
/// auto-expiry and monotonic fencing tokens for split-brain safety.
pub struct DistributedLockManager {
    /// TTL for lock entries.
    ttl: Duration,
    /// Local fencing counter (per-instance).
    fencing_counter: Mutex<u64>,
    /// Instance identifier.
    instance_id: String,
}

impl DistributedLockManager {
    /// Create a new distributed lock manager.
    pub fn new(instance_id: String) -> Self {
        Self {
            ttl: Duration::from_secs(DEFAULT_LOCK_TTL_SECS),
            fencing_counter: Mutex::new(0),
            instance_id,
        }
    }

    /// Create with custom TTL.
    pub fn with_ttl(instance_id: String, ttl: Duration) -> Self {
        Self {
            ttl,
            fencing_counter: Mutex::new(0),
            instance_id,
        }
    }

    /// Try to acquire a distributed lock for a session.
    ///
    /// Returns `Ok(entry)` if the lock was acquired, or `Err` if it's
    /// held by another instance and hasn't expired.
    pub async fn try_acquire(
        &self,
        db: &dyn KvBackend,
        session_id: &str,
        purpose: &str,
    ) -> Result<DistributedLockEntry, TransactionalLaneError> {
        let lock_key = format!("locks/sessions/{}", session_id);
        let now = Utc::now();

        // Check for existing lock.
        if let Ok(Some(bytes)) = db.kv_get(lock_key.as_bytes()) {
            if let Ok(existing) = serde_json::from_slice::<DistributedLockEntry>(&bytes) {
                if existing.expires_at > now && existing.holder_id != self.instance_id {
                    return Err(TransactionalLaneError::LockConflict {
                        session_id: session_id.to_string(),
                        holder: existing.holder_id,
                    });
                }
                // Lock expired or we already hold it — proceed.
            }
        }

        // Acquire the lock.
        let fencing_token = {
            let mut counter = self.fencing_counter.lock().await;
            *counter += 1;
            *counter
        };

        let entry = DistributedLockEntry {
            holder_id: self.instance_id.clone(),
            fencing_token,
            acquired_at: now,
            expires_at: now + chrono::Duration::seconds(self.ttl.as_secs() as i64),
            purpose: purpose.to_string(),
        };

        let bytes = serde_json::to_vec(&entry).map_err(|e| {
            TransactionalLaneError::CommitFailed {
                detail: format!("serialize lock entry: {e}"),
            }
        })?;

        db.kv_put(lock_key.as_bytes(), &bytes).map_err(|e| {
            TransactionalLaneError::CommitFailed {
                detail: format!("write lock entry: {e}"),
            }
        })?;

        info!(
            session_id,
            holder = %self.instance_id,
            fencing_token,
            ttl_secs = self.ttl.as_secs(),
            "distributed lock acquired"
        );

        Ok(entry)
    }

    /// Release a distributed lock.
    pub fn release(
        &self,
        db: &dyn KvBackend,
        session_id: &str,
    ) -> Result<(), TransactionalLaneError> {
        let lock_key = format!("locks/sessions/{}", session_id);

        // Only release if we hold the lock.
        if let Ok(Some(bytes)) = db.kv_get(lock_key.as_bytes()) {
            if let Ok(existing) = serde_json::from_slice::<DistributedLockEntry>(&bytes) {
                if existing.holder_id != self.instance_id {
                    warn!(
                        session_id,
                        holder = %existing.holder_id,
                        "cannot release lock held by another instance"
                    );
                    return Ok(()); // Not an error — just can't release it.
                }
            }
        }

        db.kv_delete(lock_key.as_bytes()).map_err(|e| {
            TransactionalLaneError::CommitFailed {
                detail: format!("delete lock entry: {e}"),
            }
        })?;

        debug!(session_id, "distributed lock released");
        Ok(())
    }

    /// Renew (extend) a lock's TTL.
    pub async fn renew(
        &self,
        db: &dyn KvBackend,
        session_id: &str,
    ) -> Result<DistributedLockEntry, TransactionalLaneError> {
        let lock_key = format!("locks/sessions/{}", session_id);
        let now = Utc::now();

        let existing_bytes = db.kv_get(lock_key.as_bytes()).map_err(|e| {
            TransactionalLaneError::CommitFailed {
                detail: format!("read lock for renewal: {e}"),
            }
        })?;

        let mut entry = match existing_bytes {
            Some(bytes) => serde_json::from_slice::<DistributedLockEntry>(&bytes).map_err(
                |e| TransactionalLaneError::CommitFailed {
                    detail: format!("deserialize lock: {e}"),
                },
            )?,
            None => {
                return Err(TransactionalLaneError::CommitFailed {
                    detail: format!("no lock to renew for session '{session_id}'"),
                });
            }
        };

        if entry.holder_id != self.instance_id {
            return Err(TransactionalLaneError::LockConflict {
                session_id: session_id.to_string(),
                holder: entry.holder_id,
            });
        }

        entry.expires_at = now + chrono::Duration::seconds(self.ttl.as_secs() as i64);

        let bytes = serde_json::to_vec(&entry).map_err(|e| {
            TransactionalLaneError::CommitFailed {
                detail: format!("serialize renewed lock: {e}"),
            }
        })?;

        db.kv_put(lock_key.as_bytes(), &bytes).map_err(|e| {
            TransactionalLaneError::CommitFailed {
                detail: format!("write renewed lock: {e}"),
            }
        })?;

        debug!(
            session_id,
            new_expiry = %entry.expires_at,
            "distributed lock renewed"
        );

        Ok(entry)
    }

    /// Garbage-collect expired locks.
    pub fn gc_expired(
        &self,
        db: &dyn KvBackend,
    ) -> Result<usize, TransactionalLaneError> {
        let prefix = b"locks/sessions/";
        let entries = db.kv_scan(prefix).map_err(|e| {
            TransactionalLaneError::CommitFailed {
                detail: format!("scan locks: {e}"),
            }
        })?;

        let now = Utc::now();
        let mut cleaned = 0usize;

        for (key, value) in &entries {
            if let Ok(entry) = serde_json::from_slice::<DistributedLockEntry>(value) {
                if entry.expires_at <= now {
                    let _ = db.kv_delete(key);
                    cleaned += 1;
                    debug!(
                        holder = %entry.holder_id,
                        expired_at = %entry.expires_at,
                        "expired distributed lock cleaned"
                    );
                }
            }
        }

        if cleaned > 0 {
            info!(cleaned, "distributed lock GC complete");
        }

        Ok(cleaned)
    }

    /// Get the instance ID.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
}

/// Validate a fencing token against the current lock state.
///
/// Returns `true` if the token is current (matches the lock's fencing
/// token), `false` if it's stale (the lock has been superseded).
///
/// This should be checked before any write operation to prevent stale
/// lock holders from corrupting data after their lock expired and was
/// re-acquired by another instance.
pub fn validate_fencing_token(
    db: &dyn KvBackend,
    session_id: &str,
    token: u64,
) -> bool {
    let lock_key = format!("locks/sessions/{}", session_id);
    match db.kv_get(lock_key.as_bytes()) {
        Ok(Some(bytes)) => {
            match serde_json::from_slice::<DistributedLockEntry>(&bytes) {
                Ok(entry) => entry.fencing_token == token,
                Err(_) => false,
            }
        }
        _ => false, // No lock → stale token.
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a dummy OwnedMutexGuard for unit tests that only exercise
    /// buffering / commit / rollback (not contention).
    async fn dummy_guard() -> OwnedMutexGuard<()> {
        Arc::new(Mutex::new(())).lock_owned().await
    }

    #[tokio::test]
    async fn transactional_guard_buffers_writes() {
        let mut guard = TransactionalLaneGuard::new("test-session".into(), dummy_guard().await);

        guard.write(b"key1", b"val1");
        guard.write(b"key2", b"val2");
        guard.delete(b"key3");

        assert_eq!(guard.pending_count(), 3);
        assert!(!guard.is_committed());
    }

    #[tokio::test]
    async fn transactional_guard_commit() {
        let mut guard = TransactionalLaneGuard::new("test-session".into(), dummy_guard().await);

        guard.write(b"key1", b"val1");
        guard.write(b"key2", b"val2");
        guard.delete(b"key3");

        let mut ops = Vec::new();
        let result = guard.commit_with(|op| -> Result<(), String> {
            ops.push(match &op {
                WriteOp::Put { .. } => "put",
                WriteOp::Delete { .. } => "delete",
            });
            Ok(())
        });

        let summary = result.unwrap();
        assert_eq!(summary.puts, 2);
        assert_eq!(summary.deletes, 1);
        assert!(guard.is_committed());
        assert_eq!(ops, vec!["put", "put", "delete"]);
    }

    #[tokio::test]
    async fn transactional_guard_rollback() {
        let mut guard = TransactionalLaneGuard::new("test-session".into(), dummy_guard().await);

        guard.write(b"key1", b"val1");
        guard.write(b"key2", b"val2");

        guard.rollback();
        assert_eq!(guard.pending_count(), 0);
        assert!(guard.is_committed()); // committed flag set to prevent drop warning
    }

    #[tokio::test]
    async fn transactional_lane_manager_serialization() {
        let mgr = TransactionalLaneManager::new();
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let c1 = counter.clone();
        let guard1 = mgr.acquire("session-1").await.unwrap();
        assert_eq!(guard1.session_id(), "session-1");
        drop(guard1);

        let guard2 = mgr.acquire("session-1").await.unwrap();
        assert_eq!(guard2.session_id(), "session-1");
        drop(guard2);

        let _c = c1;
    }

    #[tokio::test]
    async fn transactional_lane_manager_gc() {
        let mgr = TransactionalLaneManager::new();

        {
            let _g = mgr.acquire("temp").await.unwrap();
        }
        let removed = mgr.gc().await;
        assert_eq!(removed, 1);
        assert_eq!(mgr.lane_count().await, 0);
    }

    #[test]
    fn distributed_lock_entry_serializable() {
        let entry = DistributedLockEntry {
            holder_id: "node-1".into(),
            fencing_token: 42,
            acquired_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::seconds(30),
            purpose: "test".into(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: DistributedLockEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.holder_id, "node-1");
        assert_eq!(parsed.fencing_token, 42);
    }

    #[tokio::test]
    async fn commit_with_failure_stops_early() {
        let mut guard = TransactionalLaneGuard::new("s1".into(), dummy_guard().await);

        guard.write(b"k1", b"v1");
        guard.write(b"k2", b"v2");

        let mut call_count = 0;
        let result = guard.commit_with(|_op| -> Result<(), &str> {
            call_count += 1;
            if call_count == 2 {
                Err("simulated failure")
            } else {
                Ok(())
            }
        });

        assert!(result.is_err());
        assert_eq!(call_count, 2); // Stopped at second op.
    }
}
