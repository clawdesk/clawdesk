//! Transactional connection wrapper — buffered writes over `ConnectionTrait`.
//!
//! ## SochDB Transactions (GAP-02)
//!
//! `ConnectionTrait` is a flat put/get/delete/scan interface with no transaction
//! support. ClawDesk needs multi-key writes for:
//! - **Skill orchestration**: skill + selection record + metrics.
//! - **Chat replay**: turn data + index entry + counter update.
//! - **Session coordination**: group metadata + session entries.
//! - **Compaction**: moving data between tiers.
//!
//! `TransactionalConn` wraps any `ConnectionTrait` implementor and provides:
//! - **Read-your-writes**: reads check the write buffer before falling through.
//! - **Best-effort batch commit**: buffered writes applied sequentially.
//! - **Rollback**: discard all buffered writes.
//! - **Drop safety**: uncommitted transaction is auto-rolled-back on drop.
//!
//! ## Recommended: native MVCC transactions
//!
//! For `SochStore`-backed usage, prefer [`SochStore::apply_atomic_batch`] or
//! [`SochStore::with_transaction`] which use the native `EmbeddedConnection`
//! `begin()`/`commit()`/`abort()` cycle for true all-or-nothing semantics.
//! The `TransactionalConn` buffer-and-flush approach remains available as
//! a fallback for generic `ConnectionTrait` backends.

use sochdb::ConnectionTrait;
use std::collections::BTreeMap;
use tracing::{debug, warn};

/// Operation buffered in a pending transaction.
#[derive(Debug, Clone)]
enum PendingOp {
    Put(Vec<u8>),
    Delete,
}

/// Transaction state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnState {
    Active,
    Committed,
    RolledBack,
}

/// Transactional wrapper over any `ConnectionTrait`.
///
/// Buffers writes in memory and applies them sequentially on commit.
/// Reads check the buffer first (read-your-writes), then fall through
/// to the underlying connection for keys not in the buffer.
///
/// **Note:** Commit is **not atomic**. If the process crashes mid-commit,
/// a subset of writes will have been applied. See [`commit()`](Self::commit)
/// for details.
///
/// # Example
///
/// ```rust,ignore
/// use clawdesk_sochdb::transaction::TransactionalConn;
///
/// let conn = SochConn::new(store.clone());
/// let mut txn = TransactionalConn::begin(conn);
/// txn.put(b"key1", b"val1")?;
/// txn.put(b"key2", b"val2")?;
/// txn.commit()?;  // best-effort batch — NOT atomic
/// ```
pub struct TransactionalConn<C: ConnectionTrait> {
    inner: C,
    buffer: BTreeMap<Vec<u8>, PendingOp>,
    state: TxnState,
    label: String,
}

impl<C: ConnectionTrait> TransactionalConn<C> {
    /// Begin a new transaction against the given connection.
    pub fn begin(conn: C) -> Self {
        Self::begin_with_label(conn, "unnamed")
    }

    /// Begin a new transaction with a descriptive label (for logging).
    pub fn begin_with_label(conn: C, label: impl Into<String>) -> Self {
        let label = label.into();
        debug!(label = %label, "transaction started");
        Self {
            inner: conn,
            buffer: BTreeMap::new(),
            state: TxnState::Active,
            label,
        }
    }

