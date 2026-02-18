//! # clawdesk-sochdb
//!
//! SochDB adapter for ClawDesk — implements all storage traits with:
//! - ACID transactions (WAL + buffered commit)
//! - MVCC + SSI (Serializable Snapshot Isolation)
//! - Built-in HNSW vector search
//! - TOON output format (58-67% fewer tokens than JSON)
//! - Path API for O(1) session lookup
//!
//! ## WAL lifecycle
//!
//! `SochStore` manages WAL health automatically:
//! - Opens with `ConnectionConfig` that enables group commit (100-op batches,
//!   10ms max wait) for write throughput.
//! - `checkpoint_and_gc()` performs a checkpoint + GC pass — call periodically
//!   (e.g. from an idle-detection system) to keep WAL size bounded.
//! - `fsync()` forces a durable write barrier when needed.
//!
//! Replaces the four-system ad-hoc stack (JSON files + Map cache + JSONL + external LanceDB)
//! with a single embedded database.

pub mod bridge;
pub mod config;
pub mod conversation;
pub mod graph;
pub mod session;
pub mod vector;

pub use bridge::{
    SochConn,
    SochSemanticCache, SochTraceStore, SochCheckpointStore,
    SochGraphOverlay, SochTemporalGraph, SochPolicyEngine,
    SochAtomicWriter, SochAgentRegistry, SochToolRouter,
};

use clawdesk_types::error::StorageError;
use sochdb::Database;
use std::path::Path;
use tracing::info;

/// The unified SochDB storage backend.
///
/// Provides ACID-transactional access to sessions, conversations,
/// config, vectors, and graph data through a single database.
pub struct SochStore {
    db: Database,
}

impl SochStore {
    /// Open or create a SochDB database at the given path.
    ///
    /// Uses WAL-backed durable storage with group-commit enabled
    /// (batch size 100, max wait 10 ms) for optimal write throughput.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        info!(?path, "opening SochDB store");

        let config = sochdb::ConnectionConfig {
            group_commit: true,
            group_commit_batch_size: 100,
            group_commit_max_wait_us: 10_000,
            ..sochdb::ConnectionConfig::default()
        };

        let db =
            Database::open_with_config(path, config).map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        Ok(Self { db })
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let db = Database::open("./clawdesk-test-db").map_err(|e| StorageError::OpenFailed {
            detail: e.to_string(),
        })?;
        Ok(Self { db })
    }

    /// Get a reference to the underlying database.
    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Checkpoint the WAL and run GC to reclaim space.
    ///
    /// Call this periodically (e.g. every 5 minutes when idle) to keep
    /// WAL size bounded and prevent unbounded growth during long sessions.
    /// Returns the checkpoint sequence number on success.
    pub fn checkpoint_and_gc(&self) -> Result<u64, StorageError> {
        let seq = self
            .db
            .checkpoint()
            .map_err(|e| StorageError::OpenFailed {
                detail: format!("checkpoint failed: {e}"),
            })?;

        let _reclaimed = self.db.gc().map_err(|e| StorageError::OpenFailed {
            detail: format!("gc failed: {e}"),
        })?;

        info!(checkpoint_seq = seq, "WAL checkpoint + GC completed");
        Ok(seq)
    }

    /// Force an fsync to ensure all buffered writes are durable on disk.
    pub fn sync(&self) -> Result<(), StorageError> {
        self.db.fsync().map_err(|e| StorageError::OpenFailed {
            detail: format!("fsync failed: {e}"),
        })
    }
}

impl std::fmt::Debug for SochStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SochStore").finish()
    }
}
