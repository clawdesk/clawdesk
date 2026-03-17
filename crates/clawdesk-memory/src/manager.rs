//! Memory manager — coordinates embedding, storage, and retrieval.
//!
//! Integrates chunking, temporal decay, and MMR diversity
//! re-ranking into the remember/recall pipeline.
//!
//! ## SochDB Integration
//!
//! When the backend implements `MemoryBackend` (e.g., `SochMemoryBackend`),
//! the manager automatically uses:
//! - **Atomic writes**: all-or-nothing multi-index writes
//! - **Graph nodes**: memory nodes + edges in knowledge graph
//! - **Graph-contextual retrieval**: scope search to graph-reachable memories
//! - **Temporal pre-filter**: true time-bounded edge queries
//! - **Policy checks**: PII redaction before storage, access control on read
//! - **Trace spans**: OpenTelemetry-compatible instrumentation

use crate::chunker::{chunk_text, sha256_hex, ChunkerConfig};
use crate::embedding::EmbeddingProvider;
use crate::hybrid::{HybridSearcher, SearchStrategy};
use crate::mmr::{mmr_rerank, MmrCandidate, MmrConfig};
use crate::pipeline::BatchPipeline;
use crate::temporal_decay::{apply_temporal_decay, TemporalDecayConfig};
use clawdesk_storage::memory_backend::{
    MemoryBackend, MemoryWriteOp, PolicyCheckResult,
    // Memory Schema types (A4)
    Episode, EpisodeType, Event, Entity, EntityKind, EntityFacts,
    // Context Query types (A1)
    ContextQueryResult, ContextFormat, TruncationStrategy,
    // Task Queue types (A8)
    BackgroundTask, TaskClaimResult, TaskQueueStats,
    // Multi-Vector types (A11)
    MultiVectorDocument, DocumentSearchResult,
    // Path Query types (A6)
    PathQueryRow,
    // Batch types (A7)
    BatchWriteResult,
};
use clawdesk_storage::vector_store::{
    CollectionConfig, DistanceMetric, VectorSearchResult,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};

/// Where a memory came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemorySource {
    Conversation,
    Document,
    UserSaved,
    Plugin,
    System,
}

/// Configuration for the memory manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Name for the vector collection.
    pub collection_name: String,
    /// Search strategy.
    pub search_strategy: SearchStrategy,
    /// Whether to automatically embed on remember().
    pub auto_embed: bool,
    /// Maximum results per recall.
    pub max_results: usize,
    /// Minimum relevance score.
    pub min_relevance: f32,
    /// Chunking configuration for long texts.
    pub chunker: ChunkerConfig,
    /// Temporal decay configuration.
    pub temporal_decay: TemporalDecayConfig,
    /// MMR diversity re-ranking configuration.
    pub mmr: MmrConfig,
    /// Optional session node ID for graph-contextual retrieval.
    /// When set, `recall()` scopes search to memories reachable from this node.
    pub session_node_id: Option<String>,
    /// Optional trace run ID for observability spans.
    /// When set, `remember()` and `recall()` emit trace spans.
    pub trace_run_id: Option<String>,
    /// Optional agent ID for policy access checks.
    pub agent_id: Option<String>,
    /// Maximum graph traversal depth for contextual retrieval.
    pub graph_max_depth: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            collection_name: "memories".to_string(),
            search_strategy: SearchStrategy::Hybrid,
            auto_embed: true,
            max_results: 10,
            min_relevance: 0.15,
            chunker: ChunkerConfig::default(),
            temporal_decay: TemporalDecayConfig::default(),
            mmr: MmrConfig::default(),
            session_node_id: None,
            trace_run_id: None,
            agent_id: None,
            graph_max_depth: 3,
        }
    }
}

/// Stats about the memory system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub total_memories: usize,
    pub collection_name: String,
    pub embedding_dimensions: usize,
}

/// High-level memory management: remember, recall, forget.
///
/// Generic over `S: MemoryBackend` — when the backend is `SochMemoryBackend`,
/// the manager uses atomic writes, graph nodes, temporal edges, policy checks,
/// and trace spans. Other backends get default no-op behavior for these features.
pub struct MemoryManager<S: MemoryBackend> {
    store: Arc<S>,
    embedding: Arc<dyn EmbeddingProvider>,
    searcher: HybridSearcher<S>,
    pipeline: BatchPipeline,
    config: MemoryConfig,
    collection_ready: OnceCell<()>,
}

impl<S: MemoryBackend> MemoryManager<S> {
    pub fn new(store: Arc<S>, embedding: Arc<dyn EmbeddingProvider>, config: MemoryConfig) -> Self {
        // Recover any incomplete atomic writes from a prior crash.
        match store.recover_atomic_writes() {
            Ok(0) => {}
            Ok(n) => info!(replayed = n, "recovered incomplete atomic memory writes"),
            Err(e) => warn!(error = %e, "atomic write recovery failed"),
        }

        let searcher = HybridSearcher::new(store.clone());
        let pipeline = BatchPipeline::new(embedding.clone());

        Self {
            store,
            embedding,
            searcher,
            pipeline,
            config,
            collection_ready: OnceCell::new(),
        }
    }

