//! Visual Multi-Agent DAG Orchestrator with Live Execution Tracing
//!
//! Surfaces DTGG + HEFT as a visual, interactive DAG editor.
//!
//! ## Features
//!
//! 1. Real-time task decomposition graph as planner generates it
//! 2. Each node: assigned agent, estimated duration, status
//! 3. Edges: typed data flow
//! 4. HEFT schedule as Gantt chart overlay
//! 5. Manual intervention: reorder, reassign, inject human checkpoints
//! 6. Rewrite loop: graph mutations on failure shown in real-time
//!
//! ## Graph Layout (Sugiyama)
//!
//! 1. Cycle removal: O(V + E)
//! 2. Layer assignment via longest-path: O(V + E)
//! 3. Crossing minimization via barycenter: O(L × E × iters)
//! 4. Coordinate assignment: O(V + E)
//! Total: O(V² + V×E) — real-time for V < 100

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// Visual representation of a task node for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualTaskNode {
    /// Task node ID (matches DTGG NodeId)
    pub id: String,
    /// Human-readable description
    pub description: String,
    /// Assigned agent name (if any)
    pub agent_id: Option<String>,
    /// Agent display name
    pub agent_name: Option<String>,
    /// Estimated duration in milliseconds
    pub estimated_duration_ms: u64,
    /// Estimated token cost
    pub estimated_tokens: u64,
    /// Current execution status
    pub status: TaskStatus,
    /// Actual execution time (if completed)
    pub actual_duration_ms: Option<u64>,
    /// Output summary (if completed)
    pub output_summary: Option<String>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Graph generation this node belongs to (for rewrite tracking)
    pub generation: u64,
    /// HEFT scheduling info
    pub schedule: Option<ScheduleInfo>,
    /// Visual layout position (computed by Sugiyama)
    pub layout: NodeLayout,
}

/// Task execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Queued,
    Running,
    Completed,
    Failed,
    Pruned,
    Skipped,
    /// Waiting for human checkpoint approval
    AwaitingApproval,
}

/// HEFT scheduling information for a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleInfo {
    /// HEFT processor assignment (0-indexed)
    pub processor: usize,
    /// Earliest start time (in relative ms from job start)
    pub start_time_ms: u64,
    /// Finish time
    pub finish_time_ms: u64,
    /// Upward rank (HEFT priority)
    pub rank_u: f64,
}

/// Visual layout position for a node.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeLayout {
    /// Column/layer (left-to-right)
    pub layer: usize,
    /// Row position within layer
    pub position: usize,
    /// Computed x coordinate for rendering
    pub x: f64,
    /// Computed y coordinate for rendering
    pub y: f64,
}

/// Visual edge between task nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualTaskEdge {
    pub from: String,
    pub to: String,
    /// Communication cost (data transfer time)
    pub comm_cost_ms: f64,
    /// Data type flowing on this edge
    pub data_type: Option<String>,
    /// Whether data is currently flowing (animated in UI)
    pub active: bool,
}

/// Human intervention action on the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum OrchestratorAction {
    /// Reassign a task to a different agent
    Reassign { task_id: String, new_agent_id: String },
    /// Reorder task priority
    Reorder { task_id: String, new_priority: f64 },
    /// Insert a human checkpoint before a task
    InsertCheckpoint { before_task_id: String, description: String },
    /// Approve a human checkpoint
    ApproveCheckpoint { task_id: String },
    /// Reject a checkpoint (triggers replanning)
    RejectCheckpoint { task_id: String, reason: String },
    /// Skip a failed task
    SkipTask { task_id: String },
    /// Retry a failed task
    RetryTask { task_id: String },
    /// Cancel the entire job
    Cancel,
}

/// Gantt chart data for HEFT schedule visualization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GanttChart {
    /// Number of processors
    pub processor_count: usize,
    /// Per-processor task assignments
    pub processors: Vec<ProcessorTimeline>,
    /// Total makespan
    pub makespan_ms: u64,
    /// Critical path through the DAG
    pub critical_path: Vec<String>,
}

