//! Thread store — the primary API for chat-thread persistence.
//!
//! Wraps a single SochDB `Database` and provides namespaced CRUD for threads
//! and messages, following the patterns established by `agentreplay-storage`:
//!
//! - Single database, key-prefix namespacing
//! - Write-time secondary indexes
//! - Zero-padded timestamps for lexicographic ordering
//! - Cascading deletes
//! - Group-commit for throughput

use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;
use sochdb::EmbeddedConnection;
use tracing::{debug, error, info, warn};

use crate::error::{Result, ThreadStoreError};
use crate::keys;
use crate::types::*;

/// Configuration for the thread store.
#[derive(Debug, Clone)]
pub struct ThreadStoreConfig {
    /// Enable WAL group commit (recommended for write throughput).
    pub group_commit: bool,
}

impl Default for ThreadStoreConfig {
    fn default() -> Self {
        Self {
            group_commit: true,
        }
    }
}

/// Namespaced chat-thread store backed by SochDB.
///
/// Each thread is a namespace partition inside a single database:
/// - Thread metadata: `threads/{thread_id:032x}`
/// - Messages:        `msgs/{thread_id:032x}/{timestamp:020}/{msg_id:032x}`
/// - Indexes:         `idx/agent/{agent_id}/{updated:020}/{thread_id:032x}`
///
/// All reads and writes are ACID-transactional via SochDB's WAL + MVCC.
pub struct ThreadStore {
    db: Arc<EmbeddedConnection>,
    /// Serializes write access to the EmbeddedConnection.
    ///
    /// EmbeddedConnection uses a shared `active_txn_id` AtomicU64 for
    /// transaction management. Concurrent *writes* race on `ensure_txn()`,
    /// potentially causing `commit()` to commit the wrong transaction.
    /// Reads use MVCC snapshot isolation and are safe to run concurrently.
    ///
    /// RwLock allows many concurrent readers (get/scan) while serializing
    /// writers (put/delete/commit). This eliminates the global chokepoint
    /// where read-heavy workloads (multi-channel chat) were bottlenecked.
    lock: RwLock<()>,
    /// Filesystem path this store was opened at (for diagnostics / health checks).
    path: std::path::PathBuf,
    /// Whether this store is running on ephemeral (temp) storage.
    is_ephemeral: bool,
}

impl ThreadStore {
    // ════════════════════════════════════════════════════════════════════════
    // Construction
    // ════════════════════════════════════════════════════════════════════════

