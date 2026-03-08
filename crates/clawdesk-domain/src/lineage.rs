//! # Execution Lineage Graph — Durable provenance DAG for all agent workflows.
//!
//! Records every causal step of an execution as an immutable node in a
//! directed acyclic graph (DAG): request → subtask → tool call → retrieval →
//! artifact → delivery. Enables postmortem debugging, replay, compliance
//! review, and causal attribution across sessions, agents, and A2A boundaries.
//!
//! ## Design
//!
//! - **Immutable event nodes** with monotonic Lamport timestamps for partial
//!   ordering across concurrent/distributed agent boundaries.
//! - **Causal edges** link parent → child with typed relationships.
//! - **Restart-safe**: lineage is persisted via SochDB key-value storage.
//! - **Queryable**: reconstruction, replay, and root-cause tracing are O(V+E).
//!
//! ## Namespace Convention
//!
//! SochDB keys: `lineage/{run_id}/nodes/{node_id}` and `lineage/{run_id}/edges/{idx}`.
//! Enables prefix-scan reconstruction of entire runs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

// ═══════════════════════════════════════════════════════════════════════════
// Lamport clock for partial ordering
// ═══════════════════════════════════════════════════════════════════════════

/// Lamport logical clock for partial ordering across agent boundaries.
///
/// Each event increments the local counter. When receiving an external
/// event with timestamp `t`, the clock advances to `max(local, t) + 1`.
/// Total ordering within a single run uses `(lamport, sequence)`.
pub struct LamportClock {
    counter: AtomicU64,
}

impl LamportClock {
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }

    /// Tick the clock for a local event.
    pub fn tick(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Merge with an external timestamp (e.g., from an A2A agent).
    pub fn merge(&self, external: u64) -> u64 {
        loop {
            let current = self.counter.load(Ordering::SeqCst);
            let new_val = current.max(external) + 1;
            if self
                .counter
                .compare_exchange(current, new_val, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return new_val;
            }
        }
    }

    /// Current clock value without incrementing.
    pub fn current(&self) -> u64 {
        self.counter.load(Ordering::SeqCst)
    }
}

impl Default for LamportClock {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Lineage node — immutable event in the execution DAG
// ═══════════════════════════════════════════════════════════════════════════

/// Unique node identifier in the lineage graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LineageNodeId(pub String);

impl LineageNodeId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn from(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// The type of event recorded in a lineage node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum LineageEventType {
    /// A user or system request initiated a workflow.
    Request {
        session_id: String,
        agent_id: String,
        content_preview: String,
    },
    /// A sub-task was created (spawned sub-agent, pipeline step).
    Subtask {
        parent_node_id: String,
        task_description: String,
        agent_id: String,
        depth: u32,
    },
    /// An LLM call was made.
    LlmCall {
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        duration_ms: u64,
    },
    /// A tool was invoked.
    ToolCall {
        tool_name: String,
        success: bool,
        duration_ms: u64,
        output_preview: String,
    },
    /// A retrieval operation was performed (vector search, FTS, hybrid).
    Retrieval {
        query_preview: String,
        result_count: usize,
        retrieval_type: String,
        expansions_used: usize,
    },
    /// An artifact was produced or referenced.
    ArtifactProduced {
        artifact_id: String,
        artifact_name: String,
        mime_type: String,
        size_bytes: u64,
    },
    /// A delivery was attempted (channel, webhook, agent callback).
    Delivery {
        target_type: String,
        target_id: String,
        success: bool,
        attempt: u32,
    },
    /// A compaction or context management operation.
    ContextAction {
        action_type: String,
        tokens_before: usize,
        tokens_after: usize,
    },
    /// A policy decision was made (approval, denial, delegation).
    PolicyDecision {
        decision: String,
        reason: String,
        policy_layer: String,
    },
    /// The final response/output of the workflow.
    Response {
        content_preview: String,
        total_rounds: usize,
    },
    /// Custom extension node for downstream consumers.
    Custom {
        label: String,
        payload: serde_json::Value,
    },
}

/// An immutable event node in the lineage DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageNode {
    /// Unique node identifier.
    pub id: LineageNodeId,
    /// The run (execution) this node belongs to.
    pub run_id: String,
    /// Lamport timestamp for partial ordering.
    pub lamport_ts: u64,
    /// Wall-clock time for human-readable display.
    pub wall_time: DateTime<Utc>,
    /// The event recorded at this node.
    pub event: LineageEventType,
    /// Optional metadata for domain-specific extensions.
    pub metadata: HashMap<String, String>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Causal edge — relationship between nodes
// ═══════════════════════════════════════════════════════════════════════════

/// The causal relationship between two lineage nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Parent triggered child (request → subtask, subtask → tool call).
    Caused,
    /// Child produced output consumed by parent.
    Produced,
    /// Data dependency (retrieval result → LLM call).
    DependsOn,
    /// Delivery of a result.
    DeliveredTo,
    /// Policy decision gated this action.
    GatedBy,
}