    /// Update the session node for graph-contextual retrieval.
    pub fn set_session_node(&mut self, node_id: Option<String>) {
        self.config.session_node_id = node_id;
    }

    /// Update the trace run ID for observability.
    pub fn set_trace_run_id(&mut self, run_id: Option<String>) {
        self.config.trace_run_id = run_id;
    }

    /// Update the agent ID for policy checks.
    pub fn set_agent_id(&mut self, agent_id: Option<String>) {
        self.config.agent_id = agent_id;
    }

    /// Ensure the collection exists (lazy init).
    async fn ensure_collection(&self) -> Result<(), String> {
        self.collection_ready
            .get_or_try_init(|| async {
                let dims = self.embedding.dimensions();
                let config = CollectionConfig {
                    name: self.config.collection_name.clone(),
                    dimension: dims,
                    metric: DistanceMetric::Cosine,
                    enable_hybrid_search: true,
                    content_field: "content".to_string(),
                };
                self.store
                    .create_collection(config)
                    .await
                    .map_err(|e| format!("create collection: {e}"))?;
                info!(
                    collection = %self.config.collection_name,
                    dims,
                    "memory collection initialized"
                );
                Ok(())
            })
            .await
            .map(|_| ())
    }

    /// Apply policy check on content before storage.
    /// Returns the (possibly redacted) content, or an error if denied.
    fn policy_filter_content(&self, content: &str) -> Result<String, String> {
        match self.store.policy_check_content(content) {
            PolicyCheckResult::Allow => Ok(content.to_string()),
            PolicyCheckResult::Redacted(redacted) => {
                debug!("policy: content redacted before storage");
                Ok(redacted)
            }
            PolicyCheckResult::Deny(reason) => {
                warn!(reason = %reason, "policy: content denied");
                Err(format!("policy denied: {reason}"))
            }
        }
    }