/// Timeline for a single processor in the Gantt chart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorTimeline {
    pub processor_id: usize,
    pub processor_name: String,
    pub tasks: Vec<GanttTask>,
    /// Utilization percentage (0–100)
    pub utilization_pct: f32,
}

/// A single task on the Gantt chart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GanttTask {
    pub task_id: String,
    pub description: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub status: TaskStatus,
}

/// Complete visual orchestrator state for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorView {
    /// All task nodes with layout
    pub nodes: Vec<VisualTaskNode>,
    /// All edges between nodes
    pub edges: Vec<VisualTaskEdge>,
    /// Gantt chart (HEFT schedule)
    pub gantt: GanttChart,
    /// Current graph generation (increments on rewrite)
    pub generation: u64,
    /// Overall job status
    pub job_status: JobStatus,
    /// Progress percentage (0–100)
    pub progress_pct: f32,
    /// Total estimated cost
    pub estimated_cost_usd: f64,
    /// Actual cost so far
    pub actual_cost_usd: f64,
}

/// Overall job status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Planning,
    Executing,
    AwaitingInput,
    Replanning,
    Completed,
    Failed,
    Cancelled,
}

// ---------------------------------------------------------------------------
// Sugiyama Layout Algorithm
// ---------------------------------------------------------------------------

/// Compute Sugiyama layered layout for a task DAG.
///
/// 1. Layer assignment via longest-path: O(V + E)
/// 2. Crossing minimization via barycenter heuristic: O(L × E × iters)
/// 3. Coordinate assignment: O(V + E)
///
/// Total: O(V² + V×E) — real-time for V < 100.
pub fn compute_layout(
    nodes: &mut [VisualTaskNode],
    edges: &[VisualTaskEdge],
) {
    if nodes.is_empty() {
        return;
    }

    let node_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let id_to_idx: HashMap<&str, usize> = node_ids.iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();

    // Build adjacency
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut in_degree: HashMap<usize, usize> = HashMap::new();
    for i in 0..nodes.len() {
        adj.entry(i).or_default();
        in_degree.entry(i).or_insert(0);
    }
    for edge in edges {
        if let (Some(&from_idx), Some(&to_idx)) = (
            id_to_idx.get(edge.from.as_str()),
            id_to_idx.get(edge.to.as_str()),
        ) {
            adj.entry(from_idx).or_default().push(to_idx);
            *in_degree.entry(to_idx).or_insert(0) += 1;
        }
    }

    // Step 1: Layer assignment via longest path (topological order)
    let mut layers: Vec<usize> = vec![0; nodes.len()];
    let mut queue: VecDeque<usize> = VecDeque::new();

    for (&node, &deg) in &in_degree {
        if deg == 0 {
            queue.push_back(node);
        }
    }

    while let Some(node) = queue.pop_front() {
        if let Some(neighbors) = adj.get(&node) {
            for &next in neighbors {
                layers[next] = layers[next].max(layers[node] + 1);
                let deg = in_degree.get_mut(&next).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(next);
                }
            }
        }
    }

    // Step 2: Group by layer and assign positions
    let max_layer = *layers.iter().max().unwrap_or(&0);
    let mut layer_groups: Vec<Vec<usize>> = vec![Vec::new(); max_layer + 1];
    for (i, &layer) in layers.iter().enumerate() {
        layer_groups[layer].push(i);
    }

    // Barycenter heuristic for crossing minimization (1 iteration)
    for l in 1..=max_layer {
        let prev_layer = &layer_groups[l - 1];
        let prev_positions: HashMap<usize, usize> = prev_layer.iter()
            .enumerate()
            .map(|(pos, &node)| (node, pos))
            .collect();

        // Compute barycenter for each node in current layer
        let mut barycenters: Vec<(usize, f64)> = layer_groups[l].iter()
            .map(|&node| {
                let mut sum = 0.0;
                let mut count = 0;
                // Find predecessors in previous layer
                for (&from, neighbors) in &adj {
                    if neighbors.contains(&node) {
                        if let Some(&pos) = prev_positions.get(&from) {
                            sum += pos as f64;
                            count += 1;
                        }
                    }
                }
                let bc = if count > 0 { sum / count as f64 } else { node as f64 };
                (node, bc)
            })
            .collect();

        barycenters.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        layer_groups[l] = barycenters.into_iter().map(|(node, _)| node).collect();
    }

    // Step 3: Assign coordinates
    let layer_spacing = 200.0;
    let node_spacing = 100.0;

    for (layer_idx, group) in layer_groups.iter().enumerate() {
        let layer_height = group.len() as f64 * node_spacing;
        let start_y = -layer_height / 2.0;

        for (pos, &node_idx) in group.iter().enumerate() {
            nodes[node_idx].layout = NodeLayout {
                layer: layer_idx,
                position: pos,
                x: layer_idx as f64 * layer_spacing,
                y: start_y + pos as f64 * node_spacing,
            };
        }
    }
}