/// A directed causal edge in the lineage DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageEdge {
    pub from: LineageNodeId,
    pub to: LineageNodeId,
    pub kind: EdgeKind,
    /// Optional label for human-readable edge description.
    pub label: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Lineage graph — the complete DAG for a run
// ═══════════════════════════════════════════════════════════════════════════

/// A complete execution lineage graph for a single run.
///
/// Contains all nodes and edges recorded during one workflow execution.
/// Can be serialized, stored in SochDB, and reconstructed for replay/audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageGraph {
    /// The run identifier.
    pub run_id: String,
    /// When the run started.
    pub started_at: DateTime<Utc>,
    /// When the run finished (None if still running).
    pub completed_at: Option<DateTime<Utc>>,
    /// All nodes in topological order (by Lamport timestamp).
    pub nodes: Vec<LineageNode>,
    /// All causal edges.
    pub edges: Vec<LineageEdge>,
    /// Root node ID (the initial request).
    pub root_node_id: Option<LineageNodeId>,
    /// Summary statistics.
    pub stats: LineageStats,
}

/// Summary statistics for a lineage graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LineageStats {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub tool_calls: usize,
    pub llm_calls: usize,
    pub retrievals: usize,
    pub artifacts: usize,
    pub deliveries: usize,
    pub max_depth: u32,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

impl LineageGraph {
    pub fn new(run_id: String) -> Self {
        Self {
            run_id,
            started_at: Utc::now(),
            completed_at: None,
            nodes: Vec::new(),
            edges: Vec::new(),
            root_node_id: None,
            stats: LineageStats::default(),
        }
    }

