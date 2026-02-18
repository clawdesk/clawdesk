//! Embedding provider trait, implementations, and caching.
//!
//! ## Cached embedding provider
//!
//! `CachedEmbeddingProvider` wraps any `EmbeddingProvider` with a bounded
//! LRU cache (default 1024 entries, 1-hour TTL). Identical text inputs return
//! a cached `EmbeddingResult` without making an API call.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use clawdesk_types::error::MemoryError;
use clawdesk_types::estimate_tokens;

/// A single embedding vector with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResult {
    /// The embedding vector.
    pub vector: Vec<f32>,
    /// Dimensionality of the embedding.
    pub dimensions: usize,
    /// Model used for embedding.
    pub model: String,
    /// Tokens consumed for this embedding.
    pub tokens_used: u32,
}

/// Batch embedding result.
#[derive(Debug, Clone)]
pub struct BatchEmbeddingResult {
    pub embeddings: Vec<EmbeddingResult>,
    pub total_tokens: u32,
}

/// Provider trait for generating text embeddings.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync + 'static {
    /// Get the name of this provider.
    fn name(&self) -> &str;

    /// Get the dimensionality of embeddings produced by this provider.
    fn dimensions(&self) -> usize;

    /// Maximum tokens per request.
    fn max_tokens(&self) -> usize;

    /// Embed a single text string.
    async fn embed(&self, text: &str) -> Result<EmbeddingResult, MemoryError>;

    /// Embed a batch of text strings.
    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError>;
}

/// OpenAI-compatible embedding provider.
pub struct OpenAiEmbeddingProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    dimensions: usize,
}

impl OpenAiEmbeddingProvider {
    /// Create with a dedicated reqwest client.
    pub fn new(api_key: String, model: Option<String>, base_url: Option<String>) -> Self {
        let client = reqwest::Client::new();
        Self::with_client(api_key, model, base_url, client)
    }

    /// Create with a shared reqwest client (avoids per-instance connection pools).
    pub fn with_client(
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
        client: reqwest::Client,
    ) -> Self {
        let model = model.unwrap_or_else(|| "text-embedding-3-small".to_string());
        let dimensions = match model.as_str() {
            "text-embedding-3-small" => 1536,
            "text-embedding-3-large" => 3072,
            "text-embedding-ada-002" => 1536,
            _ => 1536,
        };
        Self {
            client,
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            dimensions,
        }
    }
}

#[derive(Serialize)]
struct EmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
    usage: EmbedUsage,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
    #[allow(dead_code)]
    index: usize,
}

#[derive(Deserialize)]
struct EmbedUsage {
    total_tokens: u32,
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbeddingProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn max_tokens(&self) -> usize {
        8191
    }

    async fn embed(&self, text: &str) -> Result<EmbeddingResult, MemoryError> {
        let result = self.embed_batch(&[text.to_string()]).await?;
        result
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| MemoryError::EmbeddingFailed { detail: "Empty response".to_string() })
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
        let body = EmbedRequest {
            model: self.model.clone(),
            input: texts.to_vec(),
        };

        let resp = self
            .client
            .post(format!("{}/embeddings", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("HTTP error: {e}") })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(MemoryError::EmbeddingFailed {
                detail: format!("API error {status}: {body}"),
            });
        }

        let data: EmbedResponse = resp
            .json()
            .await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("Parse error: {e}") })?;

        let embeddings = data
            .data
            .into_iter()
            .map(|d| {
                let dimensions = d.embedding.len();
                EmbeddingResult {
                    vector: d.embedding,
                    dimensions,
                    model: self.model.clone(),
                    tokens_used: 0, // per-item not available
                }
            })
            .collect();

        Ok(BatchEmbeddingResult {
            embeddings,
            total_tokens: data.usage.total_tokens,
        })
    }
}

/// Local/mock embedding provider for testing (random vectors).
pub struct MockEmbeddingProvider {
    dims: usize,
}

impl MockEmbeddingProvider {
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }
}

// ---------------------------------------------------------------------------
// Cohere embedding provider
// ---------------------------------------------------------------------------

/// Cohere embedding provider (embed-english-v3.0 / embed-multilingual-v3.0).
pub struct CohereEmbeddingProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    dimensions: usize,
    input_type: String,
}