/// Compute the critical path through a task DAG.
///
/// Uses longest-path algorithm on the DAG. O(V + E).
pub fn critical_path(
    nodes: &[VisualTaskNode],
    edges: &[VisualTaskEdge],
) -> Vec<String> {
    if nodes.is_empty() {
        return Vec::new();
    }

    let id_to_idx: HashMap<&str, usize> = nodes.iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();

    let mut dist: Vec<f64> = vec![0.0; nodes.len()];
    let mut pred: Vec<Option<usize>> = vec![None; nodes.len()];

    // Topological order via Kahn's algorithm
    let mut in_deg = vec![0usize; nodes.len()];
    let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); nodes.len()];

    for edge in edges {
        if let (Some(&from), Some(&to)) = (
            id_to_idx.get(edge.from.as_str()),
            id_to_idx.get(edge.to.as_str()),
        ) {
            adj[from].push((to, nodes[to].estimated_duration_ms as f64 + edge.comm_cost_ms));
            in_deg[to] += 1;
        }
    }

    let mut queue: VecDeque<usize> = (0..nodes.len())
        .filter(|&i| in_deg[i] == 0)
        .collect();

    // Initialize source nodes with their own duration
    for &src in queue.iter() {
        dist[src] = nodes[src].estimated_duration_ms as f64;
    }

    while let Some(u) = queue.pop_front() {
        for &(v, weight) in &adj[u] {
            let new_dist = dist[u] + weight;
            if new_dist > dist[v] {
                dist[v] = new_dist;
                pred[v] = Some(u);
            }
            in_deg[v] -= 1;
            if in_deg[v] == 0 {
                queue.push_back(v);
            }
        }
    }

    // Find the node with maximum distance (end of critical path)
    let (end_idx, _) = dist.iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
        .unwrap_or((0, &0.0));

    // Trace back
    let mut path = Vec::new();
    let mut current = Some(end_idx);
    while let Some(idx) = current {
        path.push(nodes[idx].id.clone());
        current = pred[idx];
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_linear_dag() -> (Vec<VisualTaskNode>, Vec<VisualTaskEdge>) {
        let nodes = vec![
            VisualTaskNode {
                id: "a".into(), description: "Plan".into(),
                agent_id: Some("planner".into()), agent_name: Some("Planner".into()),
                estimated_duration_ms: 100, estimated_tokens: 500,
                status: TaskStatus::Completed, actual_duration_ms: Some(95),
                output_summary: Some("Plan created".into()), error: None,
                generation: 0, schedule: None, layout: NodeLayout::default(),
            },
            VisualTaskNode {
                id: "b".into(), description: "Execute".into(),
                agent_id: Some("coder".into()), agent_name: Some("Coder".into()),
                estimated_duration_ms: 200, estimated_tokens: 1000,
                status: TaskStatus::Running, actual_duration_ms: None,
                output_summary: None, error: None,
                generation: 0, schedule: None, layout: NodeLayout::default(),
            },
            VisualTaskNode {
                id: "c".into(), description: "Review".into(),
                agent_id: Some("reviewer".into()), agent_name: Some("Reviewer".into()),
                estimated_duration_ms: 150, estimated_tokens: 700,
                status: TaskStatus::Pending, actual_duration_ms: None,
                output_summary: None, error: None,
                generation: 0, schedule: None, layout: NodeLayout::default(),
            },
        ];
        let edges = vec![
            VisualTaskEdge { from: "a".into(), to: "b".into(), comm_cost_ms: 10.0, data_type: Some("text".into()), active: true },
            VisualTaskEdge { from: "b".into(), to: "c".into(), comm_cost_ms: 10.0, data_type: Some("text".into()), active: false },
        ];
        (nodes, edges)
    }

    #[test]
    fn layout_assigns_layers() {
        let (mut nodes, edges) = make_linear_dag();
        compute_layout(&mut nodes, &edges);

        assert_eq!(nodes[0].layout.layer, 0); // a
        assert_eq!(nodes[1].layout.layer, 1); // b
        assert_eq!(nodes[2].layout.layer, 2); // c
    }

    #[test]
    fn critical_path_finds_longest() {
        let (nodes, edges) = make_linear_dag();
        let path = critical_path(&nodes, &edges);
        assert_eq!(path, vec!["a", "b", "c"]);
    }

    #[test]
    fn parallel_dag_layout() {
        let mut nodes = vec![
            VisualTaskNode {
                id: "start".into(), description: "Start".into(),
                agent_id: None, agent_name: None,
                estimated_duration_ms: 50, estimated_tokens: 100,
                status: TaskStatus::Completed, actual_duration_ms: None,
                output_summary: None, error: None,
                generation: 0, schedule: None, layout: NodeLayout::default(),
            },
            VisualTaskNode {
                id: "p1".into(), description: "Parallel 1".into(),
                agent_id: None, agent_name: None,
                estimated_duration_ms: 100, estimated_tokens: 500,
                status: TaskStatus::Running, actual_duration_ms: None,
                output_summary: None, error: None,
                generation: 0, schedule: None, layout: NodeLayout::default(),
            },
            VisualTaskNode {
                id: "p2".into(), description: "Parallel 2".into(),
                agent_id: None, agent_name: None,
                estimated_duration_ms: 200, estimated_tokens: 800,
                status: TaskStatus::Running, actual_duration_ms: None,
                output_summary: None, error: None,
                generation: 0, schedule: None, layout: NodeLayout::default(),
            },
            VisualTaskNode {
                id: "end".into(), description: "Merge".into(),
                agent_id: None, agent_name: None,
                estimated_duration_ms: 50, estimated_tokens: 100,
                status: TaskStatus::Pending, actual_duration_ms: None,
                output_summary: None, error: None,
                generation: 0, schedule: None, layout: NodeLayout::default(),
            },
        ];
        let edges = vec![
            VisualTaskEdge { from: "start".into(), to: "p1".into(), comm_cost_ms: 5.0, data_type: None, active: false },
            VisualTaskEdge { from: "start".into(), to: "p2".into(), comm_cost_ms: 5.0, data_type: None, active: false },
            VisualTaskEdge { from: "p1".into(), to: "end".into(), comm_cost_ms: 5.0, data_type: None, active: false },
            VisualTaskEdge { from: "p2".into(), to: "end".into(), comm_cost_ms: 5.0, data_type: None, active: false },
        ];

        compute_layout(&mut nodes, &edges);

        // Start at layer 0, parallel at layer 1, merge at layer 2
        assert_eq!(nodes[0].layout.layer, 0); // start
        assert_eq!(nodes[1].layout.layer, 1); // p1
        assert_eq!(nodes[2].layout.layer, 1); // p2
        assert_eq!(nodes[3].layout.layer, 2); // end
    }
}
