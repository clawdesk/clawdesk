//! Checkpoint store — latest-wins snapshot persistence.
//!
//! Each run has at most one checkpoint at any time (overwritten on each round).
//! The checkpoint captures exactly enough state to resume execution without
//! re-executing completed rounds.
//!
//! ## Storage
//!
//! ```text
//! runtime:runs:{run_id}:checkpoint  →  Checkpoint (JSON)
//! ```

use crate::types::{Checkpoint, RunId, RuntimeError, WorkflowRun};
use clawdesk_sochdb::SochStore;
use clawdesk_types::error::StorageError;
use std::sync::Arc;
use tracing::debug;

/// Persistent checkpoint store backed by SochDB.
pub struct CheckpointStore {
    store: Arc<SochStore>,
}

impl CheckpointStore {
    pub fn new(store: Arc<SochStore>) -> Self {
        Self { store }
    }

    // ── Checkpoint operations ────────────────────────────────

    /// Save a checkpoint for a run (overwrites any previous checkpoint).
    pub async fn save_checkpoint(
        &self,
        run_id: &RunId,
        checkpoint: &Checkpoint,
    ) -> Result<(), RuntimeError> {
        let key = Self::checkpoint_key(run_id);
        let bytes =
            serde_json::to_vec(checkpoint).map_err(|e| StorageError::SerializationFailed {
                detail: e.to_string(),
            })?;

        self.store
            .db()
            .put(key.as_bytes(), &bytes)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        debug!(%run_id, "checkpoint saved");
        Ok(())
    }

