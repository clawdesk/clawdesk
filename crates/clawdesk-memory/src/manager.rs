//! Memory manager — coordinates embedding, storage, and retrieval.

use crate::embedding::EmbeddingProvider;
use crate::hybrid::{HybridSearcher, SearchStrategy};
use crate::pipeline::BatchPipeline;
use clawdesk_storage::vector_store::{CollectionConfig, DistanceMetric, VectorSearchResult, VectorStore};
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
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            collection_name: "memories".to_string(),
            search_strategy: SearchStrategy::Hybrid,
            auto_embed: true,
            max_results: 10,
            min_relevance: 0.3,
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
    pub async fn remember(
        &self,
        content: &str,
        source: MemorySource,
        metadata: serde_json::Value,
    ) -> Result<String, String> {
        self.ensure_collection().await?;

        let embed_result = self
            .embedding
            .embed(content)
            .await
            .map_err(|e| format!("embed: {e}"))?;

        let id = uuid::Uuid::new_v4().to_string();
        let mut enriched_meta = metadata;
        enriched_meta["source"] = serde_json::json!(format!("{:?}", source));
        enriched_meta["content"] = serde_json::json!(content);
        enriched_meta["timestamp"] = serde_json::json!(chrono::Utc::now().to_rfc3339());

        self.store
            .insert(&self.config.collection_name, &id, &embed_result.vector, Some(enriched_meta))
            .await
            .map_err(|e| format!("insert: {e}"))?;

        debug!(id = %id, len = content.len(), "memory stored");
        Ok(id)
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

        let top_k = max_results.unwrap_or(self.config.max_results);

        let results = self
            .searcher
            .search(
                &self.config.collection_name,
                &query_result.vector,
                query,
                top_k,
                self.config.search_strategy,
            )
            .await?;

        // Filter by minimum relevance.
        let filtered: Vec<_> = results
            .into_iter()
            .filter(|r| r.score >= self.config.min_relevance)
            .collect();

        debug!(query = %query, results = filtered.len(), "memory recall");
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
