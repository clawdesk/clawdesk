//! Tiered embedding provider — guaranteed fallback to FTS-only mode.
//!
//! The tier chain ensures memory search always works regardless of external
//! service availability:
//!
//! ```text
//! CloudApi (OpenAI/Voyage/Cohere/HuggingFace) → Ollama local → FTS-only
//! ```
//!
//! ## Availability
//!
//! | Provider   | p(available) typical desktop |
//! |------------|------------------------------|
//! | OpenAI     | 0.05 (needs API key)         |
//! | Ollama     | 0.08 (needs install + pull)   |
//! | BM25/FTS   | 1.00 (pure computation)      |
//!
//! Combined: P(at least one works) = 1.0 (BM25 is absorbing).
//!
//! ## Circuit Breaker
//!
//! When the cloud/local provider fails, the tier auto-degrades to FTS-only.
//! A half-open probe re-checks the provider every 60 seconds. Successful
//! probe promotes back to full hybrid mode.

use crate::embedding::{BatchEmbeddingResult, EmbeddingProvider, EmbeddingResult};
use clawdesk_types::error::MemoryError;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// Which embedding tier is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingTier {
    /// Full vector+BM25 hybrid mode via a cloud or local provider.
    Full,
    /// FTS-only mode — BM25 keyword search, no vector embeddings.
    FtsOnly,
}

/// Configuration for the tiered provider.
#[derive(Debug, Clone)]
pub struct TieredConfig {
    /// How long to wait before re-probing a failed provider (seconds).
    pub probe_interval_secs: u64,
    /// Maximum consecutive failures before permanent degradation for this session.
    pub max_consecutive_failures: u32,
}

impl Default for TieredConfig {
    fn default() -> Self {
        Self {
            probe_interval_secs: 60,
            max_consecutive_failures: 10,
        }
    }
}

/// A tiered embedding provider that auto-degrades to FTS-only on failure.
///
/// Wraps any `EmbeddingProvider` with circuit-breaker logic:
/// - On embed failure → switches to `FtsOnly` tier
/// - After `probe_interval_secs` → re-attempts one embed (half-open probe)
/// - On probe success → promotes back to `Full` tier
pub struct TieredEmbeddingProvider {
    /// The underlying cloud/local provider (may fail).
    inner: Arc<dyn EmbeddingProvider>,
    /// Whether the provider is currently healthy.
    healthy: AtomicBool,
    /// Epoch-seconds of the last failure (for probe timing).
    last_failure_epoch: AtomicU64,
    /// Consecutive failure count.
    consecutive_failures: AtomicU64,
    /// Configuration.
    config: TieredConfig,
    /// Why the tier is degraded (for diagnostics).
    degradation_reason: std::sync::RwLock<Option<String>>,
}

impl TieredEmbeddingProvider {
    /// Create a new tiered provider wrapping the given inner provider.
    pub fn new(inner: Arc<dyn EmbeddingProvider>, config: TieredConfig) -> Self {
        Self {
            inner,
            healthy: AtomicBool::new(true),
            last_failure_epoch: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            config,
            degradation_reason: std::sync::RwLock::new(None),
        }
    }

    /// Get the current tier.
    pub fn current_tier(&self) -> EmbeddingTier {
        if self.healthy.load(Ordering::Relaxed) {
            EmbeddingTier::Full
        } else {
            EmbeddingTier::FtsOnly
        }
    }

    /// Get the degradation reason (if any).
    pub fn degradation_reason(&self) -> Option<String> {
        self.degradation_reason.read().ok().and_then(|r| r.clone())
    }

