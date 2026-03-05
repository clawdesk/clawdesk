//! DAG executor — durable pipeline execution with per-step checkpointing.
//!
//! ## Design
//!
//! The `DagExecutor` walks an `AgentPipeline`'s step graph topologically.
//! After each step completes, it:
//!
//! 1. Journals the step result.
//! 2. Saves a pipeline checkpoint.
//! 3. Renews the lease.
//!
//! On resume, completed steps are skipped (loaded from checkpoint) and
//! execution continues from the first incomplete step.
//!
//! ## Gate Steps
//!
//! When a `PipelineStep::Gate` is encountered, the run is suspended
//! (`RunState::Suspended { reason: HumanGate { .. } }`) until approved.

use crate::checkpoint::CheckpointStore;
use crate::journal::ActivityJournal;
use crate::lease::LeaseManager;
use crate::types::*;
use chrono::Utc;
use clawdesk_agents::pipeline::{
    AgentPipeline, ErrorPolicy, GateDefault, PipelineResult, PipelineStep, StepResult,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

/// Registry mapping agent IDs to their configurations.
///
/// Resolves the TODO in `execute_step` — pipeline steps can now reference
/// pre-configured agents by ID instead of using the agent_id as a model name.
pub struct AgentConfigRegistry {
    configs: tokio::sync::RwLock<HashMap<String, clawdesk_agents::runner::AgentConfig>>,
}

impl AgentConfigRegistry {
    pub fn new() -> Self {
        Self {
            configs: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Register an agent configuration.
    pub async fn register(&self, agent_id: impl Into<String>, config: clawdesk_agents::runner::AgentConfig) {
        self.configs.write().await.insert(agent_id.into(), config);
    }

    /// Look up an agent configuration by ID.
    pub async fn get(&self, agent_id: &str) -> Option<clawdesk_agents::runner::AgentConfig> {
        self.configs.read().await.get(agent_id).cloned()
    }

    /// List all registered agent IDs.
    pub async fn list_ids(&self) -> Vec<String> {
        self.configs.read().await.keys().cloned().collect()
    }

    /// Remove an agent configuration.
    pub async fn remove(&self, agent_id: &str) -> Option<clawdesk_agents::runner::AgentConfig> {
        self.configs.write().await.remove(agent_id)
    }
}

/// Executes pipelines with durable per-step checkpointing.
pub struct DagExecutor {
    checkpoint_store: Arc<CheckpointStore>,
    journal: Arc<ActivityJournal>,
    lease_manager: Arc<LeaseManager>,
    runner_factory: Arc<dyn RunnerFactory>,
    worker_id: String,
    /// Agent configuration registry for looking up pre-configured agents.
    /// When a pipeline step references an agent_id, the executor first checks
    /// this registry. If found, uses the stored config. Otherwise, falls back
    /// to creating a default config with agent_id as the model name.
    agent_configs: Arc<AgentConfigRegistry>,
}

impl DagExecutor {
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
            agent_configs: Arc::new(AgentConfigRegistry::new()),
        }
    }

    /// Create a new DagExecutor with an existing agent config registry.
    pub fn with_agent_configs(
        checkpoint_store: Arc<CheckpointStore>,
        journal: Arc<ActivityJournal>,
        lease_manager: Arc<LeaseManager>,
        runner_factory: Arc<dyn RunnerFactory>,
        worker_id: String,
        agent_configs: Arc<AgentConfigRegistry>,
    ) -> Self {
        Self {
            checkpoint_store,
            journal,
            lease_manager,
            runner_factory,
            worker_id,
            agent_configs,
        }
    }

    /// Get a reference to the agent config registry.
    pub fn agent_configs(&self) -> &Arc<AgentConfigRegistry> {
        &self.agent_configs
    }

    /// Execute a pipeline durably, creating a WorkflowRun and managing its lifecycle.
    pub async fn execute(
        &self,
        pipeline: AgentPipeline,
        retry_policy: RetryPolicy,
    ) -> Result<PipelineResult, RuntimeError> {
        // Create the workflow run.
        let mut run = WorkflowRun::new(
            WorkflowType::Pipeline {
                pipeline: pipeline.clone(),
            },
            retry_policy,
        );
        run.state = RunState::Running {
            worker_id: self.worker_id.clone(),
        };
        run.worker_id = Some(self.worker_id.clone());
        run.attempt = 1;
        run.updated_at = Utc::now();
        self.checkpoint_store.save_run(&run).await?;

        // Acquire lease.
        let lease = self.lease_manager.acquire(&run.id, &self.worker_id).await?;

        info!(
            run_id = %run.id,
            steps = pipeline.steps.len(),
            name = pipeline.metadata.name,
            "starting durable pipeline execution"
        );

        // Try to resume from checkpoint.
        let (start_step, mut step_results, mut context) =
            match self.checkpoint_store.load_checkpoint(&run.id).await? {
                Some(Checkpoint::PipelineStep {
                    step_index,
                    step_results,
                    context,
                }) => {
                    info!(
                        run_id = %run.id,
                        resume_at = step_index,
                        "resuming pipeline from checkpoint"
                    );
                    (step_index, step_results, context)
                }
                _ => (0, Vec::new(), serde_json::Value::Null),
            };

        let start_time = Instant::now();
        let mut errors = Vec::new();
        let mut fence_token = lease.fence_token;

        // Execute each step.
        for step_idx in start_step..pipeline.steps.len() {
            let step = &pipeline.steps[step_idx];

            // Renew lease before each step.
            match self
                .lease_manager
                .renew(&run.id, &self.worker_id, fence_token)
                .await
            {
                Ok(renewed) => fence_token = renewed.fence_token,
                Err(e) => {
                    warn!(run_id = %run.id, step = step_idx, %e, "lease renewal failed");
                    return Err(e);
                }
            }

            debug!(run_id = %run.id, step = step_idx, "executing pipeline step");

            let step_start = Instant::now();
            let result = self
                .execute_step(&mut run, step, step_idx, &context)
                .await;

            match result {
                Ok(StepExecution::Completed(step_result)) => {
                    // Update context with step output.
                    context = step_result.output.clone();
                    step_results.push(step_result);

                    // Checkpoint after each step.
                    let cp = Checkpoint::PipelineStep {
                        step_index: step_idx + 1,
                        step_results: step_results.clone(),
                        context: context.clone(),
                    };
                    self.checkpoint_store
                        .save_checkpoint(&run.id, &cp)
                        .await?;
                }
                Ok(StepExecution::Suspended { reason }) => {
                    // Gate step — suspend the run.
                    run.state = RunState::Suspended { reason };
                    run.updated_at = Utc::now();
                    self.checkpoint_store.save_run(&run).await?;

                    // Checkpoint at current position (will resume here).
                    let cp = Checkpoint::PipelineStep {
                        step_index: step_idx,
                        step_results: step_results.clone(),
                        context: context.clone(),
                    };
                    self.checkpoint_store
                        .save_checkpoint(&run.id, &cp)
                        .await?;

                    info!(run_id = %run.id, step = step_idx, "pipeline suspended at gate");
                    return Err(RuntimeError::Agent("pipeline suspended at gate".into()));
                }
                Err(e) => {
                    let duration_ms = step_start.elapsed().as_millis() as u64;
                    let error_msg = e.to_string();
                    errors.push(error_msg.clone());

                    let step_result = StepResult {
                        step_index: step_idx,
                        success: false,
                        output: serde_json::Value::Null,
                        duration_ms,
                        error: Some(error_msg),
                        sub_results: vec![],
                    };
                    step_results.push(step_result);

                    match pipeline.error_policy {
                        ErrorPolicy::FailFast => {
                            warn!(run_id = %run.id, step = step_idx, %e, "pipeline failed (fail-fast)");
                            break;
                        }
                        ErrorPolicy::ContinueOnError => {
                            warn!(run_id = %run.id, step = step_idx, %e, "step failed, continuing");
                            continue;
                        }
                        ErrorPolicy::Retry { .. } => {
                            // Retry is handled at the step level (future enhancement).
                            warn!(run_id = %run.id, step = step_idx, %e, "step retry not yet implemented, treating as fail-fast");
                            break;
                        }
                    }
                }
            }
        }

        let total_duration_ms = start_time.elapsed().as_millis() as u64;
        let success = errors.is_empty();
        let final_output = context.clone();

        let result = PipelineResult {
            pipeline_name: pipeline.metadata.name.clone(),
            success,
            steps: step_results,
            final_output,
            total_duration_ms,
            errors,
        };

        // Mark run completed or failed.
        if success {
            run.state = RunState::Completed {
                output: serde_json::to_value(&result).unwrap_or_default(),
            };
        } else {
            run.state = RunState::Failed {
                error: "pipeline step(s) failed".into(),
                attempts: run.attempt,
            };
        }
        run.updated_at = Utc::now();
        self.checkpoint_store.save_run(&run).await?;

        // Release lease.
        let _ = self
            .lease_manager
            .release(&run.id, &self.worker_id, fence_token)
            .await;

        Ok(result)
    }

    /// Execute a single pipeline step.
    fn execute_step<'a>(
        &'a self,
        run: &'a mut WorkflowRun,
        step: &'a PipelineStep,
        step_idx: usize,
        context: &'a serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<StepExecution, RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
        let start = Instant::now();

        match step {
            PipelineStep::Agent {
                agent_id,
                skill_id,
                timeout_secs,
                ..
            } => {
                // Look up agent config from registry by agent_id.
                // Falls back to default config with agent_id as model if not found.
                let config = match self.agent_configs.get(agent_id).await {
                    Some(registered_config) => {
                        debug!(
                            run_id = %run.id,
                            agent = agent_id,
                            model = %registered_config.model,
                            "using registered agent config"
                        );
                        registered_config
                    }
                    None => {
                        debug!(
                            run_id = %run.id,
                            agent = agent_id,
                            "no registered config, using agent_id as model"
                        );
                        clawdesk_agents::runner::AgentConfig {
                            model: agent_id.clone(),
                            ..Default::default()
                        }
                    }
                };

                let runner = self
                    .runner_factory
                    .create_runner(&config)
                    .map_err(|e| RuntimeError::Agent(e.to_string()))?;

                let prompt = match context {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                };

                let history = vec![clawdesk_providers::ChatMessage::new(
                    clawdesk_providers::MessageRole::User,
                    prompt.as_str(),
                )];

                let response = tokio::time::timeout(
                    std::time::Duration::from_secs(*timeout_secs),
                    runner.run(history, String::new()),
                )
                .await
                .map_err(|_| RuntimeError::DeadlineExceeded {
                    run_id: run.id.clone(),
                })?
                .map_err(|e| RuntimeError::Agent(e.to_string()))?;

                let duration_ms = start.elapsed().as_millis() as u64;

                // Journal the step result.
                let seq = run.next_seq();
                let entry = JournalEntry::LlmCall {
                    seq,
                    round: response.total_rounds,
                    request_hash: 0,
                    response: LlmSnapshot::from(&clawdesk_providers::ProviderResponse {
                        content: response.content.clone(),
                        model: config.model.clone(),
                        provider: String::new(),
                        usage: clawdesk_providers::TokenUsage {
                            input_tokens: response.input_tokens,
                            output_tokens: response.output_tokens,
                            cache_read_tokens: None,
                            cache_write_tokens: None,
                        },
                        tool_calls: vec![],
                        finish_reason: response.finish_reason,
                        latency: std::time::Duration::ZERO,
                    }),
                    started_at: Utc::now() - chrono::Duration::milliseconds(duration_ms as i64),
                    completed_at: Utc::now(),
                };
                self.journal.append(&run.id, &entry).await?;

                run.total_input_tokens += response.input_tokens;
                run.total_output_tokens += response.output_tokens;

                Ok(StepExecution::Completed(StepResult {
                    step_index: step_idx,
                    success: true,
                    output: serde_json::json!({ "content": response.content }),
                    duration_ms,
                    error: None,
                    sub_results: vec![],
                }))
            }

            PipelineStep::Gate {
                prompt,
                timeout_secs,
                default_action,
            } => {
                // Journal the gate decision request.
                let seq = run.next_seq();
                let entry = JournalEntry::GateDecision {
                    seq,
                    step_index: step_idx,
                    prompt: prompt.clone(),
                    approved: false,
                    decided_by: "pending".into(),
                    decided_at: Utc::now(),
                };
                self.journal.append(&run.id, &entry).await?;

                match default_action {
                    GateDefault::Proceed => {
                        // Auto-approve.
                        Ok(StepExecution::Completed(StepResult {
                            step_index: step_idx,
                            success: true,
                            output: serde_json::json!({"gate": "auto_approved"}),
                            duration_ms: 0,
                            error: None,
                            sub_results: vec![],
                        }))
                    }
                    GateDefault::Abort => {
                        Err(RuntimeError::Agent("gate aborted pipeline".into()))
                    }
                    GateDefault::Skip => {
                        // Suspend and wait for human input.
                        Ok(StepExecution::Suspended {
                            reason: SuspendReason::HumanGate {
                                prompt: prompt.clone(),
                                timeout_secs: *timeout_secs,
                            },
                        })
                    }
                }
            }

            PipelineStep::Transform {
                expression,
                description,
            } => {
                debug!(
                    run_id = %run.id,
                    step = step_idx,
                    expr = expression,
                    desc = ?description,
                    "executing transform step"
                );

                // Simple expression evaluation: if it starts with '$',
                // treat it as a JSONPath-like accessor. Otherwise, use it
                // as a template.
                let output = if expression.starts_with("$.") {
                    // Very basic JSONPath: $.field
                    let path = &expression[2..];
                    context
                        .get(path)
                        .cloned()
                        .unwrap_or(serde_json::Value::Null)
                } else {
                    serde_json::Value::String(expression.replace("{input}", &context.to_string()))
                };

                Ok(StepExecution::Completed(StepResult {
                    step_index: step_idx,
                    success: true,
                    output,
                    duration_ms: start.elapsed().as_millis() as u64,
                    error: None,
                    sub_results: vec![],
                }))
            }

            PipelineStep::Parallel { branches, .. } => {
                debug!(
                    run_id = %run.id,
                    step = step_idx,
                    branches = branches.len(),
                    "parallel execution not yet implemented — executing sequentially"
                );

                let mut sub_results = Vec::new();
                for (i, branch) in branches.iter().enumerate() {
                    match self.execute_step(run, branch, i, context).await {
                        Ok(StepExecution::Completed(r)) => sub_results.push(r),
                        Ok(StepExecution::Suspended { reason }) => {
                            return Ok(StepExecution::Suspended { reason });
                        }
                        Err(e) => {
                            sub_results.push(StepResult {
                                step_index: i,
                                success: false,
                                output: serde_json::Value::Null,
                                duration_ms: 0,
                                error: Some(e.to_string()),
                                sub_results: vec![],
                            });
                        }
                    }
                }

                let all_success = sub_results.iter().all(|r| r.success);
                let merged = serde_json::Value::Array(
                    sub_results.iter().map(|r| r.output.clone()).collect(),
                );

                Ok(StepExecution::Completed(StepResult {
                    step_index: step_idx,
                    success: all_success,
                    output: merged,
                    duration_ms: start.elapsed().as_millis() as u64,
                    error: None,
                    sub_results,
                }))
            }

            PipelineStep::Router {
                condition: _,
                routes,
                default_route,
            } => {
                // For now, always take the default route or first route.
                let target = default_route
                    .as_deref()
                    .or_else(|| routes.first().map(|(_, s)| s));

                match target {
                    Some(step) => self.execute_step(run, step, step_idx, context).await,
                    None => Err(RuntimeError::Agent("no route matched".into())),
                }
            }
        }
        }) // close Box::pin(async move { ... })
    }
}