impl CohereEmbeddingProvider {
    pub fn new(api_key: String, model: Option<String>, input_type: Option<String>) -> Self {
        let model = model.unwrap_or_else(|| "embed-english-v3.0".to_string());
        let dimensions = match model.as_str() {
            "embed-english-v3.0" => 1024,
            "embed-multilingual-v3.0" => 1024,
            "embed-english-light-v3.0" => 384,
            "embed-multilingual-light-v3.0" => 384,
            _ => 1024,
        };
        Self {
            client: reqwest::Client::new(),
            api_key,
            model,
            base_url: "https://api.cohere.ai/v1".to_string(),
            dimensions,
            input_type: input_type.unwrap_or_else(|| "search_document".to_string()),
        }
    }
}

#[derive(Serialize)]
struct CohereEmbedRequest {
    texts: Vec<String>,
    model: String,
    input_type: String,
}

#[derive(Deserialize)]
struct CohereEmbedResponse {
    embeddings: Vec<Vec<f32>>,
    meta: Option<CohereEmbedMeta>,
}

#[derive(Deserialize)]
struct CohereEmbedMeta {
    billed_units: Option<CohereUnits>,
}

#[derive(Deserialize)]
struct CohereUnits {
    input_tokens: Option<u32>,
}

#[async_trait]
impl EmbeddingProvider for CohereEmbeddingProvider {
    fn name(&self) -> &str { "cohere" }
    fn dimensions(&self) -> usize { self.dimensions }
    fn max_tokens(&self) -> usize { 512 }

    async fn embed(&self, text: &str) -> Result<EmbeddingResult, MemoryError> {
        let result = self.embed_batch(&[text.to_string()]).await?;
        result.embeddings.into_iter().next()
            .ok_or_else(|| MemoryError::EmbeddingFailed { detail: "Empty response".to_string() })
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
        let body = CohereEmbedRequest {
            texts: texts.to_vec(),
            model: self.model.clone(),
            input_type: self.input_type.clone(),
        };

        let resp = self.client
            .post(format!("{}/embed", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("HTTP error: {e}") })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(MemoryError::EmbeddingFailed { detail: format!("Cohere API error {status}: {body}") });
        }

        let data: CohereEmbedResponse = resp.json().await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("Parse error: {e}") })?;

        let total_tokens = data.meta
            .and_then(|m| m.billed_units)
            .and_then(|u| u.input_tokens)
            .unwrap_or(0);

        let embeddings = data.embeddings.into_iter().map(|vec| {
            let dimensions = vec.len();
            EmbeddingResult { vector: vec, dimensions, model: self.model.clone(), tokens_used: 0 }
        }).collect();

        Ok(BatchEmbeddingResult { embeddings, total_tokens })
    }
}

// ---------------------------------------------------------------------------
// Voyage AI embedding provider
// ---------------------------------------------------------------------------

/// Voyage AI embedding provider (voyage-3, voyage-3-lite, voyage-code-3).
pub struct VoyageEmbeddingProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    dimensions: usize,
    input_type: Option<String>,
}

impl VoyageEmbeddingProvider {
    pub fn new(api_key: String, model: Option<String>, input_type: Option<String>) -> Self {
        let model = model.unwrap_or_else(|| "voyage-3".to_string());
        let dimensions = match model.as_str() {
            "voyage-3" => 1024,
            "voyage-3-lite" => 512,
            "voyage-code-3" => 1024,
            "voyage-large-2" => 1536,
            _ => 1024,
        };
        Self {
            client: reqwest::Client::new(),
            api_key,
            model,
            base_url: "https://api.voyageai.com/v1".to_string(),
            dimensions,
            input_type,
        }
    }
}

/// Voyage uses the same request/response format as OpenAI.
#[derive(Serialize)]
struct VoyageEmbedRequest {
    model: String,
    input: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_type: Option<String>,
}

#[async_trait]
impl EmbeddingProvider for VoyageEmbeddingProvider {
    fn name(&self) -> &str { "voyage" }
    fn dimensions(&self) -> usize { self.dimensions }
    fn max_tokens(&self) -> usize { 32000 }

    async fn embed(&self, text: &str) -> Result<EmbeddingResult, MemoryError> {
        let result = self.embed_batch(&[text.to_string()]).await?;
        result.embeddings.into_iter().next()
            .ok_or_else(|| MemoryError::EmbeddingFailed { detail: "Empty response".to_string() })
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
        let body = VoyageEmbedRequest {
            model: self.model.clone(),
            input: texts.to_vec(),
            input_type: self.input_type.clone(),
        };

        let resp = self.client
            .post(format!("{}/embeddings", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("HTTP error: {e}") })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(MemoryError::EmbeddingFailed { detail: format!("Voyage API error {status}: {body}") });
        }

