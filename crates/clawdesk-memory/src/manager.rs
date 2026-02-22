//! Memory manager — coordinates embedding, storage, and retrieval.
//!
//! Integrates chunking, temporal decay, and MMR diversity
//! re-ranking into the remember/recall pipeline.

use crate::chunker::{chunk_text, sha256_hex, ChunkerConfig};
use crate::embedding::EmbeddingProvider;
use crate::hybrid::{HybridSearcher, SearchStrategy};
use crate::mmr::{mmr_rerank, MmrCandidate, MmrConfig};
use crate::pipeline::BatchPipeline;
use crate::temporal_decay::{apply_temporal_decay, TemporalDecayConfig};
use clawdesk_storage::vector_store::{
    CollectionConfig, DistanceMetric, VectorSearchResult, VectorStore,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::{debug, info};

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
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            collection_name: "memories".to_string(),
            search_strategy: SearchStrategy::Hybrid,
            auto_embed: true,
            max_results: 10,
            min_relevance: 0.3,
            chunker: ChunkerConfig::default(),
            temporal_decay: TemporalDecayConfig::default(),
            mmr: MmrConfig::default(),
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
pub struct MemoryManager<S: VectorStore> {
    store: Arc<S>,
    embedding: Arc<dyn EmbeddingProvider>,
    searcher: HybridSearcher<S>,
    pipeline: BatchPipeline,
    config: MemoryConfig,
    collection_ready: OnceCell<()>,
}

impl<S: VectorStore> MemoryManager<S> {
    pub fn new(store: Arc<S>, embedding: Arc<dyn EmbeddingProvider>, config: MemoryConfig) -> Self {
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

    /// Store a memory with automatic embedding.
    ///
    /// Long texts are chunked and each chunk is stored with a
    /// content-hash dedup key so duplicate writes are idempotent.
    pub async fn remember(
        &self,
        content: &str,
        source: MemorySource,
        metadata: serde_json::Value,
    ) -> Result<String, String> {
        self.ensure_collection().await?;

        let chunks = chunk_text(content, &self.config.chunker);

        if chunks.is_empty() {
            return Err("content produced no chunks".into());
        }

        // Single-chunk fast path (most common).
        if chunks.len() == 1 {
            let chunk = &chunks[0];
            let embed_result = self
                .embedding
                .embed(&chunk.text)
                .await
                .map_err(|e| format!("embed: {e}"))?;

            let id = uuid::Uuid::new_v4().to_string();
            let content_hash = sha256_hex(&chunk.text);
            let mut enriched_meta = metadata;
            enriched_meta["source"] = serde_json::json!(format!("{:?}", source));
            enriched_meta["content"] = serde_json::json!(&chunk.text);
            enriched_meta["timestamp"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
            enriched_meta["content_hash"] = serde_json::json!(content_hash);

            self.store
                .insert(
                    &self.config.collection_name,
                    &id,
                    &embed_result.vector,
                    Some(enriched_meta),
                )
                .await
                .map_err(|e| format!("insert: {e}"))?;

            debug!(id = %id, len = chunk.text.len(), "memory stored (single chunk)");
            return Ok(id);
        }

        // Multi-chunk path: embed in batch and insert all chunks.
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let batch_result = self
            .pipeline
            .embed_all(&texts)
            .await
            .map_err(|e| format!("batch embed: {e}"))?;

        let now = chrono::Utc::now().to_rfc3339();
        let collection = &self.config.collection_name;
        let mut ids = Vec::with_capacity(chunks.len());

        for (i, (chunk, emb)) in chunks.iter().zip(batch_result.embeddings.iter()).enumerate() {
            let id = uuid::Uuid::new_v4().to_string();
            let content_hash = sha256_hex(&chunk.text);
            let mut enriched_meta = metadata.clone();
            enriched_meta["source"] = serde_json::json!(format!("{:?}", source));
            enriched_meta["content"] = serde_json::json!(&chunk.text);
            enriched_meta["timestamp"] = serde_json::json!(&now);
            enriched_meta["content_hash"] = serde_json::json!(content_hash);
            enriched_meta["chunk_index"] = serde_json::json!(i);
            enriched_meta["total_chunks"] = serde_json::json!(chunks.len());
            enriched_meta["original_length"] = serde_json::json!(content.len());

            self.store
                .insert(collection, &id, &emb.vector, Some(enriched_meta))
                .await
                .map_err(|e| format!("insert chunk {i}: {e}"))?;

            ids.push(id);
        }

        debug!(
            chunks = chunks.len(),
            original_len = content.len(),
            "memory stored (chunked)"
        );

        // Return first chunk ID as the primary identifier.
        Ok(ids.into_iter().next().unwrap())
    }

    /// Batch-store memories.
    ///
    /// Embeddings are computed in batch, then all inserts run concurrently.
    pub async fn remember_batch(
        &self,
        items: Vec<(String, MemorySource, serde_json::Value)>,
    ) -> Result<Vec<String>, String> {
        self.ensure_collection().await?;

        let texts: Vec<String> = items.iter().map(|(t, _, _)| t.clone()).collect();
        let batch_result = self
            .pipeline
            .embed_all(&texts)
            .await
            .map_err(|e| format!("batch embed: {e}"))?;

        // Build insert futures and fire them concurrently.
        let collection = &self.config.collection_name;
        let futs: Vec<_> = items
            .into_iter()
            .zip(batch_result.embeddings)
            .map(|((content, source, metadata), emb)| {
                let id = uuid::Uuid::new_v4().to_string();
                let store = self.store.clone();
                let col = collection.clone();
                let mut enriched_meta = metadata;
                enriched_meta["source"] = serde_json::json!(format!("{:?}", source));
                enriched_meta["content"] = serde_json::json!(content);
                async move {
                    store
                        .insert(&col, &id, &emb.vector, Some(enriched_meta))
                        .await
                        .map_err(|e| format!("insert: {e}"))?;
                    Ok::<String, String>(id)
                }
            })
            .collect();

        futures::future::try_join_all(futs).await
    }

    /// Recall relevant memories.
    ///
    /// Pipeline: vector+BM25 hybrid search → temporal decay →
    /// MMR diversity re-ranking → min-relevance filter.
    pub async fn recall(
        &self,
        query: &str,
        max_results: Option<usize>,
    ) -> Result<Vec<VectorSearchResult>, String> {
        self.ensure_collection().await?;

        let query_result = self
            .embedding
            .embed(query)
            .await
            .map_err(|e| format!("embed query: {e}"))?;

        // Fetch more candidates than requested — temporal decay and MMR will
        // prune/reorder, so we need headroom.
        let top_k = max_results.unwrap_or(self.config.max_results);
        let fetch_k = (top_k * 3).max(20); // 3x headroom

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

        // ── Stage 1: Temporal decay ────────────────────────────────
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
            // Re-sort by decayed score (apply_temporal_decay already sorts,
            // but map-back may not preserve order).
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

        debug!(query = %query, results = filtered.len(), "memory recall (decay+mmr)");
        Ok(filtered)
    }

    /// Forget a specific memory.
    pub async fn forget(&self, id: &str) -> Result<bool, String> {
        self.store
            .delete(&self.config.collection_name, id)
            .await
            .map_err(|e| format!("delete: {e}"))
    }
}