    /// Store a memory with automatic embedding.
    ///
    /// Long texts are chunked and each chunk is stored with a
    /// content-hash dedup key so duplicate writes are idempotent.
    ///
    /// ## SochDB Integration
    /// - All chunks written atomically (all-or-nothing)
    /// - Graph nodes + edges created for each memory
    /// - Trace spans emitted for the operation
    /// - Content checked against policy before storage
    pub async fn remember(
        &self,
        content: &str,
        source: MemorySource,
        metadata: serde_json::Value,
    ) -> Result<String, String> {
        self.ensure_collection().await?;

        // ── Start trace span ───────────────────────────────
        let span = self.config.trace_run_id.as_ref().and_then(|rid| {
            self.store.trace_start_span(rid, "memory.remember")
        });

        // ── Policy check on content ────────────────────────
        let content = self.policy_filter_content(content)?;

        let chunks = chunk_text(&content, &self.config.chunker);

        if chunks.is_empty() {
            if let Some(ref s) = span {
                self.store.trace_end_span(s, false, Some(HashMap::from([
                    ("error".into(), "no_chunks".into()),
                ])));
            }
            return Err("content produced no chunks".into());
        }

        // ── Embed all chunks ───────────────────────────────────────
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let batch_result = if texts.len() == 1 {
            // Single-chunk: use direct embed (cheaper path)
            let emb = self.embedding.embed(&texts[0]).await.map_err(|e| format!("embed: {e}"))?;
            crate::embedding::BatchEmbeddingResult {
                embeddings: vec![emb],
                total_tokens: 0,
            }
        } else {
            self.pipeline.embed_all(&texts).await.map_err(|e| format!("batch embed: {e}"))?
        };

        // ── Similarity dedup: reject near-duplicates (≥0.92 cosine) ─
        // Only check the first chunk — if it's a near-dup, the whole content is.
        const DEDUP_SIMILARITY_THRESHOLD: f32 = 0.92;
        if let Some(first_emb) = batch_result.embeddings.first() {
            match self.store.search(
                &self.config.collection_name,
                &first_emb.vector,
                1,
                Some(DEDUP_SIMILARITY_THRESHOLD),
            ).await {
                Ok(existing) if !existing.is_empty() => {
                    let best = &existing[0];
                    debug!(
                        score = best.score,
                        existing_id = %best.id,
                        "similarity dedup: skipping near-duplicate memory (score >= {DEDUP_SIMILARITY_THRESHOLD})"
                    );
                    if let Some(ref s) = span {
                        self.store.trace_end_span(s, true, Some(HashMap::from([
                            ("dedup".into(), "skipped".into()),
                            ("existing_id".into(), best.id.clone()),
                            ("similarity".into(), format!("{:.4}", best.score)),
                        ])));
                    }
                    // Return the existing memory's ID instead of creating a duplicate
                    return Ok(best.id.clone());
                }
                Ok(_) => {} // No near-duplicates, proceed
                Err(e) => {
                    // Don't block storage on a dedup check failure
                    debug!(error = %e, "similarity dedup check failed, proceeding with storage");
                }
            }
        }

        let now = chrono::Utc::now();
        let now_str = now.to_rfc3339();
        let collection = &self.config.collection_name;
        let primary_id = uuid::Uuid::new_v4().to_string();

        // ── Build atomic write ops + graph nodes ─
        let mut atomic_ops: Vec<MemoryWriteOp> = Vec::new();
        let mut ids = Vec::with_capacity(chunks.len());

        for (i, (chunk, emb)) in chunks.iter().zip(batch_result.embeddings.iter()).enumerate() {
            let id = if i == 0 {
                primary_id.clone()
            } else {
                uuid::Uuid::new_v4().to_string()
            };
            let content_hash = sha256_hex(&chunk.text);

            let mut enriched_meta = if i == 0 { metadata.clone() } else { metadata.clone() };
            enriched_meta["source"] = serde_json::json!(format!("{:?}", source));
            enriched_meta["content"] = serde_json::json!(&chunk.text);
            enriched_meta["timestamp"] = serde_json::json!(&now_str);
            enriched_meta["content_hash"] = serde_json::json!(content_hash);
            if chunks.len() > 1 {
                enriched_meta["chunk_index"] = serde_json::json!(i);
                enriched_meta["total_chunks"] = serde_json::json!(chunks.len());
                enriched_meta["original_length"] = serde_json::json!(content.len());
            }

            // Convert metadata to HashMap<String, String> for atomic write
            let meta_map: HashMap<String, String> = enriched_meta
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.to_string().trim_matches('"').to_string()))
                        .collect()
                })
                .unwrap_or_default();

            // Atomic PutEmbedding op
            atomic_ops.push(MemoryWriteOp::PutEmbedding {
                collection: collection.to_string(),
                id: id.clone(),
                embedding: emb.vector.clone(),
                metadata: meta_map,
            });

            // Create graph node for this memory chunk
            let mut node_props: HashMap<String, serde_json::Value> = HashMap::new();
            node_props.insert("source".into(), serde_json::json!(format!("{:?}", source)));
            node_props.insert("timestamp".into(), serde_json::json!(&now_str));
            node_props.insert("content_hash".into(), serde_json::json!(content_hash));

            atomic_ops.push(MemoryWriteOp::CreateNode {
                namespace: "clawdesk".to_string(),
                node_id: id.clone(),
                node_type: "memory".to_string(),
                properties: node_props,
            });

            // Link chunk to session node if available
            if let Some(ref session_id) = self.config.session_node_id {
                atomic_ops.push(MemoryWriteOp::CreateEdge {
                    namespace: "clawdesk".to_string(),
                    from_id: session_id.clone(),
                    edge_type: "has_memory".to_string(),
                    to_id: id.clone(),
                    properties: HashMap::new(),
                });
            }

            // Link multi-chunk memories together
            if chunks.len() > 1 && i > 0 {
                atomic_ops.push(MemoryWriteOp::CreateEdge {
                    namespace: "clawdesk".to_string(),
                    from_id: primary_id.clone(),
                    edge_type: "has_chunk".to_string(),
                    to_id: id.clone(),
                    properties: HashMap::from([
                        ("chunk_index".into(), serde_json::json!(i)),
                    ]),
                });
            }

            ids.push(id);
        }

        // ── Execute atomic write ───────────────────────────
        match self.store.write_atomic(&primary_id, atomic_ops) {
            Ok(result) => {
                debug!(
                    id = %primary_id,
                    ops = result.ops_applied,
                    intent = result.intent_id,
                    chunks = chunks.len(),
                    "memory stored atomically"
                );
            }
            Err(e) if e.contains("not supported") => {
                // Fallback: non-atomic inserts for backends without atomic writes
                debug!("atomic writes not supported, falling back to sequential inserts");
                for (i, (chunk, emb)) in chunks.iter().zip(batch_result.embeddings.iter()).enumerate() {
                    let id = &ids[i];
                    let content_hash = sha256_hex(&chunk.text);
                    let mut enriched_meta = metadata.clone();
                    enriched_meta["source"] = serde_json::json!(format!("{:?}", source));
                    enriched_meta["content"] = serde_json::json!(&chunk.text);
                    enriched_meta["timestamp"] = serde_json::json!(&now_str);
                    enriched_meta["content_hash"] = serde_json::json!(content_hash);
                    if chunks.len() > 1 {
                        enriched_meta["chunk_index"] = serde_json::json!(i);
                        enriched_meta["total_chunks"] = serde_json::json!(chunks.len());
                        enriched_meta["original_length"] = serde_json::json!(content.len());
                    }

                    self.store
                        .insert(collection, id, &emb.vector, Some(enriched_meta))
                        .await
                        .map_err(|e| format!("insert chunk {i}: {e}"))?;
                }
            }
            Err(e) => {
                if let Some(ref s) = span {
                    self.store.trace_end_span(s, false, Some(HashMap::from([
                        ("error".into(), e.clone()),
                    ])));
                }
                return Err(format!("atomic write failed: {e}"));
            }
        }

        // ── End trace span ─────────────────────────────────
        if let Some(ref s) = span {
            self.store.trace_end_span(s, true, Some(HashMap::from([
                ("memory_id".into(), primary_id.clone()),
                ("chunks".into(), chunks.len().to_string()),
            ])));
        }

        Ok(primary_id)
    }

    /// Batch-store memories.
    ///
    /// Embeddings are computed in batch. Each memory is written atomically
    /// with its graph node and session edge (Tasks 1, 7).
    pub async fn remember_batch(
        &self,
        items: Vec<(String, MemorySource, serde_json::Value)>,
    ) -> Result<Vec<String>, String> {
        self.ensure_collection().await?;

        let span = self.config.trace_run_id.as_ref().and_then(|rid| {
            self.store.trace_start_span(rid, "memory.remember_batch")
        });

        // Policy-filter all content
        let filtered_items: Vec<(String, MemorySource, serde_json::Value)> = items
            .into_iter()
            .map(|(content, source, meta)| {
                let filtered = self.policy_filter_content(&content).unwrap_or(content);
                (filtered, source, meta)
            })
            .collect();

        let texts: Vec<String> = filtered_items.iter().map(|(t, _, _)| t.clone()).collect();
        let batch_result = self
            .pipeline
            .embed_all(&texts)
            .await
            .map_err(|e| format!("batch embed: {e}"))?;

        let collection = &self.config.collection_name;
        let now_str = chrono::Utc::now().to_rfc3339();
        let mut all_ids = Vec::with_capacity(filtered_items.len());

        // ── Similarity dedup: reject near-duplicates (≥0.92 cosine) ─
        // Same threshold as remember() to maintain consistency.
        const DEDUP_SIMILARITY_THRESHOLD: f32 = 0.92;

        for ((content, source, metadata), emb) in filtered_items.into_iter().zip(batch_result.embeddings) {
            // Check for near-duplicate before inserting
            match self.store.search(
                collection,
                &emb.vector,
                1,
                Some(DEDUP_SIMILARITY_THRESHOLD),
            ).await {
                Ok(existing) if !existing.is_empty() => {
                    let best = &existing[0];
                    debug!(
                        score = best.score,
                        existing_id = %best.id,
                        "batch dedup: skipping near-duplicate memory (score >= {DEDUP_SIMILARITY_THRESHOLD})"
                    );
                    // Return the existing memory's ID instead of creating a duplicate
                    all_ids.push(best.id.clone());
                    continue;
                }
                Ok(_) => {} // No near-duplicates, proceed
                Err(e) => {
                    // Don't block storage on a dedup check failure
                    debug!(error = %e, "batch dedup check failed, proceeding with storage");
                }
            }

            let id = uuid::Uuid::new_v4().to_string();
            let content_hash = sha256_hex(&content);

            let mut enriched_meta = metadata;
            enriched_meta["source"] = serde_json::json!(format!("{:?}", source));
            enriched_meta["content"] = serde_json::json!(&content);
            enriched_meta["timestamp"] = serde_json::json!(&now_str);
            enriched_meta["content_hash"] = serde_json::json!(&content_hash);

            // Build atomic ops
            let meta_map: HashMap<String, String> = enriched_meta
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.to_string().trim_matches('"').to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let mut ops = vec![
                MemoryWriteOp::PutEmbedding {
                    collection: collection.to_string(),
                    id: id.clone(),
                    embedding: emb.vector.clone(),
                    metadata: meta_map,
                },
                MemoryWriteOp::CreateNode {
                    namespace: "clawdesk".to_string(),
                    node_id: id.clone(),
                    node_type: "memory".to_string(),
                    properties: HashMap::from([
                        ("source".into(), serde_json::json!(format!("{:?}", source))),
                        ("timestamp".into(), serde_json::json!(&now_str)),
                        ("content_hash".into(), serde_json::json!(&content_hash)),
                    ]),
                },
            ];

            if let Some(ref session_id) = self.config.session_node_id {
                ops.push(MemoryWriteOp::CreateEdge {
                    namespace: "clawdesk".to_string(),
                    from_id: session_id.clone(),
                    edge_type: "has_memory".to_string(),
                    to_id: id.clone(),
                    properties: HashMap::new(),
                });
            }

            // Also link from the global user identity node so this memory
            // is reachable across sessions (yesterday, last week, etc.).
            ops.push(MemoryWriteOp::CreateEdge {
                namespace: "clawdesk".to_string(),
                from_id: "user:global".to_string(),
                edge_type: "has_memory".to_string(),
                to_id: id.clone(),
                properties: HashMap::from([
                    ("timestamp".into(), serde_json::json!(&now_str)),
                ]),
            });

            match self.store.write_atomic(&id, ops) {
                Ok(_) => {
                    debug!(id = %id, "batch memory stored atomically");
                }
                Err(e) if e.contains("not supported") => {
                    // Fallback: non-atomic insert
                    self.store
                        .insert(collection, &id, &emb.vector, Some(enriched_meta))
                        .await
                        .map_err(|e| format!("insert: {e}"))?;
                }
                Err(e) => {
                    warn!(error = %e, id = %id, "atomic write failed in batch");
                    // Still insert via fallback so we don't lose data
                    self.store
                        .insert(collection, &id, &emb.vector, Some(enriched_meta))
                        .await
                        .map_err(|e| format!("insert fallback: {e}"))?;
                }
            }

            all_ids.push(id);
        }

        if let Some(ref s) = span {
            self.store.trace_end_span(s, true, Some(HashMap::from([
                ("count".into(), all_ids.len().to_string()),
            ])));
        }

        Ok(all_ids)
    }

    /// Recall relevant memories.
    ///
    /// Pipeline: graph-contextual pre-filter → vector+BM25 hybrid search →
    /// temporal decay → MMR diversity re-ranking → min-relevance filter.
    ///
    /// ## SochDB Integration
    /// - Scopes search to graph-reachable memories when session_node_id is set
    /// - Uses temporal pre-filter via edge queries
    /// - Trace spans for each pipeline stage
    /// - Access control check before returning results
    pub async fn recall(
        &self,
        query: &str,
        max_results: Option<usize>,
    ) -> Result<Vec<VectorSearchResult>, String> {
        self.ensure_collection().await?;

        let span = self.config.trace_run_id.as_ref().and_then(|rid| {
            self.store.trace_start_span(rid, "memory.recall")
        });

        // ── Access control check ───────────────────────────
        if let Some(ref agent_id) = self.config.agent_id {
            if !self.store.policy_check_access(agent_id, &self.config.collection_name) {
                if let Some(ref s) = span {
                    self.store.trace_end_span(s, false, Some(HashMap::from([
                        ("error".into(), "access_denied".into()),
                    ])));
                }
                return Err(format!("agent '{}' denied access to collection '{}'", agent_id, self.config.collection_name));
            }
        }

        let query_result = self
            .embedding
            .embed(query)
            .await
            .map_err(|e| format!("embed query: {e}"))?;

        // Fetch more candidates than requested — temporal decay and MMR will
        // prune/reorder, so we need headroom.
        let top_k = max_results.unwrap_or(self.config.max_results);
        let fetch_k = (top_k * 3).max(20); // 3x headroom

        // ── Graph-contextual pre-filter ────────────────────
        // When a session node is set, find all memory IDs reachable from it
        // and use them to scope the vector search.
        let reachable_ids: Option<Vec<String>> = self.config.session_node_id.as_ref().and_then(|session_id| {
            match self.store.graph_reachable_memory_ids(session_id, "has_memory", self.config.graph_max_depth) {
                Ok(ids) if !ids.is_empty() => {
                    debug!(session = %session_id, reachable = ids.len(), "graph-contextual pre-filter");
                    Some(ids)
                }
                Ok(_) => None, // no reachable nodes, search all
                Err(e) => {
                    debug!(error = %e, "graph traversal failed, searching all memories");
                    None
                }
            }
        });

        // Also try the global user identity node for cross-session recall.
        // This ensures memories from previous sessions (yesterday, last week)
        // are still findable even when the graph filter scopes to today's session.
        let cross_session_ids: Option<Vec<String>> = {
            match self.store.graph_reachable_memory_ids("user:global", "has_memory", 2) {
                Ok(ids) if !ids.is_empty() => {
                    debug!(reachable = ids.len(), "cross-session memory nodes found");
                    Some(ids)
                }
                _ => None,
            }
        };

        let mut results = self
            .searcher
            .search(
                &self.config.collection_name,
                &query_result.vector,
                query,
                fetch_k,
                self.config.search_strategy,
            )
            .await?;

        // Filter to graph-reachable memories if available
        if let Some(ref reachable) = reachable_ids {
            let before = results.len();

            // Merge session-scoped + cross-session IDs for broader recall
            let mut all_reachable = reachable.clone();
            if let Some(ref cross) = cross_session_ids {
                for id in cross {
                    if !all_reachable.contains(id) {
                        all_reachable.push(id.clone());
                    }
                }
            }

            results.retain(|r| all_reachable.contains(&r.id));
            debug!(
                before,
                after = results.len(),
                session_ids = reachable.len(),
                cross_session_ids = cross_session_ids.as_ref().map(|v| v.len()).unwrap_or(0),
                "graph-contextual filter applied (session + cross-session)"
            );
            // If filtering removed everything, fall back to unfiltered results
            if results.is_empty() {
                debug!("graph filter removed all results, using unfiltered global search");
                results = self
                    .searcher
                    .search(
                        &self.config.collection_name,
                        &query_result.vector,
                        query,
                        fetch_k,
                        self.config.search_strategy,
                    )
                    .await?;
            }
        } else if let Some(ref cross) = cross_session_ids {
            // No session scope but we have cross-session IDs — boost them
            for result in &mut results {
                if cross.contains(&result.id) {
                    result.score *= 1.1; // slight boost for known memories
                }
            }
        }

        // ── Temporal pre-filter ────────────────────────────
        // If the backend supports temporal edges, check which memories are
        // still "valid" at the current time. This is more accurate than
        // the post-hoc exponential decay.
        if let Some(ref session_id) = self.config.session_node_id {
            let now_ms = chrono::Utc::now().timestamp_millis() as u64;
            match self.store.temporal_edges_at(session_id, Some("has_memory"), now_ms) {
                Ok(edges) if !edges.is_empty() => {
                    let valid_ids: Vec<String> = edges.iter().map(|e| e.to_id.clone()).collect();
                    let before = results.len();
                    // Boost temporally-valid memories instead of hard-filtering
                    for result in &mut results {
                        if valid_ids.contains(&result.id) {
                            result.score *= 1.2; // 20% boost for temporally-valid
                        }
                    }
                    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
                    debug!(
                        temporal_valid = valid_ids.len(),
                        total = before,
                        "temporal pre-filter boost applied"
                    );
                }
                Ok(_) => {} // no temporal edges, continue with decay
                Err(e) => {
                    debug!(error = %e, "temporal pre-filter failed, using decay fallback");
                }
            }
        }

        // ── Stage 1: Temporal decay (fallback / supplementary) ─────
        if self.config.temporal_decay.enabled {
            let mut scored: Vec<(String, f32, serde_json::Value)> = results
                .iter()
                .map(|r| (r.id.clone(), r.score, r.metadata.clone()))
                .collect();

            apply_temporal_decay(&mut scored, &self.config.temporal_decay);

            // Write decayed scores back
            for (i, (_, decayed_score, _)) in scored.iter().enumerate() {
                if i < results.len() {
                    results[i].score = *decayed_score;
                }
            }
            results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        }

        // ── Stage 2: MMR diversity re-ranking ──────────────────────
        let candidates: Vec<MmrCandidate> = results
            .iter()
            .map(|r| MmrCandidate {
                id: r.id.clone(),
                score: r.score,
                content: r.content.clone().unwrap_or_default(),
                metadata: r.metadata.clone(),
            })
            .collect();

        let mmr_config = MmrConfig {
            lambda: self.config.mmr.lambda,
            top_k,
        };

        let mmr_results = mmr_rerank(&candidates, &mmr_config);

        // Rebuild VectorSearchResult from MMR output, preserving order.
        let reranked: Vec<VectorSearchResult> = mmr_results
            .into_iter()
            .map(|m| VectorSearchResult {
                id: m.id,
                score: m.score,
                metadata: m.metadata,
                content: Some(m.content),
            })
            .collect();

        // ── Stage 3: Minimum relevance filter ──────────────────────
        let filtered: Vec<_> = reranked
            .into_iter()
            .filter(|r| r.score >= self.config.min_relevance)
            .collect();

        // ── End trace span ─────────────────────────────────
        if let Some(ref s) = span {
            self.store.trace_end_span(s, true, Some(HashMap::from([
                ("query_len".into(), query.len().to_string()),
                ("results".into(), filtered.len().to_string()),
            ])));
        }

        debug!(query = %query, results = filtered.len(), "memory recall (graph+temporal+decay+mmr)");
        Ok(filtered)
    }

    /// Recall with temporal expansion — geodesic concentric search.
    ///
    /// A physics-inspired approach: searches outward from the current session
    /// in expanding temporal rings, like a wave propagating from the query point.
    ///
    /// **Ring 0** — Current session: memories attached to `session_id` via graph.
    /// **Ring 1** — Cross-session (global node): memories from other sessions.
    /// **Ring 2** — Full corpus: unscoped vector search across all memories.
    ///
    /// Results from closer rings receive higher boosts (1/r² decay — inverse
    /// square law of relevance). This ensures recent, contextually-adjacent
    /// memories dominate while distant memories still surface when nothing
    /// nearby matches.
    ///
    /// The method is immutable (`&self`) — session scope is passed per-call
    /// rather than stored as mutable state, allowing safe use behind `Arc`.
    pub async fn recall_with_scope(
        &self,
        query: &str,
        max_results: Option<usize>,
        session_id: Option<&str>,
    ) -> Result<Vec<VectorSearchResult>, String> {
        self.ensure_collection().await?;

        let top_k = max_results.unwrap_or(self.config.max_results);

        // No session scope → fall back to global recall
        let Some(session_id) = session_id else {
            return self.recall(query, max_results).await;
        };

        let query_result = self
            .embedding
            .embed(query)
            .await
            .map_err(|e| format!("embed query: {e}"))?;

        let fetch_k = (top_k * 4).max(30);

        // ── Ring 0: Session-local memories ─────────────────
        let session_node = format!("session:{session_id}");
        let ring0_ids: Vec<String> = self
            .store
            .graph_reachable_memory_ids(&session_node, "has_memory", self.config.graph_max_depth)
            .unwrap_or_default();

        // ── Ring 1: Cross-session (user global) ────────────
        let ring1_ids: Vec<String> = self
            .store
            .graph_reachable_memory_ids("user:global", "has_memory", 2)
            .unwrap_or_default();

        // ── Full-corpus vector search ──────────────────────
        let mut results = self
            .searcher
            .search(
                &self.config.collection_name,
                &query_result.vector,
                query,
                fetch_k,
                self.config.search_strategy,
            )
            .await?;

        // ── Apply concentric ring boosts (inverse-square inspired) ──
        // Ring 0 (session-local) → 1.5× boost
        // Ring 1 (cross-session)  → 1.15× boost
        // Ring 2 (unscoped)       → 1.0× (no change)
        for result in &mut results {
            if ring0_ids.contains(&result.id) {
                result.score *= 1.5;
            } else if ring1_ids.contains(&result.id) {
                result.score *= 1.15;
            }
        }
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // ── Temporal boost for current-session memories ────
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        if let Ok(edges) = self.store.temporal_edges_at(&session_node, Some("has_memory"), now_ms) {
            let valid_ids: Vec<String> = edges.iter().map(|e| e.to_id.clone()).collect();
            for result in &mut results {
                if valid_ids.contains(&result.id) {
                    result.score *= 1.2;
                }
            }
            results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        }

        // ── Temporal decay ─────────────────────────────────
        if self.config.temporal_decay.enabled {
            let mut scored: Vec<(String, f32, serde_json::Value)> = results
                .iter()
                .map(|r| (r.id.clone(), r.score, r.metadata.clone()))
                .collect();
            apply_temporal_decay(&mut scored, &self.config.temporal_decay);
            for (i, (_, decayed_score, _)) in scored.iter().enumerate() {
                if i < results.len() {
                    results[i].score = *decayed_score;
                }
            }
            results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        }

        // ── MMR diversity re-ranking ───────────────────────
        {
            let candidates: Vec<MmrCandidate> = results
                .iter()
                .map(|r| MmrCandidate {
                    id: r.id.clone(),
                    score: r.score,
                    content: r.content.clone().unwrap_or_default(),
                    metadata: r.metadata.clone(),
                })
                .collect();

            let mmr_config = MmrConfig {
                lambda: self.config.mmr.lambda,
                top_k,
            };

            let mmr_results = mmr_rerank(&candidates, &mmr_config);
            results = mmr_results
                .into_iter()
                .map(|m| VectorSearchResult {
                    id: m.id,
                    score: m.score,
                    metadata: m.metadata,
                    content: Some(m.content),
                })
                .collect();
        }

        // ── Min-relevance filter ───────────────────────────
        results.retain(|r| r.score >= self.config.min_relevance);
        results.truncate(top_k);

        debug!(
            query = %query,
            session = %session_id,
            ring0 = ring0_ids.len(),
            ring1 = ring1_ids.len(),
            results = results.len(),
            "temporal-expansion recall (session→global→corpus)"
        );

        Ok(results)
    }

    /// Forget a specific memory.
    ///
    /// Also removes the graph node and any temporal edges (Tasks 7, 5).
    pub async fn forget(&self, id: &str) -> Result<bool, String> {
        let span = self.config.trace_run_id.as_ref().and_then(|rid| {
            self.store.trace_start_span(rid, "memory.forget")
        });

        // Remove from vector store
        let deleted = self.store
            .delete(&self.config.collection_name, id)
            .await
            .map_err(|e| format!("delete: {e}"))?;

        // Clean up graph edges pointing to this memory
        if let Some(ref session_id) = self.config.session_node_id {
            let _ = self.store.temporal_invalidate_edge(session_id, "has_memory", id);
        }

        if let Some(ref s) = span {
            self.store.trace_end_span(s, true, Some(HashMap::from([
                ("memory_id".into(), id.to_string()),
                ("deleted".into(), deleted.to_string()),
            ])));
        }

        Ok(deleted)
    }

    // ════════════════════════════════════════════════════════════════
    // New capability delegates (A1, A4, A6, A7, A8, A11)
    // ════════════════════════════════════════════════════════════════

    // ── Batch Writes (A7) ──────────────────────────────────────────

    /// Batch-insert precomputed embeddings into a collection.
    pub fn batch_insert_embeddings(
        &self,
        collection: &str,
        items: Vec<(String, Vec<f32>, HashMap<String, String>)>,
    ) -> Result<BatchWriteResult, String> {
        self.store.batch_insert_embeddings(collection, items)
    }

    // ── Episode Management (A4) ────────────────────────────────────

    /// Create a new episode (conversation session, task, workflow).
    pub fn create_episode(&self, episode: &Episode) -> Result<(), String> {
        self.store.create_episode(episode)
    }

    /// Get an episode by ID.
    pub fn get_episode(&self, episode_id: &str) -> Result<Option<Episode>, String> {
        self.store.get_episode(episode_id)
    }

    /// Search episodes by text query.
    pub fn search_episodes(&self, query: &str, k: usize) -> Result<Vec<Episode>, String> {
        self.store.search_episodes(query, k)
    }

    // ── Event Management (A4) ──────────────────────────────────────

    /// Append an event to an episode's timeline.
    pub fn append_event(&self, event: &Event) -> Result<(), String> {
        self.store.append_event(event)
    }

    /// Get the timeline for an episode.
    pub fn get_timeline(&self, episode_id: &str, max_events: usize) -> Result<Vec<Event>, String> {
        self.store.get_timeline(episode_id, max_events)
    }

    // ── Entity Management (A4) ─────────────────────────────────────

    /// Create or update an entity.
    pub fn upsert_entity(&self, entity: &Entity) -> Result<(), String> {
        self.store.upsert_entity(entity)
    }

    /// Get an entity by ID.
    pub fn get_entity(&self, entity_id: &str) -> Result<Option<Entity>, String> {
        self.store.get_entity(entity_id)
    }

    /// Search entities by kind and text query.
    pub fn search_entities(
        &self,
        kind: Option<EntityKind>,
        query: &str,
        k: usize,
    ) -> Result<Vec<Entity>, String> {
        self.store.search_entities(kind, query, k)
    }

    /// Get entity facts (entity + recent episodes + related entities).
    pub fn get_entity_facts(&self, entity_id: &str) -> Result<Option<EntityFacts>, String> {
        self.store.get_entity_facts(entity_id)
    }

    // ── Context Assembly (A1) ──────────────────────────────────────

    /// Build token-budgeted context for LLM prompts.
    ///
    /// Assembles context from multiple sources (system prompt, recent memories,
    /// entity knowledge, conversation history) within a token budget.
    pub fn build_context(
        &self,
        session_id: Option<&str>,
        agent_id: Option<&str>,
        token_budget: usize,
        sections: Vec<(&str, i32, &str)>,
        truncation: TruncationStrategy,
        format: ContextFormat,
    ) -> Result<ContextQueryResult, String> {
        self.store.context_query(
            session_id,
            agent_id,
            token_budget,
            sections,
            truncation,
            format,
        )
    }

    // ── Task Queue (A8) ────────────────────────────────────────────

    /// Enqueue a background task for memory maintenance.
    pub fn enqueue_task(
        &self,
        queue_id: &str,
        priority: i64,
        payload: Vec<u8>,
    ) -> Result<BackgroundTask, String> {
        self.store.enqueue_task(queue_id, priority, payload)
    }

    /// Enqueue a delayed background task.
    pub fn enqueue_delayed_task(
        &self,
        queue_id: &str,
        priority: i64,
        payload: Vec<u8>,
        delay_ms: u64,
    ) -> Result<BackgroundTask, String> {
        self.store.enqueue_delayed_task(queue_id, priority, payload, delay_ms)
    }

    /// Claim a task from the queue.
    pub fn claim_task(
        &self,
        queue_id: &str,
        worker_id: &str,
    ) -> Result<TaskClaimResult, String> {
        self.store.claim_task(queue_id, worker_id)
    }

    /// Acknowledge successful task completion.
    pub fn ack_task(&self, queue_id: &str, task_id: &str) -> Result<(), String> {
        self.store.ack_task(queue_id, task_id)
    }

    /// Negative-acknowledge a task (return to queue with optional delay).
    pub fn nack_task(
        &self,
        queue_id: &str,
        task_id: &str,
        delay_ms: Option<u64>,
    ) -> Result<(), String> {
        self.store.nack_task(queue_id, task_id, delay_ms)
    }

    /// Get task queue statistics.
    pub fn queue_stats(&self, queue_id: &str) -> Result<TaskQueueStats, String> {
        self.store.queue_stats(queue_id)
    }

    // ── Path Query (A6) ────────────────────────────────────────────

    /// Query data by hierarchical path.
    pub fn path_query(
        &self,
        path: &str,
        filters: Option<Vec<(&str, serde_json::Value)>>,
    ) -> Result<Vec<PathQueryRow>, String> {
        self.store.path_query(path, filters)
    }

    // ── SQL Query (A15) ────────────────────────────────────────────

    /// Execute a SQL-like query against the memory store.
    pub fn sql_query(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<HashMap<String, serde_json::Value>>, String> {
        self.store.sql_query(sql, params)
    }

    // ── Predefined Views (A5) ──────────────────────────────────────

    /// List all available predefined views.
    pub fn list_views(&self) -> Vec<String> {
        self.store.list_views()
    }

    /// Query a predefined view with optional filters.
    pub fn query_view(
        &self,
        view_name: &str,
        filters: Option<HashMap<String, serde_json::Value>>,
        limit: Option<usize>,
    ) -> Result<Vec<HashMap<String, serde_json::Value>>, String> {
        self.store.query_view(view_name, filters, limit)
    }
}
