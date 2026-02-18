//! Multi-provider media understanding with format negotiation and failover.
//!
//! Orchestrates media analysis across multiple provider backends (Anthropic,
//! OpenAI, Deepgram, Google, etc.) with:
//! - Capability-based provider matching
//! - Concurrent processing under bounded parallelism
//! - Provider-specific format negotiation
//! - Exponential backoff with jitter on failure
//! - Graceful degradation when providers are unavailable

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Media content types and capabilities
// ---------------------------------------------------------------------------

/// What kind of media understanding a provider supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaCapability {
    /// Image recognition / vision (OCR, scene understanding, object detection).
    ImageVision,
    /// Audio transcription (speech-to-text).
    AudioTranscription,
    /// Video analysis (frame extraction + vision + temporal understanding).
    VideoAnalysis,
    /// Document parsing (PDF, DOCX text extraction with layout).
    DocumentParsing,
    /// Audio generation (text-to-speech).
    AudioGeneration,
    /// Image generation (text-to-image, inpainting).
    ImageGeneration,
}

/// A request for media understanding.
#[derive(Debug, Clone)]
pub struct UnderstandingRequest {
    /// Raw media bytes.
    pub data: Vec<u8>,
    /// MIME type (e.g., "image/png", "audio/webm").
    pub mime_type: String,
    /// Original filename if available.
    pub filename: Option<String>,
    /// The capability needed.
    pub capability: MediaCapability,
    /// Optional prompt / instruction for the provider.
    pub prompt: Option<String>,
    /// Maximum duration to wait for a result.
    pub timeout: Duration,
}

/// Result of media understanding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnderstandingResult {
    /// The provider that produced this result.
    pub provider: String,
    /// Extracted text (transcription, OCR, description).
    pub text: String,
    /// Confidence score (0.0 – 1.0), if the provider reports one.
    pub confidence: Option<f64>,
    /// Processing duration.
    pub duration_ms: u64,
    /// Provider-specific metadata (e.g., word-level timestamps).
    pub metadata: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

/// A backend provider for media understanding.
#[async_trait]
pub trait UnderstandingProvider: Send + Sync {
    /// Provider name (e.g., "openai", "anthropic", "deepgram").
    fn name(&self) -> &str;

    /// Which capabilities this provider supports.
    fn capabilities(&self) -> Vec<MediaCapability>;

    /// Which MIME types this provider accepts for a given capability.
    fn supported_formats(&self, capability: MediaCapability) -> Vec<String>;

    /// Maximum input size in bytes (provider-imposed).
    fn max_input_bytes(&self) -> usize;

    /// Process a media understanding request.
    async fn process(&self, request: &UnderstandingRequest) -> Result<UnderstandingResult, UnderstandingError>;
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors from the understanding pipeline.
#[derive(Debug, thiserror::Error)]
pub enum UnderstandingError {
    #[error("no provider available for capability {0:?}")]
    NoProvider(MediaCapability),

    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("input too large: {size} bytes (max {max})")]
    InputTooLarge { size: usize, max: usize },

    #[error("all providers failed: {0}")]
    AllProvidersFailed(String),

    #[error("timeout after {0:?}")]
    Timeout(Duration),

    #[error("provider error ({provider}): {message}")]
    Provider { provider: String, message: String },

    #[error("rate limited by {provider}, retry after {retry_after_ms}ms")]
    RateLimited { provider: String, retry_after_ms: u64 },
}

// ---------------------------------------------------------------------------
// Provider health tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ProviderHealth {
    /// Exponential moving average of latency (ms).
    latency_ema_ms: f64,
    /// Number of consecutive failures.
    consecutive_failures: u32,
    /// When the circuit breaker opens (provider is temporarily disabled).
    circuit_open_until: Option<Instant>,
    /// Total requests.
    total_requests: u64,
    /// Successful requests.
    successes: u64,
}