    /// Open (or create) a thread store at the given filesystem path.
    ///
    /// Creates the directory if it doesn't exist (avoiding the silent-fallback
    /// bug that was fixed in clawdesk-tauri's SochDB initialization).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_config(path, ThreadStoreConfig::default())
    }

    /// Open with explicit configuration.
    pub fn open_with_config(path: impl AsRef<Path>, config: ThreadStoreConfig) -> Result<Self> {
        let path = path.as_ref();
        info!(?path, "Opening thread store");

        // Ensure the directory exists (lesson learned from Bug 1 in clawdesk-tauri)
        std::fs::create_dir_all(path).map_err(|e| ThreadStoreError::OpenFailed {
            detail: format!("create_dir_all failed: {e}"),
        })?;

        let mut db_config = sochdb_storage::database::DatabaseConfig::default();
        db_config.group_commit = config.group_commit;

        let delays = [
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(500),
            std::time::Duration::from_secs(2),
        ];
        let mut last_err = String::new();

        for (attempt, delay) in delays.iter().enumerate() {
            // On attempt 2 (third try), back up a possibly corrupt WAL.
            // BUG FIX: Use copy instead of rename to prevent data loss.
            // Previously, rename() destroyed the original WAL — if the failure
            // was transient (e.g., stale lock file from crash), the thread data
            // was permanently lost. Now we copy (preserving the original) and
            // also check for stale lock files, matching SochDB's non-destructive
            // recovery strategy.
            if attempt == 2 {
                let wal_path = path.join("wal.log");
                if wal_path.exists() {
                    let backup = path.join(format!(
                        "wal.log.backup.{}",
                        chrono::Utc::now().format("%Y%m%dT%H%M%S")
                    ));
                    warn!(
                        from = ?wal_path,
                        to = ?backup,
                        "Backing up WAL before final retry (non-destructive copy)"
                    );
                    if let Err(e) = std::fs::copy(&wal_path, &backup) {
                        warn!(error = %e, "Failed to backup WAL — continuing anyway");
                    }
                }
                // Also remove stale lock files from crashed processes
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
                            warn!(lock_path = ?lock_path, "Removing stale lock file");
                            let _ = std::fs::remove_file(&lock_path);
                        }
                    }
                }
            }

            match EmbeddedConnection::open_with_config(path, db_config.clone()) {
                Ok(db) => {
                    if attempt > 0 {
                        info!(attempt = attempt + 1, "Thread store opened after retry");
                    }
                    info!(?path, "Thread store opened — ACID storage active");
                    return Ok(Self {
                        db: Arc::new(db),
                        lock: RwLock::new(()),
                        path: path.to_path_buf(),
                        is_ephemeral: false,
                    });
                }
                Err(e) => {
                    last_err = e.to_string();
                    warn!(
                        attempt = attempt + 1,
                        delay_ms = delay.as_millis(),
                        error = %last_err,
                        "Thread store open failed — retrying"
                    );
                    std::thread::sleep(*delay);
                }
            }
        }

        // All retries exhausted
        error!(
            error = %last_err,
            path = ?path,
            "Thread store open FAILED after 3 retries"
        );
        Err(ThreadStoreError::OpenFailed {
            detail: format!("Thread store open failed after 3 retries: {}", last_err),
        })
    }

    /// Wrap an existing `Arc<EmbeddedConnection>` (for sharing with other subsystems
    /// that already have a SochDB handle, e.g. `SochStore`).
    pub fn from_shared(db: Arc<EmbeddedConnection>) -> Self {
        Self {
            db,
            lock: RwLock::new(()),
            path: std::path::PathBuf::from("<shared>"),
            is_ephemeral: false,
        }
    }

    /// Get a reference to the underlying EmbeddedConnection.
    pub fn connection(&self) -> &EmbeddedConnection {
        &self.db
    }

    /// Get a clone of the `Arc<EmbeddedConnection>`.
    pub fn connection_arc(&self) -> Arc<EmbeddedConnection> {
        Arc::clone(&self.db)
    }

    // ════════════════════════════════════════════════════════════════════════
    // Thread CRUD
    // ════════════════════════════════════════════════════════════════════════

    /// Create a new thread and return its metadata.
    pub fn create_thread(
        &self,
        agent_id: &str,
        title: &str,
        model: Option<&str>,
    ) -> Result<ThreadMeta> {
        let id = keys::new_id();
        let now = chrono::Utc::now();
        let now_rfc = now.to_rfc3339();
        let now_us = now.timestamp_micros() as u64;

        let meta = ThreadMeta {
            id,
            agent_id: agent_id.to_string(),
            title: title.to_string(),
            created_at: now_rfc.clone(),
            updated_at: now_rfc,
            message_count: 0,
            model: model.map(|s| s.to_string()),
            pinned: false,
            archived: false,
            tags: Vec::new(),
            spawn_mode: "standalone".to_string(),
            parent_thread_id: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
        };

        // Primary record
        let key = keys::thread_key(id);
        let bytes = serde_json::to_vec(&meta).map_err(|e| ThreadStoreError::Serialization {
            detail: e.to_string(),
        })?;
        self.put(&key, &bytes)?;

        // Secondary index: agent → thread
        let idx_key = keys::idx_agent_thread(agent_id, now_us, id);
        self.put(&idx_key, &[])?;

        // Reverse index: thread → agent
        let rev_key = keys::idx_thread_agent(id);
        self.put(&rev_key, agent_id.as_bytes())?;

        // Increment thread counter
        self.increment_counter(keys::META_THREAD_COUNT)?;

        debug!(thread_id = %keys::u128_to_uuid(id), %agent_id, "Thread created");
        Ok(meta)
    }

    /// Get thread metadata by id.
    pub fn get_thread(&self, thread_id: u128) -> Result<Option<ThreadMeta>> {
        let key = keys::thread_key(thread_id);
        match self.get(&key)? {
            Some(bytes) => {
                let meta: ThreadMeta =
                    serde_json::from_slice(&bytes).map_err(|e| ThreadStoreError::Corruption {
                        detail: format!("thread {}: {}", keys::u128_to_uuid(thread_id), e),
                    })?;
                Ok(Some(meta))
            }
            None => Ok(None),
        }
    }

    /// Update thread metadata (title, pinned, archived, tags, etc.).
    ///
    /// The `updater` closure receives a mutable reference to the metadata.
    /// Returns the updated metadata.
    pub fn update_thread<F>(&self, thread_id: u128, updater: F) -> Result<ThreadMeta>
    where
        F: FnOnce(&mut ThreadMeta),
    {
        let mut meta = self
            .get_thread(thread_id)?
            .ok_or_else(|| ThreadStoreError::ThreadNotFound {
                id: keys::u128_to_uuid(thread_id),
            })?;

        let old_updated_us = chrono::DateTime::parse_from_rfc3339(&meta.updated_at)
            .map(|dt| dt.timestamp_micros() as u64)
            .unwrap_or(0);

        updater(&mut meta);
        meta.updated_at = chrono::Utc::now().to_rfc3339();

        let new_updated_us = chrono::DateTime::parse_from_rfc3339(&meta.updated_at)
            .map(|dt| dt.timestamp_micros() as u64)
            .unwrap_or(0);

        // Write updated metadata
        let key = keys::thread_key(thread_id);
        let bytes = serde_json::to_vec(&meta).map_err(|e| ThreadStoreError::Serialization {
            detail: e.to_string(),
        })?;
        self.put(&key, &bytes)?;

        // Update secondary index (remove old, add new)
        let old_idx = keys::idx_agent_thread(&meta.agent_id, old_updated_us, thread_id);
        let _ = self.delete(&old_idx); // best-effort
        let new_idx = keys::idx_agent_thread(&meta.agent_id, new_updated_us, thread_id);
        self.put(&new_idx, &[])?;

        debug!(thread_id = %keys::u128_to_uuid(thread_id), "Thread updated");
        Ok(meta)
    }

    /// Delete a thread and cascade-delete all its messages, attachments,
    /// and secondary index entries.
    pub fn delete_thread(&self, thread_id: u128) -> Result<bool> {
        let meta = match self.get_thread(thread_id)? {
            Some(m) => m,
            None => return Ok(false),
        };

        // 1. Delete all messages in this thread's namespace
        let msg_prefix = keys::thread_messages_prefix(thread_id);
        let msg_keys = self.scan_keys(&msg_prefix)?;
        let msg_count = msg_keys.len();
        for key in &msg_keys {
            // Check for attachments and delete them
            if let Some(msg_bytes) = self.get(key)? {
                if let Ok(msg) = serde_json::from_slice::<Message>(&msg_bytes) {
                    if msg.has_attachment {
                        let att_key = keys::attachment_key(msg.id);
                        let _ = self.delete(&att_key);
                    }
                }
            }
            self.delete(key)?;
        }

        // 2. Delete agent → thread index entry
        let updated_us = chrono::DateTime::parse_from_rfc3339(&meta.updated_at)
            .map(|dt| dt.timestamp_micros() as u64)
            .unwrap_or(0);
        let idx_key = keys::idx_agent_thread(&meta.agent_id, updated_us, thread_id);
        let _ = self.delete(&idx_key);

        // 3. Delete thread → agent reverse index
        let rev_key = keys::idx_thread_agent(thread_id);
        let _ = self.delete(&rev_key);

        // 4. Delete thread metadata
        let key = keys::thread_key(thread_id);
        self.delete(&key)?;

        // 5. Decrement counters
        self.decrement_counter(keys::META_THREAD_COUNT)?;
        self.decrement_counter_by(keys::META_MSG_COUNT, msg_count as u64)?;

        info!(
            thread_id = %keys::u128_to_uuid(thread_id),
            messages_deleted = msg_count,
            "Thread deleted (cascade)"
        );
        Ok(true)
    }

    /// List threads, optionally filtered by agent and/or archive status.
    pub fn list_threads(&self, query: &ThreadQuery) -> Result<Vec<ThreadSummary>> {
        let raw_entries: Vec<(String, Vec<u8>)> = if let Some(ref agent_id) = query.agent_id {
            // Use the agent secondary index for O(K) instead of scanning all threads
            let prefix = keys::idx_agent_prefix(agent_id);
            let idx_entries = self.scan(&prefix)?;
            // Index values are empty — the thread_id is in the key. Fetch metadata.
            let mut results = Vec::with_capacity(idx_entries.len());
            for (idx_key, _) in &idx_entries {
                // Extract thread_id from the index key: idx/agent/{agent_id}/{ts:020}/{thread_id:032x}
                if let Some(thread_id) = extract_thread_id_from_idx_key(idx_key) {
                    let tkey = keys::thread_key(thread_id);
                    if let Some(bytes) = self.get(&tkey)? {
                        results.push((tkey, bytes));
                    }
                }
            }
            results
        } else {
            // Full scan of threads/ prefix
            self.scan(keys::all_threads_prefix())?
        };

        let mut summaries: Vec<ThreadSummary> = Vec::with_capacity(raw_entries.len());
        for (_key, bytes) in &raw_entries {
            if let Ok(meta) = serde_json::from_slice::<ThreadMeta>(bytes) {
                if !query.include_archived && meta.archived {
                    continue;
                }
                summaries.push(ThreadSummary::from(&meta));
            }
        }

        // Sort
        match query.sort {
            SortOrder::UpdatedDesc => {
                summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            }
            SortOrder::CreatedAsc => {
                // We don't store created_at in summary, but threads have it.
                // Use updated_at as proxy (or re-fetch). For now sort by updated asc.
                summaries.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
            }
            SortOrder::CreatedDesc => {
                summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            }
        }

        // Pinned threads always come first
        summaries.sort_by(|a, b| b.pinned.cmp(&a.pinned));

        // Pagination
        let start = query.offset.min(summaries.len());
        let end = query
            .limit
            .map(|l| (start + l).min(summaries.len()))
            .unwrap_or(summaries.len());
        Ok(summaries[start..end].to_vec())
    }

    // ════════════════════════════════════════════════════════════════════════
    // Message CRUD (per-thread namespace)
    // ════════════════════════════════════════════════════════════════════════

    /// Append a message to a thread's namespace.
    ///
    /// Writes the message to `msgs/{thread_id}/{timestamp}/{msg_id}` and
    /// updates the thread's `updated_at` and `message_count`.
    pub fn append_message(
        &self,
        thread_id: u128,
        role: MessageRole,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<Message> {
        let msg_id = keys::new_id();
        let ts_us = keys::now_us();

        let msg = Message {
            id: msg_id,
            thread_id,
            role,
            content: content.to_string(),
            timestamp_us: ts_us,
            metadata,
            has_attachment: false,
        };

        // Write message to the thread's namespace
        let key = keys::message_key(thread_id, ts_us, msg_id);
        let bytes = serde_json::to_vec(&msg).map_err(|e| ThreadStoreError::Serialization {
            detail: e.to_string(),
        })?;
        self.put(&key, &bytes)?;

        // Update thread metadata
        self.touch_thread(thread_id)?;

        // Increment global message counter
        self.increment_counter(keys::META_MSG_COUNT)?;

        debug!(
            thread_id = %keys::u128_to_uuid(thread_id),
            msg_id = %keys::u128_to_uuid(msg_id),
            role = role.as_str(),
            "Message appended"
        );
        Ok(msg)
    }

    /// Append a message with an attachment blob.
    pub fn append_message_with_attachment(
        &self,
        thread_id: u128,
        role: MessageRole,
        content: &str,
        metadata: Option<serde_json::Value>,
        attachment: &[u8],
    ) -> Result<Message> {
        let msg_id = keys::new_id();
        let ts_us = keys::now_us();

        let msg = Message {
            id: msg_id,
            thread_id,
            role,
            content: content.to_string(),
            timestamp_us: ts_us,
            metadata,
            has_attachment: true,
        };

        // Write message
        let key = keys::message_key(thread_id, ts_us, msg_id);
        let bytes = serde_json::to_vec(&msg).map_err(|e| ThreadStoreError::Serialization {
            detail: e.to_string(),
        })?;
        self.put(&key, &bytes)?;

        // Write attachment blob separately (agentreplay pattern: payload separation)
        let att_key = keys::attachment_key(msg_id);
        self.put(&att_key, attachment)?;

        // Update thread metadata
        self.touch_thread(thread_id)?;
        self.increment_counter(keys::META_MSG_COUNT)?;

        debug!(
            msg_id = %keys::u128_to_uuid(msg_id),
            attachment_bytes = attachment.len(),
            "Message with attachment appended"
        );
        Ok(msg)
    }

    /// Get a specific message by its id and thread.
    pub fn get_message(&self, thread_id: u128, timestamp_us: u64, msg_id: u128) -> Result<Option<Message>> {
        let key = keys::message_key(thread_id, timestamp_us, msg_id);
        match self.get(&key)? {
            Some(bytes) => {
                let msg: Message =
                    serde_json::from_slice(&bytes).map_err(|e| ThreadStoreError::Corruption {
                        detail: e.to_string(),
                    })?;
                Ok(Some(msg))
            }
            None => Ok(None),
        }
    }

    /// Get an attachment blob by message id.
    pub fn get_attachment(&self, msg_id: u128) -> Result<Option<Vec<u8>>> {
        let key = keys::attachment_key(msg_id);
        self.get(&key)
    }

    /// Load all messages in a thread, in chronological order.
    ///
    /// This scans the thread's namespace prefix `msgs/{thread_id}/` which
    /// returns messages sorted by timestamp due to zero-padded keys.
    pub fn get_thread_messages(&self, thread_id: u128) -> Result<Vec<Message>> {
        let prefix = keys::thread_messages_prefix(thread_id);
        let entries = self.scan(&prefix)?;

        let mut messages = Vec::with_capacity(entries.len());
        for (_key, bytes) in &entries {
            match serde_json::from_slice::<Message>(bytes) {
                Ok(msg) => messages.push(msg),
                Err(e) => {
                    warn!(error = %e, "Skipping corrupt message");
                }
            }
        }
        Ok(messages)
    }

    /// Load messages in a time range within a thread.
    ///
    /// Since `DurableConnection` only supports prefix scans (no `scan_range`),
    /// we scan the full thread namespace and post-filter by timestamp.
    pub fn get_thread_messages_range(
        &self,
        thread_id: u128,
        from_us: u64,
        to_us: u64,
    ) -> Result<Vec<Message>> {
        let all = self.get_thread_messages(thread_id)?;
        Ok(all
            .into_iter()
            .filter(|m| m.timestamp_us >= from_us && m.timestamp_us <= to_us)
            .collect())
    }

    /// Get the most recent N messages in a thread.
    pub fn get_recent_messages(&self, thread_id: u128, limit: usize) -> Result<Vec<Message>> {
        let all = self.get_thread_messages(thread_id)?;
        let start = all.len().saturating_sub(limit);
        Ok(all[start..].to_vec())
    }

    /// Delete a single message from a thread.
    pub fn delete_message(
        &self,
        thread_id: u128,
        timestamp_us: u64,
        msg_id: u128,
    ) -> Result<bool> {
        let key = keys::message_key(thread_id, timestamp_us, msg_id);
        // Check for attachment
        if let Some(bytes) = self.get(&key)? {
            if let Ok(msg) = serde_json::from_slice::<Message>(&bytes) {
                if msg.has_attachment {
                    let att_key = keys::attachment_key(msg_id);
                    let _ = self.delete(&att_key);
                }
            }
            self.delete(&key)?;

            // Decrement thread message count
            if let Ok(Some(mut meta)) = self.get_thread(thread_id) {
                meta.message_count = meta.message_count.saturating_sub(1);
                let tkey = keys::thread_key(thread_id);
                if let Ok(bytes) = serde_json::to_vec(&meta) {
                    let _ = self.put(&tkey, &bytes);
                }
            }
            self.decrement_counter(keys::META_MSG_COUNT)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    // ════════════════════════════════════════════════════════════════════════
    // Maintenance (WAL lifecycle, same pattern as agentreplay-storage)
    // ════════════════════════════════════════════════════════════════════════

    /// Checkpoint WAL and run garbage collection.
    ///
    /// Should be called periodically (e.g. every 60 seconds or on app exit)
    /// to keep WAL bounded and ensure durability.
    pub fn checkpoint_and_gc(&self) -> Result<u64> {
        let _guard = self.lock.write();
        let seq = self.db.checkpoint().map_err(|e| ThreadStoreError::Io {
            detail: format!("checkpoint failed: {e}"),
        })?;
        let _reclaimed = self.db.gc();
        info!(checkpoint_seq = seq, "WAL checkpoint + GC completed");
        Ok(seq)
    }

    /// Force fsync to make all buffered writes durable.
    pub fn sync(&self) -> Result<()> {
        let _guard = self.lock.write();
        self.db.fsync().map_err(|e| ThreadStoreError::Io {
            detail: format!("fsync failed: {e}"),
        })
    }

    /// Get global counters.
    pub fn stats(&self) -> Result<(u64, u64)> {
        let threads = self.read_counter(keys::META_THREAD_COUNT)?;
        let msgs = self.read_counter(keys::META_MSG_COUNT)?;
        Ok((threads, msgs))
    }

    /// Returns `true` if this store is running on ephemeral (temp) storage.
    pub fn is_ephemeral(&self) -> bool {
        self.is_ephemeral
    }

    /// Returns the filesystem path this store was opened at.
    pub fn store_path(&self) -> &std::path::Path {
        &self.path
    }

    /// Get the current thread count from the global counter.
    pub fn thread_count(&self) -> Result<u64> {
        self.read_counter(keys::META_THREAD_COUNT)
    }

    /// Mark this store as ephemeral (used by fallback initialization).
    pub fn mark_ephemeral(&mut self) {
        self.is_ephemeral = true;
    }

    // ════════════════════════════════════════════════════════════════════════
    // Private helpers
    // ════════════════════════════════════════════════════════════════════════

    /// Put a key-value pair with immediate commit for WAL durability.
    ///
    /// Without commit(), writes only reach the in-memory memtable and
    /// TxnWalBuffer but NEVER flush to the WAL file. On restart,
    /// `replay_for_recovery()` finds no committed writes → data loss.
    fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        let _guard = self.lock.write();
        self.db
            .put(key, value)
            .map_err(|e| ThreadStoreError::Io {
                detail: e.to_string(),
            })?;
        self.db
            .commit()
            .map_err(|e| ThreadStoreError::Io {
                detail: format!("commit after put failed: {e}"),
            })?;
        Ok(())
    }

    /// Get a value by key (serialized to prevent ensure_txn race).
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let _guard = self.lock.read();
        self.db
            .get(key)
            .map_err(|e| ThreadStoreError::Io {
                detail: e.to_string(),
            })
    }

    /// Delete a key with immediate commit for WAL durability.
    fn delete(&self, key: &str) -> Result<()> {
        let _guard = self.lock.write();
        self.db
            .delete(key)
            .map_err(|e| ThreadStoreError::Io {
                detail: e.to_string(),
            })?;
        self.db
            .commit()
            .map_err(|e| ThreadStoreError::Io {
                detail: format!("commit after delete failed: {e}"),
            })?;
        Ok(())
    }

    /// Prefix scan returning (key_string, value_bytes) pairs.
    fn scan(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let _guard = self.lock.read();
        self.db
            .scan(prefix)
            .map_err(|e| ThreadStoreError::Io {
                detail: e.to_string(),
            })
    }

    /// Prefix scan returning only keys.
    fn scan_keys(&self, prefix: &str) -> Result<Vec<String>> {
        let _guard = self.lock.read();
        let entries = self
            .db
            .scan(prefix)
            .map_err(|e| ThreadStoreError::Io {
                detail: e.to_string(),
            })?;
        Ok(entries.into_iter().map(|(k, _)| k).collect())
    }

    /// Update thread's `updated_at` and increment `message_count`.
    fn touch_thread(&self, thread_id: u128) -> Result<()> {
        if let Some(mut meta) = self.get_thread(thread_id)? {
            let old_updated_us = chrono::DateTime::parse_from_rfc3339(&meta.updated_at)
                .map(|dt| dt.timestamp_micros() as u64)
                .unwrap_or(0);

            meta.message_count += 1;
            meta.updated_at = chrono::Utc::now().to_rfc3339();

            let new_updated_us = chrono::DateTime::parse_from_rfc3339(&meta.updated_at)
                .map(|dt| dt.timestamp_micros() as u64)
                .unwrap_or(0);

            let key = keys::thread_key(thread_id);
            let bytes =
                serde_json::to_vec(&meta).map_err(|e| ThreadStoreError::Serialization {
                    detail: e.to_string(),
                })?;
            self.put(&key, &bytes)?;

            // Update secondary index
            let old_idx = keys::idx_agent_thread(&meta.agent_id, old_updated_us, thread_id);
            let _ = self.delete(&old_idx);
            let new_idx = keys::idx_agent_thread(&meta.agent_id, new_updated_us, thread_id);
            self.put(&new_idx, &[])?;
        }
        Ok(())
    }

    /// Atomically increment a u64 counter stored at `key`.
    fn increment_counter(&self, key: &str) -> Result<()> {
        let current = self.read_counter(key)?;
        let new_val = current + 1;
        self.put(key, &new_val.to_le_bytes())
    }

    /// Atomically decrement a u64 counter stored at `key`.
    fn decrement_counter(&self, key: &str) -> Result<()> {
        self.decrement_counter_by(key, 1)
    }

    /// Decrement a counter by N.
    fn decrement_counter_by(&self, key: &str, n: u64) -> Result<()> {
        let current = self.read_counter(key)?;
        let new_val = current.saturating_sub(n);
        self.put(key, &new_val.to_le_bytes())
    }

    /// Read a u64 counter.
    fn read_counter(&self, key: &str) -> Result<u64> {
        match self.get(key)? {
            Some(bytes) if bytes.len() == 8 => {
                Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
            }
            _ => Ok(0),
        }
    }
}

