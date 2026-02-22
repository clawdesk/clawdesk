//! # clawdesk-sochdb
//!
//! SochDB adapter for ClawDesk — implements all storage traits with:
//! - ACID transactions (WAL + buffered commit)
//! - MVCC + SSI (Serializable Snapshot Isolation)
//! - Built-in HNSW vector search
//! - TOON output format (58-67% fewer tokens than JSON)
//! - Path API for O(1) session lookup
//!
//! ## Architecture (matches agentreplay patterns)
//!
//! - **`EmbeddedConnection`** wraps the kernel `Database` which auto-recovers
//!   WAL on startup — no manual `recover()` needed.
//! - **Op lock** (`parking_lot::Mutex<()>`) serializes ALL operations
//!   (both reads and writes) to eliminate the `active_txn_id` ABA race
//!   on `EmbeddedConnection` — proven ThreadStore pattern.
//! - **Three write modes**:
//!   - `put()` — group-commit only (high throughput, eventual durability)
//!   - `put_durable()` — explicit commit after write (immediate durability)
//!   - `put_batch()` — batch of writes + single commit at end
//! - **`Drop`** implementation guarantees `checkpoint() + fsync()` on shutdown,
//!   even on panic unwind, preventing WAL growth and ensuring durability.
//!
//! ## WAL lifecycle
//!
//! - Opens with `DatabaseConfig` using group commit for write throughput.
//! - `checkpoint()` performs a checkpoint — call periodically to keep WAL bounded.
//! - `sync()` forces a durable fsync barrier when needed.
//! - On `Drop`, checkpoint + fsync are called automatically.

pub mod bridge;
pub mod config;
pub mod conversation;
pub mod federation;
pub mod graph;
pub mod memory_backend;
pub mod replay;
pub mod session;
pub mod compaction;
pub mod compaction_integrity;
pub mod transaction;
pub mod vector;

pub use bridge::{
    SochConn,
    SochSemanticCache, SochTraceStore, SochCheckpointStore,
    SochGraphOverlay, SochTemporalGraph, SochPolicyEngine,
    SochAtomicWriter, SochAgentRegistry, SochToolRouter,
};
pub use memory_backend::SochMemoryBackend;

use clawdesk_types::error::StorageError;
use parking_lot::Mutex;
use sochdb::EmbeddedConnection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn, error};

/// The unified SochDB storage backend.
///
/// Wraps `EmbeddedConnection` (which uses the kernel `Database` that auto-recovers)
/// with write serialization, graceful shutdown, and convenience write methods
/// matching agentreplay's proven patterns.
///
/// ## Serialization model
///
/// Uses a `parking_lot::Mutex<()>` instead of `RwLock<()>` to serialize **all**
/// operations (reads and writes). This eliminates the `active_txn_id` ABA race
/// on `EmbeddedConnection` — the same proven pattern used by `ThreadStore`.
/// For a desktop chat app with single-digit Hz write rates, the throughput
/// cost is negligible (~10K ops/s ceiling on NVMe).
pub struct SochStore {
    connection: EmbeddedConnection,
    /// Mutex serializes ALL operations to eliminate the `active_txn_id` race.
    /// Both reads and writes call `ensure_txn()` which shares a single
    /// `AtomicU64` — without external serialization, concurrent operations
    /// can commit the wrong (empty) transaction → silent data loss.
    op_lock: Mutex<()>,
    shutdown: AtomicBool,
    /// Whether this store is running on ephemeral (temp) storage.
    /// When true, data will NOT survive a restart.
    is_ephemeral: AtomicBool,
    /// The path this store was opened at (for diagnostics).
    store_path: PathBuf,
}

impl SochStore {
    /// Open or create a SochDB database at the given path.
    ///
    /// Uses `DatabaseConfig::default()` with group commit enabled.
    /// The kernel `Database` automatically recovers WAL on startup.
    ///
    /// ## Retry strategy
    ///
    /// Attempts up to 3 opens with exponential backoff (100ms, 500ms, 2s).
    /// On the second retry, quarantines a corrupt WAL before retrying.
    /// Includes a persistence self-test after successful open: writes a
    /// canary, commits, reads it back — proving the full write path works.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        let store_path = path.to_path_buf();
        info!(?path, "opening SochDB store (EmbeddedConnection)");

        // Log WAL file info for diagnostics
        let wal_path = path.join("wal.log");
        if wal_path.exists() {
            let wal_size = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
            info!(wal_size_bytes = wal_size, wal_path = ?wal_path, "WAL file found on disk before open");
        } else {
            info!(wal_path = ?wal_path, "No WAL file found — fresh database");
        }

