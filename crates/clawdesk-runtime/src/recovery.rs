//! Recovery manager — detects orphaned runs and reassigns or DLQs them.
//!
//! ## Design
//!
//! On startup (or periodically), `RecoveryManager::scan_and_recover()`:
//!
//! 1. Iterates all expired leases via `LeaseManager::scan_expired()`.
//! 2. For each orphaned run:
//!    - If retry policy allows → re-queue (set to Pending).
//!    - If retries exhausted → move to DLQ.
//!
//! This replaces the human operator having to manually restart crashed runs.

use crate::checkpoint::CheckpointStore;
use crate::dead_letter::DeadLetterQueue;
use crate::lease::LeaseManager;
use crate::types::*;
use chrono::Utc;
use std::sync::Arc;
use tracing::{info, warn};

/// Outcome of a single run recovery attempt.
#[derive(Debug)]
pub enum RecoveryAction {
    /// Run was re-queued for retry.
    Requeued { run_id: RunId, attempt: u32 },
    /// Run was moved to the dead letter queue.
    DeadLettered { run_id: RunId, attempts: u32 },
    /// Run was already in a terminal state — no action needed.
    AlreadyTerminal { run_id: RunId },
    /// Run metadata not found — lease cleaned up.
    Orphaned { run_id: RunId },
}

/// Scans for orphaned runs and attempts recovery.
pub struct RecoveryManager {
    checkpoint_store: Arc<CheckpointStore>,
    lease_manager: Arc<LeaseManager>,
    dead_letter_queue: Arc<DeadLetterQueue>,
}

impl RecoveryManager {
    pub fn new(
        checkpoint_store: Arc<CheckpointStore>,
        lease_manager: Arc<LeaseManager>,
        dead_letter_queue: Arc<DeadLetterQueue>,
    ) -> Self {
        Self {
            checkpoint_store,
            lease_manager,
            dead_letter_queue,
        }
    }

    /// Scan for all orphaned runs (expired leases) and recover them.
    ///
    /// Returns a list of recovery actions taken.
    pub async fn scan_and_recover(&self) -> Result<Vec<RecoveryAction>, RuntimeError> {
        let expired = self.lease_manager.scan_expired().await?;
        let mut actions = Vec::with_capacity(expired.len());

        if expired.is_empty() {
            return Ok(actions);
        }

        info!(count = expired.len(), "found orphaned runs with expired leases");

        for (run_id, lease) in &expired {
            let action = self.recover_run(run_id, lease).await?;
            actions.push(action);
        }

        Ok(actions)
    }

    /// Attempt to recover a single orphaned run.
    async fn recover_run(
        &self,
        run_id: &RunId,
        lease: &Lease,
    ) -> Result<RecoveryAction, RuntimeError> {
        // Clean up the expired lease.
        let _ = self
            .lease_manager
            .release(run_id, &lease.worker_id, lease.fence_token)
            .await;

        // Load the run metadata.
        let run = match self.checkpoint_store.load_run(run_id).await? {
            Some(run) => run,
            None => {
                warn!(%run_id, "orphaned lease with no run metadata — cleaned up");
                return Ok(RecoveryAction::Orphaned {
                    run_id: run_id.clone(),
                });
            }
        };

        // Already terminal — no recovery needed.
        if run.is_terminal() {
            return Ok(RecoveryAction::AlreadyTerminal {
                run_id: run_id.clone(),
            });
        }

        // Check retry policy.
        let error_class = ErrorClass::LeaseExpired;
        if run.attempt < run.retry_policy.max_attempts
            && run.retry_policy.should_retry(&error_class)
        {
            // Re-queue for retry.
            let mut updated = run.clone();
            updated.state = RunState::Pending;
            updated.worker_id = None;
            updated.updated_at = Utc::now();
            self.checkpoint_store.save_run(&updated).await?;

            info!(
                %run_id,
                attempt = run.attempt,
                max = run.retry_policy.max_attempts,
                "re-queued orphaned run for retry"
            );

            Ok(RecoveryAction::Requeued {
                run_id: run_id.clone(),
                attempt: run.attempt,
            })
        } else {
            // Move to DLQ.
            let checkpoint = self.checkpoint_store.load_checkpoint(run_id).await?;
            let entry = DeadLetterEntry {
                run_id: run_id.clone(),
                workflow_type: run.workflow_type.clone(),
                error: format!(
                    "lease expired after {} attempts (worker: {})",
                    run.attempt, lease.worker_id
                ),
                attempts: run.attempt,
                first_attempt_at: run.created_at,
                last_attempt_at: run.updated_at,
                last_checkpoint: checkpoint,
                total_input_tokens: run.total_input_tokens,
                total_output_tokens: run.total_output_tokens,
            };
            self.dead_letter_queue.enqueue(&entry).await?;

            // Mark run as failed.
            let mut updated = run.clone();
            updated.state = RunState::Failed {
                error: format!("lease expired, moved to DLQ"),
                attempts: run.attempt,
            };
            updated.updated_at = Utc::now();
            self.checkpoint_store.save_run(&updated).await?;

            warn!(
                %run_id,
                attempts = run.attempt,
                "orphaned run moved to dead letter queue"
            );

            Ok(RecoveryAction::DeadLettered {
                run_id: run_id.clone(),
                attempts: run.attempt,
            })
        }
    }