/// Internal enum to distinguish completed vs suspended step execution.
enum StepExecution {
    Completed(StepResult),
    Suspended { reason: SuspendReason },
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RetryPolicy, RunnerFactory};
    use clawdesk_agents::pipeline::{PipelineMetadata, PipelineStep};
    use clawdesk_agents::runner::{AgentConfig, AgentRunner};
    use clawdesk_sochdb::SochStore;
    use clawdesk_types::error::ClawDeskError;

    fn test_store() -> Arc<SochStore> {
        Arc::new(SochStore::open_in_memory().expect("in-memory store"))
    }

    struct FailingFactory;
    impl RunnerFactory for FailingFactory {
        fn create_runner(
            &self,
            _config: &AgentConfig,
        ) -> Result<AgentRunner, ClawDeskError> {
            Err(ClawDeskError::Agent(
                clawdesk_types::error::AgentError::AllProvidersExhausted,
            ))
        }
    }

    fn simple_pipeline() -> AgentPipeline {
        AgentPipeline {
            steps: vec![PipelineStep::Transform {
                expression: "hello world".into(),
                description: Some("test transform".into()),
            }],
            edges: vec![],
            error_policy: ErrorPolicy::FailFast,
            metadata: PipelineMetadata {
                name: "test-pipeline".into(),
                description: None,
                version: "1.0".into(),
                author: None,
            },
        }
    }

    #[tokio::test]
    async fn execute_transform_pipeline() {
        let store = test_store();
        let cs = Arc::new(CheckpointStore::new(Arc::clone(&store)));
        let journal = Arc::new(ActivityJournal::new(Arc::clone(&store)));
        let lm = Arc::new(LeaseManager::new(Arc::clone(&store), 30));
        let factory = Arc::new(FailingFactory);

        let executor = DagExecutor::new(cs, journal, lm, factory, "worker-1".into());
        let result = executor
            .execute(simple_pipeline(), RetryPolicy::none())
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.pipeline_name, "test-pipeline");
    }

    #[tokio::test]
    async fn gate_auto_proceed() {
        let store = test_store();
        let cs = Arc::new(CheckpointStore::new(Arc::clone(&store)));
        let journal = Arc::new(ActivityJournal::new(Arc::clone(&store)));
        let lm = Arc::new(LeaseManager::new(Arc::clone(&store), 30));
        let factory = Arc::new(FailingFactory);

        let pipeline = AgentPipeline {
            steps: vec![PipelineStep::Gate {
                prompt: "Approve?".into(),
                timeout_secs: 60,
                default_action: GateDefault::Proceed,
            }],
            edges: vec![],
            error_policy: ErrorPolicy::FailFast,
            metadata: PipelineMetadata {
                name: "gate-test".into(),
                description: None,
                version: "1.0".into(),
                author: None,
            },
        };

        let executor = DagExecutor::new(cs, journal, lm, factory, "worker-1".into());
        let result = executor.execute(pipeline, RetryPolicy::none()).await.unwrap();
        assert!(result.success);
    }
}
