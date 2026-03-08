//! Dynamic Task Graph — the live DAG that evolves at runtime.

use crate::heft::HeftScheduler;
use crate::rewrite::{RewriteRule, RewriteOutcome};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};


// ───────────────────────────────────────────────────────────────
// Core types
// ───────────────────────────────────────────────────────────────

/// Unique identifier for a task node within the graph.
pub type NodeId = String;

/// Status of an individual task node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// Waiting for dependencies.
    Pending,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Completed,
    /// Failed — may trigger replanning.
    Failed,
    /// Pruned from the graph by the replanner.
    Pruned,
    /// Skipped because an upstream node was pruned.
    Skipped,
}

/// A single task node in the dynamic graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNode {
    pub id: NodeId,
    pub description: String,
    /// Which agent (or agent capability) should handle this.
    pub agent_id: Option<String>,
    /// Estimated cost in tokens.
    pub estimated_tokens: u64,
    /// Estimated wall-clock duration in milliseconds.
    pub estimated_duration_ms: u64,
    /// Weight for HEFT scheduling (typically mean execution time).
    pub weight: f64,
    pub status: NodeStatus,
    /// The output produced by execution (if completed).
    pub output: Option<serde_json::Value>,
    /// Error message (if failed).
    pub error: Option<String>,
    /// Monotonic generation counter — incremented on each rewrite.
    pub generation: u64,
    /// Metadata for the meta-planner.
    pub metadata: HashMap<String, serde_json::Value>,
}

impl TaskNode {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            agent_id: None,
            estimated_tokens: 0,
            estimated_duration_ms: 5000,
            weight: 1.0,
            status: NodeStatus::Pending,
            output: None,
            error: None,
            generation: 0,
            metadata: HashMap::new(),
        }
    }

    pub fn with_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    pub fn with_weight(mut self, w: f64) -> Self {
        self.weight = w;
        self
    }

    pub fn with_estimated_tokens(mut self, t: u64) -> Self {
        self.estimated_tokens = t;
        self
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            NodeStatus::Completed | NodeStatus::Failed | NodeStatus::Pruned | NodeStatus::Skipped
        )
    }
}

/// An edge in the task graph (dependency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEdge {
    pub from: NodeId,
    pub to: NodeId,
    /// Estimated communication cost between the two nodes.
    pub comm_cost: f64,
}

/// Snapshot of the graph at a single point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub generation: u64,
    pub nodes: Vec<TaskNode>,
    pub edges: Vec<TaskEdge>,
}

// ───────────────────────────────────────────────────────────────
// Dynamic Task Graph
// ───────────────────────────────────────────────────────────────

/// The live, mutable task graph.
///
/// Thread-safe via internal `RwLock`. All mutation goes through
/// the `DynamicTaskGraph` methods which maintain topological
/// invariants.
pub struct DynamicTaskGraph {
    inner: Arc<RwLock<GraphInner>>,
}

struct GraphInner {
    /// All nodes keyed by ID.
    nodes: HashMap<NodeId, TaskNode>,
    /// Adjacency list: from → set of to.
    forward_edges: HashMap<NodeId, HashSet<NodeId>>,
    /// Reverse adjacency: to → set of from.
    reverse_edges: HashMap<NodeId, HashSet<NodeId>>,
    /// Communication costs per edge.
    edge_costs: HashMap<(NodeId, NodeId), f64>,
    /// Monotonic generation counter.
    generation: u64,
    /// History of graph snapshots (bounded ring buffer).
    history: VecDeque<GraphSnapshot>,
    /// Maximum history entries.
    max_history: usize,
}

