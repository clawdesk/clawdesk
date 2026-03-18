//! Persistent embedding cache backed by any key-value store.
//!
//! The existing `CachedEmbeddingProvider` in `embedding.rs` uses an
//! in-memory LRU cache that is lost on process restart. This module provides
//! a two-tier cache:
//!
//! 1. **L1 (hot)**: In-memory `HashMap` for sub-microsecond lookups
//! 2. **L2 (warm)**: SochDB key-value store for persistence across restarts
//!
//! ## Key Design
//!
//! Cache keys are `SHA-256(model_name + text)` — this ensures:
//! - Different embedding models don't collide
//! - Identical text always maps to the same key
//! - Keys are fixed-length (64 hex chars) regardless of input size
//!
//! ## Invalidation
//!
//! Entries include a generation counter. When the embedding model changes
//! (e.g., user switches from text-embedding-3-small to text-embedding-ada-002),
//! the generation is bumped and stale entries are lazily evicted on read.

use crate::chunker::sha256_hex;
use crate::embedding::{EmbeddingProvider, EmbeddingResult, BatchEmbeddingResult};
use async_trait::async_trait;
use clawdesk_types::error::MemoryError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::num::NonZeroUsize;
use tracing::{debug, info, warn};

/// A cached embedding entry stored in the persistent layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedEmbedding {
    /// The embedding vector.
    vector: Vec<f32>,
    /// Embedding model name at time of caching.
    model: String,
    /// Dimensionality of the embedding.
    dimensions: usize,
    /// Token cost of this embedding.
    tokens_used: u32,
    /// Generation counter for cache invalidation.
    generation: u64,
    /// Unix timestamp when this entry was cached.
    cached_at: i64,
}

/// Configuration for the persistent embedding cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentCacheConfig {
    /// Maximum entries in the L1 (in-memory) cache.
    pub l1_max_entries: usize,
    /// Current generation counter. Bump when the embedding model changes.
    pub generation: u64,
    /// Whether the persistent cache is enabled.
    pub enabled: bool,
}

impl Default for PersistentCacheConfig {
    fn default() -> Self {
        Self {
            l1_max_entries: 4096,
            generation: 1,
            enabled: true,
        }
    }
}

/// Trait for the persistent key-value backend.
///
/// This is intentionally minimal so it can be implemented by SochDB
/// or any other KV store without pulling in the full VectorStore trait.
#[async_trait]
pub trait EmbeddingCacheStore: Send + Sync + 'static {
    /// Get a value by key. Returns None if not found.
    async fn cache_get(&self, key: &str) -> Result<Option<Vec<u8>>, String>;
    /// Set a value by key.
    async fn cache_set(&self, key: &str, value: &[u8]) -> Result<(), String>;
    /// Delete a value by key.
    async fn cache_delete(&self, key: &str) -> Result<(), String>;
}

/// Two-tier (L1 memory + L2 persistent) embedding cache provider.
///
/// Wraps any `EmbeddingProvider` and any `EmbeddingCacheStore` to provide
/// persistent caching of embedding vectors.
pub struct PersistentCachedProvider {
    inner: Arc<dyn EmbeddingProvider>,
    store: Arc<dyn EmbeddingCacheStore>,
    /// L1 hot cache: LRU cache with O(1) access/insert/evict.
    /// Uses parking_lot::Mutex (not tokio) since all ops are CPU-bound.
    l1: parking_lot::Mutex<lru::LruCache<String, EmbeddingResult>>,
    config: PersistentCacheConfig,
}

/// LRU-ordered L1 cache — backed by the `lru` crate for O(1) access,
/// insert, and eviction with zero per-operation allocations.