impl std::fmt::Debug for ThreadStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadStore").finish()
    }
}

impl Drop for ThreadStore {
    fn drop(&mut self) {
        info!("Shutting down ThreadStore — flushing WAL and checkpointing");
        let _guard = self.lock.write();
        if let Err(e) = self.db.checkpoint() {
            warn!(error = %e, "Failed to checkpoint ThreadStore WAL on shutdown");
        }
        let _ = self.db.gc(); // optional gc
        if let Err(e) = self.db.fsync() {
            warn!(error = %e, "Failed to fsync ThreadStore WAL on shutdown");
        }
        info!("ThreadStore shutdown complete");
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Extract thread_id from an agent index key.
/// Key format: `idx/agent/{agent_id}/{ts:020}/{thread_id:032x}`
fn extract_thread_id_from_idx_key(key: &str) -> Option<u128> {
    let parts: Vec<&str> = key.rsplitn(2, '/').collect();
    if parts.len() < 2 {
        return None;
    }
    u128::from_str_radix(parts[0], 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, ThreadStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ThreadStore::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn create_and_get_thread() {
        let (_dir, store) = temp_store();
        let meta = store.create_thread("agent-1", "Hello World", None).unwrap();
        assert_eq!(meta.agent_id, "agent-1");
        assert_eq!(meta.title, "Hello World");
        assert_eq!(meta.message_count, 0);

        let loaded = store.get_thread(meta.id).unwrap().unwrap();
        assert_eq!(loaded.id, meta.id);
        assert_eq!(loaded.title, "Hello World");
    }

    #[test]
    fn append_and_load_messages() {
        let (_dir, store) = temp_store();
        let thread = store.create_thread("agent-1", "Test", None).unwrap();

        store.append_message(thread.id, MessageRole::User, "Hello", None).unwrap();
        store.append_message(thread.id, MessageRole::Assistant, "Hi there!", None).unwrap();

        let msgs = store.get_thread_messages(thread.id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, MessageRole::User);
        assert_eq!(msgs[0].content, "Hello");
        assert_eq!(msgs[1].role, MessageRole::Assistant);
        assert_eq!(msgs[1].content, "Hi there!");

        // Thread metadata should be updated
        let meta = store.get_thread(thread.id).unwrap().unwrap();
        assert_eq!(meta.message_count, 2);
    }

    #[test]
    fn list_threads_by_agent() {
        let (_dir, store) = temp_store();
        store.create_thread("agent-a", "Thread A1", None).unwrap();
        store.create_thread("agent-a", "Thread A2", None).unwrap();
        store.create_thread("agent-b", "Thread B1", None).unwrap();

        let query = ThreadQuery {
            agent_id: Some("agent-a".into()),
            ..Default::default()
        };
        let results = store.list_threads(&query).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|s| s.agent_id == "agent-a"));

        // All threads
        let all = store.list_threads(&ThreadQuery::default()).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn delete_thread_cascades() {
        let (_dir, store) = temp_store();
        let thread = store.create_thread("agent-1", "To Delete", None).unwrap();
        store.append_message(thread.id, MessageRole::User, "msg1", None).unwrap();
        store.append_message(thread.id, MessageRole::Assistant, "msg2", None).unwrap();

        let (tc, mc) = store.stats().unwrap();
        assert_eq!(tc, 1);
        assert_eq!(mc, 2);

        let deleted = store.delete_thread(thread.id).unwrap();
        assert!(deleted);

        // Thread gone
        assert!(store.get_thread(thread.id).unwrap().is_none());

        // Messages gone
        let msgs = store.get_thread_messages(thread.id).unwrap();
        assert!(msgs.is_empty());

        // Counters decremented
        let (tc, mc) = store.stats().unwrap();
        assert_eq!(tc, 0);
        assert_eq!(mc, 0);
    }

    #[test]
    fn update_thread_metadata() {
        let (_dir, store) = temp_store();
        let thread = store.create_thread("agent-1", "Original", None).unwrap();

        let updated = store.update_thread(thread.id, |m| {
            m.title = "Renamed".to_string();
            m.pinned = true;
            m.tags = vec!["important".into()];
        }).unwrap();

        assert_eq!(updated.title, "Renamed");
        assert!(updated.pinned);
        assert_eq!(updated.tags, vec!["important"]);
    }

    #[test]
    fn namespaced_isolation() {
        let (_dir, store) = temp_store();
        let t1 = store.create_thread("agent-1", "Thread 1", None).unwrap();
        let t2 = store.create_thread("agent-1", "Thread 2", None).unwrap();

        store.append_message(t1.id, MessageRole::User, "T1 msg", None).unwrap();
        store.append_message(t2.id, MessageRole::User, "T2 msg", None).unwrap();
        store.append_message(t2.id, MessageRole::User, "T2 msg2", None).unwrap();

        // Each thread's namespace only contains its own messages
        let m1 = store.get_thread_messages(t1.id).unwrap();
        let m2 = store.get_thread_messages(t2.id).unwrap();
        assert_eq!(m1.len(), 1);
        assert_eq!(m2.len(), 2);
        assert_eq!(m1[0].content, "T1 msg");
        assert_eq!(m2[0].content, "T2 msg");
    }

    #[test]
    fn attachment_roundtrip() {
        let (_dir, store) = temp_store();
        let thread = store.create_thread("agent-1", "Attach Test", None).unwrap();

        let data = b"hello binary world";
        let msg = store
            .append_message_with_attachment(
                thread.id,
                MessageRole::User,
                "See attachment",
                None,
                data,
            )
            .unwrap();

        assert!(msg.has_attachment);
        let loaded = store.get_attachment(msg.id).unwrap().unwrap();
        assert_eq!(loaded, data);
    }

    #[test]
    fn pinned_threads_sort_first() {
        let (_dir, store) = temp_store();
        store.create_thread("a", "Unpinned", None).unwrap();
        let t2 = store.create_thread("a", "Pinned", None).unwrap();
        store.create_thread("a", "Also Unpinned", None).unwrap();

        store.update_thread(t2.id, |m| m.pinned = true).unwrap();

        let list = store.list_threads(&ThreadQuery::default()).unwrap();
        assert!(list[0].pinned, "pinned thread should be first");
        assert!(!list[1].pinned);
    }

    #[test]
    fn archived_threads_hidden_by_default() {
        let (_dir, store) = temp_store();
        let t1 = store.create_thread("a", "Visible", None).unwrap();
        let t2 = store.create_thread("a", "Archived", None).unwrap();

        store.update_thread(t2.id, |m| m.archived = true).unwrap();

        // Default query hides archived
        let visible = store.list_threads(&ThreadQuery::default()).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, t1.id);

        // Explicit include_archived shows all
        let all = store
            .list_threads(&ThreadQuery {
                include_archived: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn checkpoint_and_gc() {
        let (_dir, store) = temp_store();
        store.create_thread("a", "T", None).unwrap();
        let seq = store.checkpoint_and_gc().unwrap();
        assert!(seq > 0);
    }
}
