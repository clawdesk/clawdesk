//! Orchestration Loop — connects the planner, dispatcher, and event bus.
//!
//! This is the connective tissue that transforms ClawDesk from a gateway
//! into an orchestration platform. The loop:
//!
//! ```text
//! User Request → Planner (DTGG) → Dispatcher → Execution → Rewriter → Loop
//! ```
//!
//! ## Architecture
//!
//! 1. On incoming request, the LLM decomposes intent into a task DAG.
//! 2. HEFT scheduler assigns tasks to agents by capability.
//! 3. `ready_nodes()` feeds into the `TaskDispatcher`.
//! 4. On completion/failure, `apply_rewrite_rules()` and loop.
//! 5. When `is_complete()`, format and deliver results.
//!
//! ## Complexity
//!
//! Per-iteration: O(V + E) for Kahn's algorithm + O(R) for ready nodes.
//! Total: bounded by task graph size × max rewrite iterations.

use crate::task_dispatcher::{
    DispatchResult, DispatchRoute, TaskDispatcher,
};
use clawdesk_planner::{
    DynamicTaskGraph, GraphSnapshot, HeftScheduler, NodeStatus, RewriteOutcome,
    RewriteRule, TaskNode,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Orchestrator configuration
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for the orchestration loop.
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Maximum number of rewrite iterations before giving up.
    pub max_rewrite_iterations: usize,
    /// Maximum number of concurrent task dispatches.
    pub max_concurrent_dispatches: usize,
    /// Number of virtual processors for HEFT scheduling.
    pub num_processors: usize,
    /// Whether to enable automatic retry on transient failures.
    pub enable_retry: bool,
    /// Maximum retry count per node.
    pub max_retries: u32,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            max_rewrite_iterations: 10,
            max_concurrent_dispatches: 5,
            num_processors: 5,
            enable_retry: true,
            max_retries: 2,
        }
    }
}

/// Status of the orchestration loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationStatus {
    /// Still executing tasks.
    Running,
    /// All tasks completed successfully.
    Completed,
    /// Some tasks failed after rewrite exhaustion.
    PartialFailure {
        completed: usize,
        failed: usize,
    },
    /// Orchestration was cancelled.
    Cancelled,
    /// Waiting for human input on escalated tasks.
    WaitingForHuman {
        escalated_nodes: Vec<String>,
    },
}

/// Event emitted during orchestration for monitoring/bus integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrchestrationEvent {
    /// A new task DAG was created.
    DagCreated {
        total_nodes: usize,
    },
    /// HEFT scheduling completed.
    Scheduled {
        assignments: Vec<(String, usize, f64)>,
    },
    /// A task was dispatched.
    TaskDispatched {
        node_id: String,
        route: String,
    },
    /// A task completed.
    TaskCompleted {
        node_id: String,
        duration_ms: u64,
    },
    /// A task failed.
    TaskFailed {
        node_id: String,
        error: String,
    },
    /// A rewrite rule was applied.
    RewriteApplied {
        rule: String,
        outcome: RewriteOutcome,
    },
    /// Orchestration finished.
    Finished {
        status: OrchestrationStatus,
    },
    /// A task was escalated to a human.
    Escalated {
        node_id: String,
        reason: String,
    },
}

/// Result of a full orchestration run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationResult {
    /// Final status.
    pub status: OrchestrationStatus,
    /// Outputs from completed nodes, keyed by node ID.
    pub outputs: HashMap<String, serde_json::Value>,
    /// Total wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Number of rewrite iterations performed.
    pub rewrite_count: usize,
    /// Final graph snapshot.
    pub final_snapshot: GraphSnapshot,
}

// ═══════════════════════════════════════════════════════════════════════════
// Orchestration Loop
// ═══════════════════════════════════════════════════════════════════════════

/// The orchestration loop — connects planner, dispatcher, and rewriter.
pub struct OrchestrationLoop {
    config: OrchestratorConfig,
    dispatcher: Arc<TaskDispatcher>,
    /// Channel for emitting orchestration events (for bus integration).
    event_tx: mpsc::UnboundedSender<OrchestrationEvent>,
    /// Rewrite rules to apply on task failure.
    rewrite_rules: Vec<RewriteRule>,
    /// Per-node retry counters.
    retry_counts: HashMap<String, u32>,
}

