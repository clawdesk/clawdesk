//! Graph rewriting rules for adaptive replanning.
//!
//! Each `RewriteRule` encodes a graph transformation that is applied
//! when a condition is met. Rules are composable and applied atomically.

use crate::dtgg::{DynamicTaskGraph, NodeId, NodeStatus, TaskNode};
use serde::{Deserialize, Serialize};
use tracing::info;
use std::fmt;

/// Outcome of applying a single rewrite rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteOutcome {
    pub rule_name: String,
    pub applied: bool,
    pub nodes_inserted: Vec<NodeId>,
    pub nodes_pruned: Vec<NodeId>,
    pub edges_added: Vec<(NodeId, NodeId)>,
    pub edges_removed: Vec<(NodeId, NodeId)>,
    pub description: String,
}

impl RewriteOutcome {
    pub fn noop(rule_name: &str) -> Self {
        Self {
            rule_name: rule_name.to_string(),
            applied: false,
            nodes_inserted: Vec::new(),
            nodes_pruned: Vec::new(),
            edges_added: Vec::new(),
            edges_removed: Vec::new(),
            description: "no action taken".to_string(),
        }
    }
}

/// A graph rewriting rule.
#[derive(Clone)]
pub enum RewriteRule {
    /// Prune a failed node and all unreachable descendants.
    PruneOnFailure {
        /// Node to check.
        target: NodeId,
    },

    /// Insert a new node between two existing nodes.
    InsertBetween {
        predecessor: NodeId,
        successor: NodeId,
        new_node: TaskNode,
        comm_cost_before: f64,
        comm_cost_after: f64,
    },

    /// Replace a failed node with an alternative subgraph.
    SubstituteOnFailure {
        failed_node: NodeId,
        replacement_nodes: Vec<TaskNode>,
        replacement_edges: Vec<(NodeId, NodeId, f64)>,
        /// Re-wire predecessors of the failed node to the first replacement.
        entry_node: NodeId,
        /// Re-wire the last replacement to successors of the failed node.
        exit_node: NodeId,
    },

    /// Add a parallel branch alongside an existing node for redundancy.
    AddRedundantBranch {
        original_node: NodeId,
        redundant_node: TaskNode,
    },

    /// Remove an edge to break a dependency.
    BreakDependency {
        from: NodeId,
        to: NodeId,
    },

    /// Auto-prune all failed branches in the graph.
    PruneAllFailed,

    /// Substitute a failed/low-confidence task with a human escalation subgraph.
    ///
    /// Replaces the target node with a two-node subgraph:
    /// `notify_human → await_response`
    ///
    /// The `notify_human` node dispatches to the appropriate channel.
    /// The `await_response` node enters InputRequired state and the
    /// durable runtime checkpoints the graph until the human responds.
    HumanEscalation {
        /// Node to escalate.
        target: NodeId,
        /// Reason for escalation (shown to the human).
        reason: String,
        /// Channel to notify (e.g., "slack", "telegram", "email").
        channel: Option<String>,
    },

    /// Custom rewrite via a closure (not serializable).
    Custom {
        name: String,
        /// Applied asynchronously.
        #[allow(clippy::type_complexity)]
        apply_fn: std::sync::Arc<
            dyn Fn(&DynamicTaskGraph) -> std::pin::Pin<Box<dyn std::future::Future<Output = RewriteOutcome> + Send + '_>>
                + Send
                + Sync,
        >,
    },
}

impl fmt::Debug for RewriteRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PruneOnFailure { target } => f.debug_struct("PruneOnFailure").field("target", target).finish(),
            Self::InsertBetween { predecessor, successor, new_node, .. } => f.debug_struct("InsertBetween").field("predecessor", predecessor).field("successor", successor).field("new_node", &new_node.id).finish(),
            Self::SubstituteOnFailure { failed_node, .. } => f.debug_struct("SubstituteOnFailure").field("failed_node", failed_node).finish(),
            Self::AddRedundantBranch { original_node, .. } => f.debug_struct("AddRedundantBranch").field("original_node", original_node).finish(),
            Self::BreakDependency { from, to } => f.debug_struct("BreakDependency").field("from", from).field("to", to).finish(),
            Self::PruneAllFailed => write!(f, "PruneAllFailed"),
            Self::HumanEscalation { target, reason, channel } => f.debug_struct("HumanEscalation").field("target", target).field("reason", reason).field("channel", channel).finish(),
            Self::Custom { name, .. } => f.debug_struct("Custom").field("name", name).finish(),
        }
    }
}

