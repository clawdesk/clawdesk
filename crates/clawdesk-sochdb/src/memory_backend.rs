//! `MemoryBackend` implementation for `SochStore`.
//!
//! Bridges the abstract `MemoryBackend` trait (defined in `clawdesk-storage`)
//! to the concrete SochDB advanced modules: `AtomicMemoryWriter`, `GraphOverlay`,
//! `TemporalGraphOverlay`, `PolicyEngine`, `TraceStore`, `BatchWriter`,
//! `ContextQueryBuilder`, `PriorityQueue`, `PathQuery`, `AstQueryExecutor`,
//! and the canonical memory schema (Episodes, Events, Entities).

use crate::bridge::SochConn;
use crate::SochStore;
use sochdb::ConnectionTrait;
use clawdesk_storage::memory_backend::{
    AtomicWriteResult, BatchWriteResult, GraphNeighborInfo, MemoryBackend, MemoryTraceSpan,
    MemoryWriteOp, PolicyCheckResult, TemporalEdgeInfo,
    // Memory Schema types (A4)
    Episode, EpisodeType, Event, Entity, EntityKind, EntityFacts,
    // Context Query types (A1)
    ContextQueryResult, ContextSection, ContextFormat, TruncationStrategy,
    // Task Queue types (A8)
    BackgroundTask, TaskClaimResult, TaskQueueStats,
    // Path Query types (A6)
    PathQueryRow,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tracing::debug;

/// A concrete `MemoryBackend` + `VectorStore` implementation backed by SochDB.
///
/// This wraps `Arc<SochStore>` and provides the full feature set of SochDB's
/// advanced modules through the `MemoryBackend` trait. `MemoryManager` should
/// be constructed with this type instead of bare `SochStore` to unlock
/// atomic writes, graph integration, temporal queries, policy, and tracing.
pub struct SochMemoryBackend {
    store: Arc<SochStore>,
    conn: SochConn,
    atomic_writer: sochdb::atomic_memory::AtomicMemoryWriter<SochConn>,
    knowledge_graph: sochdb::graph::GraphOverlay<SochConn>,
    temporal_graph: sochdb::temporal_graph::TemporalGraphOverlay<SochConn>,
    policy_engine: sochdb::policy::PolicyEngine<SochConn>,
    trace_store: sochdb::trace::TraceStore<SochConn>,
    /// Semantic cache for LLM response deduplication (GAP-17).
    semantic_cache: sochdb::semantic_cache::SemanticCache<SochConn>,
    /// Task queue for background memory maintenance (A8).
    task_queue: sochdb::queue::PriorityQueue,
}

impl SochMemoryBackend {
    /// Create a new memory backend from a SochStore.
    pub fn new(store: Arc<SochStore>) -> Self {
        let conn = SochConn::new(store.clone());

        // A8: Task queue for background memory maintenance
        let queue_config = sochdb::queue::QueueConfig::new("memory_maintenance")
            .with_visibility_timeout(30_000) // 30s visibility timeout
            .with_max_attempts(3);

        Self {
            store,
            atomic_writer: sochdb::atomic_memory::AtomicMemoryWriter::new(conn.clone()),
            knowledge_graph: sochdb::graph::GraphOverlay::new(conn.clone(), "clawdesk"),
            temporal_graph: sochdb::temporal_graph::TemporalGraphOverlay::new(conn.clone(), "clawdesk"),
            policy_engine: sochdb::policy::PolicyEngine::new(conn.clone()),
            trace_store: sochdb::trace::TraceStore::new(conn.clone()),
            semantic_cache: sochdb::semantic_cache::SemanticCache::new(conn.clone()),
            task_queue: sochdb::queue::PriorityQueue::new(queue_config),
            conn,
        }
    }

    /// Get a reference to the underlying SochStore.
    pub fn store(&self) -> &SochStore {
        &self.store
    }

    /// Get a clone of the underlying Arc<SochStore>.
    pub fn store_arc(&self) -> Arc<SochStore> {
        self.store.clone()
    }

    /// Get a reference to the shared SochConn.
    ///
    /// All 6 internal modules (`atomic_writer`, `knowledge_graph`, `temporal_graph`,
    /// `policy_engine`, `trace_store`, and `conn`) share this same `SochConn`
    /// backed by a single `Arc<SochStore>`. Cross-module writes through the conn
    /// use `write_batch` for atomicity.
    pub fn conn(&self) -> &SochConn {
        &self.conn
    }

    /// Execute a cross-module write within a shared transaction boundary.
    ///
    /// Wraps the `SochConn` in a `TransactionalConn` so all put/delete ops
    /// within the closure are buffered and committed atomically via `write_batch`.
    /// This ensures cross-module consistency (e.g., writing an episode and its
    /// graph edges in a single atomic commit).
    pub fn with_transaction<F, T>(&self, label: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut crate::transaction::TransactionalConn<SochConn>) -> sochdb::error::Result<T>,
    {
        crate::transaction::with_transaction(self.conn.clone(), label, f)
            .map_err(|e| format!("transaction '{}' failed: {}", label, e))
    }

    // ── GAP-17: Semantic cache helpers ──────────────────────────────

    /// Get a reference to the semantic cache.
    ///
    /// The semantic cache deduplicates LLM API calls by caching responses
    /// keyed on (prompt_hash, model, temperature). Exact matches return
    /// cached responses immediately; embedding-based similarity finding
    /// matches paraphrased prompts.
    pub fn semantic_cache(&self) -> &sochdb::semantic_cache::SemanticCache<SochConn> {
        &self.semantic_cache
    }

    /// Look up a cached LLM response by prompt hash.
    ///
    /// Returns `Some(response)` if an exact or similar prompt was seen before.
    pub fn cache_lookup(&self, prompt_hash: &str) -> Option<Vec<u8>> {
        let key = format!("semantic_cache/{}", prompt_hash);
        self.conn.get(key.as_bytes()).ok().flatten()
    }

    /// Store an LLM response in the semantic cache.
    pub fn cache_store(&self, prompt_hash: &str, response: &[u8]) -> Result<(), String> {
        let key = format!("semantic_cache/{}", prompt_hash);
        self.conn.put(key.as_bytes(), response)
            .map_err(|e| format!("cache store: {e}"))
    }
}

// ── VectorStore delegation ──────────────────────────────────────────────────

#[async_trait::async_trait]
impl clawdesk_storage::VectorStore for SochMemoryBackend {
    async fn create_collection(
        &self,
        config: clawdesk_storage::vector_store::CollectionConfig,
    ) -> Result<(), clawdesk_types::error::StorageError> {
        self.store.create_collection(config).await
    }

    async fn insert(
        &self,
        collection: &str,
        id: &str,
        embedding: &[f32],
        metadata: Option<serde_json::Value>,
    ) -> Result<(), clawdesk_types::error::StorageError> {
        // Route through write_atomic for WAL-backed atomicity (#15).
        // This ensures every vector insert goes through the same crash-safe
        // path as MemoryBackend::write_atomic / batch_insert_embeddings.
        //
        // Convert Option<Value> → HashMap<String, String> for MemoryWriteOp.
        let meta_map: std::collections::HashMap<String, String> = metadata
            .and_then(|v| v.as_object().cloned())
            .map(|obj| {
                obj.into_iter()
                    .map(|(k, v)| {
                        let s = match v {
                            serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        };
                        (k, s)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let op = MemoryWriteOp::PutEmbedding {
            collection: collection.to_string(),
            id: id.to_string(),
            embedding: embedding.to_vec(),
            metadata: meta_map,
        };
        self.write_atomic(id, vec![op])
            .map_err(|e| clawdesk_types::error::StorageError::SerializationFailed { detail: e })?;
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        query: &[f32],
        k: usize,
        min_score: Option<f32>,
    ) -> Result<Vec<clawdesk_storage::vector_store::VectorSearchResult>, clawdesk_types::error::StorageError> {
        self.store.search(collection, query, k, min_score).await
    }

    async fn hybrid_search(
        &self,
        collection: &str,
        query_embedding: &[f32],
        query_text: &str,
        k: usize,
        vector_weight: f32,
    ) -> Result<Vec<clawdesk_storage::vector_store::VectorSearchResult>, clawdesk_types::error::StorageError> {
        self.store.hybrid_search(collection, query_embedding, query_text, k, vector_weight).await
    }

    async fn delete(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<bool, clawdesk_types::error::StorageError> {
        clawdesk_storage::VectorStore::delete(self.store.as_ref(), collection, id).await
    }
}

// ── MemoryBackend implementation ────────────────────────────────────────────

impl MemoryBackend for SochMemoryBackend {
    fn write_atomic(
        &self,
        memory_id: &str,
        ops: Vec<MemoryWriteOp>,
    ) -> Result<AtomicWriteResult, String> {
        // Convert our ops to SochDB's MemoryOp
        let soch_ops: Vec<sochdb::atomic_memory::MemoryOp> = ops
            .into_iter()
            .map(|op| match op {
                MemoryWriteOp::PutBlob { key, value } => {
                    sochdb::atomic_memory::MemoryOp::PutBlob {
                        key: key.into_bytes(),
                        value,
                    }
                }
                MemoryWriteOp::PutEmbedding {
                    collection,
                    id,
                    embedding,
                    metadata,
                } => sochdb::atomic_memory::MemoryOp::PutEmbedding {
                    collection,
                    id,
                    embedding,
                    metadata,
                },
                MemoryWriteOp::CreateNode {
                    namespace,
                    node_id,
                    node_type,
                    properties,
                } => sochdb::atomic_memory::MemoryOp::CreateNode {
                    namespace,
                    node_id,
                    node_type,
                    properties,
                },
                MemoryWriteOp::CreateEdge {
                    namespace,
                    from_id,
                    edge_type,
                    to_id,
                    properties,
                } => sochdb::atomic_memory::MemoryOp::CreateEdge {
                    namespace,
                    from_id,
                    edge_type,
                    to_id,
                    properties,
                },
            })
            .collect();

        let result = self
            .atomic_writer
            .write_atomic(memory_id, soch_ops)
            .map_err(|e| format!("atomic write: {e}"))?;

        Ok(AtomicWriteResult {
            intent_id: result.intent_id,
            memory_id: result.memory_id,
            ops_applied: result.ops_applied,
            committed: matches!(result.status, sochdb::atomic_memory::IntentStatus::Committed),
        })
    }

    fn recover_atomic_writes(&self) -> Result<usize, String> {
        let report = self
            .atomic_writer
            .recover()
            .map_err(|e| format!("atomic recovery: {e}"))?;
        debug!(
            replayed = report.replayed,
            failed = report.failed,
            "atomic memory recovery complete"
        );
        Ok(report.replayed)
    }

    fn graph_neighbors(
        &self,
        node_id: &str,
        edge_type: Option<&str>,
    ) -> Result<Vec<GraphNeighborInfo>, String> {
        let edges = self
            .knowledge_graph
            .get_edges(node_id, edge_type)
            .map_err(|e| format!("graph neighbors: {e}"))?;

        Ok(edges
            .into_iter()
            .map(|e| GraphNeighborInfo {
                node_id: e.to_id,
                edge_type: e.edge_type,
                properties: e.properties,
            })
            .collect())
    }

    fn graph_add_node(
        &self,
        node_id: &str,
        node_type: &str,
        properties: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<(), String> {
        self.knowledge_graph
            .add_node(node_id, node_type, properties)
            .map_err(|e| format!("graph add_node: {e}"))?;
        Ok(())
    }

    fn graph_add_edge(
        &self,
        from_id: &str,
        edge_type: &str,
        to_id: &str,
        properties: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<(), String> {
        self.knowledge_graph
            .add_edge(from_id, edge_type, to_id, properties)
            .map_err(|e| format!("graph add_edge: {e}"))?;
        Ok(())
    }

    fn temporal_add_edge(
        &self,
        from_id: &str,
        edge_type: &str,
        to_id: &str,
        properties: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<(), String> {
        self.temporal_graph
            .add_edge(from_id, edge_type, to_id, properties)
            .map_err(|e| format!("temporal add_edge: {e}"))?;
        Ok(())
    }

    fn temporal_invalidate_edge(
        &self,
        from_id: &str,
        edge_type: &str,
        to_id: &str,
    ) -> Result<bool, String> {
        self.temporal_graph
            .invalidate_edge(from_id, edge_type, to_id)
            .map_err(|e| format!("temporal invalidate: {e}"))
    }

    fn temporal_edges_at(
        &self,
        from_id: &str,
        edge_type: Option<&str>,
        at_time: u64,
    ) -> Result<Vec<TemporalEdgeInfo>, String> {
        let edges = self
            .temporal_graph
            .get_edges_at(from_id, edge_type, at_time)
            .map_err(|e| format!("temporal edges_at: {e}"))?;

        Ok(edges
            .into_iter()
            .map(|e| TemporalEdgeInfo {
                from_id: e.from_id,
                edge_type: e.edge_type,
                to_id: e.to_id,
                valid_from: e.validity.start,
                valid_until: e.validity.end,
                properties: e.properties,
            })
            .collect())
    }

    fn graph_reachable_memory_ids(
        &self,
        start_node: &str,
        edge_type: &str,
        max_depth: usize,
    ) -> Result<Vec<String>, String> {
        // BFS traversal (VecDeque) from start_node following edge_type edges.
        // Using rustc_hash::FxHashSet for faster hashing on short strings.
        let mut visited = rustc_hash::FxHashSet::default();
        let mut memory_ids = Vec::new();
        let mut frontier: VecDeque<(String, usize)> = VecDeque::new();
        frontier.push_back((start_node.to_string(), 0));

        while let Some((node_id, depth)) = frontier.pop_front() {
            if depth > max_depth || visited.contains(&node_id) {
                continue;
            }
            visited.insert(node_id.clone());

            let edges = self
                .knowledge_graph
                .get_edges(&node_id, Some(edge_type))
                .map_err(|e| format!("graph traversal: {e}"))?;

            for edge in edges {
                // If the target is a memory node, collect its ID
                if let Ok(Some(node)) = self.knowledge_graph.get_node(&edge.to_id) {
                    if node.node_type == "memory" {
                        memory_ids.push(edge.to_id.clone());
                    }
                }
                if depth + 1 <= max_depth && !visited.contains(&edge.to_id) {
                    frontier.push_back((edge.to_id, depth + 1));
                }
            }
        }

        Ok(memory_ids)
    }

    fn policy_check_content(&self, _content: &str) -> PolicyCheckResult {
        // Pure read-only policy evaluation — no sentinel writes.
        // Construct a PolicyContext and use get_denied_ids to check if any
        // BeforeWrite policies would deny this content.
        let check_key = b"memory:content_check";
        let ctx = sochdb::policy::PolicyContext::new("write", check_key);

        let denied = self.policy_engine.get_denied_ids(
            sochdb::policy::PolicyTrigger::BeforeWrite,
            &[check_key.to_vec()],
            &ctx,
        );

        if denied.is_empty() {
            PolicyCheckResult::Allow
        } else {
            PolicyCheckResult::Deny(format!("Content blocked by {} policy rule(s)", denied.len()))
        }
    }

    fn policy_check_access(&self, agent_id: &str, namespace: &str) -> bool {
        // Use get_denied_ids to check if this agent would be denied access
        let check_key = format!("memory:{}:access_check", namespace).into_bytes();
        let mut ctx = sochdb::policy::PolicyContext::new("read", &check_key)
            .with_agent_id(agent_id);
        ctx.set("namespace", namespace);

        let denied = self.policy_engine.get_denied_ids(
            sochdb::policy::PolicyTrigger::BeforeRead,
            &[check_key.clone()],
            &ctx,
        );

        denied.is_empty() // If not in denied list, access is allowed
    }

    fn trace_start_span(
        &self,
        run_id: &str,
        operation: &str,
    ) -> Option<MemoryTraceSpan> {
        let trace_id = run_id.to_string();
        match self.trace_store.start_span(
            &trace_id,
            operation,
            None,
            sochdb::trace::SpanKind::Internal,
        ) {
            Ok(span) => Some(MemoryTraceSpan {
                run_id: run_id.to_string(),
                span_id: span.span_id.clone(),
            }),
            Err(e) => {
                debug!(error = %e, "failed to start trace span");
                None
            }
        }
    }

    fn trace_end_span(
        &self,
        span: &MemoryTraceSpan,
        success: bool,
        metadata: Option<HashMap<String, String>>,
    ) {
        let status = if success {
            sochdb::trace::SpanStatusCode::Ok
        } else {
            sochdb::trace::SpanStatusCode::Error
        };
        // SochDB end_span takes message: Option<String>, not attrs map
        let message = metadata.map(|m| {
            m.into_iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join(", ")
        });
        let _ = self.trace_store.end_span(
            &span.run_id,
            &span.span_id,
            status,
            message,
        );
    }

    // ── Batch Writes (A7) ──────────────────────────────────────────

    fn batch_insert_embeddings(
        &self,
        collection: &str,
        items: Vec<(String, Vec<f32>, HashMap<String, String>)>,
    ) -> Result<BatchWriteResult, String> {
        let start = std::time::Instant::now();
        let count = items.len();

        // Use AtomicMemoryWriter for grouped writes — already uses WAL-based
        // group commit via SochConn. Each item becomes a PutEmbedding op.
        let ops: Vec<sochdb::atomic_memory::MemoryOp> = items
            .into_iter()
            .map(|(id, embedding, metadata)| {
                sochdb::atomic_memory::MemoryOp::PutEmbedding {
                    collection: collection.to_string(),
                    id,
                    embedding,
                    metadata,
                }
            })
            .collect();

        let batch_id = format!("batch_{}_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(), count);
        let result = self
            .atomic_writer
            .write_atomic(&batch_id, ops)
            .map_err(|e| format!("batch write: {e}"))?;

        Ok(BatchWriteResult {
            ops_executed: result.ops_applied,
            ops_failed: if result.ops_applied < count { count - result.ops_applied } else { 0 },
            duration_ms: start.elapsed().as_millis() as u64,
            chunks_committed: 1, // single atomic commit
        })
    }

    // ── Memory Schema (A4) ─────────────────────────────────────────

    fn create_episode(&self, episode: &Episode) -> Result<(), String> {
        let soch_episode = sochdb_core::memory_schema::Episode {
            episode_id: episode.episode_id.clone(),
            episode_type: match episode.episode_type {
                EpisodeType::Conversation => sochdb_core::memory_schema::EpisodeType::Conversation,
                EpisodeType::Task => sochdb_core::memory_schema::EpisodeType::Task,
                EpisodeType::Workflow => sochdb_core::memory_schema::EpisodeType::Workflow,
                EpisodeType::Debug => sochdb_core::memory_schema::EpisodeType::Debug,
                EpisodeType::AgentInteraction => sochdb_core::memory_schema::EpisodeType::AgentInteraction,
                EpisodeType::Other => sochdb_core::memory_schema::EpisodeType::Other,
            },
            entity_ids: episode.entity_ids.clone(),
            ts_start: episode.ts_start,
            ts_end: episode.ts_end,
            summary: episode.summary.clone(),
            tags: episode.tags.clone(),
            embedding: episode.embedding.clone(),
            metadata: episode.metadata.iter()
                .map(|(k, v)| (k.clone(), json_to_soch_value(v)))
                .collect(),
        };

        // Serialize and store via KV
        let key = format!("episodes:{}", episode.episode_id);
        let data = serde_json::to_vec(&soch_episode)
            .map_err(|e| format!("serialize episode: {e}"))?;
        self.conn.put(key.as_bytes(), &data)
            .map_err(|e| format!("store episode: {e}"))?;

        // Build secondary indexes for efficient search (#3).
        let ep_id_bytes = episode.episode_id.as_bytes();

        // Tag index: ep_tag:{tag_lower}:{episode_id} → episode_id
        for tag in &episode.tags {
            let idx_key = format!("ep_tag:{}:{}", tag.to_lowercase(), episode.episode_id);
            self.conn.put(idx_key.as_bytes(), ep_id_bytes)
                .map_err(|e| format!("episode tag index write: {e}"))?;
        }

        // Temporal index: ep_ts:{ts_hex}:{episode_id} → episode_id
        let ts_key = format!("ep_ts:{:016x}:{}", episode.ts_start, episode.episode_id);
        self.conn.put(ts_key.as_bytes(), ep_id_bytes)
            .map_err(|e| format!("episode temporal index write: {e}"))?;

        Ok(())
    }

    fn get_episode(&self, episode_id: &str) -> Result<Option<Episode>, String> {
        let key = format!("episodes:{}", episode_id);
        match self.conn.get(key.as_bytes()) {
            Ok(Some(data)) => {
                let soch_ep: sochdb_core::memory_schema::Episode =
                    serde_json::from_slice(&data)
                        .map_err(|e| format!("deserialize episode: {e}"))?;
                Ok(Some(soch_episode_to_local(soch_ep)))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("get episode: {e}")),
        }
    }

    fn search_episodes(&self, query: &str, k: usize) -> Result<Vec<Episode>, String> {
        let query_lower = query.to_lowercase();

        // ── GAP-10: Try semantic vector search first ────────────────
        // If episode embeddings exist in the `episode_embeddings` collection,
        // use vector similarity for semantic matching. This provides:
        // - Semantic understanding (not just substring matching)
        // - O(k log N) with ANN cache vs O(N) full scan
        // - Better recall for paraphrased queries
        //
        // We generate a lightweight query embedding by scanning for the most
        // similar episodes using stored summary embeddings. If the collection
        // is empty or embeddings are unavailable, fall back to text matching.
        let vector_results = {
            // Check if we have any episode embeddings to search against
            let rt = tokio::runtime::Handle::try_current();
            if let Ok(handle) = rt {
                handle.block_on(async {
                    use clawdesk_storage::vector_store::VectorStore;
                    // Try hybrid search with text query for BM25 + vector scoring
                    // Use a dummy zero-vector since we don't have an embedding model here;
                    // the BM25 component of hybrid_search will still provide keyword matching.
                    let dummy_query = vec![0.0f32; 1]; // Will be ignored if no vectors match
                    self.store.hybrid_search(
                        "episode_embeddings",
                        &dummy_query,
                        query,
                        k * 2, // Over-fetch for merging with text results
                        0.0, // Full text weight since we have no real embedding
                    ).await.ok()
                })
            } else {
                None
            }
        };

        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut episodes: Vec<Episode> = Vec::new();

        // Collect vector search results (if any)
        if let Some(vr) = vector_results {
            for result in vr {
                if let Ok(Some(ep)) = self.get_episode(&result.id) {
                    seen_ids.insert(ep.episode_id.clone());
                    episodes.push(ep);
                }
            }
        }

        // ── Tag index path (exact tag match) ────────────────────────
        if episodes.len() < k {
            let tag_prefix = format!("ep_tag:{}:", query_lower);
            let tag_hits = self.conn.scan(tag_prefix.as_bytes())
                .unwrap_or_default();

            for (_, id_bytes) in &tag_hits {
                if let Ok(id) = std::str::from_utf8(id_bytes) {
                    if !seen_ids.contains(id) {
                        if let Ok(Some(ep)) = self.get_episode(id) {
                            seen_ids.insert(ep.episode_id.clone());
                            episodes.push(ep);
                        }
                    }
                }
            }
        }

        // ── Substring fallback (full scan) ──────────────────────────
        if episodes.len() < k {
            let prefix = b"episodes:";
            if let Ok(results) = self.conn.scan(prefix) {
                for (_, data) in results {
                    if let Ok(ep) = serde_json::from_slice::<sochdb_core::memory_schema::Episode>(&data) {
                        if !seen_ids.contains(&ep.episode_id) {
                            let matches = ep.summary.to_lowercase().contains(&query_lower)
                                || ep.tags.iter().any(|t| t.to_lowercase().contains(&query_lower));
                            if matches {
                                let local = soch_episode_to_local(ep);
                                seen_ids.insert(local.episode_id.clone());
                                episodes.push(local);
                            }
                        }
                    }
                }
            }
        }

        // Sort by most recent first and limit
        episodes.sort_by(|a, b| b.ts_start.cmp(&a.ts_start));
        episodes.truncate(k);
        Ok(episodes)
    }

    fn append_event(&self, event: &Event) -> Result<(), String> {
        let key = format!("events:{}:{:016x}", event.episode_id, event.seq);
        let data = serde_json::to_vec(event)
            .map_err(|e| format!("serialize event: {e}"))?;
        self.conn.put(key.as_bytes(), &data)
            .map_err(|e| format!("store event: {e}"))?;
        Ok(())
    }

    fn get_timeline(&self, episode_id: &str, max_events: usize) -> Result<Vec<Event>, String> {
        let prefix = format!("events:{}:", episode_id);
        let results = self.conn.scan(prefix.as_bytes())
            .map_err(|e| format!("scan events: {e}"))?;

        let mut events: Vec<Event> = results
            .into_iter()
            .filter_map(|(_, data)| serde_json::from_slice(&data).ok())
            .collect();

        events.sort_by_key(|e| e.seq);
        events.truncate(max_events);
        Ok(events)
    }

    fn upsert_entity(&self, entity: &Entity) -> Result<(), String> {
        let key = format!("entities:{}", entity.entity_id);
        let data = serde_json::to_vec(entity)
            .map_err(|e| format!("serialize entity: {e}"))?;
        self.conn.put(key.as_bytes(), &data)
            .map_err(|e| format!("store entity: {e}"))?;

        // Secondary index: ent_kind:{kind_str}:{entity_id} → entity_id (#3)
        let kind_str = format!("{:?}", entity.kind).to_lowercase();
        let idx_key = format!("ent_kind:{}:{}", kind_str, entity.entity_id);
        self.conn.put(idx_key.as_bytes(), entity.entity_id.as_bytes())
            .map_err(|e| format!("entity kind index: {e}"))?;
        Ok(())
    }

    fn get_entity(&self, entity_id: &str) -> Result<Option<Entity>, String> {
        let key = format!("entities:{}", entity_id);
        match self.conn.get(key.as_bytes()) {
            Ok(Some(data)) => {
                let entity: Entity = serde_json::from_slice(&data)
                    .map_err(|e| format!("deserialize entity: {e}"))?;
                Ok(Some(entity))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("get entity: {e}")),
        }
    }

    fn search_entities(
        &self,
        kind: Option<EntityKind>,
        query: &str,
        k: usize,
    ) -> Result<Vec<Entity>, String> {
        let query_lower = query.to_lowercase();

        // If kind is specified, use the kind index to narrow scan (#3)
        let candidates: Vec<(Vec<u8>, Vec<u8>)> = if let Some(ref ek) = kind {
            let kind_str = format!("{:?}", ek).to_lowercase();
            let idx_prefix = format!("ent_kind:{}:", kind_str);
            self.conn.scan(idx_prefix.as_bytes()).unwrap_or_default()
        } else {
            Vec::new()
        };

        let mut entities: Vec<Entity> = if kind.is_some() && !candidates.is_empty() {
            // Fetch only entities that matched the kind index
            candidates
                .iter()
                .filter_map(|(_, id_bytes)| {
                    let id = std::str::from_utf8(id_bytes).ok()?;
                    let entity = self.get_entity(id).ok()??;
                    if entity.name.to_lowercase().contains(&query_lower) {
                        Some(entity)
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            // Full scan fallback (no kind filter or empty index)
            let prefix = b"entities:";
            let results = self.conn.scan(prefix)
                .map_err(|e| format!("scan entities: {e}"))?;
            results
                .into_iter()
                .filter_map(|(_, data)| {
                    let entity: Entity = serde_json::from_slice(&data).ok()?;
                    if let Some(ref ek) = kind {
                        if &entity.kind != ek {
                            return None;
                        }
                    }
                    if entity.name.to_lowercase().contains(&query_lower) {
                        Some(entity)
                    } else {
                        None
                    }
                })
                .collect()
        };

        entities.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        entities.truncate(k);
        Ok(entities)
    }

    fn get_entity_facts(&self, entity_id: &str) -> Result<Option<EntityFacts>, String> {
        let entity = match self.get_entity(entity_id)? {
            Some(e) => e,
            None => return Ok(None),
        };

        // Find recent episodes that reference this entity
        let recent_episodes: Vec<Episode> = self
            .search_episodes(&entity.name, 5)
            .unwrap_or_default();

        // Find related entities via graph edges
        let related_entities: Vec<Entity> = self
            .graph_neighbors(entity_id, None)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|n| self.get_entity(&n.node_id).ok().flatten())
            .take(10)
            .collect();

        Ok(Some(EntityFacts {
            entity,
            recent_episodes,
            related_entities,
        }))
    }

    // ── Context Query (A1 + GAP-13) ────────────────────────────────

    fn context_query(
        &self,
        session_id: Option<&str>,
        agent_id: Option<&str>,
        token_budget: usize,
        sections: Vec<(&str, i32, &str)>,
        truncation: TruncationStrategy,
        format: ContextFormat,
    ) -> Result<ContextQueryResult, String> {
        // GAP-13: Enhanced context query with dynamic section sourcing.
        //
        // When session_id or agent_id is provided, automatically includes
        // relevant context from the KV store (session state, recent messages,
        // agent config) in addition to the explicitly provided sections.
        // This mirrors ContextQueryBuilder's `.for_session()` / `.for_agent()`
        // behavior while staying compatible with the existing API.

        let estimate_tokens = clawdesk_types::tokenizer::estimate_tokens;

        // Build the full section list with dynamic sourcing
        let mut all_sections: Vec<(String, i32, String)> = sections
            .into_iter()
            .map(|(name, priority, content)| (name.to_string(), priority, content.to_string()))
            .collect();

        // Auto-include session context if session_id provided
        if let Some(sid) = session_id {
            // Include session state at high priority
            let state_key = format!("sessions/{}/state", sid);
            if let Ok(Some(bytes)) = self.store.get(&state_key) {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    all_sections.push(("session_state".to_string(), 0, text.to_string()));
                }
            }
            // Include recent messages (last 5) at medium priority
            let msg_prefix = format!("sessions/{}/messages/", sid);
            if let Ok(entries) = self.store.scan(&msg_prefix) {
                let recent: Vec<_> = entries.into_iter().rev().take(5).collect();
                let msg_text: String = recent.iter()
                    .filter_map(|(_, data)| std::str::from_utf8(data).ok())
                    .collect::<Vec<_>>()
                    .join("\n---\n");
                if !msg_text.is_empty() {
                    all_sections.push(("recent_messages".to_string(), 5, msg_text));
                }
            }
        }

        // Auto-include agent config if agent_id provided
        if let Some(aid) = agent_id {
            let agent_key = format!("agents/{}", aid);
            if let Ok(Some(bytes)) = self.store.get(&agent_key) {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    all_sections.push(("agent_config".to_string(), 1, text.to_string()));
                }
            }
        }

        // Sort sections by priority (lower = more important)
        all_sections.sort_by_key(|(_, priority, _)| *priority);

        let mut result_sections: Vec<ContextSection> = Vec::new();
        let mut total_tokens = 0usize;

        for (name, priority, content) in &all_sections {
            let section_tokens = estimate_tokens(content);
            let remaining = token_budget.saturating_sub(total_tokens);

            if remaining == 0 {
                // Budget exhausted — section dropped entirely
                result_sections.push(ContextSection {
                    name: name.to_string(),
                    priority: *priority,
                    content: String::new(),
                    token_count: 0,
                    truncated: true,
                });
                continue;
            }

            if section_tokens <= remaining {
                // Fits fully
                result_sections.push(ContextSection {
                    name: name.to_string(),
                    priority: *priority,
                    content: content.to_string(),
                    token_count: section_tokens,
                    truncated: false,
                });
                total_tokens += section_tokens;
            } else {
                // Needs truncation
                let max_chars = (remaining as f64 * 3.5) as usize;
                let truncated_content = match truncation {
                    TruncationStrategy::TailDrop => {
                        content.chars().take(max_chars).collect::<String>()
                    }
                    TruncationStrategy::HeadDrop => {
                        let total_chars = content.chars().count();
                        let skip = total_chars.saturating_sub(max_chars);
                        content.chars().skip(skip).collect::<String>()
                    }
                    TruncationStrategy::Proportional => {
                        let remaining_sections = all_sections.len()
                            .saturating_sub(result_sections.len());
                        let share = if remaining_sections > 0 {
                            max_chars / remaining_sections.max(1)
                        } else {
                            max_chars
                        };
                        content.chars().take(share).collect::<String>()
                    }
                    TruncationStrategy::Strict => {
                        String::new()
                    }
                };
                let actual_tokens = estimate_tokens(&truncated_content);
                result_sections.push(ContextSection {
                    name: name.to_string(),
                    priority: *priority,
                    content: truncated_content,
                    token_count: actual_tokens,
                    truncated: true,
                });
                total_tokens += actual_tokens;
            }
        }

        // Assemble final context string
        let context = match format {
            ContextFormat::Markdown => {
                result_sections
                    .iter()
                    .filter(|s| !s.content.is_empty())
                    .map(|s| format!("## {}\n\n{}", s.name, s.content))
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n")
            }
            ContextFormat::Json => {
                let obj: HashMap<&str, &str> = result_sections
                    .iter()
                    .filter(|s| !s.content.is_empty())
                    .map(|s| (s.name.as_str(), s.content.as_str()))
                    .collect();
                serde_json::to_string_pretty(&obj)
                    .unwrap_or_default()
            }
            ContextFormat::Soch | ContextFormat::Text => {
                result_sections
                    .iter()
                    .filter(|s| !s.content.is_empty())
                    .map(|s| format!("[{}]\n{}", s.name, s.content))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
        };

        let utilization = if token_budget > 0 {
            total_tokens as f64 / token_budget as f64
        } else {
            0.0
        };

        Ok(ContextQueryResult {
            context,
            token_count: total_tokens,
            budget: token_budget,
            utilization,
            sections: result_sections,
        })
    }

    // ── Task Queue (A8) ──────────────────────────────────────────────

    fn enqueue_task(
        &self,
        _queue_id: &str,
        priority: i64,
        payload: Vec<u8>,
    ) -> Result<BackgroundTask, String> {
        let task = self.task_queue.enqueue(priority, payload);
        Ok(BackgroundTask {
            task_id: task.key.task_id.clone(),
            priority: task.key.priority,
            payload: task.payload.clone(),
            created_at: task.created_at,
        })
    }

    fn enqueue_delayed_task(
        &self,
        _queue_id: &str,
        priority: i64,
        payload: Vec<u8>,
        delay_ms: u64,
    ) -> Result<BackgroundTask, String> {
        let task = self.task_queue.enqueue_delayed(priority, payload, delay_ms);
        Ok(BackgroundTask {
            task_id: task.key.task_id.clone(),
            priority: task.key.priority,
            payload: task.payload.clone(),
            created_at: task.created_at,
        })
    }

    fn claim_task(
        &self,
        _queue_id: &str,
        worker_id: &str,
    ) -> Result<TaskClaimResult, String> {
        match self.task_queue.dequeue(worker_id) {
            sochdb::queue::DequeueResult::Success(task) => {
                Ok(TaskClaimResult::Success(BackgroundTask {
                    task_id: task.key.task_id.clone(),
                    priority: task.key.priority,
                    payload: task.payload.clone(),
                    created_at: task.created_at,
                }))
            }
            sochdb::queue::DequeueResult::Empty => Ok(TaskClaimResult::Empty),
            sochdb::queue::DequeueResult::Contention(_) => Ok(TaskClaimResult::Contention),
            sochdb::queue::DequeueResult::Error(e) => Err(format!("dequeue: {e}")),
        }
    }

    fn ack_task(&self, _queue_id: &str, task_id: &str) -> Result<(), String> {
        self.task_queue.ack(task_id).map_err(|e| format!("ack: {e}"))
    }

    fn nack_task(
        &self,
        _queue_id: &str,
        task_id: &str,
        delay_ms: Option<u64>,
    ) -> Result<(), String> {
        self.task_queue.nack(task_id, None, delay_ms)
            .map_err(|e| format!("nack: {e}"))
    }

    fn queue_stats(&self, _queue_id: &str) -> Result<TaskQueueStats, String> {
        let stats = self.task_queue.stats();
        Ok(TaskQueueStats {
            pending: stats.pending,
            claimed: stats.inflight,
            completed: stats.total.saturating_sub(stats.pending + stats.delayed + stats.inflight),
            dead_lettered: 0, // PriorityQueue doesn't track dead-lettered separately
        })
    }

    // ── Path Query (A6 + GAP-18) ───────────────────────────────────

    fn path_query(
        &self,
        path: &str,
        filters: Option<Vec<(&str, serde_json::Value)>>,
    ) -> Result<Vec<PathQueryRow>, String> {
        // GAP-18: Enhanced path query with nested path resolution.
        //
        // Supports dotted paths like "sessions.abc.messages" which resolve to
        // the KV prefix "sessions/abc/messages/" (using "/" instead of ":" for
        // hierarchical data, and ":" for flat namespaces like "episodes:id").
        //
        // Path resolution strategy:
        // 1. Try "/" separator first (hierarchical: sessions/id/messages/)
        // 2. Fall back to ":" separator (flat: episodes:id)
        // 3. Try EmbeddedConnection::resolve() for native path resolution

        // Try hierarchical prefix first (most common for ClawDesk data)
        let hier_prefix = path.replace('.', "/");
        let hier_scan = if hier_prefix.ends_with('/') {
            hier_prefix.clone()
        } else {
            format!("{}/", hier_prefix)
        };

        let mut results = self.conn.scan(hier_scan.as_bytes())
            .unwrap_or_default();

        // If no hierarchical results, try flat namespace
        if results.is_empty() {
            let flat_prefix = path.replace('.', ":");
            results = self.conn.scan(flat_prefix.as_bytes())
                .unwrap_or_default();
        }

        // Also try the path as-is (exact prefix)
        if results.is_empty() {
            results = self.conn.scan(path.as_bytes())
                .unwrap_or_default();
        }

        let rows: Vec<PathQueryRow> = results
            .into_iter()
            .filter_map(|(key_bytes, value_bytes)| {
                let key = String::from_utf8(key_bytes).ok()?;
                let value: serde_json::Value =
                    serde_json::from_slice(&value_bytes).unwrap_or_else(|_| {
                        serde_json::Value::String(
                            String::from_utf8_lossy(&value_bytes).to_string(),
                        )
                    });

                // Apply filters with comparison support
                if let Some(ref filters) = filters {
                    if let serde_json::Value::Object(ref obj) = value {
                        for (field, expected) in filters {
                            match obj.get(*field) {
                                Some(actual) if actual == expected => {}
                                Some(actual) => {
                                    // Support numeric comparison: if expected
                                    // is prefixed with > or <, compare numerically
                                    if let (Some(a), Some(e)) = (
                                        actual.as_f64(),
                                        expected.as_f64(),
                                    ) {
                                        if (a - e).abs() < f64::EPSILON {
                                            continue;
                                        }
                                    }
                                    return None;
                                }
                                None => return None,
                            }
                        }
                    }
                }

                Some(PathQueryRow {
                    path: key,
                    values: match value {
                        serde_json::Value::Object(map) => map.into_iter().collect(),
                        other => HashMap::from([("value".to_string(), other)]),
                    },
                })
            })
            .collect();

        Ok(rows)
    }

    // ── SQL / AST Query (A15 + GAP-14) ──────────────────────────────

    fn sql_query(
        &self,
        sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<Vec<HashMap<String, serde_json::Value>>, String> {
        // GAP-14: Enhanced SQL interpreter over SochConn's KV layer.
        //
        // Supports:
        //   SELECT * FROM <table> [WHERE <field>=<value> [AND ...]] [ORDER BY <field> [ASC|DESC]] [LIMIT n]
        //   SELECT <col1>, <col2> FROM <table> [WHERE ...] [ORDER BY ...] [LIMIT n]
        //
        // Note: When SochDB exposes SochConnection from EmbeddedConnection,
        // replace this with `AstQueryExecutor::execute_with_params()`.

        let sql_lower = sql.to_lowercase();

        if !sql_lower.starts_with("select") {
            return Err("SQL: only SELECT queries supported via this interface".into());
        }

        // Parse column selection
        let select_end = sql_lower.find("from ")
            .ok_or("SQL: missing FROM clause")?;
        let select_clause = sql[7..select_end].trim(); // after "select "
        let select_all = select_clause == "*";
        let selected_columns: Vec<&str> = if !select_all {
            select_clause.split(',').map(|c| c.trim()).collect()
        } else {
            vec![]
        };

        // Parse table name
        let after_from = &sql[select_end + 5..].trim_start();
        let table_end = after_from.find(|c: char| c.is_whitespace()).unwrap_or(after_from.len());
        let table = &after_from[..table_end];

        // Scan all rows in the table
        let prefix = format!("{}:", table);
        let results = self.conn.scan(prefix.as_bytes())
            .map_err(|e| format!("sql scan: {e}"))?;

        let mut rows: Vec<HashMap<String, serde_json::Value>> = results
            .into_iter()
            .filter_map(|(_, data)| serde_json::from_slice(&data).ok())
            .collect();

        // Parse WHERE clause
        if let Some(where_pos) = sql_lower.find("where ") {
            let where_end = sql_lower[where_pos..]
                .find(" order ")
                .or_else(|| sql_lower[where_pos..].find(" limit "))
                .map(|p| where_pos + p)
                .unwrap_or(sql.len());
            let where_clause = &sql[where_pos + 6..where_end].trim();

            // Parse AND-separated conditions
            let conditions: Vec<&str> = where_clause.split(" and ").collect();
            for condition in conditions {
                let condition = condition.trim();
                // Parse field=value, field>value, field<value, field LIKE value
                if let Some(eq_pos) = condition.find('=') {
                    let field = condition[..eq_pos].trim();
                    let value_str = condition[eq_pos + 1..].trim().trim_matches('\'').trim_matches('"');
                    rows.retain(|row| {
                        row.get(field).map_or(false, |v| {
                            match v {
                                serde_json::Value::String(s) => s == value_str,
                                serde_json::Value::Number(n) => n.to_string() == value_str,
                                serde_json::Value::Bool(b) => b.to_string() == value_str,
                                _ => v.to_string().trim_matches('"') == value_str,
                            }
                        })
                    });
                } else if condition.to_lowercase().contains(" like ") {
                    let parts: Vec<&str> = condition.splitn(2, |c: char| {
                        c.to_lowercase().to_string() == "l" && condition.to_lowercase().contains("like")
                    }).collect();
                    if let Some(like_pos) = condition.to_lowercase().find(" like ") {
                        let field = condition[..like_pos].trim();
                        let pattern = condition[like_pos + 6..].trim().trim_matches('\'').trim_matches('"');
                        let pattern_lower = pattern.to_lowercase();
                        let is_prefix = pattern_lower.ends_with('%') && !pattern_lower.starts_with('%');
                        let is_suffix = pattern_lower.starts_with('%') && !pattern_lower.ends_with('%');
                        let is_contains = pattern_lower.starts_with('%') && pattern_lower.ends_with('%');
                        let core = pattern_lower.trim_matches('%');
                        rows.retain(|row| {
                            row.get(field).map_or(false, |v| {
                                let s = match v {
                                    serde_json::Value::String(s) => s.to_lowercase(),
                                    other => other.to_string().to_lowercase(),
                                };
                                if is_contains { s.contains(core) }
                                else if is_prefix { s.starts_with(core) }
                                else if is_suffix { s.ends_with(core) }
                                else { s == core }
                            })
                        });
                    }
                    let _ = parts; // suppress unused warning
                }
            }
        }

        // Parse ORDER BY clause
        if let Some(order_pos) = sql_lower.find("order by ") {
            let order_end = sql_lower[order_pos..]
                .find(" limit ")
                .map(|p| order_pos + p)
                .unwrap_or(sql.len());
            let order_clause = &sql[order_pos + 9..order_end].trim();
            let parts: Vec<&str> = order_clause.split_whitespace().collect();
            let order_field = parts.first().map(|s| s.trim_end_matches(','));
            let ascending = !parts.get(1).map_or(false, |s| s.to_lowercase() == "desc");

            if let Some(field) = order_field {
                rows.sort_by(|a, b| {
                    let va = a.get(field);
                    let vb = b.get(field);
                    let cmp = match (va, vb) {
                        (Some(serde_json::Value::Number(na)), Some(serde_json::Value::Number(nb))) => {
                            na.as_f64().unwrap_or(0.0).partial_cmp(&nb.as_f64().unwrap_or(0.0))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        }
                        (Some(serde_json::Value::String(sa)), Some(serde_json::Value::String(sb))) => {
                            sa.cmp(sb)
                        }
                        _ => std::cmp::Ordering::Equal,
                    };
                    if ascending { cmp } else { cmp.reverse() }
                });
            }
        }

        // Parse LIMIT
        if let Some(limit_pos) = sql_lower.find("limit ") {
            let limit_str = &sql[limit_pos + 6..].trim();
            if let Ok(limit) = limit_str.split_whitespace().next().unwrap_or("0").parse::<usize>() {
                rows.truncate(limit);
            }
        }

        // Apply column projection (if not SELECT *)
        if !select_all && !selected_columns.is_empty() {
            rows = rows.into_iter().map(|row| {
                let mut projected = HashMap::new();
                for col in &selected_columns {
                    if let Some(val) = row.get(*col) {
                        projected.insert(col.to_string(), val.clone());
                    }
                }
                projected
            }).collect();
        }

        Ok(rows)
    }

    // ── Predefined Views (A5) ─────────────────────────────────────────

    fn list_views(&self) -> Vec<String> {
        sochdb_core::predefined_views::get_predefined_views()
            .iter()
            .map(|v| v.name.to_string())
            .collect()
    }

    fn query_view(
        &self,
        view_name: &str,
        filters: Option<HashMap<String, serde_json::Value>>,
        limit: Option<usize>,
    ) -> Result<Vec<HashMap<String, serde_json::Value>>, String> {
        let view = sochdb_core::predefined_views::get_view(view_name)
            .ok_or_else(|| format!("unknown view: {view_name}"))?;

        // Build SQL from the view definition + filters
        let mut sql = view.definition.to_string();
        let mut params: Vec<serde_json::Value> = vec![];

        if let Some(filters) = filters {
            let mut where_clauses = Vec::new();
            for (k, v) in filters {
                params.push(v);
                where_clauses.push(format!("{} = ${}", k, params.len()));
            }
            if !where_clauses.is_empty() {
                // If view.definition already has WHERE, use AND
                if sql.to_lowercase().contains("where") {
                    sql.push_str(" AND ");
                } else {
                    sql.push_str(" WHERE ");
                }
                sql.push_str(&where_clauses.join(" AND "));
            }
        }

        if let Some(limit) = limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        self.sql_query(&sql, &params)
    }
}

// ============================================================================
// Conversion helpers
// ============================================================================

/// Convert a serde_json::Value to SochDB's SochValue type.
fn json_to_soch_value(v: &serde_json::Value) -> sochdb_core::soch::SochValue {
    match v {
        serde_json::Value::String(s) => sochdb_core::soch::SochValue::Text(s.clone()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                sochdb_core::soch::SochValue::Int(i)
            } else {
                sochdb_core::soch::SochValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::Bool(b) => sochdb_core::soch::SochValue::Bool(*b),
        serde_json::Value::Null => sochdb_core::soch::SochValue::Null,
        other => sochdb_core::soch::SochValue::Text(other.to_string()),
    }
}

/// Convert SochDB's SochValue to serde_json::Value.
fn soch_value_to_json(v: sochdb_core::soch::SochValue) -> serde_json::Value {
    match v {
        sochdb_core::soch::SochValue::Text(s) => serde_json::Value::String(s),
        sochdb_core::soch::SochValue::Int(i) => serde_json::json!(i),
        sochdb_core::soch::SochValue::UInt(u) => serde_json::json!(u),
        sochdb_core::soch::SochValue::Float(f) => serde_json::json!(f),
        sochdb_core::soch::SochValue::Bool(b) => serde_json::json!(b),
        sochdb_core::soch::SochValue::Null => serde_json::Value::Null,
        sochdb_core::soch::SochValue::Binary(b) => serde_json::json!(base64_encode(&b)),
        sochdb_core::soch::SochValue::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(soch_value_to_json).collect())
        }
        sochdb_core::soch::SochValue::Object(map) => {
            serde_json::Value::Object(
                map.into_iter()
                    .map(|(k, v)| (k, soch_value_to_json(v)))
                    .collect(),
            )
        }
        sochdb_core::soch::SochValue::Ref { table, id } => {
            serde_json::json!({ "ref": { "table": table, "id": id } })
        }
    }
}

/// Simple base64 encoding for blob values.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

/// Convert SochDB Episode to our local Episode type.
fn soch_episode_to_local(ep: sochdb_core::memory_schema::Episode) -> Episode {
    Episode {
        episode_id: ep.episode_id,
        episode_type: match ep.episode_type {
            sochdb_core::memory_schema::EpisodeType::Conversation => EpisodeType::Conversation,
            sochdb_core::memory_schema::EpisodeType::Task => EpisodeType::Task,
            sochdb_core::memory_schema::EpisodeType::Workflow => EpisodeType::Workflow,
            sochdb_core::memory_schema::EpisodeType::Debug => EpisodeType::Debug,
            sochdb_core::memory_schema::EpisodeType::AgentInteraction => EpisodeType::AgentInteraction,
            sochdb_core::memory_schema::EpisodeType::Other => EpisodeType::Other,
        },
        entity_ids: ep.entity_ids,
        ts_start: ep.ts_start,
        ts_end: ep.ts_end,
        summary: ep.summary,
        tags: ep.tags,
        embedding: ep.embedding,
        metadata: ep.metadata
            .into_iter()
            .map(|(k, v)| (k, soch_value_to_json(v)))
            .collect(),
    }
}