    /// Compute summary statistics from the graph contents.
    pub fn compute_stats(&mut self) {
        let mut stats = LineageStats {
            total_nodes: self.nodes.len(),
            total_edges: self.edges.len(),
            ..Default::default()
        };

        for node in &self.nodes {
            match &node.event {
                LineageEventType::ToolCall { .. } => stats.tool_calls += 1,
                LineageEventType::LlmCall {
                    input_tokens,
                    output_tokens,
                    ..
                } => {
                    stats.llm_calls += 1;
                    stats.total_input_tokens += input_tokens;
                    stats.total_output_tokens += output_tokens;
                }
                LineageEventType::Retrieval { .. } => stats.retrievals += 1,
                LineageEventType::ArtifactProduced { .. } => stats.artifacts += 1,
                LineageEventType::Delivery { .. } => stats.deliveries += 1,
                LineageEventType::Subtask { depth, .. } => {
                    stats.max_depth = stats.max_depth.max(*depth);
                }
                _ => {}
            }
        }

        self.stats = stats;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Lineage collector — mutable builder used during execution
// ═══════════════════════════════════════════════════════════════════════════

/// Mutable collector for building a lineage graph during execution.
///
/// Thread-safe via internal `Mutex`. Create at run start, record events
/// as they occur, finalize at run end.
pub struct LineageCollector {
    run_id: String,
    clock: LamportClock,
    nodes: std::sync::Mutex<Vec<LineageNode>>,
    edges: std::sync::Mutex<Vec<LineageEdge>>,
    root_node_id: std::sync::Mutex<Option<LineageNodeId>>,
    started_at: DateTime<Utc>,
}

impl LineageCollector {
    pub fn new(run_id: String) -> Self {
        Self {
            run_id,
            clock: LamportClock::new(),
            nodes: std::sync::Mutex::new(Vec::new()),
            edges: std::sync::Mutex::new(Vec::new()),
            root_node_id: std::sync::Mutex::new(None),
            started_at: Utc::now(),
        }
    }

    /// Record a lineage event and return its node ID.
    pub fn record(&self, event: LineageEventType) -> LineageNodeId {
        self.record_with_metadata(event, HashMap::new())
    }

    /// Record a lineage event with custom metadata.
    pub fn record_with_metadata(
        &self,
        event: LineageEventType,
        metadata: HashMap<String, String>,
    ) -> LineageNodeId {
        let node_id = LineageNodeId::new();
        let lamport_ts = self.clock.tick();

        let node = LineageNode {
            id: node_id.clone(),
            run_id: self.run_id.clone(),
            lamport_ts,
            wall_time: Utc::now(),
            event,
            metadata,
        };

        self.nodes.lock().unwrap().push(node);
        node_id
    }

    /// Record the root request node.
    pub fn record_root(&self, event: LineageEventType) -> LineageNodeId {
        let node_id = self.record(event);
        *self.root_node_id.lock().unwrap() = Some(node_id.clone());
        node_id
    }

    /// Record a causal edge between two nodes.
    pub fn link(&self, from: &LineageNodeId, to: &LineageNodeId, kind: EdgeKind) {
        self.link_labeled(from, to, kind, None);
    }

    /// Record a labeled causal edge.
    pub fn link_labeled(
        &self,
        from: &LineageNodeId,
        to: &LineageNodeId,
        kind: EdgeKind,
        label: Option<String>,
    ) {
        let edge = LineageEdge {
            from: from.clone(),
            to: to.clone(),
            kind,
            label,
        };
        self.edges.lock().unwrap().push(edge);
    }

    /// Merge an external Lamport timestamp (from an A2A agent).
    pub fn merge_clock(&self, external_ts: u64) -> u64 {
        self.clock.merge(external_ts)
    }

    /// Current Lamport timestamp.
    pub fn current_ts(&self) -> u64 {
        self.clock.current()
    }

    /// Finalize the collector into an immutable `LineageGraph`.
    pub fn finalize(self) -> LineageGraph {
        let mut nodes = self.nodes.into_inner().unwrap();
        let edges = self.edges.into_inner().unwrap();
        let root_node_id = self.root_node_id.into_inner().unwrap();

        // Sort nodes by Lamport timestamp for topological order
        nodes.sort_by_key(|n| n.lamport_ts);

        let mut graph = LineageGraph {
            run_id: self.run_id,
            started_at: self.started_at,
            completed_at: Some(Utc::now()),
            nodes,
            edges,
            root_node_id,
            stats: LineageStats::default(),
        };

        graph.compute_stats();
        graph
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Persistence — SochDB storage interface
// ═══════════════════════════════════════════════════════════════════════════

/// SochDB key prefix for lineage data.
pub const LINEAGE_PREFIX: &str = "lineage/";

/// Generate the SochDB key for a lineage graph.
pub fn lineage_key(run_id: &str) -> String {
    format!("{}graphs/{}", LINEAGE_PREFIX, run_id)
}

/// Generate the SochDB key prefix for all lineage data of a run.
pub fn lineage_run_prefix(run_id: &str) -> String {
    format!("{}{}/", LINEAGE_PREFIX, run_id)
}

/// Serialize a lineage graph for SochDB storage.
pub fn serialize_graph(graph: &LineageGraph) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(graph)
}

/// Deserialize a lineage graph from SochDB storage.
pub fn deserialize_graph(data: &[u8]) -> Result<LineageGraph, serde_json::Error> {
    serde_json::from_slice(data)
}

// ═══════════════════════════════════════════════════════════════════════════
// Query utilities — O(V+E) graph traversal
// ═══════════════════════════════════════════════════════════════════════════

/// Find all nodes reachable from a given node (forward traversal).
pub fn descendants<'a>(graph: &'a LineageGraph, from: &LineageNodeId) -> Vec<&'a LineageNode> {
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(from);

    while let Some(current) = queue.pop_front() {
        if !visited.insert(current) {
            continue;
        }
        for edge in &graph.edges {
            if &edge.from == current && !visited.contains(&edge.to) {
                queue.push_back(&edge.to);
            }
        }
    }

    graph
        .nodes
        .iter()
        .filter(|n| visited.contains(&n.id))
        .collect()
}

/// Find all ancestor nodes of a given node (backward traversal — root-cause tracing).
pub fn ancestors<'a>(graph: &'a LineageGraph, from: &LineageNodeId) -> Vec<&'a LineageNode> {
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(from);

    while let Some(current) = queue.pop_front() {
        if !visited.insert(current) {
            continue;
        }
        for edge in &graph.edges {
            if &edge.to == current && !visited.contains(&edge.from) {
                queue.push_back(&edge.from);
            }
        }
    }

    graph
        .nodes
        .iter()
        .filter(|n| visited.contains(&n.id))
        .collect()
}