impl OrchestrationLoop {
    pub fn new(
        config: OrchestratorConfig,
        dispatcher: Arc<TaskDispatcher>,
        event_tx: mpsc::UnboundedSender<OrchestrationEvent>,
    ) -> Self {
        Self {
            config,
            dispatcher,
            event_tx,
            rewrite_rules: Vec::new(),
            retry_counts: HashMap::new(),
        }
    }

    /// Add a rewrite rule that will be applied when tasks fail.
    pub fn add_rewrite_rule(&mut self, rule: RewriteRule) {
        self.rewrite_rules.push(rule);
    }

    /// Run the orchestration loop on a task DAG.
    ///
    /// This is the main entry point. It:
    /// 1. Runs HEFT scheduling on the initial graph
    /// 2. Dispatches ready nodes  
    /// 3. Handles results (mark completed/failed)
    /// 4. Applies rewrite rules on failures
    /// 5. Loops until `is_complete()` or max iterations exceeded
    pub async fn run(&mut self, graph: &DynamicTaskGraph) -> OrchestrationResult {
        let start = std::time::Instant::now();
        let mut rewrite_count = 0;
        let mut outputs: HashMap<String, serde_json::Value> = HashMap::new();

        // Emit DAG created event
        let snapshot = graph.snapshot().await;
        let _ = self.event_tx.send(OrchestrationEvent::DagCreated {
            total_nodes: snapshot.nodes.len(),
        });

        // Phase 1: HEFT scheduling
        self.schedule_heft(graph).await;

        // Phase 2: Main execution loop
        loop {
            // Check completion
            if graph.is_complete().await {
                let final_snapshot = graph.snapshot().await;
                let failed: Vec<_> = final_snapshot
                    .nodes
                    .iter()
                    .filter(|n| n.status == NodeStatus::Failed)
                    .collect();

                let status = if failed.is_empty() {
                    OrchestrationStatus::Completed
                } else {
                    let completed = final_snapshot
                        .nodes
                        .iter()
                        .filter(|n| n.status == NodeStatus::Completed)
                        .count();
                    OrchestrationStatus::PartialFailure {
                        completed,
                        failed: failed.len(),
                    }
                };

                let _ = self.event_tx.send(OrchestrationEvent::Finished {
                    status: status.clone(),
                });

                return OrchestrationResult {
                    status,
                    outputs,
                    duration_ms: start.elapsed().as_millis() as u64,
                    rewrite_count,
                    final_snapshot,
                };
            }

            // Check rewrite iteration limit
            if rewrite_count >= self.config.max_rewrite_iterations {
                warn!(
                    rewrite_count,
                    max = self.config.max_rewrite_iterations,
                    "orchestration: max rewrite iterations reached"
                );
                let final_snapshot = graph.snapshot().await;
                let completed = final_snapshot
                    .nodes
                    .iter()
                    .filter(|n| n.status == NodeStatus::Completed)
                    .count();
                let failed = final_snapshot
                    .nodes
                    .iter()
                    .filter(|n| n.status == NodeStatus::Failed)
                    .count();

                let status = OrchestrationStatus::PartialFailure { completed, failed };
                let _ = self.event_tx.send(OrchestrationEvent::Finished {
                    status: status.clone(),
                });

                return OrchestrationResult {
                    status,
                    outputs,
                    duration_ms: start.elapsed().as_millis() as u64,
                    rewrite_count,
                    final_snapshot,
                };
            }

            // Get ready nodes
            let ready = graph.ready_nodes().await;
            if ready.is_empty() {
                // Check for deadlock: no ready nodes but graph not complete
                let snapshot = graph.snapshot().await;
                let has_pending = snapshot
                    .nodes
                    .iter()
                    .any(|n| n.status == NodeStatus::Pending);

                if has_pending {
                    // Check for escalated (InputRequired) tasks
                    let escalated: Vec<_> = snapshot
                        .nodes
                        .iter()
                        .filter(|n| {
                            n.metadata
                                .get("awaiting_human")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                        })
                        .map(|n| n.id.clone())
                        .collect();

                    if !escalated.is_empty() {
                        let status = OrchestrationStatus::WaitingForHuman {
                            escalated_nodes: escalated,
                        };
                        let final_snapshot = graph.snapshot().await;
                        let _ = self.event_tx.send(OrchestrationEvent::Finished {
                            status: status.clone(),
                        });
                        return OrchestrationResult {
                            status,
                            outputs,
                            duration_ms: start.elapsed().as_millis() as u64,
                            rewrite_count,
                            final_snapshot,
                        };
                    }

                    // Deadlock: pending nodes but none ready → apply PruneAllFailed
                    warn!("orchestration: deadlock detected, applying PruneAllFailed");
                    let rule = RewriteRule::PruneAllFailed;
                    let outcome = rule.apply(graph).await;
                    if outcome.applied {
                        rewrite_count += 1;
                        let _ = self.event_tx.send(OrchestrationEvent::RewriteApplied {
                            rule: "PruneAllFailed".into(),
                            outcome,
                        });
                    }
                    continue;
                }

                // All running — wait for in-flight tasks
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                continue;
            }

            // Dispatch ready nodes (bounded concurrency)
            let dispatch_count = ready.len().min(self.config.max_concurrent_dispatches);
            let nodes_to_dispatch = &ready[..dispatch_count];

            let mut dispatch_set: JoinSet<(String, Result<DispatchResult, String>)> = JoinSet::new();
            for node_id in nodes_to_dispatch {
                // Get node details
                let node = graph.get_node(node_id).await;
                if let Some(node) = node {
                    // Mark as running
                    graph.mark_running(node_id).await;

                    let dispatcher = Arc::clone(&self.dispatcher);
                    let nid = node_id.clone();
                    let desc = node.description.clone();
                    let input = node.output.clone().unwrap_or(serde_json::json!({}));
                    let metadata = serde_json::to_value(&node.metadata).unwrap_or_default();

                    let _ = self.event_tx.send(OrchestrationEvent::TaskDispatched {
                        node_id: nid.clone(),
                        route: format!("{}", desc),
                    });

                    let nid_owned = node_id.clone();
                    dispatch_set.spawn(async move {
                        let result = dispatcher.dispatch(&nid, &desc, input, &metadata).await;
                        (nid_owned, Ok(result))
                    });
                }
            }

            // Collect results as they complete (no head-of-line blocking)
            while let Some(join_result) = dispatch_set.join_next().await {
                let (node_id, dispatch_result) = match join_result {
                    Ok(pair) => pair,
                    Err(e) => {
                        // JoinError means the task panicked — can't recover node_id
                        warn!(error = %e, "dispatched task panicked");
                        continue;
                    }
                };
                let result = match dispatch_result {
                    Ok(r) => r,
                    Err(e) => {
                        let error_msg = format!("dispatch error: {}", e);
                        graph.mark_failed(&node_id, error_msg.clone()).await;
                        let _ = self.event_tx.send(OrchestrationEvent::TaskFailed {
                            node_id: node_id.clone(),
                            error: error_msg,
                        });
                        self.apply_rewrite_rules(graph, &node_id, &mut rewrite_count)
                            .await;
                        continue;
                    }
                };

                if result.success {
                    let output = result
                        .output
                        .unwrap_or(serde_json::json!({"status": "completed"}));
                    graph
                        .mark_completed(&node_id, output.clone())
                        .await;
                    outputs.insert(node_id.clone(), output);
                    let _ =
                        self.event_tx.send(OrchestrationEvent::TaskCompleted {
                            node_id: node_id.clone(),
                            duration_ms: result.duration_ms,
                        });
                    info!(node_id, duration_ms = result.duration_ms, "task completed");
                } else {
                    let error_msg = result
                        .error
                        .unwrap_or_else(|| "unknown error".to_string());

                    // Check if this was an escalation
                    if matches!(result.route, DispatchRoute::HumanEscalation { .. }) {
                        let _ = self.event_tx.send(OrchestrationEvent::Escalated {
                            node_id: node_id.clone(),
                            reason: error_msg.clone(),
                        });
                        continue;
                    }

                    // Handle retries
                    let retry_count =
                        self.retry_counts.entry(node_id.clone()).or_insert(0);
                    if self.config.enable_retry
                        && *retry_count < self.config.max_retries
                    {
                        *retry_count += 1;
                        info!(
                            node_id,
                            retry = *retry_count,
                            max = self.config.max_retries,
                            "retrying failed task"
                        );
                        graph.mark_failed(&node_id, error_msg.clone()).await;
                        self.apply_rewrite_rules(graph, &node_id, &mut rewrite_count)
                            .await;
                    } else {
                        graph.mark_failed(&node_id, error_msg.clone()).await;
                        let _ = self.event_tx.send(OrchestrationEvent::TaskFailed {
                            node_id: node_id.clone(),
                            error: error_msg.clone(),
                        });
                        self.apply_rewrite_rules(graph, &node_id, &mut rewrite_count)
                            .await;
                    }
                }
            }
        }
    }

