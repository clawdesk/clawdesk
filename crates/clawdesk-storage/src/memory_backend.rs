//! Extended memory backend trait — extends `VectorStore` with graph, temporal,
//! atomic write, batch write, policy, trace, memory schema, context query,
//! path query, task queue, cost model, filter pushdown, and multi-vector
//! capabilities.
//!
//! This trait bridges the gap between the minimal `VectorStore` port and the
//! rich feature set provided by SochDB's advanced modules. It enables
//! `clawdesk-memory` to leverage:
//!
//! - **Atomic writes** — all-or-nothing multi-index writes
//! - **Batch writes** — group-commit for high-throughput ingestion
//! - **Knowledge graph** — nodes, edges, traversal
//! - **Temporal graph** — time-bounded edges, point-in-time queries
//! - **Policy** — PII redaction, access control
//! - **Trace** — OpenTelemetry-compatible instrumentation
//! - **Memory schema** — Episodes, Events, Entities (canonical LLM memory)
//! - **Context query** — Token-budgeted LLM context assembly
//! - **Path query** — O(|path|) hierarchical key lookups
//! - **Task queue** — Priority-ordered durable background tasks
//! - **Cost model** — SLA-driven search budgeting
//! - **Filter pushdown** — Early predicate evaluation before vector search
//! - **Multi-vector** — Per-chunk document aggregation
//!
//! ## Design
//!
//! Each capability is optional via default no-op implementations, so backends
//! that only support basic vector storage still compile. The `SochMemoryBackend`
//! adapter in `clawdesk-sochdb` provides the full implementation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Atomic Write Types
// ============================================================================

/// A single operation within an atomic memory write intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemoryWriteOp {
    /// Store a blob/value in KV.
    PutBlob { key: String, value: Vec<u8> },
    /// Store a vector embedding.
    PutEmbedding {
        collection: String,
        id: String,
        embedding: Vec<f32>,
        metadata: HashMap<String, String>,
    },
    /// Create a graph node.
    CreateNode {
        namespace: String,
        node_id: String,
        node_type: String,
        properties: HashMap<String, serde_json::Value>,
    },
    /// Create a graph edge.
    CreateEdge {
        namespace: String,
        from_id: String,
        edge_type: String,
        to_id: String,
        properties: HashMap<String, serde_json::Value>,
    },
}

/// Result of an atomic write operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicWriteResult {
    pub intent_id: u64,
    pub memory_id: String,
    pub ops_applied: usize,
    pub committed: bool,
}

// ============================================================================
// Temporal Edge Types
// ============================================================================

/// A temporal edge with validity interval for the memory backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalEdgeInfo {
    pub from_id: String,
    pub edge_type: String,
    pub to_id: String,
    pub valid_from: u64,
    pub valid_until: Option<u64>,
    pub properties: HashMap<String, serde_json::Value>,
}

// ============================================================================
// Graph Neighbor Info
// ============================================================================

/// A neighbor discovered via graph traversal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNeighborInfo {
    pub node_id: String,
    pub edge_type: String,
    pub properties: HashMap<String, serde_json::Value>,
}

// ============================================================================
// Trace Span Types
// ============================================================================

/// A trace span for observability.
#[derive(Debug, Clone)]
pub struct MemoryTraceSpan {
    pub run_id: String,
    pub span_id: String,
}

// ============================================================================
// Policy Check Result
// ============================================================================

/// Result of a policy check.
#[derive(Debug, Clone)]
pub enum PolicyCheckResult {
    /// Content is allowed.
    Allow,
    /// Content was redacted (modified content returned).
    Redacted(String),
    /// Content is denied.
    Deny(String),
}

// ============================================================================
// Batch Write Types (A7)
// ============================================================================

/// Result of a batch write operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchWriteResult {
    pub ops_executed: usize,
    pub ops_failed: usize,
    pub duration_ms: u64,
    pub chunks_committed: usize,
}

// ============================================================================
// Memory Schema Types (A4) — Episodes, Events, Entities
// ============================================================================

/// Episode type classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpisodeType {
    Conversation,
    Task,
    Workflow,
    Debug,
    AgentInteraction,
    Other,
}

/// A bounded conversation or task run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub episode_id: String,
    pub episode_type: EpisodeType,
    pub entity_ids: Vec<String>,
    pub ts_start: u64,
    pub ts_end: u64,
    pub summary: String,
    pub tags: Vec<String>,
    pub embedding: Option<Vec<f32>>,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Role of an event within an episode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventRole {
    User,
    Assistant,
    System,
    Tool,
    External,
}