/// Filter nodes by event type.
pub fn nodes_of_type<'a>(
    graph: &'a LineageGraph,
    predicate: impl Fn(&LineageEventType) -> bool,
) -> Vec<&'a LineageNode> {
    graph.nodes.iter().filter(|n| predicate(&n.event)).collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lamport_clock_tick() {
        let clock = LamportClock::new();
        assert_eq!(clock.tick(), 0);
        assert_eq!(clock.tick(), 1);
        assert_eq!(clock.tick(), 2);
        assert_eq!(clock.current(), 3);
    }

    #[test]
    fn test_lamport_clock_merge() {
        let clock = LamportClock::new();
        clock.tick(); // 0 → counter = 1
        clock.tick(); // 1 → counter = 2
        let merged = clock.merge(10); // max(2, 10) + 1 = 11
        assert_eq!(merged, 11);
        assert_eq!(clock.tick(), 11); // continues from 11
    }

    #[test]
    fn test_lineage_collector_basic() {
        let collector = LineageCollector::new("run-1".into());

        let root = collector.record_root(LineageEventType::Request {
            session_id: "s1".into(),
            agent_id: "agent-1".into(),
            content_preview: "Find the config file".into(),
        });

        let tool = collector.record(LineageEventType::ToolCall {
            tool_name: "file_search".into(),
            success: true,
            duration_ms: 150,
            output_preview: "Found config.toml".into(),
        });

        collector.link(&root, &tool, EdgeKind::Caused);

        let response = collector.record(LineageEventType::Response {
            content_preview: "The config is at config.toml".into(),
            total_rounds: 1,
        });

        collector.link(&tool, &response, EdgeKind::Produced);

        let graph = collector.finalize();
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 2);
        assert_eq!(graph.stats.tool_calls, 1);
        assert!(graph.root_node_id.is_some());
    }

    #[test]
    fn test_lineage_descendants() {
        let collector = LineageCollector::new("run-2".into());

        let a = collector.record(LineageEventType::Request {
            session_id: "s".into(),
            agent_id: "a".into(),
            content_preview: "".into(),
        });
        let b = collector.record(LineageEventType::ToolCall {
            tool_name: "t".into(),
            success: true,
            duration_ms: 0,
            output_preview: "".into(),
        });
        let c = collector.record(LineageEventType::Response {
            content_preview: "".into(),
            total_rounds: 1,
        });

        collector.link(&a, &b, EdgeKind::Caused);
        collector.link(&b, &c, EdgeKind::Produced);

        let graph = collector.finalize();
        let desc = descendants(&graph, &a);
        // a → b → c
        assert_eq!(desc.len(), 3); // includes a itself
    }

    #[test]
    fn test_lineage_ancestors() {
        let collector = LineageCollector::new("run-3".into());

        let a = collector.record(LineageEventType::Request {
            session_id: "s".into(),
            agent_id: "a".into(),
            content_preview: "".into(),
        });
        let b = collector.record(LineageEventType::ToolCall {
            tool_name: "t".into(),
            success: true,
            duration_ms: 0,
            output_preview: "".into(),
        });

        collector.link(&a, &b, EdgeKind::Caused);

        let graph = collector.finalize();
        let anc = ancestors(&graph, &b);
        assert_eq!(anc.len(), 2); // b itself + a
    }

    #[test]
    fn test_lineage_serialization() {
        let collector = LineageCollector::new("run-4".into());
        collector.record(LineageEventType::Request {
            session_id: "s".into(),
            agent_id: "a".into(),
            content_preview: "test".into(),
        });

        let graph = collector.finalize();
        let bytes = serialize_graph(&graph).unwrap();
        let restored = deserialize_graph(&bytes).unwrap();
        assert_eq!(restored.run_id, "run-4");
        assert_eq!(restored.nodes.len(), 1);
    }

    #[test]
    fn test_nodes_of_type() {
        let collector = LineageCollector::new("run-5".into());
        collector.record(LineageEventType::ToolCall {
            tool_name: "a".into(),
            success: true,
            duration_ms: 0,
            output_preview: "".into(),
        });
        collector.record(LineageEventType::ToolCall {
            tool_name: "b".into(),
            success: false,
            duration_ms: 0,
            output_preview: "".into(),
        });
        collector.record(LineageEventType::LlmCall {
            model: "m".into(),
            input_tokens: 100,
            output_tokens: 50,
            duration_ms: 1000,
        });

        let graph = collector.finalize();
        let tools = nodes_of_type(&graph, |e| matches!(e, LineageEventType::ToolCall { .. }));
        assert_eq!(tools.len(), 2);
        assert_eq!(graph.stats.llm_calls, 1);
        assert_eq!(graph.stats.total_input_tokens, 100);
    }

    #[test]
    fn test_compute_stats() {
        let collector = LineageCollector::new("run-6".into());
        collector.record(LineageEventType::Subtask {
            parent_node_id: "".into(),
            task_description: "".into(),
            agent_id: "".into(),
            depth: 3,
        });
        collector.record(LineageEventType::ArtifactProduced {
            artifact_id: "art-1".into(),
            artifact_name: "report.pdf".into(),
            mime_type: "application/pdf".into(),
            size_bytes: 1024,
        });
        collector.record(LineageEventType::Delivery {
            target_type: "webhook".into(),
            target_id: "wh-1".into(),
            success: true,
            attempt: 1,
        });

        let graph = collector.finalize();
        assert_eq!(graph.stats.max_depth, 3);
        assert_eq!(graph.stats.artifacts, 1);
        assert_eq!(graph.stats.deliveries, 1);
    }
}