    /// Run HEFT scheduling on the current graph state.
    async fn schedule_heft(&self, graph: &DynamicTaskGraph) {
        let snapshot = graph.snapshot().await;
        let nodes: Vec<&TaskNode> = snapshot.nodes.iter().collect();

        if nodes.is_empty() {
            tracing::warn!("HEFT scheduling called on empty graph — possible race condition");
            let _ = self.event_tx.send(OrchestrationEvent::Finished {
                status: OrchestrationStatus::PartialFailure {
                    completed: 0,
                    failed: 0,
                },
            });
            return;
        }
        let edges: Vec<(&str, &str, f64)> = snapshot
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.comm_cost))
            .collect();

        let schedule =
            HeftScheduler::schedule(&nodes, &edges, self.config.num_processors);

        let assignments: Vec<(String, usize, f64)> = schedule
            .iter()
            .map(|(id, p, t)| (id.clone(), *p, *t))
            .collect();

        debug!(
            processors = self.config.num_processors,
            nodes = nodes.len(),
            "HEFT scheduling complete"
        );

        let _ = self
            .event_tx
            .send(OrchestrationEvent::Scheduled { assignments });
    }

    /// Apply rewrite rules for a failed node.
    async fn apply_rewrite_rules(
        &self,
        graph: &DynamicTaskGraph,
        failed_node_id: &str,
        rewrite_count: &mut usize,
    ) {
        // Always try PruneOnFailure for the specific node
        let prune_rule = RewriteRule::PruneOnFailure {
            target: failed_node_id.to_string(),
        };
        let outcome = prune_rule.apply(graph).await;
        if outcome.applied {
            *rewrite_count += 1;
            info!(
                node_id = failed_node_id,
                pruned = outcome.nodes_pruned.len(),
                "rewrite: pruned failed node and descendants"
            );
            let _ = self.event_tx.send(OrchestrationEvent::RewriteApplied {
                rule: "PruneOnFailure".into(),
                outcome,
            });
        }

        // Apply custom rewrite rules
        for rule in &self.rewrite_rules {
            let outcome = rule.apply(graph).await;
            if outcome.applied {
                *rewrite_count += 1;
                let _ = self.event_tx.send(OrchestrationEvent::RewriteApplied {
                    rule: format!("{:?}", rule),
                    outcome,
                });
            }
        }
    }
}

/// Helper to build a task DAG from a list of task descriptions.
///
/// Used by the gateway to convert an LLM-generated plan into a
/// `DynamicTaskGraph` that the orchestration loop can execute.
pub async fn build_dag_from_plan(
    tasks: Vec<TaskPlan>,
    edges: Vec<(String, String)>,
) -> DynamicTaskGraph {
    let graph = DynamicTaskGraph::new();

    for task in &tasks {
        let mut node = TaskNode::new(&task.id, &task.description);
        if let Some(ref agent) = task.agent_id {
            node = node.with_agent(agent);
        }
        if let Some(weight) = task.weight {
            node = node.with_weight(weight);
        }
        if let Some(tokens) = task.estimated_tokens {
            node = node.with_estimated_tokens(tokens);
        }
        node.metadata = task.metadata.clone();
        graph.insert_node(node).await;
    }

    for (from, to) in &edges {
        graph.add_edge(from, to, 0.1).await;
    }

    graph
}

/// A task plan entry (typically generated by an LLM).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub id: String,
    pub description: String,
    pub agent_id: Option<String>,
    pub weight: Option<f64>,
    pub estimated_tokens: Option<u64>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}