impl RewriteRule {
    /// Apply this rule to the graph.
    pub async fn apply(&self, graph: &DynamicTaskGraph) -> RewriteOutcome {
        match self {
            RewriteRule::PruneOnFailure { target } => {
                let node = graph.get_node(target).await;
                if let Some(n) = node {
                    if n.status == NodeStatus::Failed {
                        let pruned = graph.prune_node(target).await;
                        info!(target = target, pruned = pruned.len(), "PruneOnFailure applied");
                        return RewriteOutcome {
                            rule_name: "PruneOnFailure".into(),
                            applied: true,
                            nodes_pruned: pruned,
                            nodes_inserted: Vec::new(),
                            edges_added: Vec::new(),
                            edges_removed: Vec::new(),
                            description: format!("pruned failed node '{target}' and descendants"),
                        };
                    }
                }
                RewriteOutcome::noop("PruneOnFailure")
            }

            RewriteRule::InsertBetween {
                predecessor,
                successor,
                new_node,
                comm_cost_before,
                comm_cost_after,
            } => {
                let new_id = new_node.id.clone();
                graph.remove_edge(predecessor, successor).await;
                graph.insert_node(new_node.clone()).await;
                graph.add_edge(predecessor, &new_id, *comm_cost_before).await;
                graph.add_edge(&new_id, successor, *comm_cost_after).await;

                info!(
                    new_node = new_id,
                    predecessor = predecessor,
                    successor = successor,
                    "InsertBetween applied"
                );
                RewriteOutcome {
                    rule_name: "InsertBetween".into(),
                    applied: true,
                    nodes_inserted: vec![new_id.clone()],
                    nodes_pruned: Vec::new(),
                    edges_added: vec![
                        (predecessor.clone(), new_id.clone()),
                        (new_id, successor.clone()),
                    ],
                    edges_removed: vec![(predecessor.clone(), successor.clone())],
                    description: format!("inserted node between '{predecessor}' and '{successor}'"),
                }
            }

            RewriteRule::SubstituteOnFailure {
                failed_node,
                replacement_nodes,
                replacement_edges,
                entry_node,
                exit_node,
            } => {
                let node = graph.get_node(failed_node).await;
                if let Some(n) = node {
                    if n.status != NodeStatus::Failed {
                        return RewriteOutcome::noop("SubstituteOnFailure");
                    }
                } else {
                    return RewriteOutcome::noop("SubstituteOnFailure");
                }

                // Get predecessors and successors of the failed node from snapshot.
                let snap = graph.snapshot().await;
                let preds: Vec<String> = snap
                    .edges
                    .iter()
                    .filter(|e| e.to == *failed_node)
                    .map(|e| e.from.clone())
                    .collect();
                let succs: Vec<String> = snap
                    .edges
                    .iter()
                    .filter(|e| e.from == *failed_node)
                    .map(|e| e.to.clone())
                    .collect();

                // Prune the failed node.
                graph.prune_node(failed_node).await;

                // Insert replacement subgraph.
                let mut inserted = Vec::new();
                for rn in replacement_nodes {
                    graph.insert_node(rn.clone()).await;
                    inserted.push(rn.id.clone());
                }
                let mut edges_added = Vec::new();
                for (from, to, cost) in replacement_edges {
                    graph.add_edge(from, to, *cost).await;
                    edges_added.push((from.clone(), to.clone()));
                }

                // Wire predecessors → entry_node.
                for pred in &preds {
                    graph.add_edge(pred, entry_node, 0.1).await;
                    edges_added.push((pred.clone(), entry_node.clone()));
                }
                // Wire exit_node → successors.
                for succ in &succs {
                    // Only wire to non-pruned successors.
                    if let Some(sn) = graph.get_node(succ).await {
                        if sn.status != NodeStatus::Pruned {
                            graph.add_edge(exit_node, succ, 0.1).await;
                            edges_added.push((exit_node.clone(), succ.clone()));
                        }
                    }
                }

                info!(
                    failed = failed_node,
                    replacements = inserted.len(),
                    "SubstituteOnFailure applied"
                );
                RewriteOutcome {
                    rule_name: "SubstituteOnFailure".into(),
                    applied: true,
                    nodes_inserted: inserted,
                    nodes_pruned: vec![failed_node.clone()],
                    edges_added,
                    edges_removed: Vec::new(),
                    description: format!(
                        "substituted failed node '{failed_node}' with {} replacement nodes",
                        replacement_nodes.len()
                    ),
                }
            }

            RewriteRule::AddRedundantBranch {
                original_node,
                redundant_node,
            } => {
                let snap = graph.snapshot().await;
                let preds: Vec<String> = snap
                    .edges
                    .iter()
                    .filter(|e| e.to == *original_node)
                    .map(|e| e.from.clone())
                    .collect();
                let succs: Vec<String> = snap
                    .edges
                    .iter()
                    .filter(|e| e.from == *original_node)
                    .map(|e| e.to.clone())
                    .collect();

                let new_id = redundant_node.id.clone();
                graph.insert_node(redundant_node.clone()).await;

                let mut edges_added = Vec::new();
                for pred in &preds {
                    graph.add_edge(pred, &new_id, 0.0).await;
                    edges_added.push((pred.clone(), new_id.clone()));
                }
                for succ in &succs {
                    graph.add_edge(&new_id, succ, 0.0).await;
                    edges_added.push((new_id.clone(), succ.clone()));
                }

                RewriteOutcome {
                    rule_name: "AddRedundantBranch".into(),
                    applied: true,
                    nodes_inserted: vec![new_id],
                    nodes_pruned: Vec::new(),
                    edges_added,
                    edges_removed: Vec::new(),
                    description: format!("added redundant branch for '{original_node}'"),
                }
            }

            RewriteRule::BreakDependency { from, to } => {
                graph.remove_edge(from, to).await;
                RewriteOutcome {
                    rule_name: "BreakDependency".into(),
                    applied: true,
                    nodes_inserted: Vec::new(),
                    nodes_pruned: Vec::new(),
                    edges_added: Vec::new(),
                    edges_removed: vec![(from.clone(), to.clone())],
                    description: format!("removed dependency '{from}' → '{to}'"),
                }
            }

            RewriteRule::PruneAllFailed => {
                let failed = graph.failed_nodes().await;
                if failed.is_empty() {
                    return RewriteOutcome::noop("PruneAllFailed");
                }
                let mut all_pruned = Vec::new();
                for f in &failed {
                    let mut pruned = graph.prune_node(f).await;
                    all_pruned.append(&mut pruned);
                }
                all_pruned.sort();
                all_pruned.dedup();
                RewriteOutcome {
                    rule_name: "PruneAllFailed".into(),
                    applied: true,
                    nodes_pruned: all_pruned,
                    nodes_inserted: Vec::new(),
                    edges_added: Vec::new(),
                    edges_removed: Vec::new(),
                    description: format!("pruned {} failed branches", failed.len()),
                }
            }

            RewriteRule::Custom { name: _, apply_fn } => {
                apply_fn(graph).await
            }

            RewriteRule::HumanEscalation {
                target,
                reason,
                channel,
            } => {
                let node = graph.get_node(target).await;
                if node.is_none() {
                    return RewriteOutcome::noop("HumanEscalation");
                }

                // Get predecessors and successors of the target node.
                let snap = graph.snapshot().await;
                let preds: Vec<String> = snap
                    .edges
                    .iter()
                    .filter(|e| e.to == *target)
                    .map(|e| e.from.clone())
                    .collect();
                let succs: Vec<String> = snap
                    .edges
                    .iter()
                    .filter(|e| e.from == *target)
                    .map(|e| e.to.clone())
                    .collect();

                // Prune the original failed/low-confidence node.
                graph.prune_node(target).await;

                // Create notify_human node.
                let notify_id = format!("{}_notify_human", target);
                let mut notify_node = TaskNode::new(&notify_id, format!("Notify human: {}", reason));
                notify_node.metadata.insert(
                    "escalation_reason".to_string(),
                    serde_json::Value::String(reason.clone()),
                );
                notify_node.metadata.insert(
                    "action".to_string(),
                    serde_json::Value::String("notify_human".to_string()),
                );
                if let Some(ch) = channel {
                    notify_node.metadata.insert(
                        "channel".to_string(),
                        serde_json::Value::String(ch.clone()),
                    );
                }
                notify_node = notify_node.with_weight(0.1); // Notification is fast

                // Create await_response node.
                let await_id = format!("{}_await_response", target);
                let mut await_node = TaskNode::new(&await_id, format!("Await human response for: {}", reason));
                await_node.metadata.insert(
                    "awaiting_human".to_string(),
                    serde_json::Value::Bool(true),
                );
                await_node.metadata.insert(
                    "original_task".to_string(),
                    serde_json::Value::String(target.clone()),
                );
                await_node = await_node.with_weight(0.01); // Negligible compute weight

                // Insert nodes.
                graph.insert_node(notify_node).await;
                graph.insert_node(await_node).await;

                // Wire: notify → await
                graph.add_edge(&notify_id, &await_id, 0.0).await;

                let mut edges_added = vec![(notify_id.clone(), await_id.clone())];

                // Wire predecessors → notify_human
                for pred in &preds {
                    if let Some(pn) = graph.get_node(pred).await {
                        if pn.status != NodeStatus::Pruned {
                            graph.add_edge(pred, &notify_id, 0.1).await;
                            edges_added.push((pred.clone(), notify_id.clone()));
                        }
                    }
                }

                // Wire await_response → successors
                for succ in &succs {
                    if let Some(sn) = graph.get_node(succ).await {
                        if sn.status != NodeStatus::Pruned {
                            graph.add_edge(&await_id, succ, 0.1).await;
                            edges_added.push((await_id.clone(), succ.clone()));
                        }
                    }
                }

                info!(
                    target = target,
                    reason = reason,
                    "HumanEscalation applied — inserted notify→await subgraph"
                );

                RewriteOutcome {
                    rule_name: "HumanEscalation".into(),
                    applied: true,
                    nodes_inserted: vec![notify_id, await_id],
                    nodes_pruned: vec![target.clone()],
                    edges_added,
                    edges_removed: Vec::new(),
                    description: format!(
                        "escalated task '{}' to human: {}",
                        target, reason
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_insert_between() {
        let graph = DynamicTaskGraph::new();
        graph.insert_node(TaskNode::new("a", "Start")).await;
        graph.insert_node(TaskNode::new("c", "End")).await;
        graph.add_edge("a", "c", 1.0).await;

        let rule = RewriteRule::InsertBetween {
            predecessor: "a".into(),
            successor: "c".into(),
            new_node: TaskNode::new("b", "Middle"),
            comm_cost_before: 0.5,
            comm_cost_after: 0.5,
        };

        let outcome = rule.apply(&graph).await;
        assert!(outcome.applied);
        assert_eq!(outcome.nodes_inserted, vec!["b"]);

        let order = graph.topological_order().await.unwrap();
        let a_pos = order.iter().position(|x| x == "a").unwrap();
        let b_pos = order.iter().position(|x| x == "b").unwrap();
        let c_pos = order.iter().position(|x| x == "c").unwrap();
        assert!(a_pos < b_pos);
        assert!(b_pos < c_pos);
    }

    #[tokio::test]
    async fn test_prune_all_failed() {
        let graph = DynamicTaskGraph::new();
        graph.insert_node(TaskNode::new("a", "OK")).await;
        graph.insert_node(TaskNode::new("b", "Fail")).await;
        graph.insert_node(TaskNode::new("c", "Downstream")).await;
        graph.add_edge("a", "b", 0.0).await;
        graph.add_edge("b", "c", 0.0).await;

        graph.mark_completed("a", serde_json::json!("ok")).await;
        graph.mark_failed("b", "test error".into()).await;

        let rule = RewriteRule::PruneAllFailed;
        let outcome = rule.apply(&graph).await;
        assert!(outcome.applied);
        assert!(outcome.nodes_pruned.contains(&"b".to_string()));
    }
}