    /// Retry a run from the dead letter queue.
    ///
    /// Removes it from the DLQ and sets it back to Pending.
    pub async fn retry_from_dlq(&self, run_id: &RunId) -> Result<(), RuntimeError> {
        // Verify it's in the DLQ.
        let _entry = self
            .dead_letter_queue
            .get(run_id)
            .await?
            .ok_or_else(|| RuntimeError::RunNotFound {
                run_id: run_id.clone(),
            })?;

        // Load and update the run.
        let mut run = self
            .checkpoint_store
            .load_run(run_id)
            .await?
            .ok_or_else(|| RuntimeError::RunNotFound {
                run_id: run_id.clone(),
            })?;

        run.state = RunState::Pending;
        run.worker_id = None;
        run.updated_at = Utc::now();
        self.checkpoint_store.save_run(&run).await?;

        // Remove from DLQ.
        self.dead_letter_queue.remove(run_id).await?;

        info!(%run_id, "run retried from DLQ");
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_sochdb::SochStore;

    fn test_store() -> Arc<SochStore> {
        Arc::new(SochStore::open_in_memory().expect("in-memory store"))
    }

    #[tokio::test]
    async fn scan_empty() {
        let store = test_store();
        let cs = Arc::new(CheckpointStore::new(Arc::clone(&store)));
        let lm = Arc::new(LeaseManager::new(Arc::clone(&store), 30));
        let dlq = Arc::new(DeadLetterQueue::new(Arc::clone(&store)));

        let rm = RecoveryManager::new(cs, lm, dlq);
        let actions = rm.scan_and_recover().await.unwrap();
        assert!(actions.is_empty());
    }

    #[tokio::test]
    async fn retry_from_dlq_works() {
        let store = test_store();
        let cs = Arc::new(CheckpointStore::new(Arc::clone(&store)));
        let lm = Arc::new(LeaseManager::new(Arc::clone(&store), 30));
        let dlq = Arc::new(DeadLetterQueue::new(Arc::clone(&store)));

        // Create a failed run + DLQ entry.
        let mut run = WorkflowRun::new(
            WorkflowType::A2ATask {
                task_id: "t1".into(),
            },
            RetryPolicy::default_agent(),
        );
        let run_id = run.id.clone();
        run.state = RunState::Failed {
            error: "timeout".into(),
            attempts: 3,
        };
        cs.save_run(&run).await.unwrap();

        let entry = DeadLetterEntry {
            run_id: run_id.clone(),
            workflow_type: run.workflow_type.clone(),
            error: "timeout".into(),
            attempts: 3,
            first_attempt_at: run.created_at,
            last_attempt_at: run.updated_at,
            last_checkpoint: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
        };
        dlq.enqueue(&entry).await.unwrap();

        // Retry from DLQ.
        let rm = RecoveryManager::new(cs.clone(), lm, dlq.clone());
        rm.retry_from_dlq(&run_id).await.unwrap();

        // Verify run is back to Pending and removed from DLQ.
        let loaded = cs.load_run(&run_id).await.unwrap().unwrap();
        assert!(matches!(loaded.state, RunState::Pending));
        assert!(dlq.get(&run_id).await.unwrap().is_none());
    }
}