impl Default for ProviderHealth {
    fn default() -> Self {
        Self {
            latency_ema_ms: 100.0,
            consecutive_failures: 0,
            circuit_open_until: None,
            total_requests: 0,
            successes: 0,
        }
    }
}

impl ProviderHealth {
    fn availability(&self) -> f64 {
        if let Some(until) = self.circuit_open_until {
            if Instant::now() < until {
                return 0.0;
            }
        }
        if self.total_requests == 0 {
            return 1.0;
        }
        self.successes as f64 / self.total_requests as f64
    }

    fn record_success(&mut self, latency_ms: f64) {
        self.total_requests += 1;
        self.successes += 1;
        self.consecutive_failures = 0;
        self.circuit_open_until = None;
        // EMA with alpha = 0.3
        self.latency_ema_ms = 0.7 * self.latency_ema_ms + 0.3 * latency_ms;
    }

    fn record_failure(&mut self) {
        self.total_requests += 1;
        self.consecutive_failures += 1;
        // Circuit breaker: open after 3 consecutive failures
        if self.consecutive_failures >= 3 {
            let backoff = Duration::from_secs(2u64.pow(self.consecutive_failures.min(6)));
            self.circuit_open_until = Some(Instant::now() + backoff);
        }
    }

    fn is_available(&self) -> bool {
        if let Some(until) = self.circuit_open_until {
            Instant::now() >= until
        } else {
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Understanding dispatcher
// ---------------------------------------------------------------------------

/// Configuration for the understanding dispatcher.
#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    /// Maximum concurrent provider calls.
    pub max_concurrency: usize,
    /// Maximum retry attempts per provider.
    pub max_retries: u32,
    /// Base backoff duration for retries.
    pub base_backoff: Duration,
    /// Maximum backoff duration.
    pub max_backoff: Duration,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            max_concurrency: 4,
            max_retries: 2,
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
        }
    }
}

/// Multi-provider media understanding dispatcher.
///
/// Selects the best provider for each request based on capability matching,
/// format support, provider health, and latency. Falls back to alternative
/// providers on failure with exponential backoff.
pub struct UnderstandingDispatcher {
    providers: Vec<Arc<dyn UnderstandingProvider>>,
    health: Arc<Mutex<HashMap<String, ProviderHealth>>>,
    semaphore: Arc<Semaphore>,
    config: DispatcherConfig,
}

impl UnderstandingDispatcher {
    /// Create a new dispatcher with default configuration.
    pub fn new() -> Self {
        Self::with_config(DispatcherConfig::default())
    }

    /// Create a new dispatcher with custom configuration.
    pub fn with_config(config: DispatcherConfig) -> Self {
        Self {
            providers: Vec::new(),
            health: Arc::new(Mutex::new(HashMap::new())),
            semaphore: Arc::new(Semaphore::new(config.max_concurrency)),
            config,
        }
    }

    /// Register a provider.
    pub async fn register(&mut self, provider: Arc<dyn UnderstandingProvider>) {
        let name = provider.name().to_string();
        self.health.lock().await.entry(name).or_default();
        self.providers.push(provider);
    }

