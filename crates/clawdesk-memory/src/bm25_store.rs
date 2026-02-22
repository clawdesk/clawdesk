//! Persistent BM25 index backed by any `VectorStore`.
//!
//! The in-memory `Bm25Index` in `bm25.rs` is correct but ephemeral —
//! it loses all documents on process restart and is never wired into the search
//! pipeline. This module wraps it with lazy hydration from the vector store and
//! automatic index maintenance on writes.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────┐   hydrate_from_store()   ┌──────────────┐
//! │  SochDB      │ ──────────────────────▶   │  Bm25Index   │
//! │  (vector     │                           │  (in-memory  │
//! │   store)     │ ◀──────────────────────── │   inverted   │
//! └──────────────┘   add_document() writes   │   index)     │
//!                    to both store & index    └──────────────┘
//! ```
//!
//! On first `search()` call, the index is lazily hydrated by scanning all
//! documents in the target collection. Subsequent `add_document()` calls
//! update both the in-memory index and the vector store, keeping them in sync.

use crate::bm25::{Bm25Index, Bm25Params, Bm25Result};
use clawdesk_storage::vector_store::VectorStore;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// A persistent BM25 index that lazily hydrates from a `VectorStore`.
///
/// Thread-safe: multiple readers can search concurrently, writers acquire
/// exclusive access. The `RwLock` is tokio-native so it's safe to hold
/// across `.await` points.
pub struct PersistentBm25 {
    index: RwLock<Bm25Index>,
    store: Arc<dyn VectorStore>,
    collection: String,
    hydrated: RwLock<bool>,
}

impl PersistentBm25 {
    /// Create a new persistent BM25 index targeting the given collection.
    pub fn new(store: Arc<dyn VectorStore>, collection: String) -> Self {
        Self {
            index: RwLock::new(Bm25Index::new()),
            store,
            collection,
            hydrated: RwLock::new(false),
        }
    }

    /// Create with custom BM25 parameters.
    pub fn with_params(store: Arc<dyn VectorStore>, collection: String, params: Bm25Params) -> Self {
        Self {
            index: RwLock::new(Bm25Index::with_params(params)),
            store,
            collection,
            hydrated: RwLock::new(false),
        }
    }

    /// Ensure the in-memory index is hydrated from the vector store.
    ///
    /// This is a no-op if already hydrated. On first call, scans all documents
    /// in the collection and adds them to the in-memory BM25 index.
    pub async fn ensure_hydrated(&self) -> Result<(), String> {
        // Fast path: already hydrated.
        {
            let h = self.hydrated.read().await;
            if *h {
                return Ok(());
            }
        }

        // Slow path: acquire write lock and hydrate.
        let mut h = self.hydrated.write().await;
        if *h {
            return Ok(()); // Another task hydrated while we waited.
        }

        let start = std::time::Instant::now();

        // Scan all documents using a zero-vector search with high k.
        // This is a brute-force approach but only runs once on startup.
        // We use a 1-dimensional zero vector since we only care about
        // retrieving content for BM25 indexing, not similarity scores.
        //
        // Alternative: directly scan the SochDB KV prefix, but that
        // would require depending on SochDB internals. Using the
        // VectorStore trait keeps this module store-agnostic.
        let results = self
            .store
            .search(&self.collection, &[0.0; 1], 100_000, None)
            .await
            .map_err(|e| format!("BM25 hydration scan failed: {e}"))?;

        let mut idx = self.index.write().await;
        let mut count = 0usize;

        for result in &results {
            if let Some(ref content) = result.content {
                if !content.is_empty() {
                    idx.add_document(&result.id, content);
                    count += 1;
                }
            }
        }

        let elapsed = start.elapsed();
        info!(
            collection = %self.collection,
            documents = count,
            vocab_size = idx.vocabulary_size(),
            elapsed_ms = elapsed.as_millis(),
            "BM25 index hydrated from vector store"
        );

        *h = true;
        Ok(())
    }

    /// Add a document to both the in-memory index and (implicitly) the store.
    ///
    /// The caller is responsible for inserting the document into the vector
    /// store separately (via `MemoryManager::remember()`). This method only
    /// updates the in-memory BM25 index.
    pub async fn index_document(&self, id: &str, content: &str) {
        let mut idx = self.index.write().await;
        idx.add_document(id, content);
        debug!(id, len = content.len(), "BM25 document indexed");
    }