impl PersistentCachedProvider {
    /// Create a new persistent cached provider.
    pub fn new(
        inner: Arc<dyn EmbeddingProvider>,
        store: Arc<dyn EmbeddingCacheStore>,
        config: PersistentCacheConfig,
    ) -> Self {
        let cap = NonZeroUsize::new(config.l1_max_entries.max(1)).unwrap();
        Self {
            inner,
            store,
            l1: parking_lot::Mutex::new(lru::LruCache::new(cap)),
            config,
        }
    }

    /// Compute the cache key for a given text.
    fn cache_key(&self, text: &str) -> String {
        let input = format!("{}:{}", self.inner.name(), text);
        let hash = sha256_hex(&input);
        format!("emb_cache/{}", hash)
    }

    /// Try to get an embedding from L1 cache. O(1) with no allocations.
    async fn l1_get(&self, key: &str) -> Option<EmbeddingResult> {
        let mut l1 = self.l1.lock();
        l1.get(key).cloned()
    }

    /// Insert into L1 cache. O(1) with automatic LRU eviction.
    async fn l1_put(&self, key: String, result: EmbeddingResult) {
        let mut l1 = self.l1.lock();
        l1.put(key, result);
    }

    /// Try to get an embedding from L2 (persistent) cache.
    async fn l2_get(&self, key: &str) -> Option<EmbeddingResult> {
        match self.store.cache_get(key).await {
            Ok(Some(bytes)) => {
                match serde_json::from_slice::<CachedEmbedding>(&bytes) {
                    Ok(cached) => {
                        // Check generation — stale entries are ignored.
                        if cached.generation != self.config.generation {
                            debug!(key, expected = self.config.generation, got = cached.generation, "stale cache entry");
                            // Lazily delete stale entry.
                            let _ = self.store.cache_delete(key).await;
                            return None;
                        }
                        Some(EmbeddingResult {
                            vector: cached.vector,
                            model: cached.model,
                            dimensions: cached.dimensions,
                            tokens_used: cached.tokens_used,
                        })
                    }
                    Err(e) => {
                        warn!(key, error = %e, "corrupt cache entry, removing");
                        let _ = self.store.cache_delete(key).await;
                        None
                    }
                }
            }
            Ok(None) => None,
            Err(e) => {
                warn!(error = %e, "L2 cache read failed");
                None
            }
        }
    }

    /// Write an embedding to L2 (persistent) cache.
    async fn l2_put(&self, key: &str, result: &EmbeddingResult) {
        let cached = CachedEmbedding {
            vector: result.vector.clone(),
            model: result.model.clone(),
            dimensions: result.dimensions,
            tokens_used: result.tokens_used,
            generation: self.config.generation,
            cached_at: chrono::Utc::now().timestamp(),
        };
        match serde_json::to_vec(&cached) {
            Ok(bytes) => {
                if let Err(e) = self.store.cache_set(key, &bytes).await {
                    warn!(key, error = %e, "L2 cache write failed");
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to serialize cache entry");
            }
        }
    }

    /// Look up an embedding in L1, then L2, promoting L2 hits to L1.
    async fn lookup(&self, text: &str) -> Option<EmbeddingResult> {
        let key = self.cache_key(text);

        // L1 check.
        if let Some(result) = self.l1_get(&key).await {
            return Some(result);
        }

        // L2 check.
        if let Some(result) = self.l2_get(&key).await {
            // Promote to L1.
            self.l1_put(key, result.clone()).await;
            return Some(result);
        }

        None
    }

    /// Store an embedding in both L1 and L2.
    async fn store(&self, text: &str, result: &EmbeddingResult) {
        let key = self.cache_key(text);
        tokio::join!(
            self.l1_put(key.clone(), result.clone()),
            self.l2_put(&key, result)
        );
    }

    // ── Unified cache layer API ────────────────────────────

    /// Check if an embedding is already cached for the given text.
    ///
    /// This allows external code (e.g., the semantic cache) to check for
    /// existing embeddings before making redundant API calls. Returns
    /// `None` if the embedding is not cached.
    pub async fn get_cached(&self, text: &str) -> Option<EmbeddingResult> {
        if !self.config.enabled {
            return None;
        }
        self.lookup(text).await
    }

    /// Import an externally-computed embedding into the cache.
    ///
    /// This is the inverse of `get_cached` — when some other component
    /// (e.g., the semantic cache or MemoryManager recall pipeline) has
    /// already computed an embedding, it can share it with the embedding
    /// cache to avoid 6KB/entry duplication.
    pub async fn import_embedding(&self, text: &str, embedding: Vec<f32>) {
        if !self.config.enabled {
            return;
        }
        let result = EmbeddingResult {
            vector: embedding,
            model: self.inner.name().to_string(),
            dimensions: self.inner.dimensions(),
            tokens_used: 0, // imported, not computed
        };
        self.store(text, &result).await;
    }

    /// Get the current L1 cache size (for metrics/observability).
    pub async fn l1_size(&self) -> usize {
        self.l1.lock().len()
    }
}

#[async_trait]
impl EmbeddingProvider for PersistentCachedProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    fn max_tokens(&self) -> usize {
        self.inner.max_tokens()
    }

