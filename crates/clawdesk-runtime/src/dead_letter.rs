//! Dead letter queue — permanently failed runs.
//!
//! ## Storage
//!
//! ```text
//! runtime:dlq:{run_id}  →  DeadLetterEntry (JSON)
//! ```
//!
//! The DLQ is a simple append-only queue. Entries can be listed for
//! operator inspection, manually retried, or purged.

use crate::types::{DeadLetterEntry, RunId, RuntimeError};
use clawdesk_sochdb::SochStore;
use clawdesk_types::error::StorageError;
use std::sync::Arc;
use tracing::{debug, info};

/// Dead letter queue for permanently failed workflow runs.
pub struct DeadLetterQueue {
    store: Arc<SochStore>,
}

impl DeadLetterQueue {
    pub fn new(store: Arc<SochStore>) -> Self {
        Self { store }
    }

    /// Add a failed run to the dead letter queue.
    pub async fn enqueue(&self, entry: &DeadLetterEntry) -> Result<(), RuntimeError> {
        let key = Self::dlq_key(&entry.run_id);
        let bytes =
            serde_json::to_vec(entry).map_err(|e| StorageError::SerializationFailed {
                detail: e.to_string(),
            })?;

        self.store
            .db()
            .put(key.as_bytes(), &bytes)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        info!(run_id = %entry.run_id, attempts = entry.attempts, "run moved to DLQ");
        Ok(())
    }

    /// Load a specific DLQ entry.
    pub async fn get(&self, run_id: &RunId) -> Result<Option<DeadLetterEntry>, RuntimeError> {
        let key = Self::dlq_key(run_id);
        match self.store.db().get(key.as_bytes()) {
            Ok(Some(bytes)) => {
                let entry = serde_json::from_slice(&bytes).map_err(|e| {
                    RuntimeError::CheckpointCorrupted {
                        detail: format!("DLQ deserialization: {e}"),
                    }
                })?;
                Ok(Some(entry))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(StorageError::OpenFailed {
                detail: e.to_string(),
            }
            .into()),
        }
    }

    /// List all entries in the dead letter queue.
    pub async fn list(&self) -> Result<Vec<DeadLetterEntry>, RuntimeError> {
        let prefix = "runtime:dlq:";
        let entries = self
            .store
            .db()
            .scan(prefix.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        let mut result = Vec::new();
        for (_key, val) in &entries {
            if let Ok(entry) = serde_json::from_slice::<DeadLetterEntry>(val) {
                result.push(entry);
            }
        }

        Ok(result)
    }

    /// Remove an entry from the DLQ (e.g., after successful retry or purge).
    pub async fn remove(&self, run_id: &RunId) -> Result<(), RuntimeError> {
        let key = Self::dlq_key(run_id);
        let _ = self.store.db().delete(key.as_bytes());
        debug!(%run_id, "DLQ entry removed");
        Ok(())
    }

    /// Purge all entries from the DLQ.
    pub async fn purge(&self) -> Result<usize, RuntimeError> {
        let prefix = "runtime:dlq:";
        let entries = self
            .store
            .db()
            .scan(prefix.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        let count = entries.len();
        for (key, _) in &entries {
            let _ = self.store.db().delete(key);
        }

        info!(count, "DLQ purged");
        Ok(count)
    }

    /// Count of entries in the DLQ.
    pub async fn count(&self) -> Result<usize, RuntimeError> {
        let prefix = "runtime:dlq:";
        let entries = self
            .store
            .db()
            .scan(prefix.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;
        Ok(entries.len())
    }

    fn dlq_key(run_id: &RunId) -> String {
        format!("runtime:dlq:{}", run_id)
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RetryPolicy, WorkflowType};
    use chrono::Utc;

    fn test_store() -> Arc<SochStore> {
        Arc::new(SochStore::open_in_memory().expect("in-memory store"))
    }

    fn test_entry() -> DeadLetterEntry {
        DeadLetterEntry {
            run_id: RunId::new(),
            workflow_type: WorkflowType::A2ATask {
                task_id: "task-1".into(),
            },
            error: "provider timeout".into(),
            attempts: 3,
            first_attempt_at: Utc::now(),
            last_attempt_at: Utc::now(),
            last_checkpoint: None,
            total_input_tokens: 5000,
            total_output_tokens: 2000,
        }
    }

    #[tokio::test]
    async fn enqueue_and_get() {
        let store = test_store();
        let dlq = DeadLetterQueue::new(store);

        let entry = test_entry();
        let run_id = entry.run_id.clone();

        dlq.enqueue(&entry).await.unwrap();
        let loaded = dlq.get(&run_id).await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().attempts, 3);
    }

    #[tokio::test]
    async fn list_entries() {
        let store = test_store();
        let dlq = DeadLetterQueue::new(store);

        for _ in 0..5 {
            dlq.enqueue(&test_entry()).await.unwrap();
        }

        let entries = dlq.list().await.unwrap();
        assert_eq!(entries.len(), 5);
    }

    #[tokio::test]
    async fn remove_entry() {
        let store = test_store();
        let dlq = DeadLetterQueue::new(store);

        let entry = test_entry();
        let run_id = entry.run_id.clone();

        dlq.enqueue(&entry).await.unwrap();
        dlq.remove(&run_id).await.unwrap();
        assert!(dlq.get(&run_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn purge_all() {
        let store = test_store();
        let dlq = DeadLetterQueue::new(store);

        for _ in 0..3 {
            dlq.enqueue(&test_entry()).await.unwrap();
        }

        let purged = dlq.purge().await.unwrap();
        assert_eq!(purged, 3);
        assert_eq!(dlq.count().await.unwrap(), 0);
    }
}
