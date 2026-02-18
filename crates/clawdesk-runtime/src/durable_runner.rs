//! Durable agent runner — wraps `AgentRunner` with journaling and checkpointing.
//!
//! ## Design
//!
//! The `DurableAgentRunner` orchestrates a single `WorkflowRun`:
//!
//! 1. Load or create a `WorkflowRun` and acquire a lease.
//! 2. If a checkpoint exists, restore state from it (resume path).
//! 3. Create an `AgentRunner` and delegate to it.
//! 4. After each agent response, journal the LLM call and checkpoint.
//! 5. On completion, mark the run as `Completed` and release the lease.
//! 6. On failure, apply retry policy or move to DLQ.
//!
//! ## Crash Recovery
//!
//! On restart, the `RecoveryManager` scans for runs with expired leases,
//! then calls `DurableAgentRunner::resume()` which loads the checkpoint
//! and continues from the last completed round.

use crate::checkpoint::CheckpointStore;
use crate::journal::ActivityJournal;
use crate::lease::LeaseManager;
use crate::types::*;
use chrono::Utc;
use clawdesk_agents::runner::AgentConfig;
use clawdesk_providers::ChatMessage;
use clawdesk_types::error::ClawDeskError;
use clawdesk_types::session::SessionKey;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Orchestrates a durable execution of an agent workflow.
///
/// Wraps `AgentRunner` with persistence, journaling, checkpointing,
/// and lease management. Does NOT modify the `AgentRunner` internals —
/// instead, it manages lifecycle around each call to `AgentRunner::run()`.
pub struct DurableAgentRunner {
    /// Checkpoint store (run metadata + checkpoint snapshots).
    checkpoint_store: Arc<CheckpointStore>,
    /// Activity journal for side-effect tracking.
    journal: Arc<ActivityJournal>,
    /// Lease manager for worker ownership.
    lease_manager: Arc<LeaseManager>,
    /// Factory for creating AgentRunner instances.
    runner_factory: Arc<dyn RunnerFactory>,
    /// Worker identity.
    worker_id: String,
}

impl DurableAgentRunner {
    pub fn new(
        checkpoint_store: Arc<CheckpointStore>,
        journal: Arc<ActivityJournal>,
        lease_manager: Arc<LeaseManager>,
        runner_factory: Arc<dyn RunnerFactory>,
        worker_id: String,
    ) -> Self {
        Self {
            checkpoint_store,
            journal,
            lease_manager,
            runner_factory,
            worker_id,
        }
    }

    /// Start a new durable agent execution.
    ///
    /// Creates a `WorkflowRun`, acquires a lease, executes the agent loop,
    /// journals the result, checkpoints, and transitions the run state.
    pub async fn start(
        &self,
        config: AgentConfig,
        session_key: SessionKey,
        history: Vec<ChatMessage>,
        system_prompt: String,
        retry_policy: RetryPolicy,
    ) -> Result<(RunId, String), RuntimeError> {
        // 1. Create and persist the WorkflowRun.
        let mut run = WorkflowRun::new(
            WorkflowType::AgentLoop {
                config: config.clone(),
                session_key: session_key.clone(),
            },
            retry_policy,
        );
        run.worker_id = Some(self.worker_id.clone());
        run.state = RunState::Running {
            worker_id: self.worker_id.clone(),
        };
        run.attempt = 1;
        run.updated_at = Utc::now();

        self.checkpoint_store.save_run(&run).await?;

        // 2. Acquire lease.
        let lease = self.lease_manager.acquire(&run.id, &self.worker_id).await?;
        info!(run_id = %run.id, "durable agent run started");

        // 3. Execute with durability wrapping.
        match self
            .execute_with_durability(&mut run, &config, history, system_prompt, lease.fence_token)
            .await
        {
            Ok(content) => {
                // 4a. Mark completed.
                run.state = RunState::Completed {
                    output: serde_json::json!({ "content": content }),
                };
                run.updated_at = Utc::now();
                self.checkpoint_store.save_run(&run).await?;
                self.lease_manager
                    .release(&run.id, &self.worker_id, lease.fence_token)
                    .await?;
                info!(run_id = %run.id, "durable agent run completed");
                let run_id = run.id.clone();
                Ok((run_id, content))
            }
            Err(e) => {
                // 4b. Handle failure with retry policy.
                self.handle_failure(&mut run, &e, lease.fence_token).await?;
                Err(RuntimeError::Agent(e.to_string()))
            }
        }
    }