    async fn embed(&self, text: &str) -> Result<EmbeddingResult, MemoryError> {
        if !self.config.enabled {
            return self.inner.embed(text).await;
        }

        if let Some(cached) = self.lookup(text).await {
            return Ok(cached);
        }

        let result = self.inner.embed(text).await?;
        self.store(text, &result).await;
        Ok(result)
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
        if !self.config.enabled {
            return self.inner.embed_batch(texts).await;
        }

        let mut results: Vec<(usize, EmbeddingResult)> = Vec::with_capacity(texts.len());
        let mut uncached: Vec<(usize, String)> = Vec::new();

        // Check cache for each text.
        for (i, text) in texts.iter().enumerate() {
            if let Some(cached) = self.lookup(text).await {
                results.push((i, cached));
            } else {
                uncached.push((i, text.clone()));
            }
        }

        if uncached.is_empty() {
            // All cached — reconstruct in order.
            results.sort_by_key(|(i, _)| *i);
            let total_tokens: u32 = results.iter().map(|(_, r)| r.tokens_used).sum();
            return Ok(BatchEmbeddingResult {
                embeddings: results.into_iter().map(|(_, r)| r).collect(),
                total_tokens,
            });
        }

        // Compute uncached embeddings.
        let uncached_texts: Vec<String> = uncached.iter().map(|(_, t)| t.clone()).collect();
        let batch = self.inner.embed_batch(&uncached_texts).await?;

        for ((idx, text), emb) in uncached.iter().zip(batch.embeddings.into_iter()) {
            self.store(text, &emb).await;
            results.push((*idx, emb));
        }

        results.sort_by_key(|(i, _)| *i);
        let total_tokens: u32 = results.iter().map(|(_, r)| r.tokens_used).sum();
        Ok(BatchEmbeddingResult {
            embeddings: results.into_iter().map(|(_, r)| r).collect(),
            total_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// In-memory store for testing.
    struct MemStore {
        data: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl MemStore {
        fn new() -> Self {
            Self { data: Mutex::new(HashMap::new()) }
        }
    }

    #[async_trait]
    impl EmbeddingCacheStore for MemStore {
        async fn cache_get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.data.lock().await.get(key).cloned())
        }
        async fn cache_set(&self, key: &str, value: &[u8]) -> Result<(), String> {
            self.data.lock().await.insert(key.to_string(), value.to_vec());
            Ok(())
        }
        async fn cache_delete(&self, key: &str) -> Result<(), String> {
            self.data.lock().await.remove(key);
            Ok(())
        }
    }

    /// Mock provider that counts calls.
    struct CountingProvider {
        call_count: AtomicUsize,
    }

    impl CountingProvider {
        fn new() -> Self {
            Self { call_count: AtomicUsize::new(0) }
        }
        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EmbeddingProvider for CountingProvider {
        fn name(&self) -> &str { "counting-mock" }
        fn dimensions(&self) -> usize { 3 }
        fn max_tokens(&self) -> usize { 8192 }

        async fn embed(&self, _text: &str) -> Result<EmbeddingResult, MemoryError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(EmbeddingResult {
                vector: vec![1.0, 2.0, 3.0],
                dimensions: 3,
                model: "counting-mock".into(),
                tokens_used: 5,
            })
        }

        async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
            self.call_count.fetch_add(texts.len(), Ordering::SeqCst);
            Ok(BatchEmbeddingResult {
                embeddings: texts.iter().map(|_| EmbeddingResult {
                    vector: vec![1.0, 2.0, 3.0],
                    dimensions: 3,
                    model: "counting-mock".into(),
                    tokens_used: 5,
                }).collect(),
                total_tokens: (texts.len() * 5) as u32,
            })
        }
    }

    #[tokio::test]
    async fn cache_hit_avoids_provider_call() {
        let provider = Arc::new(CountingProvider::new());
        let store = Arc::new(MemStore::new());
        let cached = PersistentCachedProvider::new(
            provider.clone(),
            store,
            PersistentCacheConfig::default(),
        );

        // First call — cache miss.
        let r1 = cached.embed("hello world").await.unwrap();
        assert_eq!(provider.calls(), 1);

        // Second call — cache hit (L1).
        let r2 = cached.embed("hello world").await.unwrap();
        assert_eq!(provider.calls(), 1); // No additional call.
        assert_eq!(r1.vector, r2.vector);
    }

    #[tokio::test]
    async fn l2_persistence_survives_l1_eviction() {
        let provider = Arc::new(CountingProvider::new());
        let store = Arc::new(MemStore::new());
        let cached = PersistentCachedProvider::new(
            provider.clone(),
            store.clone(),
            PersistentCacheConfig {
                l1_max_entries: 1, // Very small L1.
                ..Default::default()
            },
        );

        // Fill L1 with "a".
        cached.embed("a").await.unwrap();
        assert_eq!(provider.calls(), 1);

        // "b" evicts "a" from L1.
        cached.embed("b").await.unwrap();
        assert_eq!(provider.calls(), 2);

        // "a" should still be in L2 — no provider call.
        cached.embed("a").await.unwrap();
        assert_eq!(provider.calls(), 2);
    }

    #[tokio::test]
    async fn generation_mismatch_invalidates() {
        let provider = Arc::new(CountingProvider::new());
        let store = Arc::new(MemStore::new());

        // Gen 1: cache "hello".
        {
            let cached = PersistentCachedProvider::new(
                provider.clone(),
                store.clone(),
                PersistentCacheConfig { generation: 1, ..Default::default() },
            );
            cached.embed("hello").await.unwrap();
            assert_eq!(provider.calls(), 1);
        }

        // Gen 2: "hello" should not hit L2 cache.
        {
            let cached = PersistentCachedProvider::new(
                provider.clone(),
                store.clone(),
                PersistentCacheConfig { generation: 2, ..Default::default() },
            );
            cached.embed("hello").await.unwrap();
            assert_eq!(provider.calls(), 2); // Fresh call.
        }
    }

    #[tokio::test]
    async fn batch_uses_cache() {
        let provider = Arc::new(CountingProvider::new());
        let store = Arc::new(MemStore::new());
        let cached = PersistentCachedProvider::new(
            provider.clone(),
            store,
            PersistentCacheConfig::default(),
        );

        // Pre-cache "a".
        cached.embed("a").await.unwrap();
        assert_eq!(provider.calls(), 1);

        // Batch with "a" (cached) + "b" (uncached).
        let batch = cached.embed_batch(&["a".into(), "b".into()]).await.unwrap();
        assert_eq!(batch.embeddings.len(), 2);
        assert_eq!(provider.calls(), 2); // Only "b" needed a call.
    }
}
