//! Async fan-out executor — runs `FanoutPlan` phases against real agents.
//!
//! ## Overview
//!
//! The `fanout` module defines plan structures (`FanoutPlan`, `FanoutPhase`,
//! `FanoutConfig`) but has no runtime executor. This module bridges that gap:
//! it takes a `FanoutPlan` and dispatches each phase to agents via the
//! `AgentBackend` trait (from `clawdesk-agents`), collecting outputs and
//! applying the configured merge/vote strategy.
//!
//! ## Scheduling model
//!
//! Given k agents and a parallelism cap p, each `Concurrent` phase launches
//! min(k, p) tasks via `JoinSet`. Sequential pipelines degenerate to
//! p = 1 (one agent per phase, output piped forward).
//!
//! ## Error handling
//!
//! Individual agent failures do NOT abort the entire fan-out (unless all
//! agents in a phase fail). Partial results are collected and passed to
//! the merge/vote step with `success = false` markers.

use crate::fanout::{
    AgentOutput, FanoutPhase, FanoutPlan, FanoutResult, FanoutStrategy,
    merge_concatenate, merge_first_success, merge_vote,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Agent dispatch trait
// ═══════════════════════════════════════════════════════════════════════════

/// Dispatch a task to an agent and return the text output.
///
/// This is deliberately lighter than `AgentBackend` from clawdesk-agents —
/// the gateway fan-out only needs "send input, get output". Production
/// implementations can wrap an HTTP client hitting the A2A `send_task` RPC
/// or delegate locally to `AgentRunner`.
#[async_trait]
pub trait FanoutDispatch: Send + Sync + 'static {
    /// Execute a single agent with the given input text.
    async fn dispatch(
        &self,
        agent_id: &str,
        input: &str,
        timeout: Duration,
    ) -> Result<String, FanoutError>;
}

// ═══════════════════════════════════════════════════════════════════════════
// Error type
// ═══════════════════════════════════════════════════════════════════════════

/// Errors during fan-out execution.
#[derive(Debug, thiserror::Error)]
pub enum FanoutError {
    #[error("agent '{agent_id}' failed: {detail}")]
    AgentFailed { agent_id: String, detail: String },

    #[error("phase timed out after {timeout_secs}s")]
    PhaseTimeout { timeout_secs: u64 },

    #[error("all agents in phase failed")]
    AllAgentsFailed,

    #[error("merge agent '{agent_id}' failed: {detail}")]
    MergeFailed { agent_id: String, detail: String },

    #[error("vote did not reach threshold ({threshold})")]
    VoteBelowThreshold { threshold: f64 },

    #[error("cancelled")]
    Cancelled,
}

// ═══════════════════════════════════════════════════════════════════════════
// Fan-out executor
// ═══════════════════════════════════════════════════════════════════════════

/// Executes a `FanoutPlan` by dispatching each phase to agents.
pub struct FanoutExecutor<D: FanoutDispatch> {
    dispatch: Arc<D>,
    /// Maximum concurrent agents within a single `Concurrent` phase.
    max_parallelism: usize,
    /// Cancellation token for cooperative shutdown.
    cancel: tokio_util::sync::CancellationToken,
}