/// Performance metrics for an event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventMetrics {
    pub duration_us: u64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cost_micros: Option<u64>,
}

/// A timestamped step within an episode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub episode_id: String,
    pub seq: u64,
    pub ts: u64,
    pub role: EventRole,
    pub tool_name: Option<String>,
    pub input_toon: String,
    pub output_toon: String,
    pub error: Option<String>,
    pub metrics: EventMetrics,
}

/// Entity kind classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityKind {
    User,
    Project,
    Document,
    Service,
    Agent,
    Organization,
    Custom,
}

/// A persistent entity (user, project, document, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub entity_id: String,
    pub kind: EntityKind,
    pub name: String,
    pub attributes: HashMap<String, serde_json::Value>,
    pub embedding: Option<Vec<f32>>,
    pub metadata: HashMap<String, serde_json::Value>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Facts about an entity including recent episodes and relationships.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityFacts {
    pub entity: Entity,
    pub recent_episodes: Vec<Episode>,
    pub related_entities: Vec<Entity>,
}

// ============================================================================
// Context Query Types (A1)
// ============================================================================

/// Truncation strategy for context assembly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TruncationStrategy {
    TailDrop,
    HeadDrop,
    Proportional,
    Strict,
}

/// Output format for assembled context.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ContextFormat {
    Soch,
    Json,
    Markdown,
    Text,
}

/// A section of assembled context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSection {
    pub name: String,
    pub priority: i32,
    pub content: String,
    pub token_count: usize,
    pub truncated: bool,
}

/// Result of a context query execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextQueryResult {
    pub context: String,
    pub token_count: usize,
    pub budget: usize,
    pub sections: Vec<ContextSection>,
    pub utilization: f64,
}

// ============================================================================
// Task Queue Types (A8)
// ============================================================================

/// Priority and payload for a background task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub task_id: String,
    pub priority: i64,
    pub payload: Vec<u8>,
    pub created_at: u64,
}

/// Task claim result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskClaimResult {
    Success(BackgroundTask),
    Empty,
    Contention,
}

/// Task queue statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskQueueStats {
    pub pending: usize,
    pub claimed: usize,
    pub completed: usize,
    pub dead_lettered: usize,
}

// ============================================================================
// Cost Model / Query Budget Types (A9)
// ============================================================================

/// Search quality profile for adaptive budgeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchProfile {
    /// p99 ≤ 5ms, recall ≥ 0.80 — for autocomplete, interactive typing
    LowLatency,
    /// p99 ≤ 20ms, recall ≥ 0.90 — for direct questions
    Balanced,
    /// p99 ≤ 100ms, recall ≥ 0.99 — for thorough research queries
    HighRecall,
}

/// Budget summary after a query.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryCostSummary {
    pub ram_bytes_used: u64,
    pub ssd_reads: u32,
    pub cpu_cycles: u64,
    pub latency_ms: f64,
    pub estimated_recall: f32,
}

// ============================================================================
// Filter Pushdown Types (A12)
// ============================================================================

/// A filter predicate for pushdown evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FilterPredicate {
    Eq(String, serde_json::Value),
    Ne(String, serde_json::Value),
    Gt(String, serde_json::Value),
    Lt(String, serde_json::Value),
    In(String, Vec<serde_json::Value>),
    Contains(String, String),
    And(Vec<FilterPredicate>),
    Or(Vec<FilterPredicate>),
}

/// Options for a filtered vector search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilteredSearchOptions {
    pub collection: String,
    pub query_embedding: Vec<f32>,
    pub query_text: Option<String>,
    pub top_k: usize,
    pub filters: Vec<FilterPredicate>,
    pub search_profile: Option<SearchProfile>,
}

// ============================================================================
// Multi-Vector Document Types (A11)
// ============================================================================

/// Aggregation method for multi-vector documents.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum AggregationMethod {
    /// Best-matching chunk determines document score (ColBERT-like)
    MaxSim,
    /// Average chunk relevance
    MeanPool,
    /// Sum of chunk scores (sparse-like)
    Sum,
}

/// A document with multiple vector embeddings (one per chunk).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiVectorDocument {
    pub id: String,
    pub vectors: Vec<Vec<f32>>,
    pub text_chunks: Vec<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Document-level search result with aggregated score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSearchResult {
    pub doc_id: String,
    pub score: f32,
    pub best_chunk_index: usize,
    pub chunk_scores: Vec<f32>,
    pub metadata: HashMap<String, serde_json::Value>,
}