    /// Process a media understanding request.
    ///
    /// Selects the best provider, handles failover, and respects concurrency limits.
    pub async fn process(&self, request: &UnderstandingRequest) -> Result<UnderstandingResult, UnderstandingError> {
        let _permit = self.semaphore.acquire().await
            .map_err(|_| UnderstandingError::NoProvider(request.capability))?;

        // Find providers that support both the capability and format.
        let candidates = self.rank_providers(request).await;

        if candidates.is_empty() {
            return Err(UnderstandingError::NoProvider(request.capability));
        }

        let mut last_error = String::new();
        for provider in &candidates {
            match self.try_provider(provider, request).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    warn!(provider = provider.name(), error = %e, "provider failed, trying next");
                    last_error = e.to_string();
                }
            }
        }

        Err(UnderstandingError::AllProvidersFailed(last_error))
    }

    /// Rank providers by suitability for a request.
    ///
    /// Score = capability_match × (1 / latency_ema) × availability
    async fn rank_providers(&self, request: &UnderstandingRequest) -> Vec<Arc<dyn UnderstandingProvider>> {
        let health = self.health.lock().await;
        let mut scored: Vec<(f64, &Arc<dyn UnderstandingProvider>)> = self
            .providers
            .iter()
            .filter(|p| {
                // Must support the capability.
                if !p.capabilities().contains(&request.capability) {
                    return false;
                }
                // Must accept the MIME type.
                let formats = p.supported_formats(request.capability);
                if !formats.is_empty() && !formats.iter().any(|f| mime_matches(f, &request.mime_type)) {
                    return false;
                }
                // Must accept the input size.
                if request.data.len() > p.max_input_bytes() {
                    return false;
                }
                // Must not be circuit-broken.
                if let Some(h) = health.get(p.name()) {
                    if !h.is_available() {
                        return false;
                    }
                }
                true
            })
            .map(|p| {
                let h = health.get(p.name()).cloned().unwrap_or_default();
                let latency_score = 1.0 / (h.latency_ema_ms + 1.0);
                let availability = h.availability();
                let score = latency_score * availability;
                (score, p)
            })
            .collect();

        // Sort descending by score.
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|(_, p)| Arc::clone(p)).collect()
    }

    /// Try a single provider with retry logic.
    async fn try_provider(
        &self,
        provider: &Arc<dyn UnderstandingProvider>,
        request: &UnderstandingRequest,
    ) -> Result<UnderstandingResult, UnderstandingError> {
        let name = provider.name().to_string();

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let backoff = self.backoff_duration(attempt);
                debug!(provider = %name, attempt, backoff_ms = backoff.as_millis(), "retrying");
                tokio::time::sleep(backoff).await;
            }

            let start = Instant::now();
            match provider.process(request).await {
                Ok(result) => {
                    let latency = start.elapsed().as_millis() as f64;
                    self.health.lock().await
                        .entry(name.clone())
                        .or_default()
                        .record_success(latency);
                    info!(provider = %name, latency_ms = latency, "understanding complete");
                    return Ok(result);
                }
                Err(UnderstandingError::RateLimited { retry_after_ms, .. }) => {
                    self.health.lock().await
                        .entry(name.clone())
                        .or_default()
                        .record_failure();
                    if attempt < self.config.max_retries {
                        tokio::time::sleep(Duration::from_millis(retry_after_ms)).await;
                        continue;
                    }
                    return Err(UnderstandingError::RateLimited {
                        provider: name,
                        retry_after_ms,
                    });
                }
                Err(e) => {
                    self.health.lock().await
                        .entry(name.clone())
                        .or_default()
                        .record_failure();
                    if attempt == self.config.max_retries {
                        return Err(e);
                    }
                }
            }
        }

        Err(UnderstandingError::Provider {
            provider: name,
            message: "max retries exceeded".into(),
        })
    }

    /// Exponential backoff with jitter.
    fn backoff_duration(&self, attempt: u32) -> Duration {
        let base_ms = self.config.base_backoff.as_millis() as u64;
        let exp_ms = base_ms.saturating_mul(2u64.pow(attempt.saturating_sub(1)));
        let max_ms = self.config.max_backoff.as_millis() as u64;
        let capped = exp_ms.min(max_ms);
        // Add jitter: random value in [0, base_ms]
        let jitter = fastrand::u64(0..=base_ms);
        Duration::from_millis(capped + jitter)
    }

    /// Get provider health stats (for monitoring / debugging).
    pub async fn health_stats(&self) -> HashMap<String, ProviderHealthInfo> {
        let health = self.health.lock().await;
        health
            .iter()
            .map(|(name, h)| {
                (
                    name.clone(),
                    ProviderHealthInfo {
                        latency_ema_ms: h.latency_ema_ms,
                        availability: h.availability(),
                        total_requests: h.total_requests,
                        successes: h.successes,
                        consecutive_failures: h.consecutive_failures,
                        circuit_open: h.circuit_open_until.map(|u| u > Instant::now()).unwrap_or(false),
                    },
                )
            })
            .collect()
    }

    /// Number of registered providers.
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }
}