        // Voyage uses the same response structure as OpenAI
        let data: EmbedResponse = resp.json().await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("Parse error: {e}") })?;

        let embeddings = data.data.into_iter().map(|d| {
            let dimensions = d.embedding.len();
            EmbeddingResult { vector: d.embedding, dimensions, model: self.model.clone(), tokens_used: 0 }
        }).collect();

        Ok(BatchEmbeddingResult { embeddings, total_tokens: data.usage.total_tokens })
    }
}

// ---------------------------------------------------------------------------
// Ollama embedding provider (local)
// ---------------------------------------------------------------------------

/// Ollama local embedding provider (nomic-embed-text, mxbai-embed-large, etc.).
pub struct OllamaEmbeddingProvider {
    client: reqwest::Client,
    model: String,
    base_url: String,
    dimensions: usize,
}

impl OllamaEmbeddingProvider {
    pub fn new(model: Option<String>, base_url: Option<String>) -> Self {
        let model = model.unwrap_or_else(|| "nomic-embed-text".to_string());
        let dimensions = match model.as_str() {
            "nomic-embed-text" => 768,
            "mxbai-embed-large" => 1024,
            "all-minilm" => 384,
            "snowflake-arctic-embed" => 1024,
            _ => 768,
        };
        Self {
            client: reqwest::Client::new(),
            model,
            base_url: base_url.unwrap_or_else(|| "http://localhost:11434".to_string()),
            dimensions,
        }
    }
}

#[derive(Serialize)]
struct OllamaEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl EmbeddingProvider for OllamaEmbeddingProvider {
    fn name(&self) -> &str { "ollama" }
    fn dimensions(&self) -> usize { self.dimensions }
    fn max_tokens(&self) -> usize { 8192 }

    async fn embed(&self, text: &str) -> Result<EmbeddingResult, MemoryError> {
        let result = self.embed_batch(&[text.to_string()]).await?;
        result.embeddings.into_iter().next()
            .ok_or_else(|| MemoryError::EmbeddingFailed { detail: "Empty response".to_string() })
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
        let body = OllamaEmbedRequest {
            model: self.model.clone(),
            input: texts.to_vec(),
        };

        let resp = self.client
            .post(format!("{}/api/embed", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("HTTP error: {e}") })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(MemoryError::EmbeddingFailed { detail: format!("Ollama API error {status}: {body}") });
        }

        let data: OllamaEmbedResponse = resp.json().await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("Parse error: {e}") })?;

        let embeddings = data.embeddings.into_iter().map(|vec| {
            let dimensions = vec.len();
            EmbeddingResult { vector: vec, dimensions, model: self.model.clone(), tokens_used: 0 }
        }).collect();

        Ok(BatchEmbeddingResult { embeddings, total_tokens: 0 })
    }
}

// ---------------------------------------------------------------------------
// HuggingFace Inference API embedding provider
// ---------------------------------------------------------------------------

/// HuggingFace Inference API embedding provider.
pub struct HuggingFaceEmbeddingProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    dimensions: usize,
}

impl HuggingFaceEmbeddingProvider {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        let model = model.unwrap_or_else(|| "sentence-transformers/all-MiniLM-L6-v2".to_string());
        let dimensions = match model.as_str() {
            "sentence-transformers/all-MiniLM-L6-v2" => 384,
            "sentence-transformers/all-mpnet-base-v2" => 768,
            "BAAI/bge-small-en-v1.5" => 384,
            "BAAI/bge-base-en-v1.5" => 768,
            "BAAI/bge-large-en-v1.5" => 1024,
            _ => 384,
        };
        Self {
            client: reqwest::Client::new(),
            api_key,
            model,
            base_url: "https://api-inference.huggingface.co".to_string(),
            dimensions,
        }
    }
}

#[derive(Serialize)]
struct HfEmbedRequest {
    inputs: Vec<String>,
}

#[async_trait]
impl EmbeddingProvider for HuggingFaceEmbeddingProvider {
    fn name(&self) -> &str { "huggingface" }
    fn dimensions(&self) -> usize { self.dimensions }
    fn max_tokens(&self) -> usize { 512 }

