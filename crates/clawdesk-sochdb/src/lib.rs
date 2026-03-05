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
pub mod cold_tier;
pub mod config;
pub mod conversation;
pub mod federation;
pub mod graph;
pub mod health;
pub mod lifecycle;
pub mod memory_backend;
pub mod replay;
pub mod schema;
pub mod session;
pub mod session_index;
pub mod structured_trace;
pub mod compaction;
pub mod compaction_integrity;
pub mod transaction;
pub mod vector;
pub mod wire;

pub use bridge::{
    SochConn,
    SochSemanticCache, SochTraceStore, SochCheckpointStore,
    SochGraphOverlay, SochTemporalGraph, SochPolicyEngine,
    SochAtomicWriter, SochAgentRegistry, SochToolRouter,
};
pub use health::StorageHealth;
pub use lifecycle::LifecycleManager;
pub use memory_backend::SochMemoryBackend;
pub use schema::MigrationRegistry;
pub use session_index::SessionIndex;
pub use structured_trace::StructuredTracing;

use clawdesk_types::error::StorageError;
use parking_lot::RwLock;
use sochdb::EmbeddedConnection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn, error};

// ═══════════════════════════════════════════════════════════════════════════
// GAP-03: Discriminated error mapping from SochDB → StorageError
// ═══════════════════════════════════════════════════════════════════════════

/// Map a SochDB `ClientError` to the appropriate `StorageError` variant.
///
/// Previously, ALL SochDB errors were collapsed into `StorageError::OpenFailed`,
/// discarding all diagnostic information (log₂(18) ≈ 4.17 bits → 0 bits).
/// This function preserves the error classification so callers can:
/// - Retry on transient errors (`TransactionConflict`)
/// - Fail-fast on corruption (`WalCorruption`)
/// - Distinguish "key not found" from "database broken"
fn map_sochdb_error(e: sochdb::error::ClientError, context: &str) -> StorageError {
    use sochdb::error::ClientError;
    match e {
        // ── Key/path not found → StorageError::NotFound ──────────
        ClientError::NotFound(detail) => StorageError::NotFound {
            key: format!("{context}: {detail}"),
        },
        ClientError::PathNotFound(detail) => StorageError::NotFound {
            key: format!("{context}: path {detail}"),
        },

        // ── Transaction/concurrency conflicts → retryable ───────
        ClientError::Transaction(detail) => StorageError::TransactionConflict {
            key: format!("{context}: {detail}"),
        },
        ClientError::SerializationFailure { our_txn, conflicting_txn, .. } => {
            StorageError::TransactionConflict {
                key: format!("{context}: txn {our_txn} conflicts with {conflicting_txn}"),
            }
        }
        ClientError::Visibility(detail) => StorageError::TransactionConflict {
            key: format!("{context}: MVCC visibility: {detail}"),
        },

        // ── WAL/durability errors → corruption ──────────────────
        ClientError::Wal(detail) => StorageError::WalCorruption {
            detail: format!("{context}: {detail}"),
        },

        // ── Serialization errors → SerializationFailed ──────────
        ClientError::Serialization(detail) => StorageError::SerializationFailed {
            detail: format!("{context}: {detail}"),
        },

        // ── I/O errors → Io ─────────────────────────────────────
        ClientError::Io(io_err) => StorageError::Io(io_err),

        // ── Everything else → OpenFailed with full context ──────
        // Schema, Validation, Constraint, Vector, PqNotTrained,
        // TypeMismatch, TokenBudgetExceeded, Parse, ScalarPath,
        // PoolExhausted, Storage, Internal
        other => StorageError::OpenFailed {
            detail: format!("{context}: {other}"),
        },
    }
}

/// The unified SochDB storage backend.
///
/// Wraps `EmbeddedConnection` (which uses the kernel `Database` that auto-recovers)
/// with write serialization, graceful shutdown, and convenience write methods
/// matching agentreplay's proven patterns.
///
/// ## Serialization model (GAP-08)
///
/// Uses a `parking_lot::RwLock<()>` to allow concurrent reads while serializing
/// writes. The original `Mutex<()>` serialized ALL operations (including reads)
/// to prevent the `active_txn_id` ABA race on `EmbeddedConnection`.
///
/// Under Amdahl's Law, if reads are 80% of operations:
/// - Mutex: max speedup = 1/(0.2 + 0.8/P) = 2.5× at P=4
/// - RwLock: reads run at full parallelism, max speedup → 1/0.2 = 5×
///
/// Write operations acquire the exclusive (write) lock. Read operations
/// (get, scan) acquire the shared (read) lock. MVCC guarantees snapshot
/// isolation for readers, so concurrent reads are safe.
pub struct SochStore {
    connection: EmbeddedConnection,
    /// RwLock allows concurrent reads while serializing writes.
    /// Write operations (put, delete, commit) acquire exclusive lock.
    /// Read operations (get, scan) acquire shared lock.
    /// MVCC snapshot isolation guarantees read consistency.
    op_lock: RwLock<()>,
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