/// Externally-visible health info for a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHealthInfo {
    pub latency_ema_ms: f64,
    pub availability: f64,
    pub total_requests: u64,
    pub successes: u64,
    pub consecutive_failures: u32,
    pub circuit_open: bool,
}

/// Check if a format spec matches a MIME type.
///
/// Supports wildcards: "image/*" matches "image/png".
fn mime_matches(spec: &str, mime: &str) -> bool {
    if spec == "*/*" {
        return true;
    }
    if spec == mime {
        return true;
    }
    // Check wildcard subtype: "image/*" matches "image/png"
    if let Some(prefix) = spec.strip_suffix("/*") {
        return mime.starts_with(prefix) && mime.as_bytes().get(prefix.len()) == Some(&b'/');
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct MockProvider {
        name: String,
        capabilities: Vec<MediaCapability>,
        formats: Vec<String>,
        max_bytes: usize,
        should_fail: bool,
    }

    #[async_trait]
    impl UnderstandingProvider for MockProvider {
        fn name(&self) -> &str {
            &self.name
        }

        fn capabilities(&self) -> Vec<MediaCapability> {
            self.capabilities.clone()
        }

        fn supported_formats(&self, _capability: MediaCapability) -> Vec<String> {
            self.formats.clone()
        }

        fn max_input_bytes(&self) -> usize {
            self.max_bytes
        }

        async fn process(&self, _request: &UnderstandingRequest) -> Result<UnderstandingResult, UnderstandingError> {
            if self.should_fail {
                return Err(UnderstandingError::Provider {
                    provider: self.name.clone(),
                    message: "mock failure".into(),
                });
            }
            Ok(UnderstandingResult {
                provider: self.name.clone(),
                text: "mock result".into(),
                confidence: Some(0.95),
                duration_ms: 100,
                metadata: HashMap::new(),
            })
        }
    }

    fn mock_request(capability: MediaCapability) -> UnderstandingRequest {
        UnderstandingRequest {
            data: vec![0u8; 100],
            mime_type: "image/png".into(),
            filename: Some("test.png".into()),
            capability,
            prompt: None,
            timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn test_mime_matches_exact() {
        assert!(mime_matches("image/png", "image/png"));
        assert!(!mime_matches("image/png", "image/jpeg"));
    }

    #[test]
    fn test_mime_matches_wildcard() {
        assert!(mime_matches("image/*", "image/png"));
        assert!(mime_matches("image/*", "image/jpeg"));
        assert!(!mime_matches("image/*", "audio/mp3"));
        assert!(mime_matches("*/*", "anything/here"));
    }

    #[test]
    fn test_provider_health_default() {
        let h = ProviderHealth::default();
        assert_eq!(h.availability(), 1.0);
        assert!(h.is_available());
    }

    #[test]
    fn test_provider_health_success() {
        let mut h = ProviderHealth::default();
        h.record_success(50.0);
        assert_eq!(h.successes, 1);
        assert_eq!(h.total_requests, 1);
        assert_eq!(h.consecutive_failures, 0);
    }

    #[test]
    fn test_provider_health_circuit_breaker() {
        let mut h = ProviderHealth::default();
        h.record_failure();
        h.record_failure();
        assert!(h.is_available()); // 2 failures, not yet open
        h.record_failure();
        assert!(!h.is_available()); // 3 failures, circuit opens
    }

    #[test]
    fn test_provider_health_recovery() {
        let mut h = ProviderHealth::default();
        h.record_failure();
        h.record_failure();
        h.record_failure();
        // Simulate circuit breaker timeout by removing it
        h.circuit_open_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(h.is_available()); // Past the deadline
        h.record_success(100.0);
        assert_eq!(h.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn test_dispatcher_no_provider() {
        let dispatcher = UnderstandingDispatcher::new();
        let req = mock_request(MediaCapability::ImageVision);
        let result = dispatcher.process(&req).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), UnderstandingError::NoProvider(_)));
    }

    #[tokio::test]
    async fn test_dispatcher_success() {
        let mut dispatcher = UnderstandingDispatcher::new();
        dispatcher.register(Arc::new(MockProvider {
            name: "openai".into(),
            capabilities: vec![MediaCapability::ImageVision],
            formats: vec!["image/*".into()],
            max_bytes: 1_000_000,
            should_fail: false,
        })).await;

        let req = mock_request(MediaCapability::ImageVision);
        let result = dispatcher.process(&req).await.unwrap();
        assert_eq!(result.provider, "openai");
        assert_eq!(result.text, "mock result");
    }

    #[tokio::test]
    async fn test_dispatcher_failover() {
        let mut dispatcher = UnderstandingDispatcher::new();
        // First provider fails
        dispatcher.register(Arc::new(MockProvider {
            name: "anthropic".into(),
            capabilities: vec![MediaCapability::ImageVision],
            formats: vec!["image/*".into()],
            max_bytes: 1_000_000,
            should_fail: true,
        })).await;
        // Second provider succeeds
        dispatcher.register(Arc::new(MockProvider {
            name: "openai".into(),
            capabilities: vec![MediaCapability::ImageVision],
            formats: vec!["image/*".into()],
            max_bytes: 1_000_000,
            should_fail: false,
        })).await;

        let config = DispatcherConfig {
            max_retries: 0, // No retry per provider — just fail over
            ..Default::default()
        };
        let mut dispatcher = UnderstandingDispatcher::with_config(config);
        dispatcher.register(Arc::new(MockProvider {
            name: "anthropic".into(),
            capabilities: vec![MediaCapability::ImageVision],
            formats: vec!["image/*".into()],
            max_bytes: 1_000_000,
            should_fail: true,
        })).await;
        dispatcher.register(Arc::new(MockProvider {
            name: "openai".into(),
            capabilities: vec![MediaCapability::ImageVision],
            formats: vec!["image/*".into()],
            max_bytes: 1_000_000,
            should_fail: false,
        })).await;

        let req = mock_request(MediaCapability::ImageVision);
        let result = dispatcher.process(&req).await.unwrap();
        assert_eq!(result.provider, "openai");
    }

    #[tokio::test]
    async fn test_dispatcher_format_filter() {
        let mut dispatcher = UnderstandingDispatcher::new();
        // This provider only accepts audio
        dispatcher.register(Arc::new(MockProvider {
            name: "deepgram".into(),
            capabilities: vec![MediaCapability::AudioTranscription],
            formats: vec!["audio/*".into()],
            max_bytes: 100_000_000,
            should_fail: false,
        })).await;

        // Request image understanding — deepgram can't handle it
        let req = mock_request(MediaCapability::ImageVision);
        let result = dispatcher.process(&req).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_health_stats() {
        let mut dispatcher = UnderstandingDispatcher::new();
        dispatcher.register(Arc::new(MockProvider {
            name: "openai".into(),
            capabilities: vec![MediaCapability::ImageVision],
            formats: vec![],
            max_bytes: 1_000_000,
            should_fail: false,
        })).await;

        let req = mock_request(MediaCapability::ImageVision);
        let _ = dispatcher.process(&req).await;

        let stats = dispatcher.health_stats().await;
        assert!(stats.contains_key("openai"));
        assert_eq!(stats["openai"].successes, 1);
    }

    #[test]
    fn test_backoff_duration() {
        let config = DispatcherConfig {
            base_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(10),
            ..Default::default()
        };
        let dispatcher = UnderstandingDispatcher::with_config(config);
        let d = dispatcher.backoff_duration(1);
        // base (100ms) + jitter (0..100ms) → 100..200ms
        assert!(d >= Duration::from_millis(100));
        assert!(d <= Duration::from_millis(200));
    }
}