    async fn embed(&self, text: &str) -> Result<EmbeddingResult, MemoryError> {
        let result = self.embed_batch(&[text.to_string()]).await?;
        result.embeddings.into_iter().next()
            .ok_or_else(|| MemoryError::EmbeddingFailed { detail: "Empty response".to_string() })
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
        let body = HfEmbedRequest {
            inputs: texts.to_vec(),
        };

        let resp = self.client
            .post(format!("{}/pipeline/feature-extraction/{}", self.base_url, self.model))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("HTTP error: {e}") })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(MemoryError::EmbeddingFailed { detail: format!("HuggingFace API error {status}: {body}") });
        }

        // HuggingFace returns Vec<Vec<f32>> for feature-extraction pipeline
        let vectors: Vec<Vec<f32>> = resp.json().await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("Parse error: {e}") })?;

        let embeddings = vectors.into_iter().map(|vec| {
            let dimensions = vec.len();
            EmbeddingResult { vector: vec, dimensions, model: self.model.clone(), tokens_used: 0 }
        }).collect();

        Ok(BatchEmbeddingResult { embeddings, total_tokens: 0 })
    }
}

#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn max_tokens(&self) -> usize {
        8192
    }

    async fn embed(&self, text: &str) -> Result<EmbeddingResult, MemoryError> {
        // Deterministic hash-based embedding for reproducible tests.
        let mut vector = vec![0.0f32; self.dims];
        let bytes = text.as_bytes();
        for (i, v) in vector.iter_mut().enumerate() {
            let hash = bytes
                .iter()
                .enumerate()
                .fold(0u64, |acc, (j, &b)| {
                    acc.wrapping_add((b as u64).wrapping_mul((i + j + 1) as u64))
                });
            *v = ((hash % 2000) as f32 / 1000.0) - 1.0;
        }
        // Normalize to unit vector.
        let norm: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut vector {
                *v /= norm;
            }
        }

        Ok(EmbeddingResult {
            dimensions: self.dims,
            vector,
            model: "mock".to_string(),
            tokens_used: estimate_tokens(text) as u32,
        })
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
        let mut embeddings = Vec::with_capacity(texts.len());
        let mut total_tokens = 0;
        for text in texts {
            let result = self.embed(text).await?;
            total_tokens += result.tokens_used;
            embeddings.push(result);
        }
        Ok(BatchEmbeddingResult {
            embeddings,
            total_tokens,
        })
    }
}

// ---------------------------------------------------------------------------
// Cached embedding provider — LRU + TTL wrapper
// ---------------------------------------------------------------------------

/// Configuration for the embedding cache.
pub struct EmbeddingCacheConfig {
    /// Maximum number of cached entries.
    pub max_entries: usize,
    /// Time-to-live for cached entries.
    pub ttl: Duration,
}

impl Default for EmbeddingCacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 1024,
            ttl: Duration::from_secs(3600),
        }
    }
}

struct CacheEntry {
    result: EmbeddingResult,
    inserted: Instant,
}

/// An embedding provider wrapper that caches results by input text.
///
/// Uses a bounded LRU eviction policy with TTL expiry. Thread-safe via `Mutex`.
pub struct CachedEmbeddingProvider<P: EmbeddingProvider> {
    inner: P,
    cache: Mutex<EmbeddingCache>,
    ttl: Duration,
}

struct EmbeddingCache {
    entries: HashMap<String, CacheEntry>,
    order: VecDeque<String>,
    max_entries: usize,
}

impl EmbeddingCache {
    fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(max_entries),
            order: VecDeque::with_capacity(max_entries),
            max_entries,
        }
    }

    fn get(&mut self, key: &str, ttl: Duration) -> Option<EmbeddingResult> {
        if let Some(entry) = self.entries.get(key) {
            if entry.inserted.elapsed() < ttl {
                // Move to back (MRU).
                if let Some(pos) = self.order.iter().position(|k| k == key) {
                    self.order.remove(pos);
                }
                self.order.push_back(key.to_string());
                return Some(entry.result.clone());
            }
            // TTL expired — remove.
            self.entries.remove(key);
            if let Some(pos) = self.order.iter().position(|k| k == key) {
                self.order.remove(pos);
            }
        }
        None
    }

    fn insert(&mut self, key: String, result: EmbeddingResult) {
        // Evict LRU if at capacity.
        while self.entries.len() >= self.max_entries {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            CacheEntry {
                result,
                inserted: Instant::now(),
            },
        );
    }
}