    /// Load the latest checkpoint for a run.
    pub async fn load_checkpoint(
        &self,
        run_id: &RunId,
    ) -> Result<Option<Checkpoint>, RuntimeError> {
        let key = Self::checkpoint_key(run_id);
        match self.store.db().get(key.as_bytes()) {
            Ok(Some(bytes)) => {
                let cp = serde_json::from_slice(&bytes).map_err(|e| {
                    RuntimeError::CheckpointCorrupted {
                        detail: e.to_string(),
                    }
                })?;
                Ok(Some(cp))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(StorageError::OpenFailed {
                detail: e.to_string(),
            }
            .into()),
        }
    }

    /// Delete the checkpoint for a run (cleanup).
    pub async fn delete_checkpoint(&self, run_id: &RunId) -> Result<(), RuntimeError> {
        let key = Self::checkpoint_key(run_id);
        let _ = self.store.db().delete(key.as_bytes());
        Ok(())
    }

    // ── Run metadata operations ──────────────────────────────

    /// Persist a WorkflowRun to SochDB.
    pub async fn save_run(&self, run: &WorkflowRun) -> Result<(), RuntimeError> {
        let key = Self::run_key(&run.id);
        let bytes =
            serde_json::to_vec(run).map_err(|e| StorageError::SerializationFailed {
                detail: e.to_string(),
            })?;

        self.store
            .db()
            .put(key.as_bytes(), &bytes)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        // Update secondary index: state → run_id.
        let idx_key = format!("runtime:index:state:{}:{}", run.state.label(), run.id);
        self.store
            .db()
            .put(idx_key.as_bytes(), run.id.0.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        // Update worker index if running.
        if let Some(ref worker_id) = run.worker_id {
            let widx = format!("runtime:index:worker:{}:{}", worker_id, run.id);
            self.store
                .db()
                .put(widx.as_bytes(), run.id.0.as_bytes())
                .map_err(|e| StorageError::OpenFailed {
                    detail: e.to_string(),
                })?;
        }

        Ok(())
    }

    /// Load a WorkflowRun by ID.
    pub async fn load_run(&self, run_id: &RunId) -> Result<Option<WorkflowRun>, RuntimeError> {
        let key = Self::run_key(run_id);
        match self.store.db().get(key.as_bytes()) {
            Ok(Some(bytes)) => {
                let run = serde_json::from_slice(&bytes).map_err(|e| {
                    RuntimeError::CheckpointCorrupted {
                        detail: format!("WorkflowRun deserialization: {e}"),
                    }
                })?;
                Ok(Some(run))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(StorageError::OpenFailed {
                detail: e.to_string(),
            }
            .into()),
        }
    }

    /// Load all runs in a given state.
    pub async fn load_runs_by_state(
        &self,
        state_label: &str,
    ) -> Result<Vec<RunId>, RuntimeError> {
        let prefix = format!("runtime:index:state:{}:", state_label);
        let entries = self
            .store
            .db()
            .scan(prefix.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        let run_ids = entries
            .iter()
            .filter_map(|(_, v)| {
                String::from_utf8(v.clone())
                    .ok()
                    .map(|s| RunId(s))
            })
            .collect();

        Ok(run_ids)
    }

    /// Load all runs owned by a specific worker.
    pub async fn load_runs_by_worker(
        &self,
        worker_id: &str,
    ) -> Result<Vec<RunId>, RuntimeError> {
        let prefix = format!("runtime:index:worker:{}:", worker_id);
        let entries = self
            .store
            .db()
            .scan(prefix.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        let run_ids = entries
            .iter()
            .filter_map(|(_, v)| {
                String::from_utf8(v.clone())
                    .ok()
                    .map(|s| RunId(s))
            })
            .collect();

        Ok(run_ids)
    }

    // ── Key builders ─────────────────────────────────────────

    fn checkpoint_key(run_id: &RunId) -> String {
        format!("runtime:runs:{}:checkpoint", run_id)
    }

    fn run_key(run_id: &RunId) -> String {
        format!("runtime:runs:{}", run_id)
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GuardSnapshot, RetryPolicy, RunState, WorkflowType};
    use chrono::Utc;

    fn test_store() -> Arc<SochStore> {
        Arc::new(SochStore::open_in_memory().expect("in-memory store"))
    }

    #[tokio::test]
    async fn save_and_load_checkpoint() {
        let store = test_store();
        let cs = CheckpointStore::new(store);
        let run_id = RunId::new();

        let cp = Checkpoint::AgentLoop {
            round: 5,
            messages: vec![],
            total_input_tokens: 1000,
            total_output_tokens: 500,
            guard_state: GuardSnapshot {
                estimated_tokens: 1500,
                compaction_count: 1,
                circuit_breaker_failures: 0,
            },
        };

        cs.save_checkpoint(&run_id, &cp).await.unwrap();
        let loaded = cs.load_checkpoint(&run_id).await.unwrap();
        assert!(loaded.is_some());

        if let Some(Checkpoint::AgentLoop { round, .. }) = loaded {
            assert_eq!(round, 5);
        } else {
            panic!("wrong checkpoint type");
        }
    }

    #[tokio::test]
    async fn checkpoint_overwrite() {
        let store = test_store();
        let cs = CheckpointStore::new(store);
        let run_id = RunId::new();

        for round in 0..5 {
            let cp = Checkpoint::AgentLoop {
                round,
                messages: vec![],
                total_input_tokens: round as u64 * 100,
                total_output_tokens: round as u64 * 50,
                guard_state: GuardSnapshot {
                    estimated_tokens: 0,
                    compaction_count: 0,
                    circuit_breaker_failures: 0,
                },
            };
            cs.save_checkpoint(&run_id, &cp).await.unwrap();
        }

        // Only the latest checkpoint should be loaded.
        if let Some(Checkpoint::AgentLoop { round, .. }) =
            cs.load_checkpoint(&run_id).await.unwrap()
        {
            assert_eq!(round, 4);
        }
    }

    #[tokio::test]
    async fn save_and_load_run() {
        let store = test_store();
        let cs = CheckpointStore::new(store);

        let run = WorkflowRun::new(
            WorkflowType::A2ATask {
                task_id: "task-123".into(),
            },
            RetryPolicy::default_agent(),
        );
        let run_id = run.id.clone();

        cs.save_run(&run).await.unwrap();
        let loaded = cs.load_run(&run_id).await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id, run_id);
    }

    #[tokio::test]
    async fn load_runs_by_state() {
        let store = test_store();
        let cs = CheckpointStore::new(store);

        // Create 3 pending runs.
        for _ in 0..3 {
            let run = WorkflowRun::new(
                WorkflowType::A2ATask {
                    task_id: "t".into(),
                },
                RetryPolicy::none(),
            );
            cs.save_run(&run).await.unwrap();
        }

        let pending = cs.load_runs_by_state("pending").await.unwrap();
        assert_eq!(pending.len(), 3);
    }

    #[tokio::test]
    async fn missing_checkpoint_returns_none() {
        let store = test_store();
        let cs = CheckpointStore::new(store);
        let result = cs.load_checkpoint(&RunId::new()).await.unwrap();
        assert!(result.is_none());
    }
}