impl<D: FanoutDispatch> FanoutExecutor<D> {
    /// Create a new executor with the given dispatch backend.
    pub fn new(dispatch: Arc<D>) -> Self {
        Self {
            dispatch,
            max_parallelism: 10,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// Set maximum parallelism for concurrent phases.
    pub fn with_max_parallelism(mut self, max: usize) -> Self {
        self.max_parallelism = max.max(1);
        self
    }

    /// Set the cancellation token.
    pub fn with_cancellation(mut self, cancel: tokio_util::sync::CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Execute the full fan-out plan.
    ///
    /// Returns a `FanoutResult` with per-agent outputs and the final
    /// merged output.
    pub async fn execute(&self, plan: &FanoutPlan) -> Result<FanoutResult, FanoutError> {
        let total_start = Instant::now();
        let timeout = Duration::from_secs(plan.config.timeout_secs);
        let mut all_outputs: HashMap<String, AgentOutput> = HashMap::new();
        let mut current_input = plan.input.clone();

        info!(
            strategy = ?plan.config.strategy,
            agents = ?plan.config.agents,
            phases = plan.phases.len(),
            "starting fan-out execution"
        );

        for (phase_idx, phase) in plan.phases.iter().enumerate() {
            if self.cancel.is_cancelled() {
                return Err(FanoutError::Cancelled);
            }

            // Enforce global timeout
            if total_start.elapsed() > timeout {
                warn!("global timeout reached at phase {phase_idx}");
                return Err(FanoutError::PhaseTimeout {
                    timeout_secs: plan.config.timeout_secs,
                });
            }

            let remaining = timeout.saturating_sub(total_start.elapsed());

            match phase {
                FanoutPhase::Concurrent(agent_ids) => {
                    let phase_outputs = self
                        .execute_concurrent(agent_ids, &current_input, remaining)
                        .await?;

                    // For sequential strategy, pipe output of each phase to next
                    if plan.config.strategy == FanoutStrategy::Sequential {
                        if let Some(output) = phase_outputs.values().find(|o| o.success) {
                            current_input = output.output.clone();
                        }
                    } else {
                        // For parallel/merge/vote, accumulate
                        let successful: Vec<_> =
                            phase_outputs.values().filter(|o| o.success).collect();
                        if !successful.is_empty() {
                            current_input = merge_concatenate(
                                &phase_outputs.values().cloned().collect::<Vec<_>>(),
                            );
                        }
                    }

                    all_outputs.extend(phase_outputs);
                }

                FanoutPhase::MergeStep(merge_agent_id) => {
                    debug!(merge_agent = %merge_agent_id, "executing merge step");
                    let merge_start = Instant::now();

                    match self
                        .dispatch
                        .dispatch(merge_agent_id, &current_input, remaining)
                        .await
                    {
                        Ok(output) => {
                            let duration_ms = merge_start.elapsed().as_millis() as u64;
                            current_input = output.clone();
                            all_outputs.insert(
                                merge_agent_id.clone(),
                                AgentOutput {
                                    agent_id: merge_agent_id.clone(),
                                    output,
                                    duration_ms,
                                    success: true,
                                    error: None,
                                },
                            );
                        }
                        Err(e) => {
                            return Err(FanoutError::MergeFailed {
                                agent_id: merge_agent_id.clone(),
                                detail: e.to_string(),
                            });
                        }
                    }
                }

                FanoutPhase::VoteStep { threshold } => {
                    debug!(threshold, "executing vote step");
                    let outputs_vec: Vec<AgentOutput> = all_outputs.values().cloned().collect();
                    match merge_vote(&outputs_vec, *threshold) {
                        Some(winner) => {
                            current_input = winner;
                        }
                        None => {
                            return Err(FanoutError::VoteBelowThreshold {
                                threshold: *threshold,
                            });
                        }
                    }
                }
            }
        }

        let total_duration_ms = total_start.elapsed().as_millis() as u64;

        info!(
            total_duration_ms,
            agent_count = all_outputs.len(),
            "fan-out execution complete"
        );

        Ok(FanoutResult {
            agent_outputs: all_outputs,
            final_output: current_input,
            strategy: plan.config.strategy,
            total_duration_ms,
        })
    }

    /// Run a set of agents concurrently via JoinSet, respecting max_parallelism.
    async fn execute_concurrent(
        &self,
        agent_ids: &[String],
        input: &str,
        timeout: Duration,
    ) -> Result<HashMap<String, AgentOutput>, FanoutError> {
        let mut results: HashMap<String, AgentOutput> = HashMap::new();

        // Process in chunks of max_parallelism
        for chunk in agent_ids.chunks(self.max_parallelism) {
            let mut join_set = JoinSet::new();

            for agent_id in chunk {
                let dispatch = Arc::clone(&self.dispatch);
                let agent_id = agent_id.clone();
                let input = input.to_string();
                let cancel = self.cancel.clone();

                join_set.spawn(async move {
                    let start = Instant::now();

                    if cancel.is_cancelled() {
                        return AgentOutput {
                            agent_id: agent_id.clone(),
                            output: String::new(),
                            duration_ms: 0,
                            success: false,
                            error: Some("cancelled".into()),
                        };
                    }

                    match dispatch.dispatch(&agent_id, &input, timeout).await {
                        Ok(output) => {
                            let duration_ms = start.elapsed().as_millis() as u64;
                            debug!(agent = %agent_id, duration_ms, "agent completed");
                            AgentOutput {
                                agent_id,
                                output,
                                duration_ms,
                                success: true,
                                error: None,
                            }
                        }
                        Err(e) => {
                            let duration_ms = start.elapsed().as_millis() as u64;
                            error!(agent = %agent_id, error = %e, "agent failed");
                            AgentOutput {
                                agent_id,
                                output: String::new(),
                                duration_ms,
                                success: false,
                                error: Some(e.to_string()),
                            }
                        }
                    }
                });
            }

            // Collect results from this chunk
            while let Some(result) = join_set.join_next().await {
                match result {
                    Ok(output) => {
                        results.insert(output.agent_id.clone(), output);
                    }
                    Err(e) => {
                        warn!(error = %e, "JoinSet task panicked");
                    }
                }
            }
        }

        // Check if at least one succeeded
        let any_success = results.values().any(|o| o.success);
        if !any_success && !results.is_empty() {
            return Err(FanoutError::AllAgentsFailed);
        }

        Ok(results)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fanout::{plan_fanout, FanoutConfig, FanoutStrategy};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock dispatch that echoes agent_id + input.
    struct EchoDispatch {
        call_count: AtomicUsize,
    }

    impl EchoDispatch {
        fn new() -> Self {
            Self {
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl FanoutDispatch for EchoDispatch {
        async fn dispatch(
            &self,
            agent_id: &str,
            input: &str,
            _timeout: Duration,
        ) -> Result<String, FanoutError> {
            self.call_count.fetch_add(1, Ordering::Relaxed);
            Ok(format!("[{agent_id}]: {input}"))
        }
    }

    /// Mock dispatch that fails for specific agents.
    struct FailingDispatch {
        fail_agents: Vec<String>,
    }

    #[async_trait]
    impl FanoutDispatch for FailingDispatch {
        async fn dispatch(
            &self,
            agent_id: &str,
            input: &str,
            _timeout: Duration,
        ) -> Result<String, FanoutError> {
            if self.fail_agents.contains(&agent_id.to_string()) {
                Err(FanoutError::AgentFailed {
                    agent_id: agent_id.into(),
                    detail: "mock failure".into(),
                })
            } else {
                Ok(format!("[{agent_id}]: {input}"))
            }
        }
    }

    fn parallel_config(agents: Vec<&str>) -> FanoutConfig {
        FanoutConfig {
            strategy: FanoutStrategy::Parallel,
            agents: agents.into_iter().map(String::from).collect(),
            merge_agent: None,
            timeout_secs: 120,
            max_concurrent: 10,
            vote_threshold: 0.5,
        }
    }

    #[tokio::test]
    async fn parallel_fanout_dispatches_all_agents() {
        let dispatch = Arc::new(EchoDispatch::new());
        let executor = FanoutExecutor::new(Arc::clone(&dispatch));

        let config = parallel_config(vec!["a", "b", "c"]);
        let plan = plan_fanout(&config, "hello").unwrap();

        let result = executor.execute(&plan).await.unwrap();

        assert_eq!(result.agent_outputs.len(), 3);
        assert!(result.agent_outputs.contains_key("a"));
        assert!(result.agent_outputs.contains_key("b"));
        assert!(result.agent_outputs.contains_key("c"));
        assert_eq!(dispatch.call_count.load(Ordering::Relaxed), 3);
        assert!(result.total_duration_ms < 5000); // should be fast
    }

    #[tokio::test]
    async fn sequential_fanout_chains_output() {
        let dispatch = Arc::new(EchoDispatch::new());
        let executor = FanoutExecutor::new(Arc::clone(&dispatch));

        let config = FanoutConfig {
            strategy: FanoutStrategy::Sequential,
            agents: vec!["step-1".into(), "step-2".into()],
            merge_agent: None,
            timeout_secs: 120,
            max_concurrent: 10,
            vote_threshold: 0.5,
        };
        let plan = plan_fanout(&config, "start").unwrap();

        let result = executor.execute(&plan).await.unwrap();

        // step-2 should have received step-1's output as input
        let step2_output = &result.agent_outputs["step-2"];
        assert!(step2_output.output.contains("step-1"));
        assert!(step2_output.output.contains("start"));
    }

    #[tokio::test]
    async fn merge_fanout_calls_merge_agent() {
        let dispatch = Arc::new(EchoDispatch::new());
        let executor = FanoutExecutor::new(Arc::clone(&dispatch));

        let config = FanoutConfig {
            strategy: FanoutStrategy::Merge,
            agents: vec!["a".into(), "b".into()],
            merge_agent: Some("synthesiser".into()),
            timeout_secs: 120,
            max_concurrent: 10,
            vote_threshold: 0.5,
        };
        let plan = plan_fanout(&config, "data").unwrap();

        let result = executor.execute(&plan).await.unwrap();

        assert!(result.agent_outputs.contains_key("synthesiser"));
        assert_eq!(dispatch.call_count.load(Ordering::Relaxed), 3); // a, b, synthesiser
    }

    #[tokio::test]
    async fn partial_failure_still_produces_result() {
        let dispatch = Arc::new(FailingDispatch {
            fail_agents: vec!["bad".into()],
        });
        let executor = FanoutExecutor::new(dispatch);

        let config = parallel_config(vec!["good", "bad"]);
        let plan = plan_fanout(&config, "test").unwrap();

        let result = executor.execute(&plan).await.unwrap();

        assert!(result.agent_outputs["good"].success);
        assert!(!result.agent_outputs["bad"].success);
        assert!(result.final_output.contains("[good]"));
    }

    #[tokio::test]
    async fn all_agents_fail_returns_error() {
        let dispatch = Arc::new(FailingDispatch {
            fail_agents: vec!["x".into(), "y".into()],
        });
        let executor = FanoutExecutor::new(dispatch);

        let config = parallel_config(vec!["x", "y"]);
        let plan = plan_fanout(&config, "test").unwrap();

        let result = executor.execute(&plan).await;
        assert!(matches!(result, Err(FanoutError::AllAgentsFailed)));
    }

    #[tokio::test]
    async fn cancellation_stops_execution() {
        let dispatch = Arc::new(EchoDispatch::new());
        let cancel = tokio_util::sync::CancellationToken::new();
        let executor = FanoutExecutor::new(Arc::clone(&dispatch))
            .with_cancellation(cancel.clone());

        // Cancel before execution
        cancel.cancel();

        let config = parallel_config(vec!["a"]);
        let plan = plan_fanout(&config, "test").unwrap();

        let result = executor.execute(&plan).await;
        assert!(matches!(result, Err(FanoutError::Cancelled)));
    }

    #[tokio::test]
    async fn max_parallelism_respected() {
        use std::sync::atomic::AtomicU32;

        struct ConcurrencyTracker {
            concurrent: AtomicU32,
            max_concurrent: AtomicU32,
        }

        #[async_trait]
        impl FanoutDispatch for ConcurrencyTracker {
            async fn dispatch(
                &self,
                agent_id: &str,
                _input: &str,
                _timeout: Duration,
            ) -> Result<String, FanoutError> {
                let current = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_concurrent.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                self.concurrent.fetch_sub(1, Ordering::SeqCst);
                Ok(format!("[{agent_id}]"))
            }
        }

        let tracker = Arc::new(ConcurrencyTracker {
            concurrent: AtomicU32::new(0),
            max_concurrent: AtomicU32::new(0),
        });

        let executor = FanoutExecutor::new(Arc::clone(&tracker))
            .with_max_parallelism(2);

        // 4 agents, max parallelism 2 → should process in 2 chunks
        let config = parallel_config(vec!["a", "b", "c", "d"]);
        let plan = plan_fanout(&config, "input").unwrap();

        let result = executor.execute(&plan).await.unwrap();

        assert_eq!(result.agent_outputs.len(), 4);
        // Max concurrent should be ≤ 2
        assert!(tracker.max_concurrent.load(Ordering::SeqCst) <= 2);
    }
}