impl DynamicTaskGraph {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(GraphInner {
                nodes: HashMap::new(),
                forward_edges: HashMap::new(),
                reverse_edges: HashMap::new(),
                edge_costs: HashMap::new(),
                generation: 0,
                history: VecDeque::new(),
                max_history: 50,
            })),
        }
    }

    // ─── Mutation primitives ───────────────────────────────────

    /// Insert a new task node. O(1).
    pub async fn insert_node(&self, node: TaskNode) {
        let mut g = self.inner.write().await;
        let id = node.id.clone();
        g.nodes.insert(id.clone(), node);
        g.forward_edges.entry(id.clone()).or_default();
        g.reverse_edges.entry(id).or_default();
        g.generation += 1;
        debug!(generation = g.generation, "node inserted");
    }

    /// Add a directed edge (dependency). O(1).
    pub async fn add_edge(&self, from: &str, to: &str, comm_cost: f64) {
        let mut g = self.inner.write().await;
        g.forward_edges
            .entry(from.to_string())
            .or_default()
            .insert(to.to_string());
        g.reverse_edges
            .entry(to.to_string())
            .or_default()
            .insert(from.to_string());
        g.edge_costs
            .insert((from.to_string(), to.to_string()), comm_cost);
        g.generation += 1;
    }

    /// Remove an edge. O(1).
    pub async fn remove_edge(&self, from: &str, to: &str) {
        let mut g = self.inner.write().await;
        if let Some(set) = g.forward_edges.get_mut(from) {
            set.remove(to);
        }
        if let Some(set) = g.reverse_edges.get_mut(to) {
            set.remove(from);
        }
        g.edge_costs.remove(&(from.to_string(), to.to_string()));
        g.generation += 1;
    }

    /// Prune a node and all downstream nodes that become unreachable.
    pub async fn prune_node(&self, node_id: &str) -> Vec<NodeId> {
        let mut g = self.inner.write().await;
        let mut pruned = Vec::new();
        Self::prune_recursive(&mut g, node_id, &mut pruned);
        g.generation += 1;
        info!(pruned = ?pruned, "nodes pruned from graph");
        pruned
    }

    fn prune_recursive(g: &mut GraphInner, id: &str, pruned: &mut Vec<NodeId>) {
        if let Some(node) = g.nodes.get_mut(id) {
            if node.status == NodeStatus::Pruned {
                return;
            }
            node.status = NodeStatus::Pruned;
            pruned.push(id.to_string());
        }
        // Prune downstream nodes that have no remaining live predecessors.
        let successors: Vec<NodeId> = g
            .forward_edges
            .get(id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();

        for succ in successors {
            let has_live_pred = g
                .reverse_edges
                .get(&succ)
                .map(|preds| {
                    preds.iter().any(|p| {
                        g.nodes
                            .get(p)
                            .map(|n| !n.is_terminal() || n.status == NodeStatus::Completed)
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);

            if !has_live_pred {
                Self::prune_recursive(g, &succ, pruned);
            }
        }
    }

    /// Replace a subgraph rooted at `old_root` with a new set of nodes/edges.
    pub async fn substitute_subgraph(
        &self,
        old_root: &str,
        new_nodes: Vec<TaskNode>,
        new_edges: Vec<(String, String, f64)>,
    ) {
        // First prune old subgraph.
        self.prune_node(old_root).await;

        // Then insert new nodes and edges.
        for node in new_nodes {
            self.insert_node(node).await;
        }
        for (from, to, cost) in new_edges {
            self.add_edge(&from, &to, cost).await;
        }
        info!(old_root = old_root, "subgraph substituted");
    }

    // ─── Status updates ────────────────────────────────────────

    /// Mark a node as running.
    pub async fn mark_running(&self, node_id: &str) {
        let mut g = self.inner.write().await;
        if let Some(node) = g.nodes.get_mut(node_id) {
            node.status = NodeStatus::Running;
        }
    }

    /// Mark a node as completed with its output.
    pub async fn mark_completed(&self, node_id: &str, output: serde_json::Value) {
        let mut g = self.inner.write().await;
        if let Some(node) = g.nodes.get_mut(node_id) {
            node.status = NodeStatus::Completed;
            node.output = Some(output);
        }
    }

    /// Mark a node as failed.
    pub async fn mark_failed(&self, node_id: &str, error: String) {
        let mut g = self.inner.write().await;
        if let Some(node) = g.nodes.get_mut(node_id) {
            node.status = NodeStatus::Failed;
            node.error = Some(error);
        }
    }

    // ─── Queries ───────────────────────────────────────────────

    /// Compute topological order via Kahn's algorithm. O(V + E).
    pub async fn topological_order(&self) -> Result<Vec<NodeId>, String> {
        let g = self.inner.read().await;
        Self::kahn(&g)
    }

    fn kahn(g: &GraphInner) -> Result<Vec<NodeId>, String> {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for id in g.nodes.keys() {
            in_degree.entry(id.as_str()).or_insert(0);
        }
        for (_, targets) in &g.forward_edges {
            for t in targets {
                *in_degree.entry(t.as_str()).or_insert(0) += 1;
            }
        }

        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut order = Vec::with_capacity(g.nodes.len());
        while let Some(id) = queue.pop_front() {
            // Skip pruned/skipped nodes.
            if let Some(node) = g.nodes.get(id) {
                if node.status == NodeStatus::Pruned || node.status == NodeStatus::Skipped {
                    // Still decrement successors so they can proceed.
                }
            }
            order.push(id.to_string());
            if let Some(succs) = g.forward_edges.get(id) {
                for s in succs {
                    if let Some(deg) = in_degree.get_mut(s.as_str()) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(s.as_str());
                        }
                    }
                }
            }
        }

        if order.len() != g.nodes.len() {
            return Err("cycle detected in task graph".into());
        }
        Ok(order)
    }

    /// Get nodes that are ready to execute (all predecessors completed).
    pub async fn ready_nodes(&self) -> Vec<NodeId> {
        let g = self.inner.read().await;
        let mut ready = Vec::new();
        for (id, node) in &g.nodes {
            if node.status != NodeStatus::Pending {
                continue;
            }
            let preds = g.reverse_edges.get(id);
            let all_done = preds
                .map(|ps| {
                    ps.iter().all(|p| {
                        g.nodes
                            .get(p)
                            .map(|n| n.is_terminal())
                            .unwrap_or(true)
                    })
                })
                .unwrap_or(true);
            if all_done {
                ready.push(id.clone());
            }
        }
        ready
    }

    /// Get all failed nodes.
    pub async fn failed_nodes(&self) -> Vec<NodeId> {
        let g = self.inner.read().await;
        g.nodes
            .iter()
            .filter(|(_, n)| n.status == NodeStatus::Failed)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Check if the graph is fully resolved.
    pub async fn is_complete(&self) -> bool {
        let g = self.inner.read().await;
        g.nodes.values().all(|n| n.is_terminal())
    }

    /// Snapshot the current graph state.
    pub async fn snapshot(&self) -> GraphSnapshot {
        let g = self.inner.read().await;
        let nodes: Vec<TaskNode> = g.nodes.values().cloned().collect();
        let mut edges = Vec::new();
        for (from, targets) in &g.forward_edges {
            for to in targets {
                let cc = g
                    .edge_costs
                    .get(&(from.clone(), to.clone()))
                    .copied()
                    .unwrap_or(0.0);
                edges.push(TaskEdge {
                    from: from.clone(),
                    to: to.clone(),
                    comm_cost: cc,
                });
            }
        }
        GraphSnapshot {
            timestamp: chrono::Utc::now(),
            generation: g.generation,
            nodes,
            edges,
        }
    }

    /// Number of nodes.
    pub async fn node_count(&self) -> usize {
        self.inner.read().await.nodes.len()
    }

    /// Get a node by ID.
    pub async fn get_node(&self, id: &str) -> Option<TaskNode> {
        self.inner.read().await.nodes.get(id).cloned()
    }

    /// Current generation.
    pub async fn generation(&self) -> u64 {
        self.inner.read().await.generation
    }

    /// Save a snapshot to history.
    pub async fn checkpoint(&self) {
        let snap = self.snapshot().await;
        let mut g = self.inner.write().await;
        if g.history.len() >= g.max_history {
            g.history.pop_front();
        }
        g.history.push_back(snap);
    }

    // ─── Replanning ────────────────────────────────────────────

    /// Apply a set of rewrite rules to the graph.
    /// Returns the list of outcomes (insertions, prunings, substitutions).
    pub async fn apply_rewrite_rules(&self, rules: &[RewriteRule]) -> Vec<RewriteOutcome> {
        let mut outcomes = Vec::new();
        for rule in rules {
            let outcome = rule.apply(self).await;
            outcomes.push(outcome);
        }
        outcomes
    }

    /// Run HEFT scheduling on the current graph for `num_processors` agents.
    pub async fn schedule_heft(&self, num_processors: usize) -> Vec<(NodeId, usize, f64)> {
        let g = self.inner.read().await;
        let nodes: Vec<&TaskNode> = g.nodes.values().collect();
        let mut edges: Vec<(String, String, f64)> = Vec::new();
        for (from, targets) in &g.forward_edges {
            for to in targets {
                let cc = g
                    .edge_costs
                    .get(&(from.clone(), to.clone()))
                    .copied()
                    .unwrap_or(0.0);
                edges.push((from.clone(), to.clone(), cc));
            }
        }
        let edge_refs: Vec<(&str, &str, f64)> = edges
            .iter()
            .map(|(f, t, c)| (f.as_str(), t.as_str(), *c))
            .collect();
        HeftScheduler::schedule(&nodes, &edge_refs, num_processors)
    }
}

impl Default for DynamicTaskGraph {
    fn default() -> Self {
        Self::new()
    }
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_basic_graph_operations() {
        let graph = DynamicTaskGraph::new();

        graph.insert_node(TaskNode::new("a", "Research")).await;
        graph.insert_node(TaskNode::new("b", "Analyze")).await;
        graph.insert_node(TaskNode::new("c", "Write")).await;
        graph.add_edge("a", "b", 0.1).await;
        graph.add_edge("b", "c", 0.1).await;

        assert_eq!(graph.node_count().await, 3);

        let order = graph.topological_order().await.unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn test_ready_nodes() {
        let graph = DynamicTaskGraph::new();
        graph.insert_node(TaskNode::new("a", "Step 1")).await;
        graph.insert_node(TaskNode::new("b", "Step 2")).await;
        graph.add_edge("a", "b", 0.0).await;

        let ready = graph.ready_nodes().await;
        assert_eq!(ready, vec!["a"]);

        graph
            .mark_completed("a", serde_json::json!({"result": "done"}))
            .await;
        let ready = graph.ready_nodes().await;
        assert_eq!(ready, vec!["b"]);
    }

    #[tokio::test]
    async fn test_prune_cascading() {
        let graph = DynamicTaskGraph::new();
        graph.insert_node(TaskNode::new("a", "Root")).await;
        graph.insert_node(TaskNode::new("b", "Mid")).await;
        graph.insert_node(TaskNode::new("c", "Leaf")).await;
        graph.add_edge("a", "b", 0.0).await;
        graph.add_edge("b", "c", 0.0).await;

        let pruned = graph.prune_node("a").await;
        assert_eq!(pruned.len(), 3);

        let node_c = graph.get_node("c").await.unwrap();
        assert_eq!(node_c.status, NodeStatus::Pruned);
    }

    #[tokio::test]
    async fn test_substitute_subgraph() {
        let graph = DynamicTaskGraph::new();
        graph.insert_node(TaskNode::new("root", "Root task")).await;
        graph
            .insert_node(TaskNode::new("old_step", "Failing step"))
            .await;
        graph.add_edge("root", "old_step", 0.0).await;

        graph
            .mark_completed("root", serde_json::json!("ok"))
            .await;

        // Substitute old_step with two new steps.
        graph
            .substitute_subgraph(
                "old_step",
                vec![
                    TaskNode::new("new_a", "Alternative step A"),
                    TaskNode::new("new_b", "Alternative step B"),
                ],
                vec![("new_a".into(), "new_b".into(), 0.1)],
            )
            .await;

        assert_eq!(graph.node_count().await, 4); // root + old_step(pruned) + new_a + new_b
        let old = graph.get_node("old_step").await.unwrap();
        assert_eq!(old.status, NodeStatus::Pruned);
    }

    #[tokio::test]
    async fn test_cycle_detection() {
        let graph = DynamicTaskGraph::new();
        graph.insert_node(TaskNode::new("a", "A")).await;
        graph.insert_node(TaskNode::new("b", "B")).await;
        graph.add_edge("a", "b", 0.0).await;
        graph.add_edge("b", "a", 0.0).await;

        let result = graph.topological_order().await;
        assert!(result.is_err());
    }
}