// ============================================================================
// Path Query Types (A6)
// ============================================================================

/// Result row from a path query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathQueryRow {
    pub path: String,
    pub values: HashMap<String, serde_json::Value>,
}

// ============================================================================
// MemoryBackend Trait
// ============================================================================

/// Extended memory backend with graph, temporal, atomic, batch, policy, trace,
/// memory schema, context query, path query, task queue, cost model, filter
/// pushdown, and multi-vector capabilities.
///
/// Extends the base `VectorStore` contract with operations that SochDB's
/// advanced modules provide.
///
/// All methods have default no-op implementations so backends that only
/// support basic vector storage still compile. The `SochMemoryBackend`
/// adapter provides the full-featured implementation.
#[async_trait]
pub trait MemoryBackend: super::VectorStore {
    // ── Atomic Writes ─────────────────────────────────────────────

    /// Write multiple operations atomically (all-or-nothing).
    /// Returns the result with intent_id for crash recovery.
    fn write_atomic(
        &self,
        memory_id: &str,
        ops: Vec<MemoryWriteOp>,
    ) -> Result<AtomicWriteResult, String> {
        let _ = (memory_id, ops);
        Err("atomic writes not supported by this backend".into())
    }

    /// Recover incomplete atomic writes after a crash.
    /// Returns the number of intents replayed.
    fn recover_atomic_writes(&self) -> Result<usize, String> {
        Ok(0)
    }

    // ── Knowledge Graph ───────────────────────────────────────────

    /// Get neighbor node IDs from a graph node, optionally filtered by edge type.
    fn graph_neighbors(
        &self,
        node_id: &str,
        edge_type: Option<&str>,
    ) -> Result<Vec<GraphNeighborInfo>, String> {
        let _ = (node_id, edge_type);
        Ok(vec![])
    }