        let mut config = sochdb_storage::database::DatabaseConfig::default();
        config.group_commit = true;

        // ── Pre-flight: clean up stale lock files from crashed processes ──
        // Check BEFORE the retry loop so the first attempt has a clean slate.
        for lock_name in &[".lock", "db.lock"] {
            let lock_path = path.join(lock_name);
            if lock_path.exists() {
                let is_stale = std::fs::read_to_string(&lock_path)
                    .ok()
                    .and_then(|contents| contents.trim().parse::<u32>().ok())
                    .map(|pid| {
                        !std::process::Command::new("kill")
                            .args(["-0", &pid.to_string()])
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false)
                    })
                    .unwrap_or(true);
                if is_stale {
                    warn!(lock_path = ?lock_path, "Pre-flight: removing stale lock file");
                    let _ = std::fs::remove_file(&lock_path);
                }
            }
        }

        // ── Retry with exponential backoff ─────────────────────────
        let delays = [
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(500),
            std::time::Duration::from_secs(2),
        ];
        let mut last_err = String::new();

        for (attempt, delay) in delays.iter().enumerate() {
            // On attempt 2 (third try), quarantine a possibly corrupt WAL.
            // COPY instead of rename — preserve the original so data is NOT
            // destroyed if the failure was transient (e.g., stale lock file).
            if attempt == 2 && wal_path.exists() {
                let quarantine = path.join(format!(
                    "wal.log.backup.{}",
                    chrono::Utc::now().format("%Y%m%dT%H%M%S")
                ));
                warn!(
                    from = ?wal_path,
                    to = ?quarantine,
                    "Backing up WAL before final retry (non-destructive copy)"
                );
                if let Err(e) = std::fs::copy(&wal_path, &quarantine) {
                    warn!(error = %e, "Failed to backup WAL — continuing anyway");
                }
                // Also remove the lock file in case it's stale from a crashed process.
                // Try both known lock file names (.lock used by EmbeddedConnection,
                // db.lock used by older versions).
                for lock_name in &[".lock", "db.lock"] {
                    let lock_path = path.join(lock_name);
                    if lock_path.exists() {
                        // Check if the PID in the lock file is still alive
                        let is_stale = std::fs::read_to_string(&lock_path)
                            .ok()
                            .and_then(|contents| contents.trim().parse::<u32>().ok())
                            .map(|pid| {
                                // Use `kill -0 <pid>` to check if the process still exists
                                !std::process::Command::new("kill")
                                    .args(["-0", &pid.to_string()])
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .status()
                                    .map(|s| s.success())
                                    .unwrap_or(false)
                            })
                            .unwrap_or(true); // if we can't read/parse, assume stale

                        if is_stale {
                            warn!(lock_path = ?lock_path, "Removing stale lock file (process not running)");
                            let _ = std::fs::remove_file(&lock_path);
                        } else {
                            warn!(lock_path = ?lock_path, "Lock file held by running process — not removing");
                        }
                    }
                }
            }

            match EmbeddedConnection::open_with_config(path, config.clone()) {
                Ok(connection) => {
                    if attempt > 0 {
                        info!(attempt = attempt + 1, "SochDB opened after retry");
                    }
                    info!("SochDB store opened — WAL auto-recovered by kernel Database");

                    // ── Persistence self-test ──────────────
                    let canary_key = "_clawdesk/canary";
                    // Check if previous run's canary exists
                    match connection.get(canary_key) {
                        Ok(Some(bytes)) => {
                            let prev_ts = String::from_utf8_lossy(&bytes);
                            info!(
                                previous_canary = %prev_ts,
                                "Persistence canary FOUND — WAL recovery is working"
                            );
                        }
                        Ok(None) => {
                            warn!("Persistence canary NOT found — first run or data was lost");
                        }
                        Err(e) => {
                            error!(error = %e, "Failed to read persistence canary");
                        }
                    }

                    // Check how many sessions exist after recovery
                    match connection.scan("chats/") {
                        Ok(entries) => {
                            info!(
                                session_count = entries.len(),
                                "Sessions found in SochDB after recovery"
                            );
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to scan sessions after recovery");
                        }
                    }

                    // Write-read self-test: proves the full put → commit → get path works
                    let now = chrono::Utc::now().to_rfc3339();
                    let canary_value = format!("canary::{}", now);
                    if let Err(e) = connection.put(canary_key, canary_value.as_bytes()) {
                        error!(error = %e, "Self-test FAILED: cannot write canary");
                        return Err(StorageError::OpenFailed {
                            detail: format!("Self-test write failed: {e}"),
                        });
                    }
                    if let Err(e) = connection.commit() {
                        error!(error = %e, "Self-test FAILED: cannot commit canary");
                        return Err(StorageError::OpenFailed {
                            detail: format!("Self-test commit failed: {e}"),
                        });
                    }
                    // Verify read-back
                    match connection.get(canary_key) {
                        Ok(Some(bytes)) if bytes == canary_value.as_bytes() => {
                            info!("Persistence self-test PASSED — write→commit→read verified");
                        }
                        Ok(Some(_)) => {
                            error!("Self-test FAILED: canary read-back mismatch");
                            return Err(StorageError::OpenFailed {
                                detail: "Self-test: canary read-back mismatch".into(),
                            });
                        }
                        Ok(None) => {
                            error!("Self-test FAILED: canary not found after commit");
                            return Err(StorageError::OpenFailed {
                                detail: "Self-test: canary not found after commit".into(),
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "Self-test FAILED: canary read error");
                            return Err(StorageError::OpenFailed {
                                detail: format!("Self-test read failed: {e}"),
                            });
                        }
                    }

                    return Ok(Self {
                        connection,
                        op_lock: Mutex::new(()),
                        shutdown: AtomicBool::new(false),
                        is_ephemeral: AtomicBool::new(false),
                        store_path,
                    });
                }
                Err(e) => {
                    last_err = e.to_string();
                    warn!(
                        attempt = attempt + 1,
                        delay_ms = delay.as_millis(),
                        error = %last_err,
                        "SochDB open failed — retrying"
                    );
                    std::thread::sleep(*delay);
                }
            }
        }

        // All retries exhausted — return error, do NOT fall back to ephemeral
        error!(
            error = %last_err,
            path = ?path,
            "SochDB open FAILED after 3 retries — refusing to start with ephemeral storage"
        );
        Err(StorageError::OpenFailed {
            detail: format!(
                "SochDB open failed after 3 retries (path: {}): {}",
                path.display(),
                last_err
            ),
        })
    }

    /// Open an ephemeral database in a temp directory (for testing / fallback).
    ///
    /// Uses the system temp directory with a PID-scoped name so multiple
    /// instances don't collide. Data will NOT survive across restarts.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let tmp_dir = std::env::temp_dir().join(format!("clawdesk-ephemeral-{}", std::process::id()));
        let store_path = tmp_dir.clone();
        warn!(
            path = ?store_path,
            "Opening EPHEMERAL SochDB store — data will NOT survive restart"
        );
        let connection = EmbeddedConnection::open(&tmp_dir)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;
        Ok(Self {
            connection,
            op_lock: Mutex::new(()),
            shutdown: AtomicBool::new(false),
            is_ephemeral: AtomicBool::new(true),
            store_path,
        })
    }

    /// Returns `true` if this store is running on ephemeral (temp) storage.
    ///
    /// When ephemeral, data will NOT survive an app restart.
    /// The UI should show a warning banner when this returns `true`.
    pub fn is_ephemeral(&self) -> bool {
        self.is_ephemeral.load(Ordering::Relaxed)
    }

    /// Returns the path this store was opened at.
    pub fn store_path(&self) -> &Path {
        &self.store_path
    }

    // =========================================================================
    // Write API (mirrors agentreplay's put / put_durable / put_batch)
    // =========================================================================

    /// Put a single key-value pair (group-commit only, no explicit commit).
    ///
    /// Relies on `DatabaseConfig::group_commit` to batch writes for throughput.
    /// Use `put_durable()` for writes that must be immediately committed.
    pub fn put(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        let _guard = self.op_lock.lock();
        self.connection.put(key, value)
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("put failed: {e}"),
            })
    }

    /// Put a single key-value pair with immediate commit (durable).
    ///
    /// Acquires write lock, writes, and commits in one atomic operation.
    /// Use for critical data that must survive a crash immediately.
    pub fn put_durable(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        let _guard = self.op_lock.lock();
        self.connection.put(key, value)
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("put_durable write failed: {e}"),
            })?;
        self.connection.commit()
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("put_durable commit failed: {e}"),
            })?;
        Ok(())
    }

    /// Put a batch of key-value pairs with a single commit at the end.
    ///
    /// More efficient than individual `put_durable()` calls for bulk writes.
    pub fn put_batch(&self, entries: &[(&str, &[u8])]) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }
        let _guard = self.op_lock.lock();
        for (key, value) in entries {
            self.connection.put(key, value)
                .map_err(|e| StorageError::OpenFailed {
                    detail: format!("put_batch write failed for key {key}: {e}"),
                })?;
        }
        self.connection.commit()
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("put_batch commit failed: {e}"),
            })?;
        Ok(())
    }

    // =========================================================================
    // Read API
    // =========================================================================

    /// Get a value by key.
    ///
    /// Acquires write lock to prevent racing with `ensure_txn()` on the
    /// shared `EmbeddedConnection`. Without this, a concurrent read could
    /// overwrite `active_txn_id`, causing `commit()` to commit the wrong
    /// (empty) transaction — resulting in silent data loss.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let _guard = self.op_lock.lock();
        self.connection.get(key)
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("get failed: {e}"),
            })
    }

    /// Scan all key-value pairs matching a prefix.
    ///
    /// Returns `Vec<(String, Vec<u8>)>` — keys are strings (not byte arrays).
    /// Acquires write lock to prevent `ensure_txn()` race (see `get()` docs).
    pub fn scan(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StorageError> {
        let _guard = self.op_lock.lock();
        self.connection.scan(prefix)
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("scan failed: {e}"),
            })
    }

    // =========================================================================
    // Delete API
    // =========================================================================

    /// Delete a key (under write lock).
    pub fn delete(&self, key: &str) -> Result<(), StorageError> {
        let _guard = self.op_lock.lock();
        self.connection.delete(key)
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("delete failed: {e}"),
            })
    }

    // =========================================================================
    // Transaction API
    // =========================================================================

    /// Commit the active transaction.
    ///
    /// Acquires write lock to prevent racing with concurrent put/get calls
    /// that share the same `active_txn_id`. Without the lock, a concurrent
    /// `put_durable()` could commit-and-clear the transaction between our
    /// last `put()` and this `commit()`, causing "No active transaction".
    ///
    /// Returns the commit sequence number on success.
    pub fn commit(&self) -> Result<u64, StorageError> {
        let _guard = self.op_lock.lock();
        self.connection.commit()
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("commit failed: {e}"),
            })
    }

    // =========================================================================
    // Maintenance
    // =========================================================================

    /// Get a reference to the underlying EmbeddedConnection.
    ///
    /// Used by `SochConn` bridge for advanced SochDB modules that need
    /// direct connection access (SemanticCache, TraceStore, etc.).
    pub fn connection(&self) -> &EmbeddedConnection {
        &self.connection
    }

    /// Checkpoint the WAL (flush memtable → SSTables, truncate WAL).
    ///
    /// Call this periodically (e.g. every 5 minutes when idle) to keep
    /// WAL size bounded and prevent unbounded growth during long sessions.
    pub fn checkpoint(&self) -> Result<u64, StorageError> {
        let _guard = self.op_lock.lock();
        self.connection.checkpoint()
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("checkpoint failed: {e}"),
            })
    }

    /// Checkpoint + GC in one operation.
    pub fn checkpoint_and_gc(&self) -> Result<u64, StorageError> {
        let _guard = self.op_lock.lock();
        let seq = self.connection.checkpoint()
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("checkpoint failed: {e}"),
            })?;

        let _reclaimed = self.connection.gc();

        info!(checkpoint_seq = seq, "WAL checkpoint + GC completed");
        Ok(seq)
    }

    /// Force an fsync to ensure all buffered writes are durable on disk.
    pub fn sync(&self) -> Result<(), StorageError> {
        self.connection.fsync()
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("fsync failed: {e}"),
            })
    }

    /// Graceful shutdown: checkpoint + fsync.
    ///
    /// Idempotent — safe to call multiple times (guarded by `AtomicBool`).
    /// Also called automatically by `Drop`.
    pub fn shutdown(&self) -> Result<(), StorageError> {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return Ok(()); // Already shutdown
        }
        info!("Shutting down SochDB store");

        // Checkpoint under op lock
        {
            let _guard = self.op_lock.lock();
            if let Err(e) = self.connection.checkpoint() {
                warn!(error = %e, "Failed to checkpoint on shutdown");
            }
        }

        // Final fsync
        if let Err(e) = self.connection.fsync() {
            warn!(error = %e, "Failed to fsync after checkpoint on shutdown");
        }

        info!("SochDB store shutdown complete");
        Ok(())
    }
}

impl Drop for SochStore {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown() {
            error!(error = %e, "Error during SochDB store shutdown");
        }
    }
}

impl std::fmt::Debug for SochStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SochStore").finish()
    }
}
