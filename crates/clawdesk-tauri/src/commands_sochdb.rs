//! Advanced SochDB Tauri commands — exposes SochDB's full power as IPC endpoints.
//!
//! ## Modules exposed
//!
//! - **Semantic Cache**: LLM response caching (exact + embedding similarity)
//! - **Trace Store**: OpenTelemetry-compatible agent run tracing
//! - **Checkpoint Store**: Durable workflow state for multi-step agent tasks
//! - **Knowledge Graph**: Entity–relationship graph with BFS/DFS/shortest path
//! - **Temporal Graph**: Time-bounded edges with point-in-time queries
//! - **Policy Engine**: Access control, rate limiting, audit logging
//! - **Atomic Memory**: All-or-nothing writes across KV + vector + graph
//! - **Agent Registry**: Multi-agent capability routing

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::State;

// ═══════════════════════════════════════════════════════════════════════════
// Shared response types for the frontend
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeInfo {
    pub id: String,
    pub node_type: String,
    pub properties: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdgeInfo {
    pub from_id: String,
    pub edge_type: String,
    pub to_id: String,
    pub properties: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRunInfo {
    pub trace_id: String,
    pub name: String,
    pub start_time: u64,
    pub end_time: Option<u64>,
    pub status: String,
    pub total_tokens: u64,
    pub cost_millicents: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSpanInfo {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub start_time: u64,
    pub end_time: Option<u64>,
    pub duration_us: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheLookupInfo {
    pub hit: bool,
    pub match_type: String,
    pub result: Option<String>,
    pub latency_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub run_id: String,
    pub node_id: String,
    pub seq: u64,
    pub timestamp: u64,
    pub state_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunInfo {
    pub run_id: String,
    pub workflow: String,
    pub status: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub latest_checkpoint_seq: u64,
    pub latest_event_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicWriteInfo {
    pub memory_id: String,
    pub ops_applied: usize,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegistryInfo {
    pub agent_id: String,
    pub capabilities: Vec<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubgraphInfo {
    pub nodes: Vec<GraphNodeInfo>,
    pub edges: Vec<GraphEdgeInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalEdgeInfo {
    pub from_id: String,
    pub edge_type: String,
    pub to_id: String,
    pub valid_start: u64,
    pub valid_end: Option<u64>,
    pub properties: HashMap<String, serde_json::Value>,
    pub version: u64,
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. SEMANTIC CACHE — avoid redundant LLM calls
// ═══════════════════════════════════════════════════════════════════════════

/// Look up a cached LLM response by query text + optional embedding.
#[tauri::command]
pub fn cache_lookup(
    state: State<'_, AppState>,
    query: String,
    namespace: String,
    query_embedding: Option<Vec<f32>>,
) -> Result<CacheLookupInfo, String> {
    let result = state
        .semantic_cache
        .lookup(&query, &namespace, 0, query_embedding.as_deref())
        .map_err(|e| format!("Cache lookup failed: {e}"))?;

    Ok(CacheLookupInfo {
        hit: result.is_hit(),
        match_type: format!("{:?}", result.match_type),
        result: result.result().map(|b| String::from_utf8_lossy(b).to_string()),
        latency_us: result.latency_us,
    })
}

/// Store an LLM response in the semantic cache.
#[tauri::command]
pub fn cache_store(
    state: State<'_, AppState>,
    query: String,
    namespace: String,
    result: String,
    embedding: Option<Vec<f32>>,
    source_docs: Vec<String>,
    ttl_secs: Option<u64>,
) -> Result<String, String> {
    let ttl = ttl_secs.map(std::time::Duration::from_secs);
    let key = state
        .semantic_cache
        .store(&query, &namespace, 0, result.as_bytes(), embedding, source_docs, ttl)
        .map_err(|e| format!("Cache store failed: {e}"))?;

    Ok(format!("{:?}", key))
}

/// Invalidate all cache entries derived from a source document.
#[tauri::command]
pub fn cache_invalidate_source(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<usize, String> {
    state
        .semantic_cache
        .invalidate_by_source(&doc_id)
        .map_err(|e| format!("Cache invalidation failed: {e}"))
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. TRACE STORE — agent run observability
// ═══════════════════════════════════════════════════════════════════════════

/// Start a new trace run for an agent operation.
#[tauri::command]
pub fn trace_start_run(
    state: State<'_, AppState>,
    name: String,
    resource: HashMap<String, String>,
) -> Result<TraceRunInfo, String> {
    let run = state
        .trace_store
        .start_run(name, resource)
        .map_err(|e| format!("Trace start failed: {e}"))?;

    Ok(TraceRunInfo {
        trace_id: run.trace_id,
        name: run.name,
        start_time: run.start_time,
        end_time: run.end_time,
        status: format!("{:?}", run.status),
        total_tokens: run.total_tokens,
        cost_millicents: run.cost_millicents,
    })
}

/// End a trace run.
#[tauri::command]
pub fn trace_end_run(
    state: State<'_, AppState>,
    trace_id: String,
) -> Result<(), String> {
    state
        .trace_store
        .end_run(&trace_id, sochdb::trace::TraceStatus::Ok)
        .map_err(|e| format!("Trace end failed: {e}"))
}

/// Start a span within a trace run.
#[tauri::command]
pub fn trace_start_span(
    state: State<'_, AppState>,
    trace_id: String,
    name: String,
    parent_span_id: Option<String>,
    kind: String,
) -> Result<TraceSpanInfo, String> {
    let span_kind = match kind.as_str() {
        "server" => sochdb::trace::SpanKind::Server,
        "client" => sochdb::trace::SpanKind::Client,
        "producer" => sochdb::trace::SpanKind::Producer,
        "consumer" => sochdb::trace::SpanKind::Consumer,
        _ => sochdb::trace::SpanKind::Internal,
    };

    let span = state
        .trace_store
        .start_span(&trace_id, name, parent_span_id, span_kind)
        .map_err(|e| format!("Span start failed: {e}"))?;

    Ok(TraceSpanInfo {
        trace_id: span.trace_id,
        span_id: span.span_id,
        parent_span_id: span.parent_span_id,
        name: span.name,
        kind: format!("{:?}", span.kind),
        start_time: span.start_time,
        end_time: span.end_time,
        duration_us: span.duration_us,
    })
}

/// End a span within a trace run.
#[tauri::command]
pub fn trace_end_span(
    state: State<'_, AppState>,
    trace_id: String,
    span_id: String,
    status: String,
    message: Option<String>,
) -> Result<(), String> {
    let code = match status.as_str() {
        "ok" => sochdb::SpanStatusCode::Ok,
        "error" => sochdb::SpanStatusCode::Error,
        _ => sochdb::SpanStatusCode::Unset,
    };
    state
        .trace_store
        .end_span(&trace_id, &span_id, code, message)
        .map_err(|e| format!("Span end failed: {e}"))
}

/// Get all spans for a trace run.
#[tauri::command]
pub fn trace_get_spans(
    state: State<'_, AppState>,
    trace_id: String,
) -> Result<Vec<TraceSpanInfo>, String> {
    let spans = state
        .trace_store
        .get_spans(&trace_id)
        .map_err(|e| format!("Get spans failed: {e}"))?;

    Ok(spans.into_iter().map(|s| TraceSpanInfo {
        trace_id: s.trace_id,
        span_id: s.span_id,
        parent_span_id: s.parent_span_id,
        name: s.name,
        kind: format!("{:?}", s.kind),
        start_time: s.start_time,
        end_time: s.end_time,
        duration_us: s.duration_us,
    }).collect())
}

/// Get a trace run by ID.
#[tauri::command]
pub fn trace_get_run(
    state: State<'_, AppState>,
    trace_id: String,
) -> Result<Option<TraceRunInfo>, String> {
    let run = state
        .trace_store
        .get_run(&trace_id)
        .map_err(|e| format!("Get run failed: {e}"))?;

    Ok(run.map(|r| TraceRunInfo {
        trace_id: r.trace_id,
        name: r.name,
        start_time: r.start_time,
        end_time: r.end_time,
        status: format!("{:?}", r.status),
        total_tokens: r.total_tokens,
        cost_millicents: r.cost_millicents,
    }))
}

/// Update token/cost metrics for a trace run.
#[tauri::command]
pub fn trace_update_metrics(
    state: State<'_, AppState>,
    trace_id: String,
    tokens: u64,
    cost_millicents: u64,
) -> Result<(), String> {
    state
        .trace_store
        .update_run_metrics(&trace_id, tokens, cost_millicents)
        .map_err(|e| format!("Update metrics failed: {e}"))
}

/// Log a tool call event within a span.
#[tauri::command]
pub fn trace_log_tool_call(
    state: State<'_, AppState>,
    trace_id: String,
    span_id: String,
    tool_name: String,
    arguments: String,
    result: Option<String>,
    duration_us: u64,
    success: bool,
) -> Result<(), String> {
    let event = sochdb::trace::ToolCallEvent {
        tool_name,
        arguments,
        result,
        duration_us,
        success,
        error: None,
    };
    state
        .trace_store
        .log_tool_call(&trace_id, &span_id, event)
        .map_err(|e| format!("Log tool call failed: {e}"))
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. CHECKPOINT STORE — durable workflow state
// ═══════════════════════════════════════════════════════════════════════════

/// Create a new workflow run.
#[tauri::command]
pub fn checkpoint_create_run(
    state: State<'_, AppState>,
    run_id: String,
    workflow: String,
    params: HashMap<String, serde_json::Value>,
) -> Result<WorkflowRunInfo, String> {
    use sochdb::CheckpointStore;
    let meta = state
        .checkpoint_store
        .create_run(&run_id, &workflow, params)
        .map_err(|e| format!("Create run failed: {e}"))?;

    Ok(WorkflowRunInfo {
        run_id: meta.run_id,
        workflow: meta.workflow,
        status: format!("{:?}", meta.status),
        created_at: meta.created_at,
        updated_at: meta.updated_at,
        latest_checkpoint_seq: meta.latest_checkpoint_seq,
        latest_event_seq: meta.latest_event_seq,
    })
}

/// Save a checkpoint for a workflow node.
#[tauri::command]
pub fn checkpoint_save(
    state: State<'_, AppState>,
    run_id: String,
    node_id: String,
    state_json: String,
    metadata: Option<HashMap<String, String>>,
) -> Result<CheckpointInfo, String> {
    use sochdb::CheckpointStore;
    let meta = state
        .checkpoint_store
        .save_checkpoint(&run_id, &node_id, state_json.as_bytes(), metadata)
        .map_err(|e| format!("Save checkpoint failed: {e}"))?;

    Ok(CheckpointInfo {
        run_id: meta.run_id,
        node_id: meta.node_id,
        seq: meta.seq,
        timestamp: meta.timestamp,
        state_size: meta.state_size,
    })
}

/// Load the latest checkpoint for a workflow node.
#[tauri::command]
pub fn checkpoint_load(
    state: State<'_, AppState>,
    run_id: String,
    node_id: String,
) -> Result<Option<String>, String> {
    use sochdb::CheckpointStore;
    let cp = state
        .checkpoint_store
        .load_checkpoint(&run_id, &node_id)
        .map_err(|e| format!("Load checkpoint failed: {e}"))?;

    Ok(cp.map(|c| String::from_utf8_lossy(&c.state).to_string()))
}

/// List all checkpoints for a workflow run.
#[tauri::command]
pub fn checkpoint_list(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<Vec<CheckpointInfo>, String> {
    use sochdb::CheckpointStore;
    let metas = state
        .checkpoint_store
        .list_checkpoints(&run_id)
        .map_err(|e| format!("List checkpoints failed: {e}"))?;

    Ok(metas.into_iter().map(|m| CheckpointInfo {
        run_id: m.run_id,
        node_id: m.node_id,
        seq: m.seq,
        timestamp: m.timestamp,
        state_size: m.state_size,
    }).collect())
}

/// Get workflow run metadata.
#[tauri::command]
pub fn checkpoint_get_run(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<Option<WorkflowRunInfo>, String> {
    use sochdb::CheckpointStore;
    let meta = state
        .checkpoint_store
        .get_run(&run_id)
        .map_err(|e| format!("Get run failed: {e}"))?;

    Ok(meta.map(|m| WorkflowRunInfo {
        run_id: m.run_id,
        workflow: m.workflow,
        status: format!("{:?}", m.status),
        created_at: m.created_at,
        updated_at: m.updated_at,
        latest_checkpoint_seq: m.latest_checkpoint_seq,
        latest_event_seq: m.latest_event_seq,
    }))
}

/// Delete a workflow run and all its checkpoints/events.
#[tauri::command]
pub fn checkpoint_delete_run(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<bool, String> {
    use sochdb::CheckpointStore;
    state
        .checkpoint_store
        .delete_run(&run_id)
        .map_err(|e| format!("Delete run failed: {e}"))
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. KNOWLEDGE GRAPH — entity relationships
// ═══════════════════════════════════════════════════════════════════════════

/// Add a node to the knowledge graph.
#[tauri::command]
pub fn graph_add_node(
    state: State<'_, AppState>,
    node_id: String,
    node_type: String,
    properties: Option<HashMap<String, serde_json::Value>>,
) -> Result<GraphNodeInfo, String> {
    let node = state
        .knowledge_graph
        .add_node(&node_id, &node_type, properties)
        .map_err(|e| format!("Add node failed: {e}"))?;

    Ok(GraphNodeInfo {
        id: node.id,
        node_type: node.node_type,
        properties: node.properties,
    })
}

/// Get a node from the knowledge graph.
#[tauri::command]
pub fn graph_get_node(
    state: State<'_, AppState>,
    node_id: String,
) -> Result<Option<GraphNodeInfo>, String> {
    let node = state
        .knowledge_graph
        .get_node(&node_id)
        .map_err(|e| format!("Get node failed: {e}"))?;

    Ok(node.map(|n| GraphNodeInfo {
        id: n.id,
        node_type: n.node_type,
        properties: n.properties,
    }))
}

/// Delete a node from the knowledge graph (with optional cascade).
#[tauri::command]
pub fn graph_delete_node(
    state: State<'_, AppState>,
    node_id: String,
    cascade: bool,
) -> Result<bool, String> {
    state
        .knowledge_graph
        .delete_node(&node_id, cascade)
        .map_err(|e| format!("Delete node failed: {e}"))
}

/// Add an edge between two nodes.
#[tauri::command]
pub fn graph_add_edge(
    state: State<'_, AppState>,
    from_id: String,
    edge_type: String,
    to_id: String,
    properties: Option<HashMap<String, serde_json::Value>>,
) -> Result<GraphEdgeInfo, String> {
    let edge = state
        .knowledge_graph
        .add_edge(&from_id, &edge_type, &to_id, properties)
        .map_err(|e| format!("Add edge failed: {e}"))?;

    Ok(GraphEdgeInfo {
        from_id: edge.from_id,
        edge_type: edge.edge_type,
        to_id: edge.to_id,
        properties: edge.properties,
    })
}

/// Get outgoing edges from a node.
#[tauri::command]
pub fn graph_get_edges(
    state: State<'_, AppState>,
    from_id: String,
    edge_type: Option<String>,
) -> Result<Vec<GraphEdgeInfo>, String> {
    let edges = state
        .knowledge_graph
        .get_edges(&from_id, edge_type.as_deref())
        .map_err(|e| format!("Get edges failed: {e}"))?;

    Ok(edges.into_iter().map(|e| GraphEdgeInfo {
        from_id: e.from_id,
        edge_type: e.edge_type,
        to_id: e.to_id,
        properties: e.properties,
    }).collect())
}

/// Find the shortest path between two nodes.
#[tauri::command]
pub fn graph_shortest_path(
    state: State<'_, AppState>,
    from_id: String,
    to_id: String,
    max_depth: usize,
) -> Result<Option<Vec<String>>, String> {
    state
        .knowledge_graph
        .shortest_path(&from_id, &to_id, max_depth, None)
        .map_err(|e| format!("Shortest path failed: {e}"))
}

/// Get a subgraph starting from a node with BFS up to max_depth.
#[tauri::command]
pub fn graph_get_subgraph(
    state: State<'_, AppState>,
    start_id: String,
    max_depth: usize,
) -> Result<SubgraphInfo, String> {
    let sg = state
        .knowledge_graph
        .get_subgraph(&start_id, max_depth, None)
        .map_err(|e| format!("Get subgraph failed: {e}"))?;

    Ok(SubgraphInfo {
        nodes: sg.nodes.into_values().map(|n| GraphNodeInfo {
            id: n.id,
            node_type: n.node_type,
            properties: n.properties,
        }).collect(),
        edges: sg.edges.into_iter().map(|e| GraphEdgeInfo {
            from_id: e.from_id,
            edge_type: e.edge_type,
            to_id: e.to_id,
            properties: e.properties,
        }).collect(),
    })
}

/// Get all nodes of a specific type.
#[tauri::command]
pub fn graph_get_nodes_by_type(
    state: State<'_, AppState>,
    node_type: String,
    limit: usize,
) -> Result<Vec<GraphNodeInfo>, String> {
    let nodes = state
        .knowledge_graph
        .get_nodes_by_type(&node_type, limit)
        .map_err(|e| format!("Get nodes by type failed: {e}"))?;

    Ok(nodes.into_iter().map(|n| GraphNodeInfo {
        id: n.id,
        node_type: n.node_type,
        properties: n.properties,
    }).collect())
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. TEMPORAL GRAPH — time-travel over agent beliefs
// ═══════════════════════════════════════════════════════════════════════════

/// Add a temporal edge (valid from now).
#[tauri::command]
pub fn temporal_add_edge(
    state: State<'_, AppState>,
    from_id: String,
    edge_type: String,
    to_id: String,
    properties: Option<HashMap<String, serde_json::Value>>,
) -> Result<TemporalEdgeInfo, String> {
    let edge = state
        .temporal_graph
        .add_edge(&from_id, &edge_type, &to_id, properties)
        .map_err(|e| format!("Add temporal edge failed: {e}"))?;

    Ok(TemporalEdgeInfo {
        from_id: edge.from_id,
        edge_type: edge.edge_type,
        to_id: edge.to_id,
        valid_start: edge.validity.start,
        valid_end: edge.validity.end,
        properties: edge.properties,
        version: edge.version,
    })
}

/// Invalidate a temporal edge at the current time.
#[tauri::command]
pub fn temporal_invalidate_edge(
    state: State<'_, AppState>,
    from_id: String,
    edge_type: String,
    to_id: String,
) -> Result<bool, String> {
    state
        .temporal_graph
        .invalidate_edge(&from_id, &edge_type, &to_id)
        .map_err(|e| format!("Invalidate edge failed: {e}"))
}

/// Get edges valid at a specific point in time.
#[tauri::command]
pub fn temporal_edges_at(
    state: State<'_, AppState>,
    from_id: String,
    edge_type: Option<String>,
    at_time: u64,
) -> Result<Vec<TemporalEdgeInfo>, String> {
    let edges = state
        .temporal_graph
        .get_edges_at(&from_id, edge_type.as_deref(), at_time)
        .map_err(|e| format!("Get edges at time failed: {e}"))?;

    Ok(edges.into_iter().map(|e| TemporalEdgeInfo {
        from_id: e.from_id,
        edge_type: e.edge_type,
        to_id: e.to_id,
        valid_start: e.validity.start,
        valid_end: e.validity.end,
        properties: e.properties,
        version: e.version,
    }).collect())
}

/// Get the full history of an edge (all versions).
#[tauri::command]
pub fn temporal_edge_history(
    state: State<'_, AppState>,
    from_id: String,
    edge_type: String,
    to_id: String,
) -> Result<Vec<TemporalEdgeInfo>, String> {
    let edges = state
        .temporal_graph
        .edge_history(&from_id, &edge_type, &to_id)
        .map_err(|e| format!("Edge history failed: {e}"))?;

    Ok(edges.into_iter().map(|e| TemporalEdgeInfo {
        from_id: e.from_id,
        edge_type: e.edge_type,
        to_id: e.to_id,
        valid_start: e.validity.start,
        valid_end: e.validity.end,
        properties: e.properties,
        version: e.version,
    }).collect())
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. POLICY ENGINE — access control & audit
// ═══════════════════════════════════════════════════════════════════════════

/// Enable audit logging in the policy engine.
#[tauri::command]
pub fn policy_enable_audit(state: State<'_, AppState>) -> Result<(), String> {
    state.policy_engine.enable_audit();
    Ok(())
}

/// Get recent audit log entries from the policy engine.
#[tauri::command]
pub fn policy_get_audit_log(
    state: State<'_, AppState>,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let entries = state.policy_engine.get_audit_log(limit);
    Ok(entries.into_iter().map(|e| {
        serde_json::json!({
            "operation": e.operation,
            "key": e.key,
            "agent_id": e.agent_id,
            "session_id": e.session_id,
            "result": e.result,
        })
    }).collect())
}

/// Add a rate limit policy.
#[tauri::command]
pub fn policy_add_rate_limit(
    state: State<'_, AppState>,
    operation: String,
    max_per_minute: u32,
    scope: String,
) -> Result<(), String> {
    state.policy_engine.add_rate_limit(&operation, max_per_minute, &scope);
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. ATOMIC MEMORY — transactional multi-index writes
// ═══════════════════════════════════════════════════════════════════════════

/// Perform an atomic write across KV + vector + graph indexes.
///
/// All operations succeed or all fail — no torn state on crash.
#[tauri::command]
pub fn atomic_memory_write(
    state: State<'_, AppState>,
    memory_id: String,
    blobs: Vec<(String, String)>,
    graph_nodes: Vec<(String, String, String)>,
    graph_edges: Vec<(String, String, String, String)>,
) -> Result<AtomicWriteInfo, String> {
    let mut ops = Vec::new();

    // Add blob operations
    for (key, value) in blobs {
        ops.push(sochdb::MemoryOp::PutBlob {
            key: key.into_bytes(),
            value: value.into_bytes(),
        });
    }

    // Add graph node operations
    for (node_id, node_type, _ns) in graph_nodes {
        ops.push(sochdb::MemoryOp::CreateNode {
            namespace: "clawdesk".to_string(),
            node_id,
            node_type,
            properties: HashMap::new(),
        });
    }

    // Add graph edge operations
    for (from_id, edge_type, to_id, _ns) in graph_edges {
        ops.push(sochdb::MemoryOp::CreateEdge {
            namespace: "clawdesk".to_string(),
            from_id,
            edge_type,
            to_id,
            properties: HashMap::new(),
        });
    }

    let result = state
        .atomic_writer
        .write_atomic(memory_id, ops)
        .map_err(|e| format!("Atomic write failed: {e}"))?;

    Ok(AtomicWriteInfo {
        memory_id: result.memory_id,
        ops_applied: result.ops_applied,
        status: format!("{:?}", result.status),
    })
}

/// Recover any incomplete atomic writes from WAL (call on startup).
#[tauri::command]
pub fn atomic_memory_recover(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let report = state
        .atomic_writer
        .recover()
        .map_err(|e| format!("Recovery failed: {e}"))?;

    Ok(serde_json::json!({
        "replayed": report.replayed,
        "failed": report.failed,
        "already_committed": report.already_committed,
        "already_aborted": report.already_aborted,
        "corrupted": report.corrupted,
    }))
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. AGENT REGISTRY — multi-agent capability routing
// ═══════════════════════════════════════════════════════════════════════════

/// Register an agent with tool capabilities.
#[tauri::command]
pub fn registry_register_agent(
    state: State<'_, AppState>,
    agent_id: String,
    capabilities: Vec<String>,
) -> Result<AgentRegistryInfo, String> {
    let caps: Vec<sochdb::routing::ToolCategory> = capabilities
        .iter()
        .map(|c| match c.as_str() {
            "code" => sochdb::routing::ToolCategory::Code,
            "search" => sochdb::routing::ToolCategory::Search,
            "database" => sochdb::routing::ToolCategory::Database,
            "web" => sochdb::routing::ToolCategory::Web,
            "file" => sochdb::routing::ToolCategory::File,
            "git" => sochdb::routing::ToolCategory::Git,
            "shell" => sochdb::routing::ToolCategory::Shell,
            "memory" => sochdb::routing::ToolCategory::Memory,
            "vector" => sochdb::routing::ToolCategory::Vector,
            "graph" => sochdb::routing::ToolCategory::Graph,
            _ => sochdb::routing::ToolCategory::Custom,
        })
        .collect();

    let config = sochdb::routing::AgentConfig::default();
    let agent = state.agent_registry.register_agent(&agent_id, caps, config);

    Ok(AgentRegistryInfo {
        agent_id: agent.agent_id.clone(),
        capabilities: capabilities,
        status: format!("{:?}", agent.status),
    })
}

/// List all registered agents.
#[tauri::command]
pub fn registry_list_agents(
    state: State<'_, AppState>,
) -> Result<Vec<AgentRegistryInfo>, String> {
    let agents = state.agent_registry.list_agents();
    Ok(agents.into_iter().map(|a| AgentRegistryInfo {
        agent_id: a.agent_id.clone(),
        capabilities: a.capabilities.iter().map(|c| format!("{:?}", c)).collect(),
        status: format!("{:?}", a.status),
    }).collect())
}

/// Find agents capable of handling required tool categories.
#[tauri::command]
pub fn registry_find_capable(
    state: State<'_, AppState>,
    required: Vec<String>,
    exclude: Vec<String>,
) -> Result<Vec<AgentRegistryInfo>, String> {
    let caps: Vec<sochdb::routing::ToolCategory> = required
        .iter()
        .map(|c| match c.as_str() {
            "code" => sochdb::routing::ToolCategory::Code,
            "search" => sochdb::routing::ToolCategory::Search,
            "database" => sochdb::routing::ToolCategory::Database,
            "web" => sochdb::routing::ToolCategory::Web,
            "file" => sochdb::routing::ToolCategory::File,
            _ => sochdb::routing::ToolCategory::Custom,
        })
        .collect();

    let agents = state.agent_registry.find_capable_agents(&caps, &exclude);
    Ok(agents.into_iter().map(|a| AgentRegistryInfo {
        agent_id: a.agent_id.clone(),
        capabilities: a.capabilities.iter().map(|c| format!("{:?}", c)).collect(),
        status: format!("{:?}", a.status),
    }).collect())
}

/// Unregister an agent.
#[tauri::command]
pub fn registry_unregister_agent(
    state: State<'_, AppState>,
    agent_id: String,
) -> Result<bool, String> {
    Ok(state.agent_registry.unregister_agent(&agent_id))
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. SOCHDB DIRECT — raw path queries and stats
// ═══════════════════════════════════════════════════════════════════════════

/// Perform a WAL checkpoint + GC to reclaim space.
#[tauri::command]
pub fn sochdb_checkpoint(state: State<'_, AppState>) -> Result<u64, String> {
    state
        .soch_store
        .checkpoint_and_gc()
        .map_err(|e| format!("Checkpoint failed: {e}"))
}

/// Force an fsync to ensure all buffered writes are durable.
#[tauri::command]
pub fn sochdb_sync(state: State<'_, AppState>) -> Result<(), String> {
    state
        .soch_store
        .sync()
        .map_err(|e| format!("Sync failed: {e}"))
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. STORAGE HEALTH — unified status for all storage subsystems
// ═══════════════════════════════════════════════════════════════════════════

/// Get storage health across all subsystems.
///
/// Returns overall status + per-store details + recommendations.
/// The UI should show a persistent banner when `any_ephemeral` is true.
#[tauri::command]
pub fn storage_health(
    state: State<'_, AppState>,
) -> Result<clawdesk_sochdb::health::StorageHealth, String> {
    Ok(clawdesk_sochdb::health::StorageHealth::check(
        &state.soch_store,
        Some(&state.thread_store),
    ))
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. LIFECYCLE — cascading delete operations
// ═══════════════════════════════════════════════════════════════════════════

/// Delete a session with full cascade (state + messages + summaries + indexes +
/// tool history + graph + traces + checkpoints + memory namespace).
#[tauri::command]
pub fn lifecycle_delete_session(
    state: State<'_, AppState>,
    session_id: String,
    agent_id: Option<String>,
) -> Result<clawdesk_sochdb::lifecycle::LifecycleReport, String> {
    state.lifecycle_manager.delete_session_full(
        &session_id,
        agent_id.as_deref(),
    )
}

/// Delete a thread with full cascade (ThreadStore + memory namespace + graph + traces).
#[tauri::command]
pub fn lifecycle_delete_thread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<clawdesk_sochdb::lifecycle::LifecycleReport, String> {
    let id = u128::from_str_radix(&thread_id.replace('-', ""), 16)
        .map_err(|e| format!("Invalid thread ID: {e}"))?;
    state.lifecycle_manager.delete_thread_full(id)
}

/// Delete an agent and ALL related data (sessions, threads, config).
#[tauri::command]
pub fn lifecycle_delete_agent(
    state: State<'_, AppState>,
    agent_id: String,
) -> Result<clawdesk_sochdb::lifecycle::LifecycleReport, String> {
    state.lifecycle_manager.delete_agent_full(&agent_id)
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. STRUCTURED TRACING — queryable span attributes
// ═══════════════════════════════════════════════════════════════════════════

/// Set structured attributes on a trace span.
#[tauri::command]
pub fn trace_set_span_attributes(
    state: State<'_, AppState>,
    trace_id: String,
    span_id: String,
    attributes: HashMap<String, serde_json::Value>,
) -> Result<(), String> {
    state.structured_tracing.set_span_attributes(&trace_id, &span_id, &attributes)
}

/// Add a structured event to a trace span.
#[tauri::command]
pub fn trace_add_span_event(
    state: State<'_, AppState>,
    trace_id: String,
    span_id: String,
    event_name: String,
    attributes: HashMap<String, serde_json::Value>,
) -> Result<(), String> {
    state.structured_tracing.add_span_event(&trace_id, &span_id, &event_name, &attributes)
}

/// Get structured attributes for a trace span.
#[tauri::command]
pub fn trace_get_span_attributes(
    state: State<'_, AppState>,
    trace_id: String,
    span_id: String,
) -> Result<Option<clawdesk_sochdb::structured_trace::SpanAttributes>, String> {
    state.structured_tracing.get_span_attributes(&trace_id, &span_id)
}

/// Query spans by a specific attribute value.
#[tauri::command]
pub fn trace_query_spans_by_attribute(
    state: State<'_, AppState>,
    trace_id: String,
    filter_key: String,
    filter_value: serde_json::Value,
) -> Result<Vec<(String, clawdesk_sochdb::structured_trace::SpanAttributes)>, String> {
    state.structured_tracing.query_spans_by_attribute(&trace_id, &filter_key, &filter_value)
}

/// Set run-level structured attributes.
#[tauri::command]
pub fn trace_set_run_attributes(
    state: State<'_, AppState>,
    trace_id: String,
    attributes: HashMap<String, serde_json::Value>,
) -> Result<(), String> {
    state.structured_tracing.set_run_attributes(&trace_id, &attributes)
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. SESSION INDEXES — efficient session listing
// ═══════════════════════════════════════════════════════════════════════════

/// List session IDs ordered by last activity (most recent first).
#[tauri::command]
pub fn sessions_by_activity(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<String>, String> {
    state.session_index.list_by_activity(limit.unwrap_or(100))
}

/// List session IDs filtered by channel.
#[tauri::command]
pub fn sessions_by_channel(
    state: State<'_, AppState>,
    channel: String,
    limit: Option<usize>,
) -> Result<Vec<String>, String> {
    state.session_index.list_by_channel(&channel, limit.unwrap_or(100))
}

/// List session IDs filtered by agent.
#[tauri::command]
pub fn sessions_by_agent(
    state: State<'_, AppState>,
    agent_id: String,
    limit: Option<usize>,
) -> Result<Vec<String>, String> {
    state.session_index.list_by_agent(&agent_id, limit.unwrap_or(100))
}

/// Rebuild all session indexes from primary data.
#[tauri::command]
pub fn sessions_rebuild_indexes(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    state.session_index.rebuild_all()
}