    /// Check if a half-open probe is due (enough time has passed since last failure).
    fn should_probe(&self) -> bool {
        if self.healthy.load(Ordering::Relaxed) {
            return false;
        }
        let last_fail = self.last_failure_epoch.load(Ordering::Relaxed);
        if last_fail == 0 {
            return false;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        now.saturating_sub(last_fail) >= self.config.probe_interval_secs
    }

    /// Record a failure and potentially degrade.
    fn record_failure(&self, err: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        self.last_failure_epoch.store(now, Ordering::Relaxed);
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;

        if failures >= self.config.max_consecutive_failures as u64 || failures == 1 {
            self.healthy.store(false, Ordering::Relaxed);
            if let Ok(mut reason) = self.degradation_reason.write() {
                *reason = Some(format!(
                    "Provider '{}' failed ({} consecutive): {}",
                    self.inner.name(),
                    failures,
                    err
                ));
            }
            warn!(
                provider = self.inner.name(),
                failures,
                "Embedding provider degraded → FTS-only mode"
            );
        }
    }

    /// Record a success and promote back to full mode.
    fn record_success(&self) {
        let was_unhealthy = !self.healthy.swap(true, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
        if was_unhealthy {
            if let Ok(mut reason) = self.degradation_reason.write() {
                *reason = None;
            }
            info!(
                provider = self.inner.name(),
                "Embedding provider recovered → Full hybrid mode"
            );
        }
    }

    /// Attempt an embed, with circuit-breaker logic.
    /// Returns `Ok(Some(result))` on success, `Ok(None)` if in FTS-only mode,
    /// or `Err` on a fresh failure (first time — caller can decide to retry).
    pub async fn try_embed(&self, text: &str) -> Result<Option<EmbeddingResult>, MemoryError> {
        // If healthy, try directly
        if self.healthy.load(Ordering::Relaxed) {
            match self.inner.embed(text).await {
                Ok(result) => {
                    self.record_success();
                    return Ok(Some(result));
                }
                Err(e) => {
                    self.record_failure(&e.to_string());
                    return Err(e);
                }
            }
        }

        // Unhealthy — check if we should probe
        if self.should_probe() {
            debug!(
                provider = self.inner.name(),
                "Half-open probe: re-checking embedding provider"
            );
            match self.inner.embed(text).await {
                Ok(result) => {
                    self.record_success();
                    return Ok(Some(result));
                }
                Err(e) => {
                    self.record_failure(&e.to_string());
                    debug!(
                        provider = self.inner.name(),
                        error = %e,
                        "Probe failed, staying in FTS-only mode"
                    );
                }
            }
        }

        // FTS-only — no embedding available
        Ok(None)
    }

    /// Attempt a batch embed, with circuit-breaker logic.
    pub async fn try_embed_batch(
        &self,
        texts: &[String],
    ) -> Result<Option<BatchEmbeddingResult>, MemoryError> {
        if self.healthy.load(Ordering::Relaxed) {
            match self.inner.embed_batch(texts).await {
                Ok(result) => {
                    self.record_success();
                    return Ok(Some(result));
                }
                Err(e) => {
                    self.record_failure(&e.to_string());
                    return Err(e);
                }
            }
        }

        if self.should_probe() && !texts.is_empty() {
            debug!(
                provider = self.inner.name(),
                "Half-open probe (batch): re-checking embedding provider"
            );
            // Probe with just the first text to minimize cost
            match self.inner.embed(&texts[0]).await {
                Ok(_) => {
                    self.record_success();
                    // Provider is back — do the full batch
                    match self.inner.embed_batch(texts).await {
                        Ok(result) => return Ok(Some(result)),
                        Err(e) => {
                            self.record_failure(&e.to_string());
                        }
                    }
                }
                Err(e) => {
                    self.record_failure(&e.to_string());
                    debug!(
                        provider = self.inner.name(),
                        error = %e,
                        "Batch probe failed, staying in FTS-only mode"
                    );
                }
            }
        }

        Ok(None)
    }
}

/// Implement `EmbeddingProvider` so TieredEmbeddingProvider can be used
/// as a drop-in replacement. When in FTS-only mode, embed calls return
/// a zero vector (which will produce ~0 cosine similarity, effectively
/// disabling vector search and letting BM25 carry the results).
#[async_trait]
impl EmbeddingProvider for TieredEmbeddingProvider {
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
        match self.try_embed(text).await {
            Ok(Some(result)) => Ok(result),
            Ok(None) => {
                // FTS-only mode: return a zero vector
                Ok(EmbeddingResult {
                    vector: vec![0.0; self.inner.dimensions()],
                    dimensions: self.inner.dimensions(),
                    model: format!("{}/fts-fallback", self.inner.name()),
                    tokens_used: 0,
                })
            }
            Err(e) => {
                // First failure — degrade and return zero vector
                warn!(
                    provider = self.inner.name(),
                    error = %e,
                    "Embedding failed, degrading to FTS-only"
                );
                Ok(EmbeddingResult {
                    vector: vec![0.0; self.inner.dimensions()],
                    dimensions: self.inner.dimensions(),
                    model: format!("{}/fts-fallback", self.inner.name()),
                    tokens_used: 0,
                })
            }
        }
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
        match self.try_embed_batch(texts).await {
            Ok(Some(result)) => Ok(result),
            Ok(None) | Err(_) => {
                // FTS-only — return zero vectors for all texts
                let dims = self.inner.dimensions();
                let embeddings = texts
                    .iter()
                    .map(|_| EmbeddingResult {
                        vector: vec![0.0; dims],
                        dimensions: dims,
                        model: format!("{}/fts-fallback", self.inner.name()),
                        tokens_used: 0,
                    })
                    .collect();
                Ok(BatchEmbeddingResult {
                    embeddings,
                    total_tokens: 0,
                })
            }
        }
    }
}

/// Build the best available embedding provider from environment.
///
/// Tier order:
/// 1. OpenAI (if `OPENAI_API_KEY` set)
/// 2. Voyage (if `VOYAGE_API_KEY` set)
/// 3. Cohere (if `COHERE_API_KEY` set)
/// 4. HuggingFace (if `HF_API_KEY` set)
/// 5. Ollama (localhost, always attempted)
///
/// Whichever provider is selected, it is wrapped in `TieredEmbeddingProvider`
/// for circuit-breaker degradation to FTS-only.
pub fn build_tiered_provider() -> Arc<TieredEmbeddingProvider> {
    use crate::embedding::*;

    let config = TieredConfig::default();

    // Try cloud providers first (in order of preference)
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        info!("Memory embedding tier: OpenAI text-embedding-3-small (with FTS fallback)");
        let inner = Arc::new(OpenAiEmbeddingProvider::new(key, None, None));
        return Arc::new(TieredEmbeddingProvider::new(inner, config));
    }