                    // ── WAL backup cleanup ─────────────────────────
                    // After a successful open + self-test, remove old WAL
                    // backup files from previous retry-quarantine cycles.
                    // These are safety copies and are no longer needed once
                    // the database opens and passes verification.
                    if let Ok(entries) = std::fs::read_dir(path) {
                        let mut cleaned = 0u32;
                        for entry in entries.flatten() {
                            let name = entry.file_name();
                            let name_str = name.to_string_lossy();
                            if name_str.starts_with("wal.log.backup.") || name_str.starts_with("wal.log.corrupt.") {
                                if let Err(e) = std::fs::remove_file(entry.path()) {
                                    warn!(file = %name_str, error = %e, "Failed to remove old WAL backup");
                                } else {
                                    cleaned += 1;
                                }
                            }
                        }
                        if cleaned > 0 {
                            info!(cleaned, "Cleaned up old WAL backup/corrupt files after successful open");
                        }
                    }

                    return Ok(Self {
                        connection,
                        op_lock: RwLock::new(()),
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
    ///
    /// **Emits a `WARN` log** — use [`open_ephemeral_quiet`] for subsystems
    /// (like the gateway server) that intentionally use ephemeral storage.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let store = Self::open_ephemeral_quiet()?;
        warn!(
            path = ?store.store_path,
            "Opening EPHEMERAL SochDB store — data will NOT survive restart"
        );
        Ok(store)
    }

    /// Open an ephemeral database without emitting a warning.
    ///
    /// Identical to [`open_in_memory`] but logs at `DEBUG` level.
    /// Use this for subsystems (embedded gateway, tests) that are *expected*
    /// to run on throwaway storage.
    pub fn open_ephemeral_quiet() -> Result<Self, StorageError> {
        let tmp_dir = std::env::temp_dir().join(format!("clawdesk-ephemeral-{}", std::process::id()));
        let store_path = tmp_dir.clone();
        debug!(
            path = ?store_path,
            "Opening ephemeral SochDB store (quiet)"
        );
        let connection = EmbeddedConnection::open(&tmp_dir)
            .map_err(|e| map_sochdb_error(e, "open ephemeral"))?;
        Ok(Self {
            connection,
            op_lock: RwLock::new(()),
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
    // Atomic transaction API (GAP-02)
    // =========================================================================

    /// Apply a batch of puts and deletes as a single MVCC transaction.
    ///
    /// Uses `EmbeddedConnection::begin()` + operations + `commit()` under
    /// a single write lock, guaranteeing all-or-nothing semantics. If any
    /// operation fails, the transaction is aborted and no changes are applied.
    ///
    /// This replaces `TransactionalConn`'s non-atomic buffer-and-flush commit
    /// for SochStore-backed usage. For N operations:
    /// - Old: N individual puts with N lock acquisitions → partial state on crash
    /// - New: 1 lock acquisition, 1 begin, N writes, 1 commit → atomic
    pub fn apply_atomic_batch(
        &self,
        puts: &[(&str, &[u8])],
        deletes: &[&str],
    ) -> Result<u64, StorageError> {
        if puts.is_empty() && deletes.is_empty() {
            return Ok(0);
        }
        let _guard = self.op_lock.write();
        self.connection.begin()
            .map_err(|e| map_sochdb_error(e, "atomic_batch begin"))?;

        // Apply all deletes first (order: deletes before puts for idempotency)
        for key in deletes {
            if let Err(e) = self.connection.delete(key) {
                let _ = self.connection.abort();
                return Err(map_sochdb_error(e, &format!("atomic_batch delete '{key}'")));
            }
        }
        // Apply all puts
        for (key, value) in puts {
            if let Err(e) = self.connection.put(key, value) {
                let _ = self.connection.abort();
                return Err(map_sochdb_error(e, &format!("atomic_batch put '{key}'")));
            }
        }

        let seq = self.connection.commit()
            .map_err(|e| map_sochdb_error(e, "atomic_batch commit"))?;
        self.connection.fsync()
            .map_err(|e| map_sochdb_error(e, "atomic_batch fsync"))?;
        Ok(seq)
    }

    /// Execute a closure within a native MVCC transaction.
    ///
    /// Acquires the write lock, calls `begin()`, executes `f`, and commits.
    /// If `f` returns `Err`, the transaction is aborted.
    ///
    /// The closure receives `&EmbeddedConnection` to perform raw put/get/delete
    /// calls within the transaction boundary.
    ///
    /// # Example
    /// ```rust,ignore
    /// store.with_transaction("delete-cascade", |conn| {
    ///     conn.delete("sessions/abc/state")?;
    ///     conn.delete("sessions/abc/messages/1")?;
    ///     conn.delete("sessions/abc/messages/2")?;
    ///     Ok(3)
    /// })?;
    /// ```
    pub fn with_transaction<F, T>(&self, label: &str, f: F) -> Result<T, StorageError>
    where
        F: FnOnce(&EmbeddedConnection) -> Result<T, sochdb::error::ClientError>,
    {
        let _guard = self.op_lock.write();
        self.connection.begin()
            .map_err(|e| map_sochdb_error(e, &format!("{label}: begin")))?;

        match f(&self.connection) {
            Ok(result) => {
                self.connection.commit()
                    .map_err(|e| map_sochdb_error(e, &format!("{label}: commit")))?;
                self.connection.fsync()
                    .map_err(|e| map_sochdb_error(e, &format!("{label}: fsync")))?;
                debug!(label, "atomic transaction committed");
                Ok(result)
            }
            Err(e) => {
                let _ = self.connection.abort();
                warn!(label, error = %e, "atomic transaction aborted");
                Err(map_sochdb_error(e, &format!("{label}: operation failed")))
            }
        }
    }

    // =========================================================================
    // Write API (mirrors agentreplay's put / put_durable / put_batch)
    // =========================================================================

    /// Put a single key-value pair (group-commit only, no explicit commit).
    ///
    /// Relies on `DatabaseConfig::group_commit` to batch writes for throughput.
    /// Use `put_durable()` for writes that must be immediately committed.
    pub fn put(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        let _guard = self.op_lock.write();
        self.connection.put(key, value)
            .map_err(|e| map_sochdb_error(e, "put"))
    }

    /// Put a single key-value pair with immediate commit (durable).
    ///
    /// Acquires write lock, writes, and commits in one atomic operation.
    /// Use for critical data that must survive a crash immediately.
    pub fn put_durable(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        let _guard = self.op_lock.write();
        self.connection.put(key, value)
            .map_err(|e| map_sochdb_error(e, "put_durable write"))?;
        self.connection.commit()
            .map_err(|e| map_sochdb_error(e, "put_durable commit"))?;
        self.connection.fsync()
            .map_err(|e| map_sochdb_error(e, "put_durable fsync"))?;
        Ok(())
    }

    /// Put a batch of key-value pairs with a single commit at the end.
    ///
    /// More efficient than individual `put_durable()` calls for bulk writes.
    pub fn put_batch(&self, entries: &[(&str, &[u8])]) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }
        let _guard = self.op_lock.write();
        for (key, value) in entries {
            self.connection.put(key, value)
                .map_err(|e| map_sochdb_error(e, &format!("put_batch write for key {key}")))?;
        }
        self.connection.commit()
            .map_err(|e| map_sochdb_error(e, "put_batch commit"))?;
        self.connection.fsync()
            .map_err(|e| map_sochdb_error(e, "put_batch fsync"))?;
        Ok(())
    }

    // =========================================================================
    // Read API
    // =========================================================================

    /// Get a value by key.
    ///
    /// Acquires shared (read) lock — multiple concurrent reads are allowed.
    /// MVCC snapshot isolation guarantees consistent reads.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let _guard = self.op_lock.read();
        self.connection.get(key)
            .map_err(|e| map_sochdb_error(e, "get"))
    }

    /// Scan all key-value pairs matching a prefix.
    ///
    /// Returns `Vec<(String, Vec<u8>)>` — keys are strings (not byte arrays).
    /// Acquires shared (read) lock — concurrent reads allowed.
    pub fn scan(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StorageError> {
        let _guard = self.op_lock.read();
        self.connection.scan(prefix)
            .map_err(|e| map_sochdb_error(e, "scan"))
    }

    /// Scan key-value pairs in a lexicographic range `[start, end]`.
    ///
    /// GAP-09: Enables O(R) bounded retrieval instead of O(N) full prefix
    /// scan + in-memory filter. Uses EmbeddedConnection::scan_range() directly.
    pub fn scan_range(&self, start: &str, end: &str) -> Result<Vec<(String, Vec<u8>)>, StorageError> {
        let _guard = self.op_lock.read();
        self.connection.scan_range(start, end)
            .map_err(|e| map_sochdb_error(e, "scan_range"))
    }

    // =========================================================================
    // Snapshot reads (GAP-15)
    // =========================================================================

    /// Read multiple keys in a single consistent snapshot.
    ///
    /// Acquires the read lock ONCE and performs all gets within the same
    /// MVCC snapshot, guaranteeing that all returned values reflect the
    /// same point-in-time state.
    ///
    /// Without this, individual `get()` calls each acquire their own lock
    /// and may see different transaction states.
    pub fn get_batch(&self, keys: &[&str]) -> Result<Vec<(String, Option<Vec<u8>>)>, StorageError> {
        let _guard = self.op_lock.read();
        let mut results = Vec::with_capacity(keys.len());
        for key in keys {
            let value = self.connection.get(key)
                .map_err(|e| map_sochdb_error(e, &format!("get_batch '{key}'")))?;
            results.push((key.to_string(), value));
        }
        Ok(results)
    }

    /// Read multiple prefixes in a single consistent snapshot.
    ///
    /// Returns all key-value pairs matching any of the given prefixes,
    /// all from the same MVCC snapshot. Useful for constructing a consistent
    /// view across multiple data categories (e.g., session state + messages
    /// + summaries for a single session).
    pub fn scan_batch(&self, prefixes: &[&str]) -> Result<Vec<(String, Vec<u8>)>, StorageError> {
        let _guard = self.op_lock.read();
        let mut results = Vec::new();
        for prefix in prefixes {
            let entries = self.connection.scan(prefix)
                .map_err(|e| map_sochdb_error(e, &format!("scan_batch '{prefix}'")))?;
            results.extend(entries);
        }
        Ok(results)
    }

    // =========================================================================
    // Delete API
    // =========================================================================

    /// Delete a key (under write lock).
    pub fn delete(&self, key: &str) -> Result<(), StorageError> {
        let _guard = self.op_lock.write();
        self.connection.delete(key)
            .map_err(|e| map_sochdb_error(e, "delete"))
    }

    /// Delete a key with immediate commit (durable).
    ///
    /// Acquires write lock, deletes, and commits in one atomic operation.
    /// Prevents the race in `delete()` + `commit()` where an interleaving
    /// `put_durable()` can re-write the key between the two calls.
    pub fn delete_durable(&self, key: &str) -> Result<(), StorageError> {
        let _guard = self.op_lock.write();
        self.connection.delete(key)
            .map_err(|e| map_sochdb_error(e, "delete_durable delete"))?;
        self.connection.commit()
            .map_err(|e| map_sochdb_error(e, "delete_durable commit"))?;
        self.connection.fsync()
            .map_err(|e| map_sochdb_error(e, "delete_durable fsync"))?;
        Ok(())
    }

    /// Delete all keys matching a prefix, with a single commit at the end.
    ///
    /// Returns the number of keys deleted. Useful for bulk cleanup (e.g. clearing all chats).
    pub fn delete_prefix(&self, prefix: &str) -> Result<usize, StorageError> {
        let _guard = self.op_lock.write();
        let entries = self.connection.scan(prefix)
            .map_err(|e| map_sochdb_error(e, "delete_prefix scan"))?;
        let count = entries.len();
        for (key, _) in &entries {
            self.connection.delete(key)
                .map_err(|e| map_sochdb_error(e, &format!("delete_prefix delete '{key}'")))?;
        }
        if count > 0 {
            self.connection.commit()
                .map_err(|e| map_sochdb_error(e, "delete_prefix commit"))?;
            self.connection.fsync()
                .map_err(|e| map_sochdb_error(e, "delete_prefix fsync"))?;
        }
        Ok(count)
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
        let _guard = self.op_lock.write();
        self.connection.commit()
            .map_err(|e| map_sochdb_error(e, "commit"))
    }

    // =========================================================================
    // Maintenance
    // =========================================================================

    /// Truncate the WAL file to 0 bytes.
    ///
    /// The in-memory memtable retains all data for the current session, but
    /// a crash/restart after truncation will only recover data written AFTER
    /// the truncation. Use after `delete_prefix` when you need deletions to
    /// truly survive restart (the WAL recovery path does not preserve
    /// tombstones — deleted entries re-appear as empty-value writes).
    pub fn truncate_wal(&self) -> Result<(), StorageError> {
        let _guard = self.op_lock.write();
        self.connection.truncate_wal()
            .map_err(|e| map_sochdb_error(e, "truncate_wal"))
    }

    /// Atomically clear all chat-related data from the WAL.
    ///
    /// Because SochDB's WAL recovery does not distinguish tombstones from
    /// regular writes, `delete_prefix` tombstones are lost across restarts.
    /// This method works around the limitation by:
    ///
    /// 1. Deleting chat data in the memtable (tombstones)
    /// 2. Collecting all non-chat data that must survive
    /// 3. Truncating the WAL (physically empty)
    /// 4. Re-writing the non-chat data to the fresh WAL
    ///
    /// After this, the WAL only contains non-chat data. On restart, WAL
    /// replay recovers only the preserved data — deleted chats stay deleted.
    pub fn clear_all_chat_data(&self) -> Result<usize, StorageError> {
        let _guard = self.op_lock.write();

        // ── Step 1: Delete chat-related prefixes (tombstones in memtable) ──
        let chat_prefixes = [
            "chats/",
            "chat_index/",
            "chat_sessions/",
            "tool_history/",
            "sessions/",
            "turns/",
            "archive/",
            "archive_index/",
        ];
        let mut total_deleted = 0usize;
        for prefix in &chat_prefixes {
            let entries = self.connection.scan(prefix)
                .map_err(|e| map_sochdb_error(e, &format!("clear_all_chat_data scan '{prefix}'")))?;
            let count = entries.len();
            for (key, _) in &entries {
                self.connection.delete(key)
                    .map_err(|e| map_sochdb_error(e, &format!("clear_all_chat_data delete '{key}'")))?;
            }
            if count > 0 {
                self.connection.commit()
                    .map_err(|e| map_sochdb_error(e, &format!("clear_all_chat_data commit after '{prefix}'")))?;
            }
            total_deleted += count;
        }

        // ── Step 2: Collect non-chat data from memtable ──────────────────
        // Tombstones correctly shadow deleted chat data, so scanning non-chat
        // prefixes returns only the data we want to preserve.
        let preserve_prefixes = [
            "agents/",
            "config/",
            "graph/",
            "pipelines/",
            "pipeline_runs/",
            "notifications/",
            "clipboard/",
            "canvases/",
            "journal/",
            "skills/",
            "runtime:",
        ];
        let mut preserved: Vec<(String, Vec<u8>)> = Vec::new();
        for prefix in &preserve_prefixes {
            if let Ok(entries) = self.connection.scan(prefix) {
                preserved.extend(entries);
            }
        }
        // Also preserve the singleton key "channel_origins"
        if let Ok(Some(val)) = self.connection.get("channel_origins") {
            preserved.push(("channel_origins".to_string(), val));
        }

        let preserved_count = preserved.len();

        // ── Step 3: Truncate WAL (memtable stays intact) ─────────────────
        self.connection.truncate_wal()
            .map_err(|e| map_sochdb_error(e, "clear_all_chat_data truncate_wal"))?;

        // ── Step 4: Re-write preserved data to fresh WAL ─────────────────
        if !preserved.is_empty() {
            for (key, value) in &preserved {
                self.connection.put(key, value)
                    .map_err(|e| map_sochdb_error(e, &format!("clear_all_chat_data re-put '{key}'")))?;
            }
            self.connection.commit()
                .map_err(|e| map_sochdb_error(e, "clear_all_chat_data commit preserved"))?;
            self.connection.fsync()
                .map_err(|e| map_sochdb_error(e, "clear_all_chat_data fsync preserved"))?;
        }

        tracing::info!(
            deleted = total_deleted,
            preserved = preserved_count,
            "clear_all_chat_data: WAL truncated and non-chat data re-persisted"
        );
        Ok(total_deleted)
    }

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
        let _guard = self.op_lock.write();
        self.connection.checkpoint()
            .map_err(|e| map_sochdb_error(e, "checkpoint"))
    }

    /// Checkpoint + GC in one operation.
    pub fn checkpoint_and_gc(&self) -> Result<u64, StorageError> {
        let _guard = self.op_lock.write();
        let seq = self.connection.checkpoint()
            .map_err(|e| map_sochdb_error(e, "checkpoint"))?;

        let _reclaimed = self.connection.gc();

        info!(checkpoint_seq = seq, "WAL checkpoint + GC completed");
        Ok(seq)
    }

    /// Force an fsync to ensure all buffered writes are durable on disk.
    pub fn sync(&self) -> Result<(), StorageError> {
        self.connection.fsync()
            .map_err(|e| map_sochdb_error(e, "fsync"))
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
            let _guard = self.op_lock.write();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_all_chat_data_removes_chat_preserves_non_chat() {
        let store = SochStore::open_ephemeral_quiet().unwrap();

        // Write chat data under various prefixes
        store.put_durable("chats/abc123", b"session-data").unwrap();
        store.put_durable("chats/def456", b"session-data-2").unwrap();
        store.put_durable("tool_history/abc123", b"tools").unwrap();
        store.put_durable("chat_index/agent1/2024-01-01/abc123", b"idx").unwrap();
        store.put_durable("sessions/abc123/messages/100", b"msg").unwrap();

        // Write non-chat data that must survive
        store.put_durable("agents/my-agent", b"agent-config").unwrap();
        store.put_durable("config/main", b"app-config").unwrap();
        store.put_durable("pipelines/p1", b"pipeline").unwrap();
        store.put_durable("channel_origins", b"origins").unwrap();

        // Verify everything exists
        assert!(store.get("chats/abc123").unwrap().is_some());
        assert!(store.get("agents/my-agent").unwrap().is_some());

        // Clear all chat data
        let deleted = store.clear_all_chat_data().unwrap();
        assert!(deleted >= 5, "should delete at least 5 chat entries, got {deleted}");

        // Chat data should be gone
        assert!(store.scan("chats/").unwrap().is_empty());
        assert!(store.scan("tool_history/").unwrap().is_empty());
        assert!(store.scan("chat_index/").unwrap().is_empty());
        assert!(store.scan("sessions/").unwrap().is_empty());

        // Non-chat data should be preserved
        assert_eq!(store.get("agents/my-agent").unwrap().unwrap(), b"agent-config");
        assert_eq!(store.get("config/main").unwrap().unwrap(), b"app-config");
        assert_eq!(store.get("pipelines/p1").unwrap().unwrap(), b"pipeline");
        assert_eq!(store.get("channel_origins").unwrap().unwrap(), b"origins");
    }

    #[test]
    fn clear_all_chat_data_survives_reopen() {
        // This test simulates the restart scenario by closing and reopening the store
        let tmp_dir = std::env::temp_dir().join(format!(
            "clawdesk-clear-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();

        // Phase 1: Write data + clear chats
        {
            let store = SochStore::open(&tmp_dir).unwrap();
            store.put_durable("chats/abc", b"session-json").unwrap();
            store.put_durable("chats/def", b"session-json-2").unwrap();
            store.put_durable("agents/my-agent", b"agent-config").unwrap();
            store.put_durable("config/main", b"app-config").unwrap();

            let deleted = store.clear_all_chat_data().unwrap();
            assert!(deleted >= 2);

            // Drop triggers shutdown (checkpoint + fsync)
        }

        // Phase 2: Reopen — chat data should NOT come back
        {
            let store = SochStore::open(&tmp_dir).unwrap();

            // Chat data must be gone (the whole point of this fix)
            let chats = store.scan("chats/").unwrap();
            assert!(
                chats.is_empty(),
                "Chat data resurrected after restart! Found {} entries: {:?}",
                chats.len(),
                chats.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>()
            );

            // Non-chat data must survive
            assert_eq!(
                store.get("agents/my-agent").unwrap().unwrap(),
                b"agent-config",
                "agents/ data lost after clear + restart"
            );
            assert_eq!(
                store.get("config/main").unwrap().unwrap(),
                b"app-config",
                "config/ data lost after clear + restart"
            );
        }

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn clear_all_chat_data_empty_db_is_noop() {
        // Use a unique temp dir to avoid contamination from other tests
        let tmp_dir = std::env::temp_dir().join(format!(
            "clawdesk-noop-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let store = SochStore::open(&tmp_dir).unwrap();
        let deleted = store.clear_all_chat_data().unwrap();
        assert_eq!(deleted, 0);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn truncate_wal_does_not_panic() {
        let store = SochStore::open_ephemeral_quiet().unwrap();
        store.put_durable("test/key", b"value").unwrap();
        store.truncate_wal().unwrap();
        // After truncation, memtable still has the data for the current session
        assert_eq!(store.get("test/key").unwrap().unwrap(), b"value");
    }
}
