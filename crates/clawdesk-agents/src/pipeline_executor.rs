//! Pipeline executor — runtime engine for the `AgentPipeline` DAG.
//!
//! ## Pipeline Execution Engine
//!
//! Consumes the `AgentPipeline` DAG from `pipeline.rs`, executes steps in
//! topological order using `tokio::JoinSet` for parallel fan-out, manages
//! intermediate results, and handles error policies (FailFast, ContinueOnError,
//! Retry with exponential backoff).
//!
//! ## Scheduling
//!
//! Pipeline execution is a classic DAG scheduling problem. Optimal makespan for
//! a DAG with n steps and critical path length C on p processors is bounded by:
//!     T_p ≥ max(C, W/p)
//! where W = total work. The executor achieves this via:
//! - Topological order from Kahn's algorithm (O(V + E))
//! - Parallel fan-out via `tokio::JoinSet` for `Parallel` steps
//! - Merge is O(k) for Concat/Structured and O(k log k) for Best
//!
//! ## Agent delegation
//!
//! The executor delegates to an `AgentBackend` trait for actual agent calls.
//! In production this wraps `AgentRunner`; in tests a mock can be injected.

use crate::pipeline::{
    AgentPipeline, ErrorPolicy, GateDefault, MergeStrategy, PipelineResult, PipelineStep,
    RoutingCondition, StepResult,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Agent backend trait — abstracts the actual agent execution
// ═══════════════════════════════════════════════════════════════════════════

/// Backend for executing a single agent step.
///
/// Implementations wrap `AgentRunner`, ACP protocol calls, or test mocks.
/// Each call receives the agent ID, an optional skill, and the input text.
#[async_trait]
pub trait AgentBackend: Send + Sync + 'static {
    /// Execute a single agent with the given input and return the output text.
    async fn execute_agent(
        &self,
        agent_id: &str,
        skill_id: Option<&str>,
        input: &str,
        timeout: Duration,
    ) -> Result<String, PipelineError>;

    /// Request human approval for a gate step. Returns `true` if approved.
    /// Default implementation auto-approves (for non-interactive pipelines).
    async fn request_gate_approval(
        &self,
        prompt: &str,
        timeout: Duration,
    ) -> Result<bool, PipelineError> {
        let _ = (prompt, timeout);
        Ok(true) // Default: auto-approve
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Contextual backend — enriches per-step input with memory + skills
// ═══════════════════════════════════════════════════════════════════════════

/// Per-step context enrichment for pipeline execution.
///
/// Wraps an `AgentBackend` and injects memory recall results and skill
/// suggestions into each step's input. This enables pipeline steps to
/// benefit from context that would normally only be available in the
/// main chat flow.
///
/// ## Architecture
///
/// ```text
/// PipelineExecutor
///   └→ ContextualBackend::execute_agent(agent_id, input)
///        ├→ memory_fn(input) → memory_context
///        ├→ skill_fn(input)  → skill_suggestions
///        ├→ enrich input with context
///        └→ inner.execute_agent(agent_id, enriched_input)
/// ```
pub struct ContextualBackend<B: AgentBackend> {
    inner: B,
    /// Optional async callback for memory recall per step.
    /// Takes the step input text and returns relevant memory fragments.
    memory_fn: Option<
        Arc<
            dyn Fn(
                    String,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send>>
                + Send
                + Sync,
        >,
    >,
    /// Optional async callback for skill suggestions per step.
    /// Takes the step input text and returns suggested skill IDs with relevance.
    skill_fn: Option<
        Arc<
            dyn Fn(
                    String,
                )
                    -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Vec<(String, f64)>> + Send>,
                > + Send
                + Sync,
        >,
    >,
}

impl<B: AgentBackend> ContextualBackend<B> {
    /// Create a contextual backend wrapping an existing backend.
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            memory_fn: None,
            skill_fn: None,
        }
    }

    /// Set the memory recall callback (called per pipeline step).
    pub fn with_memory(
        mut self,
        memory_fn: Arc<
            dyn Fn(
                    String,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        self.memory_fn = Some(memory_fn);
        self
    }

    /// Set the skill suggestion callback (called per pipeline step).
    pub fn with_skills(
        mut self,
        skill_fn: Arc<
            dyn Fn(
                    String,
                )
                    -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Vec<(String, f64)>> + Send>,
                > + Send
                + Sync,
        >,
    ) -> Self {
        self.skill_fn = Some(skill_fn);
        self
    }
}

#[async_trait]
impl<B: AgentBackend> AgentBackend for ContextualBackend<B> {
    async fn execute_agent(
        &self,
        agent_id: &str,
        skill_id: Option<&str>,
        input: &str,
        timeout: Duration,
    ) -> Result<String, PipelineError> {
        let mut enriched_parts = Vec::new();

        // Inject memory context if available
        if let Some(ref mem_fn) = self.memory_fn {
            let memories = (mem_fn)(input.to_string()).await;
            if !memories.is_empty() {
                enriched_parts.push(format!(
                    "<pipeline_memory_context>\n{}\n</pipeline_memory_context>",
                    memories.join("\n---\n")
                ));
            }
        }

        // Inject skill suggestions if available
        if let Some(ref skill_fn) = self.skill_fn {
            let suggestions = (skill_fn)(input.to_string()).await;
            if !suggestions.is_empty() {
                let skill_list: Vec<String> = suggestions
                    .iter()
                    .map(|(id, rel)| format!("- {} (relevance: {:.2})", id, rel))
                    .collect();
                enriched_parts.push(format!(
                    "<pipeline_skill_context>\nSuggested skills for this step:\n{}\n</pipeline_skill_context>",
                    skill_list.join("\n")
                ));
            }
        }

        // Build enriched input
        let enriched_input = if enriched_parts.is_empty() {
            input.to_string()
        } else {
            enriched_parts.push(input.to_string());
            enriched_parts.join("\n\n")
        };

        self.inner
            .execute_agent(agent_id, skill_id, &enriched_input, timeout)
            .await
    }

    async fn request_gate_approval(
        &self,
        prompt: &str,
        timeout: Duration,
    ) -> Result<bool, PipelineError> {
        self.inner.request_gate_approval(prompt, timeout).await
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Error types
// ═══════════════════════════════════════════════════════════════════════════

/// Pipeline execution error.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("agent '{agent_id}' failed: {detail}")]
    AgentFailed { agent_id: String, detail: String },

    #[error("pipeline validation failed: {0}")]
    ValidationFailed(String),

    #[error("step {step_index} timed out after {timeout_secs}s")]
    StepTimeout { step_index: usize, timeout_secs: u64 },

    #[error("gate '{prompt}' was rejected")]
    GateRejected { prompt: String },

    #[error("gate '{prompt}' timed out, default action = Abort")]
    GateTimeout { prompt: String },

    #[error("no route matched for condition")]
    NoRouteMatch,

    #[error("pipeline contains a cycle")]
    CycleDetected,

    #[error("transform failed: {0}")]
    TransformFailed(String),

    #[error("cancelled")]
    Cancelled,
}

// ═══════════════════════════════════════════════════════════════════════════
// Pipeline events
// ═══════════════════════════════════════════════════════════════════════════

/// Events emitted during pipeline execution for monitoring/tracing.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// Pipeline execution started.
    Started { pipeline_name: String, step_count: usize },
    /// A step started executing.
    StepStarted { step_index: usize, step_type: String },
    /// A step completed.
    StepCompleted { step_index: usize, success: bool, duration_ms: u64 },
    /// Pipeline finished.
    Finished { success: bool, total_duration_ms: u64 },
    /// Gate is waiting for approval.
    GateWaiting { prompt: String, timeout_secs: u64 },
    /// Error during execution.
    Error { step_index: usize, error: String },
}

// ═══════════════════════════════════════════════════════════════════════════
// Pipeline executor
// ═══════════════════════════════════════════════════════════════════════════

/// Runtime executor for `AgentPipeline` DAGs.
///
/// Executes steps in topological order, handles parallel fan-out via
/// `JoinSet`, applies error policies, and manages intermediate results.
pub struct PipelineExecutor {
    backend: Arc<dyn AgentBackend>,
    cancel: tokio_util::sync::CancellationToken,
    event_tx: Option<broadcast::Sender<PipelineEvent>>,
    /// Maximum parallel branches (bounds `JoinSet` concurrency).
    max_parallelism: usize,
}

impl PipelineExecutor {
    /// Create a new pipeline executor.
    pub fn new(backend: Arc<dyn AgentBackend>) -> Self {
        Self {
            backend,
            cancel: tokio_util::sync::CancellationToken::new(),
            event_tx: None,
            max_parallelism: 16,
        }
    }

    /// Set the cancellation token.
    pub fn with_cancellation(mut self, cancel: tokio_util::sync::CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Set the event channel for monitoring.
    pub fn with_event_channel(mut self, tx: broadcast::Sender<PipelineEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Set maximum parallelism for fan-out steps.
    pub fn with_max_parallelism(mut self, max: usize) -> Self {
        self.max_parallelism = max.max(1);
        self
    }

    fn emit(&self, event: PipelineEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(event);
        }
    }

    /// Execute a pipeline, returning the aggregate result.
    ///
    /// Steps are executed in topological order. Each step receives the
    /// output of the previous step as input (for linear pipelines) or
    /// a merged result (after parallel fan-out).
    pub async fn execute(
        &self,
        pipeline: &AgentPipeline,
        initial_input: &str,
    ) -> Result<PipelineResult, PipelineError> {
        // Validate first
        if let Err(errors) = pipeline.validate() {
            return Err(PipelineError::ValidationFailed(errors.join("; ")));
        }

        let order = pipeline
            .topological_order()
            .map_err(|_| PipelineError::CycleDetected)?;

        let start = Instant::now();
        self.emit(PipelineEvent::Started {
            pipeline_name: pipeline.metadata.name.clone(),
            step_count: pipeline.steps.len(),
        });

        // Track per-step outputs for DAG wiring
        let mut step_outputs: HashMap<usize, Value> = HashMap::new();
        let mut step_results: Vec<StepResult> = Vec::with_capacity(pipeline.steps.len());
        let mut errors: Vec<String> = Vec::new();

        // Initial input goes into a virtual "step -1" position
        let mut last_output = Value::String(initial_input.to_string());

        for &step_idx in &order {
            if self.cancel.is_cancelled() {
                return Err(PipelineError::Cancelled);
            }

            let step = &pipeline.steps[step_idx];

            // Determine input: from predecessor edges or last_output
            let input = self.resolve_input(step_idx, &pipeline.edges, &step_outputs, &last_output);

            let step_start = Instant::now();
            self.emit(PipelineEvent::StepStarted {
                step_index: step_idx,
                step_type: step_type_name(step).to_string(),
            });

            let result = self
                .execute_step(step, step_idx, &input, &pipeline.error_policy)
                .await;

            let duration_ms = step_start.elapsed().as_millis() as u64;

            match result {
                Ok(step_result) => {
                    let output = step_result.output.clone();
                    step_outputs.insert(step_idx, output.clone());
                    last_output = output;

                    self.emit(PipelineEvent::StepCompleted {
                        step_index: step_idx,
                        success: step_result.success,
                        duration_ms,
                    });
                    step_results.push(step_result);
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    self.emit(PipelineEvent::Error {
                        step_index: step_idx,
                        error: error_msg.clone(),
                    });
                    self.emit(PipelineEvent::StepCompleted {
                        step_index: step_idx,
                        success: false,
                        duration_ms,
                    });

                    step_results.push(StepResult {
                        step_index: step_idx,
                        success: false,
                        output: Value::Null,
                        duration_ms,
                        error: Some(error_msg.clone()),
                        sub_results: vec![],
                    });

                    match &pipeline.error_policy {
                        ErrorPolicy::FailFast => {
                            let total_ms = start.elapsed().as_millis() as u64;
                            self.emit(PipelineEvent::Finished {
                                success: false,
                                total_duration_ms: total_ms,
                            });
                            return Ok(PipelineResult {
                                pipeline_name: pipeline.metadata.name.clone(),
                                success: false,
                                steps: step_results,
                                final_output: Value::Null,
                                total_duration_ms: total_ms,
                                errors: vec![error_msg],
                            });
                        }
                        ErrorPolicy::ContinueOnError => {
                            errors.push(error_msg);
                            continue;
                        }
                        ErrorPolicy::Retry { .. } => {
                            // Retry is handled inside execute_step, if we get here
                            // it means all retries failed
                            errors.push(error_msg);
                            continue;
                        }
                    }
                }
            }
        }

        let total_ms = start.elapsed().as_millis() as u64;
        let success = errors.is_empty();

        self.emit(PipelineEvent::Finished {
            success,
            total_duration_ms: total_ms,
        });

        Ok(PipelineResult {
            pipeline_name: pipeline.metadata.name.clone(),
            success,
            steps: step_results,
            final_output: last_output,
            total_duration_ms: total_ms,
            errors,
        })
    }

    /// Resolve input for a step from its predecessor edges.
    fn resolve_input(
        &self,
        step_idx: usize,
        edges: &[(usize, usize)],
        step_outputs: &HashMap<usize, Value>,
        fallback: &Value,
    ) -> String {
        // Find all predecessors for this step
        let predecessors: Vec<usize> = edges
            .iter()
            .filter(|(_, to)| *to == step_idx)
            .map(|(from, _)| *from)
            .collect();

        if predecessors.is_empty() {
            // Root step — use fallback (initial input)
            value_to_string(fallback)
        } else if predecessors.len() == 1 {
            // Single predecessor — use its output directly
            step_outputs
                .get(&predecessors[0])
                .map(value_to_string)
                .unwrap_or_default()
        } else {
            // Multiple predecessors (fan-in) — merge as JSON object
            let merged: Value = json!(
                predecessors
                    .iter()
                    .filter_map(|idx| step_outputs.get(idx).map(|v| (format!("step_{}", idx), v.clone())))
                    .collect::<serde_json::Map<String, Value>>()
            );
            value_to_string(&merged)
        }
    }

    /// Execute a single step, handling retries per error policy.
    async fn execute_step(
        &self,
        step: &PipelineStep,
        step_idx: usize,
        input: &str,
        error_policy: &ErrorPolicy,
    ) -> Result<StepResult, PipelineError> {
        let (max_attempts, backoff_ms) = match error_policy {
            ErrorPolicy::Retry {
                max_attempts,
                backoff_ms,
            } => (*max_attempts, *backoff_ms),
            _ => (1, 0),
        };

        let mut last_err = None;
        for attempt in 0..max_attempts {
            if attempt > 0 {
                // Exponential backoff with 10% jitter
                let delay = backoff_ms * (1u64 << (attempt - 1).min(6));
                let jitter = (delay as f64 * 0.1 * rand_f64()) as u64;
                tokio::time::sleep(Duration::from_millis(delay + jitter)).await;
                debug!(step_idx, attempt, delay_ms = delay + jitter, "retrying step");
            }

            match self.execute_step_inner(step, step_idx, input).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    warn!(step_idx, attempt, error = %e, "step execution failed");
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or(PipelineError::AgentFailed {
            agent_id: "unknown".into(),
            detail: "all retry attempts exhausted".into(),
        }))
    }

    /// Execute a single step (no retry wrapper).
    ///
    /// Boxed future because Router/Parallel steps recurse.
    fn execute_step_inner<'a>(
        &'a self,
        step: &'a PipelineStep,
        step_idx: usize,
        input: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<StepResult, PipelineError>> + Send + 'a>> {
        Box::pin(async move {
        let start = Instant::now();

        match step {
            PipelineStep::Agent {
                agent_id,
                skill_id,
                input_transform,
                timeout_secs,
            } => {
                // Apply input transform if present
                let effective_input = if let Some(transform) = input_transform {
                    apply_transform(transform, input)?
                } else {
                    input.to_string()
                };

                let output = self
                    .backend
                    .execute_agent(
                        agent_id,
                        skill_id.as_deref(),
                        &effective_input,
                        Duration::from_secs(*timeout_secs),
                    )
                    .await?;

                Ok(StepResult {
                    step_index: step_idx,
                    success: true,
                    output: Value::String(output),
                    duration_ms: start.elapsed().as_millis() as u64,
                    error: None,
                    sub_results: vec![],
                })
            }

            PipelineStep::Parallel { branches, merge } => {
                self.execute_parallel(step_idx, branches, merge, input, &start)
                    .await
            }

            PipelineStep::Router {
                condition,
                routes,
                default_route,
            } => {
                // Evaluate condition against input
                let matched_route = match condition {
                    RoutingCondition::ContainsKeyword { keywords } => {
                        let input_lower = input.to_lowercase();
                        routes
                            .iter()
                            .find(|(name, _)| keywords.iter().any(|k| input_lower.contains(&k.to_lowercase())))
                            .or_else(|| routes.first()) // fallback to first
                    }
                    RoutingCondition::OutputLength { threshold } => {
                        if input.len() > *threshold {
                            routes.iter().find(|(name, _)| name == "long")
                        } else {
                            routes.iter().find(|(name, _)| name == "short")
                        }
                        .or(routes.first())
                    }
                    RoutingCondition::JsonPath { expression } => {
                        // Simple JSON path: check if field exists and is truthy
                        if let Ok(val) = serde_json::from_str::<Value>(input) {
                            let field_val = val.get(expression.trim_start_matches("$."));
                            if field_val.map_or(false, |v| !v.is_null()) {
                                routes.first()
                            } else {
                                routes.get(1).or(routes.first())
                            }
                        } else {
                            routes.first()
                        }
                    }
                    RoutingCondition::Always => routes.first(),
                };

                if let Some((route_name, route_step)) = matched_route {
                    debug!(step_idx, route = %route_name, "routing to matched route");
                    self.execute_step_inner(route_step, step_idx, input).await
                } else if let Some(default) = default_route {
                    debug!(step_idx, "routing to default route");
                    self.execute_step_inner(default, step_idx, input).await
                } else {
                    Err(PipelineError::NoRouteMatch)
                }
            }

            PipelineStep::Gate {
                prompt,
                timeout_secs,
                default_action,
            } => {
                self.emit(PipelineEvent::GateWaiting {
                    prompt: prompt.clone(),
                    timeout_secs: *timeout_secs,
                });

                let timeout = Duration::from_secs(*timeout_secs);
                match tokio::time::timeout(
                    timeout,
                    self.backend.request_gate_approval(prompt, timeout),
                )
                .await
                {
                    Ok(Ok(true)) => {
                        // Approved — pass through input unchanged
                        Ok(StepResult {
                            step_index: step_idx,
                            success: true,
                            output: Value::String(input.to_string()),
                            duration_ms: start.elapsed().as_millis() as u64,
                            error: None,
                            sub_results: vec![],
                        })
                    }
                    Ok(Ok(false)) => Err(PipelineError::GateRejected {
                        prompt: prompt.clone(),
                    }),
                    Ok(Err(e)) => Err(e),
                    Err(_) => match default_action {
                        GateDefault::Proceed => Ok(StepResult {
                            step_index: step_idx,
                            success: true,
                            output: Value::String(input.to_string()),
                            duration_ms: start.elapsed().as_millis() as u64,
                            error: None,
                            sub_results: vec![],
                        }),
                        GateDefault::Skip => Ok(StepResult {
                            step_index: step_idx,
                            success: true,
                            output: Value::Null,
                            duration_ms: start.elapsed().as_millis() as u64,
                            error: Some("gate timed out, skipped".into()),
                            sub_results: vec![],
                        }),
                        GateDefault::Abort => Err(PipelineError::GateTimeout {
                            prompt: prompt.clone(),
                        }),
                    },
                }
            }

            PipelineStep::Transform {
                expression,
                description: _,
            } => {
                let output = apply_transform(expression, input)?;
                Ok(StepResult {
                    step_index: step_idx,
                    success: true,
                    output: Value::String(output),
                    duration_ms: start.elapsed().as_millis() as u64,
                    error: None,
                    sub_results: vec![],
                })
            }
        }
        }) // end Box::pin
    }

    /// Execute parallel branches via `JoinSet`, then merge results.
    ///
    /// Makespan = max(branch_i duration) for successful branches.
    /// For k branches, merge is O(k) for Concat/Structured, O(k log k) for Best.
    async fn execute_parallel(
        &self,
        step_idx: usize,
        branches: &[PipelineStep],
        merge: &MergeStrategy,
        input: &str,
        start: &Instant,
    ) -> Result<StepResult, PipelineError> {
        let mut join_set: JoinSet<(usize, Result<StepResult, PipelineError>)> = JoinSet::new();

        let concurrency = branches.len().min(self.max_parallelism);

        // Share input across all branches via Arc<str> — O(1) per branch
        // instead of O(|input|) String clone per branch.
        let shared_input: Arc<str> = Arc::from(input);

        for (branch_idx, branch) in branches.iter().enumerate().take(concurrency) {
            let backend = Arc::clone(&self.backend);
            let branch = branch.clone();
            let input = Arc::clone(&shared_input);
            let cancel = self.cancel.clone();

            join_set.spawn(async move {
                if cancel.is_cancelled() {
                    return (branch_idx, Err(PipelineError::Cancelled));
                }
                // New executor per branch — avoids borrowing self across spawn
                let executor = PipelineExecutor::new(backend);
                let sub_step_idx = step_idx * 100 + branch_idx;
                let result = executor
                    .execute_step_inner(&branch, sub_step_idx, &input)
                    .await;
                (branch_idx, result)
            });
        }

        // Collect results, preserving order
        let mut branch_results: Vec<(usize, StepResult)> = Vec::with_capacity(branches.len());
        let mut branch_errors: Vec<String> = Vec::new();

        while let Some(join_result) = join_set.join_next().await {
            match join_result {
                Ok((idx, Ok(result))) => {
                    branch_results.push((idx, result));
                }
                Ok((idx, Err(e))) => {
                    branch_errors.push(format!("branch {}: {}", idx, e));
                }
                Err(e) => {
                    branch_errors.push(format!("branch panicked: {}", e));
                }
            }
        }

        // Sort by original branch index
        branch_results.sort_by_key(|(idx, _)| *idx);
        let sub_results: Vec<StepResult> = branch_results.iter().map(|(_, r)| r.clone()).collect();

        // Apply merge strategy
        let merged_output = merge_results(&branch_results, merge)?;

        Ok(StepResult {
            step_index: step_idx,
            success: branch_errors.is_empty(),
            output: merged_output,
            duration_ms: start.elapsed().as_millis() as u64,
            error: if branch_errors.is_empty() {
                None
            } else {
                Some(branch_errors.join("; "))
            },
            sub_results,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Merge strategies
// ═══════════════════════════════════════════════════════════════════════════

fn merge_results(
    results: &[(usize, StepResult)],
    strategy: &MergeStrategy,
) -> Result<Value, PipelineError> {
    if results.is_empty() {
        return Ok(Value::Null);
    }

    match strategy {
        MergeStrategy::Concat => {
            let merged: String = results
                .iter()
                .map(|(_, r)| value_to_string(&r.output))
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");
            Ok(Value::String(merged))
        }

        MergeStrategy::Structured => {
            let map: serde_json::Map<String, Value> = results
                .iter()
                .map(|(idx, r)| (format!("branch_{}", idx), r.output.clone()))
                .collect();
            Ok(Value::Object(map))
        }

        MergeStrategy::FirstSuccess => {
            results
                .iter()
                .find(|(_, r)| r.success)
                .map(|(_, r)| r.output.clone())
                .ok_or(PipelineError::AgentFailed {
                    agent_id: "parallel".into(),
                    detail: "no successful branch".into(),
                })
        }

        MergeStrategy::Best { score_field } => {
            // Parse each result's output as JSON, extract score field, pick best
            let mut scored: Vec<(f64, &Value)> = results
                .iter()
                .filter(|(_, r)| r.success)
                .filter_map(|(_, r)| {
                    if let Value::Object(map) = &r.output {
                        map.get(score_field)
                            .and_then(|v| v.as_f64())
                            .map(|score| (score, &r.output))
                    } else {
                        // Try parsing string output as JSON
                        None
                    }
                })
                .collect();

            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

            scored
                .first()
                .map(|(_, v)| (*v).clone())
                .ok_or(PipelineError::AgentFailed {
                    agent_id: "parallel".into(),
                    detail: format!("no branch has score field '{}'", score_field),
                })
        }

        MergeStrategy::Council {
            expert_weights,
            conflict_threshold,
            synthesis_agent_id: _,
            max_recommendations,
        } => {
            // Dempster-Shafer evidence combination.
            //
            // Each expert's output is parsed to extract hypotheses with mass
            // assignments. Dempster's rule of combination fuses mass functions
            // pairwise with O(f² × k) complexity for k experts with f focal
            // elements each — essentially free for practical agent counts.
            //
            // The conflict measure K ∈ [0,1) directly quantifies inter-expert
            // disagreement. K close to 1 triggers human escalation.
            //
            // Belief/Plausibility intervals: Bel(A) ≤ P(A) ≤ Pl(A).

            let mut weighted_outputs: Vec<Value> = Vec::new();
            for (i, (_, result)) in results.iter().enumerate() {
                let weight = expert_weights.get(i).copied().unwrap_or(1.0);
                weighted_outputs.push(json!({
                    "expert_index": i,
                    "weight": weight,
                    "output": result.output,
                    "success": result.success,
                }));
            }

            // Extract mass functions from expert outputs.
            // Each expert maps hypothesis labels → mass values ∈ [0,1].
            // "uncertainty" captures mass assigned to the full frame Ω.
            let mass_functions: Vec<std::collections::HashMap<String, f64>> = results
                .iter()
                .enumerate()
                .filter(|(_, (_, r))| r.success)
                .map(|(i, (_, r))| {
                    let weight = expert_weights.get(i).copied().unwrap_or(1.0);
                    extract_mass_function(&r.output, weight)
                })
                .collect();

            // Pairwise Dempster combination
            let (fused, conflict) = if mass_functions.len() >= 2 {
                let mut combined = mass_functions[0].clone();
                let mut total_conflict = 0.0f64;
                for mf in &mass_functions[1..] {
                    let (new_combined, k) = dempster_combine(&combined, mf);
                    total_conflict = 1.0 - (1.0 - total_conflict) * (1.0 - k);
                    combined = new_combined;
                }
                (combined, total_conflict)
            } else if mass_functions.len() == 1 {
                (mass_functions[0].clone(), 0.0)
            } else {
                (std::collections::HashMap::new(), 1.0)
            };

            // Compute belief and plausibility intervals
            let mut hypotheses: Vec<Value> = Vec::new();
            let mut entries: Vec<(&String, &f64)> = fused.iter()
                .filter(|(k, _)| *k != "uncertainty")
                .collect();
            entries.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));

            for (hyp, &mass) in entries.iter().take(*max_recommendations) {
                // Belief = mass of exactly this hypothesis
                let belief = mass;
                // Plausibility = 1 - mass of all non-overlapping hypotheses
                let plausibility = 1.0 - fused.iter()
                    .filter(|(k, _)| k != hyp && *k != "uncertainty")
                    .map(|(_, v)| v)
                    .sum::<f64>();
                hypotheses.push(json!({
                    "hypothesis": hyp,
                    "mass": mass,
                    "belief": belief,
                    "plausibility": plausibility.max(belief),
                }));
            }

            Ok(json!({
                "experts": weighted_outputs,
                "fused_masses": fused,
                "ranked_hypotheses": hypotheses,
                "conflict": conflict,
                "conflict_threshold": conflict_threshold,
                "conflict_alert": conflict > *conflict_threshold,
                "max_recommendations": max_recommendations,
                "combination_method": "dempster_shafer",
                "expert_count": mass_functions.len(),
            }))
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Dempster-Shafer evidence combination
// ═══════════════════════════════════════════════════════════════════════════

/// Extract a mass function from an expert's output.
///
/// If the output is a JSON object with numeric values, those are treated as
/// mass assignments. String outputs get a single hypothesis from hash-based
/// categorization. The expert weight scales the assigned mass.
fn extract_mass_function(
    output: &Value,
    weight: f64,
) -> std::collections::HashMap<String, f64> {
    let mut masses = std::collections::HashMap::new();
    let weight = weight.clamp(0.0, 1.0);

    match output {
        Value::Object(map) => {
            // Try to extract numeric "scores" or "recommendations"
            let mut total_mass = 0.0f64;
            for (key, val) in map {
                if let Some(score) = val.as_f64() {
                    let scaled = score.abs() * weight;
                    masses.insert(key.clone(), scaled);
                    total_mass += scaled;
                } else if let Some(s) = val.as_str() {
                    // String values → hash to hypothesis
                    masses.insert(s.to_string(), weight);
                    total_mass += weight;
                }
            }
            // Normalize so masses sum to 1.0
            if total_mass > 0.0 {
                for v in masses.values_mut() {
                    *v /= total_mass;
                }
            }
            // Remaining mass goes to uncertainty (frame Ω)
            let assigned: f64 = masses.values().sum();
            if assigned < 1.0 {
                masses.insert("uncertainty".to_string(), 1.0 - assigned);
            }
        }
        Value::String(s) => {
            // Single hypothesis with weighted mass
            masses.insert(s.clone(), weight);
            masses.insert("uncertainty".to_string(), 1.0 - weight);
        }
        _ => {
            let key = value_to_string(output);
            if !key.is_empty() {
                masses.insert(key, weight);
                masses.insert("uncertainty".to_string(), 1.0 - weight);
            } else {
                masses.insert("uncertainty".to_string(), 1.0);
            }
        }
    }

    masses
}

/// Dempster's rule of combination for two mass functions.
///
/// m_12(A) = (1/K) × Σ_{B∩C=A} m_1(B) × m_2(C)
/// where K = 1 - Σ_{B∩C=∅} m_1(B) × m_2(C)
///
/// Returns (combined_mass_function, conflict_K).
/// Complexity: O(f₁ × f₂) where f_i = |focal elements of m_i|.
fn dempster_combine(
    m1: &std::collections::HashMap<String, f64>,
    m2: &std::collections::HashMap<String, f64>,
) -> (std::collections::HashMap<String, f64>, f64) {
    let mut combined = std::collections::HashMap::new();
    let mut conflict_mass = 0.0f64;

    for (a, &ma) in m1 {
        for (b, &mb) in m2 {
            let product = ma * mb;
            if product < 1e-12 {
                continue;
            }

            // Compute intersection
            let intersection = if a == "uncertainty" {
                // Ω ∩ B = B
                Some(b.clone())
            } else if b == "uncertainty" {
                // A ∩ Ω = A
                Some(a.clone())
            } else if a == b {
                // A ∩ A = A
                Some(a.clone())
            } else {
                // A ∩ B = ∅ (distinct singletons)
                None
            };

            match intersection {
                Some(hyp) => {
                    *combined.entry(hyp).or_insert(0.0) += product;
                }
                None => {
                    conflict_mass += product;
                }
            }
        }
    }

    // Normalize by (1 - K)
    let normalization = 1.0 - conflict_mass;
    if normalization > 1e-12 {
        for v in combined.values_mut() {
            *v /= normalization;
        }
    }

    (combined, conflict_mass)
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn step_type_name(step: &PipelineStep) -> &'static str {
    match step {
        PipelineStep::Agent { .. } => "agent",
        PipelineStep::Parallel { .. } => "parallel",
        PipelineStep::Router { .. } => "router",
        PipelineStep::Gate { .. } => "gate",
        PipelineStep::Transform { .. } => "transform",
    }
}

/// Apply a simple transform expression to input text.
///
/// Supported expressions:
/// - `$.field` — Extract a JSON field
/// - `uppercase` / `lowercase` — Case transform
/// - `truncate(N)` — Truncate to N characters
/// - Anything else — treat as a template with `{input}` placeholder
fn apply_transform(expression: &str, input: &str) -> Result<String, PipelineError> {
    if expression.starts_with("$.") {
        // JSON field extraction
        let field = &expression[2..];
        if let Ok(val) = serde_json::from_str::<Value>(input) {
            val.get(field)
                .map(value_to_string)
                .ok_or_else(|| PipelineError::TransformFailed(format!("field '{}' not found", field)))
        } else {
            Err(PipelineError::TransformFailed("input is not valid JSON".into()))
        }
    } else if expression == "uppercase" {
        Ok(input.to_uppercase())
    } else if expression == "lowercase" {
        Ok(input.to_lowercase())
    } else if expression.starts_with("truncate(") && expression.ends_with(')') {
        let n: usize = expression[9..expression.len() - 1]
            .parse()
            .map_err(|_| PipelineError::TransformFailed("invalid truncate length".into()))?;
        Ok(input.chars().take(n).collect())
    } else {
        // Template replacement
        Ok(expression.replace("{input}", input))
    }
}

/// Simple deterministic float in [0, 1) for jitter.
/// Not cryptographic — uses thread-local state.
fn rand_f64() -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0x517c_c1b7_2722_0a95);
    let mut s = SEED.fetch_add(1, Ordering::Relaxed);
    // Xorshift64
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    SEED.store(s, Ordering::Relaxed);
    (s & 0x001F_FFFF_FFFF_FFFF) as f64 / (1u64 << 53) as f64
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::PipelineBuilder;

    /// Mock agent backend that echoes input with agent ID prefix.
    struct EchoBackend;

    #[async_trait]
    impl AgentBackend for EchoBackend {
        async fn execute_agent(
            &self,
            agent_id: &str,
            _skill_id: Option<&str>,
            input: &str,
            _timeout: Duration,
        ) -> Result<String, PipelineError> {
            Ok(format!("[{}] processed: {}", agent_id, input))
        }
    }

    /// Backend that fails on a specific agent.
    struct FailingBackend {
        fail_agent: String,
    }

    #[async_trait]
    impl AgentBackend for FailingBackend {
        async fn execute_agent(
            &self,
            agent_id: &str,
            _skill_id: Option<&str>,
            input: &str,
            _timeout: Duration,
        ) -> Result<String, PipelineError> {
            if agent_id == self.fail_agent {
                Err(PipelineError::AgentFailed {
                    agent_id: agent_id.to_string(),
                    detail: "intentional failure".into(),
                })
            } else {
                Ok(format!("[{}] {}", agent_id, input))
            }
        }
    }

    #[tokio::test]
    async fn linear_pipeline_executes() {
        let executor = PipelineExecutor::new(Arc::new(EchoBackend));
        let pipeline = PipelineBuilder::new("Linear Test")
            .agent("step1", None)
            .agent("step2", None)
            .build();

        let result = executor.execute(&pipeline, "hello").await.unwrap();
        assert!(result.success);
        assert_eq!(result.steps.len(), 2);
        // step1 processes "hello", step2 processes step1's output
        assert!(value_to_string(&result.final_output).contains("[step2]"));
        assert!(value_to_string(&result.final_output).contains("[step1]"));
    }

    #[tokio::test]
    async fn parallel_pipeline_executes() {
        let executor = PipelineExecutor::new(Arc::new(EchoBackend));
        let pipeline = PipelineBuilder::new("Parallel Test")
            .parallel(
                vec![
                    PipelineStep::Agent {
                        agent_id: "a".into(),
                        skill_id: None,
                        input_transform: None,
                        timeout_secs: 60,
                    },
                    PipelineStep::Agent {
                        agent_id: "b".into(),
                        skill_id: None,
                        input_transform: None,
                        timeout_secs: 60,
                    },
                ],
                MergeStrategy::Concat,
            )
            .build();

        let result = executor.execute(&pipeline, "data").await.unwrap();
        assert!(result.success);
        let output = value_to_string(&result.final_output);
        assert!(output.contains("[a]"));
        assert!(output.contains("[b]"));
    }

    #[tokio::test]
    async fn failfast_aborts_on_error() {
        let executor = PipelineExecutor::new(Arc::new(FailingBackend {
            fail_agent: "step2".into(),
        }));
        let pipeline = PipelineBuilder::new("FailFast")
            .agent("step1", None)
            .agent("step2", None)
            .agent("step3", None)
            .build();

        let result = executor.execute(&pipeline, "input").await.unwrap();
        assert!(!result.success);
        // step3 should not have executed
        assert!(result.steps.len() <= 2);
    }

    #[tokio::test]
    async fn continue_on_error_proceeds() {
        let executor = PipelineExecutor::new(Arc::new(FailingBackend {
            fail_agent: "step2".into(),
        }));
        let pipeline = PipelineBuilder::new("ContinueOnError")
            .error_policy(ErrorPolicy::ContinueOnError)
            .agent("step1", None)
            .agent("step2", None)
            .agent("step3", None)
            .build();

        let result = executor.execute(&pipeline, "input").await.unwrap();
        // All 3 steps should have been attempted
        assert_eq!(result.steps.len(), 3);
        assert!(!result.errors.is_empty());
    }

    #[tokio::test]
    async fn gate_auto_approves_by_default() {
        let executor = PipelineExecutor::new(Arc::new(EchoBackend));
        let pipeline = PipelineBuilder::new("Gate Test")
            .agent("step1", None)
            .gate("Review?", Duration::from_secs(5))
            .agent("step2", None)
            .build();

        let result = executor.execute(&pipeline, "data").await.unwrap();
        assert!(result.success);
        assert_eq!(result.steps.len(), 3);
    }

    #[tokio::test]
    async fn transform_step_works() {
        let executor = PipelineExecutor::new(Arc::new(EchoBackend));
        let pipeline = PipelineBuilder::new("Transform")
            .agent("step1", None)
            .transform("uppercase")
            .build();

        let result = executor.execute(&pipeline, "hello").await.unwrap();
        assert!(result.success);
        let final_out = value_to_string(&result.final_output);
        assert_eq!(final_out, final_out.to_uppercase());
    }
}