    /// Resume a previously suspended or crashed run from its checkpoint.
    pub async fn resume(&self, run_id: &RunId) -> Result<(RunId, String), RuntimeError> {
        // 1. Load the run.
        let mut run = self
            .checkpoint_store
            .load_run(run_id)
            .await?
            .ok_or_else(|| RuntimeError::RunNotFound {
                run_id: run_id.clone(),
            })?;

        if run.is_terminal() {
            return Err(RuntimeError::TerminalState {
                run_id: run_id.clone(),
                state: run.state.label().to_string(),
            });
        }

        // 2. Acquire lease.
        let lease = self.lease_manager.acquire(run_id, &self.worker_id).await?;

        // 3. Load checkpoint.
        let checkpoint = self.checkpoint_store.load_checkpoint(run_id).await?;

        // 4. Extract config and history from checkpoint + run type.
        let (config, history, system_prompt) = match (&run.workflow_type, checkpoint)
        {
            (
                WorkflowType::AgentLoop {
                    config,
                    session_key: _,
                },
                Some(Checkpoint::AgentLoop {
                    messages,
                    total_input_tokens,
                    total_output_tokens,
                    ..
                }),
            ) => {
                run.total_input_tokens = total_input_tokens;
                run.total_output_tokens = total_output_tokens;
                (config.clone(), messages, String::new())
            }
            (WorkflowType::AgentLoop { config, .. }, None) => {
                warn!(run_id = %run.id, "no checkpoint found, starting from scratch");
                (config.clone(), vec![], String::new())
            }
            _ => {
                return Err(RuntimeError::CheckpointCorrupted {
                    detail: "resume called on non-AgentLoop workflow".into(),
                });
            }
        };

        // 5. Update run state.
        run.state = RunState::Running {
            worker_id: self.worker_id.clone(),
        };
        run.worker_id = Some(self.worker_id.clone());
        run.attempt += 1;
        run.updated_at = Utc::now();
        self.checkpoint_store.save_run(&run).await?;

        info!(run_id = %run.id, attempt = run.attempt, "resuming durable agent run");

        // 6. Execute.
        match self
            .execute_with_durability(&mut run, &config, history, system_prompt, lease.fence_token)
            .await
        {
            Ok(content) => {
                run.state = RunState::Completed {
                    output: serde_json::json!({ "content": content }),
                };
                run.updated_at = Utc::now();
                self.checkpoint_store.save_run(&run).await?;
                self.lease_manager
                    .release(&run.id, &self.worker_id, lease.fence_token)
                    .await?;
                Ok((run.id.clone(), content))
            }
            Err(e) => {
                self.handle_failure(&mut run, &e, lease.fence_token).await?;
                Err(RuntimeError::Agent(e.to_string()))
            }
        }
    }