    /// Create a graph node.
    fn graph_add_node(
        &self,
        node_id: &str,
        node_type: &str,
        properties: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<(), String> {
        let _ = (node_id, node_type, properties);
        Ok(())
    }

    /// Create a graph edge.
    fn graph_add_edge(
        &self,
        from_id: &str,
        edge_type: &str,
        to_id: &str,
        properties: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<(), String> {
        let _ = (from_id, edge_type, to_id, properties);
        Ok(())
    }

    // ── Temporal Graph ────────────────────────────────────────────

    /// Add a temporal edge (valid from now, open-ended).
    fn temporal_add_edge(
        &self,
        from_id: &str,
        edge_type: &str,
        to_id: &str,
        properties: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<(), String> {
        let _ = (from_id, edge_type, to_id, properties);
        Ok(())
    }

    /// Invalidate a temporal edge at the current time.
    fn temporal_invalidate_edge(
        &self,
        from_id: &str,
        edge_type: &str,
        to_id: &str,
    ) -> Result<bool, String> {
        let _ = (from_id, edge_type, to_id);
        Ok(false)
    }

    /// Get temporal edges valid at a specific timestamp (millis).
    fn temporal_edges_at(
        &self,
        from_id: &str,
        edge_type: Option<&str>,
        at_time: u64,
    ) -> Result<Vec<TemporalEdgeInfo>, String> {
        let _ = (from_id, edge_type, at_time);
        Ok(vec![])
    }

    /// Get memory IDs reachable from a node via graph traversal,
    /// useful for constraining vector search to session-relevant memories.
    fn graph_reachable_memory_ids(
        &self,
        start_node: &str,
        edge_type: &str,
        max_depth: usize,
    ) -> Result<Vec<String>, String> {
        let _ = (start_node, edge_type, max_depth);
        Ok(vec![])
    }

    // ── Policy ────────────────────────────────────────────────────

    /// Check content against PII redaction rules before storage.
    fn policy_check_content(&self, content: &str) -> PolicyCheckResult {
        let _ = content;
        PolicyCheckResult::Allow
    }

    /// Check if an agent has access to a specific memory namespace.
    fn policy_check_access(&self, agent_id: &str, namespace: &str) -> bool {
        let _ = (agent_id, namespace);
        true
    }

    // ── Trace / Observability ─────────────────────────────────────

    /// Start a trace span for a memory operation.
    fn trace_start_span(
        &self,
        run_id: &str,
        operation: &str,
    ) -> Option<MemoryTraceSpan> {
        let _ = (run_id, operation);
        None
    }

    /// End a trace span.
    fn trace_end_span(
        &self,
        span: &MemoryTraceSpan,
        success: bool,
        metadata: Option<HashMap<String, String>>,
    ) {
        let _ = (span, success, metadata);
    }

    // ── Batch Writes (A7) ─────────────────────────────────────────

    /// Write multiple key-value pairs in a single group-committed batch.
    /// Returns the batch result with execution stats.
    fn batch_insert_embeddings(
        &self,
        collection: &str,
        items: Vec<(String, Vec<f32>, HashMap<String, String>)>,
    ) -> Result<BatchWriteResult, String> {
        let _ = (collection, items);
        Err("batch writes not supported by this backend".into())
    }

    // ── Memory Schema (A4) ────────────────────────────────────────

    /// Create a new episode (conversation/task run).
    fn create_episode(&self, episode: &Episode) -> Result<(), String> {
        let _ = episode;
        Err("memory schema not supported by this backend".into())
    }

    /// Get an episode by ID.
    fn get_episode(&self, episode_id: &str) -> Result<Option<Episode>, String> {
        let _ = episode_id;
        Ok(None)
    }

    /// Search episodes by query text.
    fn search_episodes(&self, query: &str, k: usize) -> Result<Vec<Episode>, String> {
        let _ = (query, k);
        Ok(vec![])
    }

    /// Append an event to an episode's timeline.
    fn append_event(&self, event: &Event) -> Result<(), String> {
        let _ = event;
        Err("memory schema not supported by this backend".into())
    }

    /// Get the timeline of events for an episode.
    fn get_timeline(&self, episode_id: &str, max_events: usize) -> Result<Vec<Event>, String> {
        let _ = (episode_id, max_events);
        Ok(vec![])
    }

    /// Create or update a persistent entity.
    fn upsert_entity(&self, entity: &Entity) -> Result<(), String> {
        let _ = entity;
        Err("memory schema not supported by this backend".into())
    }

    /// Get an entity by ID.
    fn get_entity(&self, entity_id: &str) -> Result<Option<Entity>, String> {
        let _ = entity_id;
        Ok(None)
    }

    /// Search entities by kind and query text.
    fn search_entities(
        &self,
        kind: Option<EntityKind>,
        query: &str,
        k: usize,
    ) -> Result<Vec<Entity>, String> {
        let _ = (kind, query, k);
        Ok(vec![])
    }

    /// Get facts about an entity (entity + recent episodes + related entities).
    fn get_entity_facts(&self, entity_id: &str) -> Result<Option<EntityFacts>, String> {
        let _ = entity_id;
        Ok(None)
    }

    // ── Context Query (A1) ────────────────────────────────────────

    /// Assemble token-budgeted LLM context from multiple data sources.
    ///
    /// Sections are packed in priority order within the token budget.
    /// Lower priority numbers = higher importance. Supports truncation
    /// strategies (TailDrop, HeadDrop, Proportional, Strict) and multiple
    /// output formats (JSON, Markdown, Text, Soch).
    fn context_query(
        &self,
        session_id: Option<&str>,
        agent_id: Option<&str>,
        token_budget: usize,
        sections: Vec<(&str, i32, &str)>,
        truncation: TruncationStrategy,
        format: ContextFormat,
    ) -> Result<ContextQueryResult, String> {
        let _ = (session_id, agent_id, token_budget, sections, truncation, format);
        Err("context query not supported by this backend".into())
    }

    // ── Task Queue (A8) ───────────────────────────────────────────

    /// Enqueue a background task with priority.
    fn enqueue_task(
        &self,
        queue_id: &str,
        priority: i64,
        payload: Vec<u8>,
    ) -> Result<BackgroundTask, String> {
        let _ = (queue_id, priority, payload);
        Err("task queue not supported by this backend".into())
    }

    /// Enqueue a delayed background task (visible after delay_ms).
    fn enqueue_delayed_task(
        &self,
        queue_id: &str,
        priority: i64,
        payload: Vec<u8>,
        delay_ms: u64,
    ) -> Result<BackgroundTask, String> {
        let _ = (queue_id, priority, payload, delay_ms);
        Err("task queue not supported by this backend".into())
    }

    /// Claim the next available task from a queue.
    fn claim_task(
        &self,
        queue_id: &str,
        worker_id: &str,
    ) -> Result<TaskClaimResult, String> {
        let _ = (queue_id, worker_id);
        Ok(TaskClaimResult::Empty)
    }

    /// Acknowledge successful task completion.
    fn ack_task(&self, queue_id: &str, task_id: &str) -> Result<(), String> {
        let _ = (queue_id, task_id);
        Ok(())
    }

    /// Negatively acknowledge a task (requeue with optional delay).
    fn nack_task(
        &self,
        queue_id: &str,
        task_id: &str,
        delay_ms: Option<u64>,
    ) -> Result<(), String> {
        let _ = (queue_id, task_id, delay_ms);
        Ok(())
    }

    /// Get queue statistics.
    fn queue_stats(&self, queue_id: &str) -> Result<TaskQueueStats, String> {
        let _ = queue_id;
        Ok(TaskQueueStats::default())
    }

    // ── Cost Model / Query Budget (A9) ────────────────────────────

    /// Execute a vector search with SLA-driven budgeting.
    ///
    /// The search profile determines the latency/recall tradeoff:
    /// - `LowLatency`: p99 ≤ 5ms, recall ≥ 0.80 (autocomplete)
    /// - `Balanced`: p99 ≤ 20ms, recall ≥ 0.90 (questions)
    /// - `HighRecall`: p99 ≤ 100ms, recall ≥ 0.99 (research)
    async fn search_with_budget(
        &self,
        collection: &str,
        query: &[f32],
        k: usize,
        profile: SearchProfile,
    ) -> Result<(Vec<super::vector_store::VectorSearchResult>, QueryCostSummary), String> {
        // Default: ignore profile, run standard search
        let results = self
            .search(collection, query, k, None)
            .await
            .map_err(|e| e.to_string())?;
        Ok((results, QueryCostSummary::default()))
    }

    // ── Filter Pushdown (A12) ─────────────────────────────────────

    /// Execute a vector search with early predicate pushdown.
    ///
    /// Filters are evaluated before distance computation, skipping
    /// vectors that don't match the predicates. This is 5-20× faster
    /// than post-search filtering for selective queries.
    async fn search_with_filters(
        &self,
        options: FilteredSearchOptions,
    ) -> Result<Vec<super::vector_store::VectorSearchResult>, String> {
        // Default: ignore filters, run standard search
        let results = self
            .search(&options.collection, &options.query_embedding, options.top_k, None)
            .await
            .map_err(|e| e.to_string())?;
        Ok(results)
    }

    // ── Multi-Vector Documents (A11) ──────────────────────────────

    /// Insert a multi-vector document (one vector per chunk).
    fn insert_multi_vector(
        &self,
        collection: &str,
        doc: MultiVectorDocument,
    ) -> Result<(), String> {
        let _ = (collection, doc);
        Err("multi-vector not supported by this backend".into())
    }

    /// Search for documents using multi-vector aggregation.
    async fn search_multi_vector(
        &self,
        collection: &str,
        query: &[f32],
        k: usize,
        aggregation: AggregationMethod,
    ) -> Result<Vec<DocumentSearchResult>, String> {
        let _ = (collection, query, k, aggregation);
        Ok(vec![])
    }

    // ── Path Query (A6) ───────────────────────────────────────────

    /// Execute an O(|path|) hierarchical path query.
    ///
    /// Uses TCH (Trie-Compressed Hierarchy) for fast resolution.
    /// Example: `path_query("users.alice.preferences.coding", None)`
    fn path_query(
        &self,
        path: &str,
        filters: Option<Vec<(&str, serde_json::Value)>>,
    ) -> Result<Vec<PathQueryRow>, String> {
        let _ = (path, filters);
        Err("path query not supported by this backend".into())
    }

    // ── AST Query / SQL (A15) ─────────────────────────────────────

    /// Execute a SQL query against the memory store.
    ///
    /// Supports SQL-92 (SELECT, INSERT, UPDATE, DELETE) with parametric
    /// queries using `$1` or `?` placeholders.
    fn sql_query(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<HashMap<String, serde_json::Value>>, String> {
        let _ = (sql, params);
        Err("SQL queries not supported by this backend".into())
    }

    // ── Predefined Views (A5) ─────────────────────────────────────

    /// Get list of available predefined view names.
    fn list_views(&self) -> Vec<String> {
        vec![]
    }

    /// Query a predefined view by name.
    fn query_view(
        &self,
        view_name: &str,
        filters: Option<HashMap<String, serde_json::Value>>,
        limit: Option<usize>,
    ) -> Result<Vec<HashMap<String, serde_json::Value>>, String> {
        let _ = (view_name, filters, limit);
        Err("predefined views not supported by this backend".into())
    }
}