    if let Ok(key) = std::env::var("VOYAGE_API_KEY") {
        info!("Memory embedding tier: Voyage (with FTS fallback)");
        let inner = Arc::new(VoyageEmbeddingProvider::new(key, None, None));
        return Arc::new(TieredEmbeddingProvider::new(inner, config));
    }

    if let Ok(key) = std::env::var("COHERE_API_KEY") {
        info!("Memory embedding tier: Cohere (with FTS fallback)");
        let inner = Arc::new(CohereEmbeddingProvider::new(key, None, None));
        return Arc::new(TieredEmbeddingProvider::new(inner, config));
    }

    if let Ok(key) = std::env::var("HF_API_KEY") {
        info!("Memory embedding tier: HuggingFace (with FTS fallback)");
        let inner = Arc::new(HuggingFaceEmbeddingProvider::new(key, None));
        return Arc::new(TieredEmbeddingProvider::new(inner, config));
    }

    // Fall back to Ollama (may or may not be running)
    let base_url = std::env::var("OLLAMA_HOST").ok();
    info!(
        base_url = ?base_url,
        "Memory embedding tier: Ollama nomic-embed-text (with FTS fallback)"
    );
    let inner = Arc::new(OllamaEmbeddingProvider::new(None, base_url));
    Arc::new(TieredEmbeddingProvider::new(inner, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::MockEmbeddingProvider;

    #[tokio::test]
    async fn healthy_provider_returns_embeddings() {
        let mock = Arc::new(MockEmbeddingProvider::new(384));
        let tiered = TieredEmbeddingProvider::new(mock, TieredConfig::default());

        assert_eq!(tiered.current_tier(), EmbeddingTier::Full);
        let result = tiered.embed("test").await.unwrap();
        assert_eq!(result.dimensions, 384);
        assert_eq!(tiered.current_tier(), EmbeddingTier::Full);
    }

    #[test]
    fn degradation_reason_initially_none() {
        let mock = Arc::new(MockEmbeddingProvider::new(384));
        let tiered = TieredEmbeddingProvider::new(mock, TieredConfig::default());
        assert!(tiered.degradation_reason().is_none());
    }
}
