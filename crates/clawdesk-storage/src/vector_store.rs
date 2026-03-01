//! Vector similarity search for memory and RAG.

use async_trait::async_trait;
use clawdesk_types::error::StorageError;
use serde::{Deserialize, Serialize};

/// Distance metric for vector similarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistanceMetric {
    Cosine,
    Euclidean,
    DotProduct,
}

/// Configuration for a vector collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionConfig {
    pub name: String,
    pub dimension: usize,
    pub metric: DistanceMetric,
    pub enable_hybrid_search: bool,
    pub content_field: String,
}

/// A vector search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorSearchResult {
    pub id: String,
    pub score: f32,
    pub metadata: serde_json::Value,
    pub content: Option<String>,
}

/// Port: vector similarity search.
#[async_trait]
pub trait VectorStore: Send + Sync + 'static {
    /// Create or open a vector collection.
    async fn create_collection(
        &self,
        config: CollectionConfig,
    ) -> Result<(), StorageError>;

    /// Insert a vector with metadata.
    async fn insert(
        &self,
        collection: &str,
        id: &str,
        embedding: &[f32],
        metadata: Option<serde_json::Value>,
    ) -> Result<(), StorageError>;

    /// Search for nearest neighbors.
    async fn search(
        &self,
        collection: &str,
        query: &[f32],
        k: usize,
        min_score: Option<f32>,
    ) -> Result<Vec<VectorSearchResult>, StorageError>;

    /// Hybrid search combining vector similarity and keyword (BM25).
    async fn hybrid_search(
        &self,
        collection: &str,
        query_embedding: &[f32],
        query_text: &str,
        k: usize,
        vector_weight: f32,
    ) -> Result<Vec<VectorSearchResult>, StorageError>;

    /// Pure keyword/BM25 search — no embedding vector required.
    ///
    /// This is the FTS-only fallback when all embedding providers are
    /// degraded. Default implementation returns an empty vec (no FTS
    /// support); SochDB's `SochVectorStore` overrides with real BM25.
    async fn keyword_search(
        &self,
        collection: &str,
        query_text: &str,
        k: usize,
    ) -> Result<Vec<VectorSearchResult>, StorageError> {
        let _ = (collection, query_text, k);
        Ok(Vec::new())
    }

    /// Delete a vector by ID.
    async fn delete(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<bool, StorageError>;
}