impl<P: EmbeddingProvider> CachedEmbeddingProvider<P> {
    /// Wrap an embedding provider with default cache settings.
    pub fn new(inner: P) -> Self {
        Self::with_config(inner, EmbeddingCacheConfig::default())
    }

    /// Wrap with custom cache configuration.
    pub fn with_config(inner: P, config: EmbeddingCacheConfig) -> Self {
        Self {
            inner,
            cache: Mutex::new(EmbeddingCache::new(config.max_entries)),
            ttl: config.ttl,
        }
    }
}

#[async_trait]
impl<P: EmbeddingProvider> EmbeddingProvider for CachedEmbeddingProvider<P> {
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
        // Check cache.
        {
            let mut cache = self.cache.lock().await;
            if let Some(cached) = cache.get(text, self.ttl) {
                return Ok(cached);
            }
        }

        // Cache miss — call inner provider.
        let result = self.inner.embed(text).await?;

        // Store in cache.
        {
            let mut cache = self.cache.lock().await;
            cache.insert(text.to_string(), result.clone());
        }

        Ok(result)
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
        let mut results = Vec::with_capacity(texts.len());
        let mut uncached_texts = Vec::new();
        let mut uncached_indices = Vec::new();

        // Partition into cached vs uncached.
        {
            let mut cache = self.cache.lock().await;
            for (i, text) in texts.iter().enumerate() {
                if let Some(cached) = cache.get(text, self.ttl) {
                    results.push((i, cached));
                } else {
                    uncached_texts.push(text.clone());
                    uncached_indices.push(i);
                }
            }
        }

        // Fetch uncached embeddings.
        let mut total_tokens = 0;
        if !uncached_texts.is_empty() {
            let batch_result = self.inner.embed_batch(&uncached_texts).await?;
            total_tokens = batch_result.total_tokens;

            let mut cache = self.cache.lock().await;
            for (idx, embedding) in uncached_indices
                .into_iter()
                .zip(batch_result.embeddings.into_iter())
            {
                cache.insert(texts[idx].clone(), embedding.clone());
                results.push((idx, embedding));
            }
        }

        // Add tokens from cached results.
        for (_, r) in &results {
            total_tokens += r.tokens_used;
        }

        // Sort by original index.
        results.sort_by_key(|(i, _)| *i);
        let embeddings = results.into_iter().map(|(_, r)| r).collect();

        Ok(BatchEmbeddingResult {
            embeddings,
            total_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_provider() {
        let provider = MockEmbeddingProvider::new(128);
        let result = provider.embed("hello world").await.unwrap();
        assert_eq!(result.dimensions, 128);
        assert_eq!(result.vector.len(), 128);

        // Verify unit norm.
        let norm: f32 = result.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_mock_batch() {
        let provider = MockEmbeddingProvider::new(64);
        let texts = vec!["hello".to_string(), "world".to_string()];
        let result = provider.embed_batch(&texts).await.unwrap();
        assert_eq!(result.embeddings.len(), 2);
    }

    #[tokio::test]
    async fn test_deterministic_embedding() {
        let provider = MockEmbeddingProvider::new(32);
        let r1 = provider.embed("test").await.unwrap();
        let r2 = provider.embed("test").await.unwrap();
        assert_eq!(r1.vector, r2.vector);
    }

    #[tokio::test]
    async fn test_cached_provider_dedup() {
        let inner = MockEmbeddingProvider::new(32);
        let cached = CachedEmbeddingProvider::new(inner);

        // First call populates cache.
        let r1 = cached.embed("hello").await.unwrap();
        // Second call should return cached result (same vector).
        let r2 = cached.embed("hello").await.unwrap();
        assert_eq!(r1.vector, r2.vector);

        // Different text should produce different vector.
        let r3 = cached.embed("world").await.unwrap();
        assert_ne!(r1.vector, r3.vector);
    }

    #[tokio::test]
    async fn test_cached_batch_partial() {
        let inner = MockEmbeddingProvider::new(16);
        let cached = CachedEmbeddingProvider::new(inner);

        // Warm up "hello" in the cache.
        let _ = cached.embed("hello").await.unwrap();

        // Batch with one cached + one uncached.
        let texts = vec!["hello".to_string(), "goodbye".to_string()];
        let batch = cached.embed_batch(&texts).await.unwrap();
        assert_eq!(batch.embeddings.len(), 2);
    }
}