    /// Buffer a put operation.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> sochdb::error::Result<()> {
        assert_eq!(
            self.state,
            TxnState::Active,
            "cannot write to a {} transaction",
            match self.state {
                TxnState::Committed => "committed",
                TxnState::RolledBack => "rolled-back",
                TxnState::Active => unreachable!(),
            }
        );
        self.buffer.insert(key.to_vec(), PendingOp::Put(value.to_vec()));
        Ok(())
    }

    /// Buffer a delete operation.
    pub fn delete(&mut self, key: &[u8]) -> sochdb::error::Result<()> {
        assert_eq!(
            self.state,
            TxnState::Active,
            "cannot write to a finished transaction"
        );
        self.buffer.insert(key.to_vec(), PendingOp::Delete);
        Ok(())
    }

    /// Read a key — checks write buffer first, then underlying connection.
    pub fn get(&self, key: &[u8]) -> sochdb::error::Result<Option<Vec<u8>>> {
        // Check local buffer first (read-your-writes)
        if let Some(op) = self.buffer.get(key) {
            return match op {
                PendingOp::Put(v) => Ok(Some(v.clone())),
                PendingOp::Delete => Ok(None), // deleted in this txn
            };
        }
        // Fall through to underlying connection
        self.inner.get(key)
    }

    /// Scan by prefix — merges buffer entries with underlying scan results.
    pub fn scan(&self, prefix: &[u8]) -> sochdb::error::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        // Get underlying results
        let base = self.inner.scan(prefix)?;
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = base.into_iter().collect();

        // Apply buffer on top
        for (key, op) in &self.buffer {
            if key.starts_with(prefix) {
                match op {
                    PendingOp::Put(v) => {
                        merged.insert(key.clone(), v.clone());
                    }
                    PendingOp::Delete => {
                        merged.remove(key);
                    }
                }
            }
        }

        Ok(merged.into_iter().collect())
    }

    /// Commit the transaction — apply all buffered writes sequentially.
    ///
    /// Operations are applied one-by-one via individual `put`/`delete` calls
    /// on the `ConnectionTrait` backend. While this traverses the buffer
    /// sequentially, callers using `SochConn` should prefer
    /// [`commit_atomic()`](Self::commit_atomic) for true MVCC atomicity.
    ///
    /// For generic `ConnectionTrait` backends without native transaction
    /// support, this remains the fallback commit path.
    pub fn commit(mut self) -> sochdb::error::Result<CommitResult> {
        assert_eq!(self.state, TxnState::Active, "cannot commit a finished transaction");

        let puts = self.buffer.iter().filter(|(_, op)| matches!(op, PendingOp::Put(_))).count();
        let deletes = self.buffer.iter().filter(|(_, op)| matches!(op, PendingOp::Delete)).count();

        for (key, op) in &self.buffer {
            match op {
                PendingOp::Put(value) => {
                    self.inner.put(key, value)?;
                }
                PendingOp::Delete => {
                    self.inner.delete(key)?;
                }
            }
        }

        self.state = TxnState::Committed;
        debug!(
            label = %self.label,
            puts,
            deletes,
            "transaction committed"
        );

        Ok(CommitResult { puts, deletes })
    }

    /// Rollback the transaction — discard all buffered writes.
    pub fn rollback(mut self) -> RollbackResult {
        let discarded = self.buffer.len();
        self.buffer.clear();
        self.state = TxnState::RolledBack;
        debug!(label = %self.label, discarded, "transaction rolled back");
        RollbackResult { discarded }
    }

    /// Number of buffered operations.
    pub fn pending_count(&self) -> usize {
        self.buffer.len()
    }

    /// Current transaction state.
    pub fn state(&self) -> TxnState {
        self.state
    }

    /// Get a reference to the underlying connection.
    pub fn inner(&self) -> &C {
        &self.inner
    }

    /// Transaction label.
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl<C: ConnectionTrait> Drop for TransactionalConn<C> {
    fn drop(&mut self) {
        if self.state == TxnState::Active && !self.buffer.is_empty() {
            warn!(
                label = %self.label,
                pending = self.buffer.len(),
                "transaction dropped without commit — discarding {} buffered operations",
                self.buffer.len()
            );
            self.buffer.clear();
            self.state = TxnState::RolledBack;
        }
    }
}

/// Result of a successful commit.
#[derive(Debug, Clone)]
pub struct CommitResult {
    pub puts: usize,
    pub deletes: usize,
}

/// Result of a rollback.
#[derive(Debug, Clone)]
pub struct RollbackResult {
    pub discarded: usize,
}