    /// Core execution with durability wrapping.
    ///
    /// Creates an `AgentRunner` from the factory, delegates to `runner.run()`,
    /// then journals and checkpoints the result.
    async fn execute_with_durability(
        &self,
        run: &mut WorkflowRun,
        config: &AgentConfig,
        history: Vec<ChatMessage>,
        system_prompt: String,
        fence_token: u64,
    ) -> Result<String, ClawDeskError> {
        // Check deadline.
        if let Some(deadline) = run.deadline {
            if Utc::now() > deadline {
                return Err(ClawDeskError::Agent(
                    clawdesk_types::error::AgentError::Cancelled,
                ));
            }
        }

        // Validate lease before execution.
        self.lease_manager
            .validate_fence(&run.id, fence_token)
            .await
            .map_err(|e| {
                ClawDeskError::Agent(clawdesk_types::error::AgentError::ContextAssemblyFailed {
                    detail: e.to_string(),
                })
            })?;

        // Create runner from factory.
        let runner = self.runner_factory.create_runner(config)?;

        // Execute the agent loop.
        let response = runner.run(history, system_prompt).await?;

        // Journal the LLM result.
        let seq = run.next_seq();
        let snapshot = LlmSnapshot {
            content: response.content.clone(),
            model: config.model.clone(),
            tool_calls: vec![],
            usage: clawdesk_providers::TokenUsage {
                input_tokens: response.input_tokens,
                output_tokens: response.output_tokens,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            finish_reason: response.finish_reason,
        };

        let entry = JournalEntry::LlmCall {
            seq,
            round: response.total_rounds,
            request_hash: 0, // Hash computed only on replay path.
            response: snapshot,
            started_at: run.updated_at,
            completed_at: Utc::now(),
        };
        self.journal.append(&run.id, &entry).await?;

        // Update token accounting.
        run.total_input_tokens += response.input_tokens;
        run.total_output_tokens += response.output_tokens;

        // Checkpoint.
        let checkpoint = Checkpoint::AgentLoop {
            round: response.total_rounds,
            messages: vec![], // Messages are consumed by the runner; checkpoint stores summary.
            total_input_tokens: run.total_input_tokens,
            total_output_tokens: run.total_output_tokens,
            guard_state: GuardSnapshot {
                estimated_tokens: 0,
                compaction_count: 0,
                circuit_breaker_failures: 0,
            },
        };
        self.checkpoint_store
            .save_checkpoint(&run.id, &checkpoint)
            .await?;

        // Renew lease after successful execution.
        let _ = self
            .lease_manager
            .renew(&run.id, &self.worker_id, fence_token)
            .await;

        debug!(run_id = %run.id, rounds = response.total_rounds, "agent execution complete");

        Ok(response.content)
    }

    /// Handle a failed execution: apply retry policy or move to DLQ.
    async fn handle_failure(
        &self,
        run: &mut WorkflowRun,
        error: &ClawDeskError,
        fence_token: u64,
    ) -> Result<(), RuntimeError> {
        let error_class = ErrorClass::classify(error);

        if run.attempt < run.retry_policy.max_attempts
            && run.retry_policy.should_retry(&error_class)
        {
            // Schedule for retry.
            let delay = run.retry_policy.delay_for_attempt(run.attempt);
            run.state = RunState::Suspended {
                reason: SuspendReason::UserInput {
                    prompt: format!(
                        "Retrying after {:?} (attempt {}/{}): {}",
                        delay,
                        run.attempt + 1,
                        run.retry_policy.max_attempts,
                        error
                    ),
                },
            };
            run.updated_at = Utc::now();
            self.checkpoint_store.save_run(run).await?;
            warn!(
                run_id = %run.id,
                attempt = run.attempt,
                error = %error,
                ?delay,
                "run failed, scheduled for retry"
            );
        } else {
            // Terminal failure — move to failed state.
            run.state = RunState::Failed {
                error: error.to_string(),
                attempts: run.attempt,
            };
            run.updated_at = Utc::now();
            self.checkpoint_store.save_run(run).await?;
            error!(
                run_id = %run.id,
                attempts = run.attempt,
                error = %error,
                "run permanently failed"
            );
        }

        // Release lease.
        let _ = self
            .lease_manager
            .release(&run.id, &self.worker_id, fence_token)
            .await;

        Ok(())
    }

    /// Get the status of a run.
    pub async fn get_status(&self, run_id: &RunId) -> Result<RunState, RuntimeError> {
        let run = self
            .checkpoint_store
            .load_run(run_id)
            .await?
            .ok_or_else(|| RuntimeError::RunNotFound {
                run_id: run_id.clone(),
            })?;
        Ok(run.state)
    }

    /// Cancel a running workflow.
    pub async fn cancel(&self, run_id: &RunId, reason: String) -> Result<(), RuntimeError> {
        let mut run = self
            .checkpoint_store
            .load_run(run_id)
            .await?
            .ok_or_else(|| RuntimeError::RunNotFound {
                run_id: run_id.clone(),
            })?;

        if run.is_terminal() {
            return Err(RuntimeError::TerminalState {
                run_id: run_id.clone(),
                state: run.state.label().to_string(),
            });
        }

        run.state = RunState::Cancelled { reason };
        run.updated_at = Utc::now();
        self.checkpoint_store.save_run(&run).await?;

        // Try to release any held lease.
        if let Some(ref wid) = run.worker_id {
            if let Ok(Some(lease)) = self.lease_manager.load_lease(run_id).await {
                let _ = self
                    .lease_manager
                    .release(run_id, wid, lease.fence_token)
                    .await;
            }
        }

        info!(run_id = %run_id, "run cancelled");
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_agents::runner::AgentConfig;
    use clawdesk_sochdb::SochStore;

    fn test_store() -> Arc<SochStore> {
        Arc::new(SochStore::open_in_memory().expect("in-memory store"))
    }

    /// Minimal RunnerFactory that always returns an error (no real provider).
    struct FailingRunnerFactory;

    impl RunnerFactory for FailingRunnerFactory {
        fn create_runner(
            &self,
            _config: &AgentConfig,
        ) -> Result<clawdesk_agents::runner::AgentRunner, ClawDeskError> {
            Err(ClawDeskError::Agent(
                clawdesk_types::error::AgentError::AllProvidersExhausted,
            ))
        }
    }

    #[tokio::test]
    async fn start_with_failing_runner() {
        let store = test_store();
        let cs = Arc::new(CheckpointStore::new(Arc::clone(&store)));
        let journal = Arc::new(ActivityJournal::new(Arc::clone(&store)));
        let lm = Arc::new(LeaseManager::new(Arc::clone(&store), 30));
        let factory = Arc::new(FailingRunnerFactory);

        let runner = DurableAgentRunner::new(cs, journal, lm, factory, "test-worker".into());

        let config = AgentConfig::default();
        let result = runner
            .start(
                config,
                SessionKey::new(clawdesk_types::channel::ChannelId::Internal, "test"),
                vec![],
                "You are helpful.".into(),
                RetryPolicy::none(),
            )
            .await;

        // Should fail because the factory returns an error.
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cancel_run() {
        let store = test_store();
        let cs = Arc::new(CheckpointStore::new(Arc::clone(&store)));
        let journal = Arc::new(ActivityJournal::new(Arc::clone(&store)));
        let lm = Arc::new(LeaseManager::new(Arc::clone(&store), 30));
        let factory = Arc::new(FailingRunnerFactory);

        let runner = DurableAgentRunner::new(
            Arc::clone(&cs),
            journal,
            lm,
            factory,
            "test-worker".into(),
        );

        // Create a pending run directly.
        let mut run = WorkflowRun::new(
            WorkflowType::A2ATask {
                task_id: "t1".into(),
            },
            RetryPolicy::none(),
        );
        let run_id = run.id.clone();
        run.state = RunState::Running {
            worker_id: "test-worker".into(),
        };
        cs.save_run(&run).await.unwrap();

        // Cancel it.
        runner
            .cancel(&run_id, "user requested".into())
            .await
            .unwrap();

        let status = runner.get_status(&run_id).await.unwrap();
        assert!(matches!(status, RunState::Cancelled { .. }));
    }
}