    /// Search the BM25 index. Hydrates lazily on first call.
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<Bm25Result>, String> {
        self.ensure_hydrated().await?;
        let idx = self.index.read().await;
        Ok(idx.search(query, top_k))
    }

    /// Number of indexed documents.
    pub async fn len(&self) -> usize {
        let idx = self.index.read().await;
        idx.len()
    }

    /// Whether the index is empty.
    pub async fn is_empty(&self) -> bool {
        let idx = self.index.read().await;
        idx.is_empty()
    }

    /// Force a full re-hydration from the vector store.
    ///
    /// Useful when the store has been modified externally (e.g., bulk import).
    pub async fn rebuild(&self) -> Result<(), String> {
        {
            let mut idx = self.index.write().await;
            idx.clear();
        }
        {
            let mut h = self.hydrated.write().await;
            *h = false;
        }
        self.ensure_hydrated().await
    }

    /// Index statistics.
    pub async fn stats(&self) -> Bm25Stats {
        let idx = self.index.read().await;
        let h = self.hydrated.read().await;
        Bm25Stats {
            document_count: idx.len(),
            vocabulary_size: idx.vocabulary_size(),
            hydrated: *h,
        }
    }
}

/// Statistics about the BM25 index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Bm25Stats {
    pub document_count: usize,
    pub vocabulary_size: usize,
    pub hydrated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use clawdesk_storage::vector_store::{CollectionConfig, VectorSearchResult};
    use clawdesk_types::error::StorageError;

    /// Mock VectorStore that returns canned results for hydration.
    struct MockStore {
        docs: Vec<VectorSearchResult>,
    }

    #[async_trait]
    impl VectorStore for MockStore {
        async fn create_collection(&self, _config: CollectionConfig) -> Result<(), StorageError> {
            Ok(())
        }

        async fn insert(
            &self,
            _collection: &str,
            _id: &str,
            _embedding: &[f32],
            _metadata: Option<serde_json::Value>,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn search(
            &self,
            _collection: &str,
            _query_embedding: &[f32],
            _k: usize,
            _min_score: Option<f32>,
        ) -> Result<Vec<VectorSearchResult>, StorageError> {
            Ok(self.docs.clone())
        }

        async fn hybrid_search(
            &self,
            _collection: &str,
            _query_embedding: &[f32],
            _query_text: &str,
            _k: usize,
            _vector_weight: f32,
        ) -> Result<Vec<VectorSearchResult>, StorageError> {
            Ok(self.docs.clone())
        }

        async fn delete(&self, _collection: &str, _id: &str) -> Result<bool, StorageError> {
            Ok(true)
        }
    }

    fn mock_store(docs: Vec<(&str, &str)>) -> Arc<MockStore> {
        Arc::new(MockStore {
            docs: docs
                .into_iter()
                .map(|(id, content)| VectorSearchResult {
                    id: id.to_string(),
                    score: 1.0,
                    metadata: serde_json::json!({}),
                    content: Some(content.to_string()),
                })
                .collect(),
        })
    }

    #[tokio::test]
    async fn hydrates_from_store() {
        let store = mock_store(vec![
            ("1", "the quick brown fox"),
            ("2", "hello world from rust"),
        ]);
        let bm25 = PersistentBm25::new(store.clone(), "test".into());

        assert!(bm25.is_empty().await);

        let results = bm25.search("quick fox", 10).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "1");

        let stats = bm25.stats().await;
        assert_eq!(stats.document_count, 2);
        assert!(stats.hydrated);
    }

    #[tokio::test]
    async fn index_document_adds_to_memory() {
        let store = mock_store(vec![]);
        let bm25 = PersistentBm25::new(store.clone(), "test".into());

        // Force hydration (empty store).
        bm25.ensure_hydrated().await.unwrap();

        // Add a document.
        bm25.index_document("x", "rust programming language").await;

        let results = bm25.search("rust", 5).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "x");
    }

    #[tokio::test]
    async fn rebuild_clears_and_rehydrates() {
        let store = mock_store(vec![("1", "hello world")]);
        let bm25 = PersistentBm25::new(store.clone(), "test".into());

        bm25.ensure_hydrated().await.unwrap();
        assert_eq!(bm25.len().await, 1);

        // Add an extra doc in-memory.
        bm25.index_document("2", "extra doc").await;
        assert_eq!(bm25.len().await, 2);

        // Rebuild should lose the in-memory-only doc.
        bm25.rebuild().await.unwrap();
        assert_eq!(bm25.len().await, 1);
    }
}