/// Execute a closure within a transaction.
///
/// If the closure returns `Ok`, the transaction is committed.
/// If it returns `Err`, the transaction is rolled back.
///
/// **Note:** For `SochConn`-backed transactions, prefer
/// [`SochStore::with_transaction`] or [`SochStore::apply_atomic_batch`]
/// for true MVCC atomicity.
///
/// # Example
///
/// ```rust,ignore
/// let result = with_transaction(conn, "my-op", |txn| {
///     txn.put(b"key", b"value")?;
///     Ok(42)
/// })?;
/// ```
pub fn with_transaction<C, F, T>(
    conn: C,
    label: &str,
    f: F,
) -> sochdb::error::Result<T>
where
    C: ConnectionTrait,
    F: FnOnce(&mut TransactionalConn<C>) -> sochdb::error::Result<T>,
{
    let mut txn = TransactionalConn::begin_with_label(conn, label);
    match f(&mut txn) {
        Ok(result) => {
            txn.commit()?;
            Ok(result)
        }
        Err(e) => {
            txn.rollback();
            Err(e)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory ConnectionTrait for testing.
    struct MemConn(Mutex<HashMap<Vec<u8>, Vec<u8>>>);

    impl MemConn {
        fn new() -> Self {
            Self(Mutex::new(HashMap::new()))
        }
    }

    impl ConnectionTrait for MemConn {
        fn put(&self, key: &[u8], value: &[u8]) -> sochdb::error::Result<()> {
            self.0.lock().unwrap().insert(key.to_vec(), value.to_vec());
            Ok(())
        }

        fn get(&self, key: &[u8]) -> sochdb::error::Result<Option<Vec<u8>>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }

        fn delete(&self, key: &[u8]) -> sochdb::error::Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }

        fn scan(&self, prefix: &[u8]) -> sochdb::error::Result<Vec<(Vec<u8>, Vec<u8>)>> {
            let guard = self.0.lock().unwrap();
            Ok(guard
                .iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect())
        }
    }

    #[test]
    fn read_your_writes() {
        let conn = MemConn::new();
        let mut txn = TransactionalConn::begin(conn);

        txn.put(b"key1", b"val1").unwrap();
        assert_eq!(txn.get(b"key1").unwrap(), Some(b"val1".to_vec()));
        // Not yet committed, underlying conn should be empty
        assert_eq!(txn.inner().get(b"key1").unwrap(), None);
    }

    #[test]
    fn commit_applies_all_writes() {
        let conn = MemConn::new();
        let mut txn = TransactionalConn::begin(conn);

        txn.put(b"k1", b"v1").unwrap();
        txn.put(b"k2", b"v2").unwrap();
        txn.put(b"k3", b"v3").unwrap();

        let result = txn.commit().unwrap();
        assert_eq!(result.puts, 3);
        assert_eq!(result.deletes, 0);
    }

    #[test]
    fn rollback_discards_writes() {
        let conn = MemConn::new();
        let mut txn = TransactionalConn::begin(conn);

        txn.put(b"key1", b"val1").unwrap();
        txn.put(b"key2", b"val2").unwrap();

        let result = txn.rollback();
        assert_eq!(result.discarded, 2);
    }

    #[test]
    fn delete_in_transaction() {
        let conn = MemConn::new();
        conn.put(b"existing", b"value").unwrap();

        let mut txn = TransactionalConn::begin(conn);

        // Key exists in underlying conn
        assert_eq!(txn.get(b"existing").unwrap(), Some(b"value".to_vec()));

        // Delete in transaction
        txn.delete(b"existing").unwrap();

        // Now reads as None (buffered delete)
        assert_eq!(txn.get(b"existing").unwrap(), None);

        let result = txn.commit().unwrap();
        assert_eq!(result.deletes, 1);
    }

    #[test]
    fn scan_merges_buffer() {
        let conn = MemConn::new();
        conn.put(b"ns/a", b"1").unwrap();
        conn.put(b"ns/b", b"2").unwrap();
        conn.put(b"other/x", b"9").unwrap();

        let mut txn = TransactionalConn::begin(conn);
        txn.put(b"ns/c", b"3").unwrap(); // add new
        txn.delete(b"ns/a").unwrap(); // remove existing

        let results = txn.scan(b"ns/").unwrap();
        let keys: Vec<&[u8]> = results.iter().map(|(k, _)| k.as_slice()).collect();
        assert!(keys.contains(&b"ns/b".as_slice()));
        assert!(keys.contains(&b"ns/c".as_slice()));
        assert!(!keys.contains(&b"ns/a".as_slice())); // deleted
        assert!(!keys.contains(&b"other/x".as_slice())); // wrong prefix
    }

    #[test]
    fn with_transaction_commits_on_success() {
        let conn = MemConn::new();
        let result = with_transaction(conn, "test", |txn| {
            txn.put(b"key", b"value")?;
            Ok(42)
        })
        .unwrap();

        assert_eq!(result, 42);
    }

    #[test]
    fn pending_count_tracks_operations() {
        let conn = MemConn::new();
        let mut txn = TransactionalConn::begin(conn);

        assert_eq!(txn.pending_count(), 0);
        txn.put(b"k1", b"v1").unwrap();
        assert_eq!(txn.pending_count(), 1);
        txn.put(b"k2", b"v2").unwrap();
        assert_eq!(txn.pending_count(), 2);
        txn.delete(b"k1").unwrap(); // overwrites the put for k1
        assert_eq!(txn.pending_count(), 2); // same key, still 2 entries
    }
}
