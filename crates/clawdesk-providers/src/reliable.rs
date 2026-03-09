//! Reliable provider wrapper — retry, backoff, and provider failover.
//!
//! Wraps one or more `Provider` instances with three-level failover:
//!
//! 1. **Retry loop (inner):** Retries the same provider with exponential
//!    backoff for transient errors (rate limit, timeout, server errors).
//! 2. **Provider chain (middle):** Falls through to the next provider when
//!    the current one is exhausted or returns non-retryable errors.
//! 3. **Model fallback (outer):** Tries fallback models if the primary model
//!    fails across all providers.
//!
//! ## Error classification
//!
//! - **Retryable:** `RateLimit`, `Timeout`, `ServerError(>=500)`
//! - **Non-retryable:** `AuthFailure`, `Billing`, `ModelNotFound`, 4xx
//! - **Abort-all:** `ContextLengthExceeded` — immediately aborts all retry
//!   and fallback attempts since the request itself is too large.
//!
//! Uses `ProfileRotator` for API key rotation rather than building it
//! into the failover logic.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

use crate::{Provider, ProviderRequest, ProviderResponse, StreamChunk};

/// Cheap non-cryptographic random u64 using thread-local state.
/// Good enough for jitter — not for security.
fn cheap_random_u64() -> u64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = Cell::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
        );
    }
    STATE.with(|s| {
        // xorshift64
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        x
    })
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the reliable provider wrapper.
#[derive(Debug, Clone)]
pub struct ReliableConfig {
    /// Maximum retries per provider per attempt.
    pub max_retries: u32,
    /// Base backoff duration (exponentially increased on each retry).
    pub base_backoff: Duration,
    /// Maximum backoff cap per retry step.
    pub max_backoff: Duration,
    /// Jitter strategy applied to each backoff duration.
    pub jitter: JitterStrategy,
    /// Model fallback chains: primary model → list of fallbacks.
    pub model_fallbacks: HashMap<String, Vec<String>>,
}

/// Jitter strategy to decorrelate retry storms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JitterStrategy {
    /// No jitter — pure exponential backoff.
    None,
    /// Full jitter: uniform random in [0, backoff].
    Full,
    /// Equal jitter: backoff/2 + uniform random in [0, backoff/2].
    Equal,
    /// Decorrelated jitter: min(max_backoff, random_between(base, prev * 3)).
    Decorrelated,
}

impl Default for ReliableConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
            jitter: JitterStrategy::Equal,
            model_fallbacks: HashMap::new(),
        }
    }
}

impl ReliableConfig {
    /// Add a model fallback chain.
    pub fn with_model_fallback(
        mut self,
        primary: impl Into<String>,
        fallbacks: Vec<String>,
    ) -> Self {
        self.model_fallbacks.insert(primary.into(), fallbacks);
        self
    }
}

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// Classify whether an error is retryable.
fn is_retryable(err: &ProviderError) -> bool {
    use clawdesk_types::error::ProviderErrorKind;
    matches!(err.kind, ProviderErrorKind::RateLimit { .. } | ProviderErrorKind::Timeout { .. })
        || matches!(err.kind, ProviderErrorKind::ServerError { status } if status >= 500)
}

/// Classify whether an error should abort ALL retries and fallbacks.
fn is_abort_all(err: &ProviderError) -> bool {
    use clawdesk_types::error::ProviderErrorKind;
    matches!(err.kind, ProviderErrorKind::ContextLengthExceeded { .. })
}

/// Classify whether an error should skip to the next provider.
fn is_skip_provider(err: &ProviderError) -> bool {
    use clawdesk_types::error::ProviderErrorKind;
    matches!(
        err.kind,
        ProviderErrorKind::AuthFailure { .. }
            | ProviderErrorKind::Billing
            | ProviderErrorKind::ModelNotFound { .. }
    )
}

// ---------------------------------------------------------------------------
// Provider implementation
// ---------------------------------------------------------------------------

/// A provider that wraps one or more providers with retry and failover logic.
///
/// ## Usage
///
/// ```rust,ignore
/// use clawdesk_providers::reliable::{ReliableProvider, ReliableConfig};
///
/// let reliable = ReliableProvider::new(
///     vec![
///         ("anthropic".to_string(), primary_provider),
///         ("openai".to_string(), fallback_provider),
///     ],
///     ReliableConfig::default()
///         .with_model_fallback("claude-sonnet-4-20250514", vec!["gpt-4o".into()]),
/// );
/// ```
pub struct ReliableProvider {
    /// Named providers in priority order.
    providers: Vec<(String, Arc<dyn Provider>)>,
    config: ReliableConfig,
}

impl ReliableProvider {
    /// Create a new reliable provider wrapping the given providers.
    pub fn new(
        providers: Vec<(String, Arc<dyn Provider>)>,
        config: ReliableConfig,
    ) -> Self {
        assert!(!providers.is_empty(), "at least one provider required");
        Self { providers, config }
    }

    /// Create with a single provider (retry-only, no failover).
    pub fn single(provider: Arc<dyn Provider>, config: ReliableConfig) -> Self {
        let name = provider.name().to_string();
        Self {
            providers: vec![(name, provider)],
            config,
        }
    }

    /// Calculate exponential backoff duration with jitter for a given attempt.
    fn backoff_duration(&self, attempt: u32) -> Duration {
        let base_ms = self.config.base_backoff.as_millis() as u64;
        let max_ms = self.config.max_backoff.as_millis() as u64;
        let exp_ms = base_ms.saturating_mul(2u64.saturating_pow(attempt)).min(max_ms);

        let jittered_ms = match self.config.jitter {
            JitterStrategy::None => exp_ms,
            JitterStrategy::Full => {
                // Uniform random in [0, exp_ms].
                let r = cheap_random_u64() % (exp_ms + 1);
                r
            }
            JitterStrategy::Equal => {
                // exp_ms/2 + uniform random in [0, exp_ms/2].
                let half = exp_ms / 2;
                let r = cheap_random_u64() % (half + 1);
                half + r
            }
            JitterStrategy::Decorrelated => {
                // random_between(base_ms, exp_ms * 3), capped at max_ms.
                let upper = (exp_ms.saturating_mul(3)).min(max_ms);
                if upper <= base_ms {
                    base_ms
                } else {
                    base_ms + (cheap_random_u64() % (upper - base_ms + 1))
                }
            }
        };

        Duration::from_millis(jittered_ms.min(max_ms))
    }

    /// Try a request against a single provider with retries.
    async fn try_with_retries(
        &self,
        provider: &dyn Provider,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let mut last_err = None;

        for attempt in 0..=self.config.max_retries {
            match provider.complete(request).await {
                Ok(resp) => return Ok(resp),
                Err(err) => {
                    if is_abort_all(&err) {
                        return Err(err);
                    }

                    if !is_retryable(&err) || attempt == self.config.max_retries {
                        last_err = Some(err);
                        break;
                    }

                    let backoff = self.backoff_duration(attempt);
                    warn!(
                        provider = %provider.name(),
                        attempt = attempt + 1,
                        backoff_ms = %backoff.as_millis(),
                        error = %err,
                        "retrying after transient error"
                    );
                    tokio::time::sleep(backoff).await;
                    last_err = Some(err);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| ProviderError::server_error(provider.name().to_string(), 500)))
    }

    /// Try a streaming request against a single provider with retries.
    ///
    /// Each retry uses an isolated per-attempt channel so partial
    /// chunks from a failed attempt are never forwarded to `chunk_tx`. A spawned
    /// forwarding task streams chunks in real-time. If the attempt fails after
    /// chunks have already been forwarded, we skip further retries (to avoid
    /// duplicate content). Retries only proceed when zero chunks have reached
    /// the caller.
    async fn try_stream_with_retries(
        &self,
        provider: &dyn Provider,
        request: &ProviderRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), ProviderError> {
        let mut last_err = None;

        for attempt in 0..=self.config.max_retries {
            // Create a per-attempt channel to isolate partial chunks.
            let (attempt_tx, mut attempt_rx) = tokio::sync::mpsc::channel::<StreamChunk>(64);
            let forwarded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let forwarded_flag = forwarded.clone();
            let outer_tx = chunk_tx.clone();

            // Forward chunks from attempt channel → outer channel in real time.
            let fwd_handle = tokio::spawn(async move {
                while let Some(chunk) = attempt_rx.recv().await {
                    forwarded_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    if outer_tx.send(chunk).await.is_err() {
                        break;
                    }
                }
            });

            match provider.stream(request, attempt_tx).await {
                Ok(()) => {
                    // stream() completed — all chunks sent to attempt_tx.
                    // attempt_tx is dropped, so fwd_handle will drain and finish.
                    let _ = fwd_handle.await;
                    return Ok(());
                }
                Err(err) => {
                    // Abort forwarding to minimize partial-chunk leakage.
                    fwd_handle.abort();

                    if is_abort_all(&err) {
                        return Err(err);
                    }

                    // If any chunks were already forwarded, do NOT retry —
                    // a retry would send duplicate content to the caller.
                    if forwarded.load(std::sync::atomic::Ordering::Relaxed) {
                        warn!(
                            provider = %provider.name(),
                            attempt = attempt + 1,
                            error = %err,
                            "stream failed after partial delivery, skipping retry to avoid duplicates"
                        );
                        return Err(err);
                    }

                    if !is_retryable(&err) || attempt == self.config.max_retries {
                        last_err = Some(err);
                        break;
                    }

                    let backoff = self.backoff_duration(attempt);
                    warn!(
                        provider = %provider.name(),
                        attempt = attempt + 1,
                        backoff_ms = %backoff.as_millis(),
                        error = %err,
                        "retrying stream after transient error (no chunks delivered yet)"
                    );
                    tokio::time::sleep(backoff).await;
                    last_err = Some(err);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| ProviderError::server_error(provider.name().to_string(), 500)))
    }

    /// Get the model chain: primary model + fallbacks.
    fn model_chain<'a>(&'a self, model: &'a str) -> Vec<&'a str> {
        let mut chain = vec![model];
        if let Some(fallbacks) = self.config.model_fallbacks.get(model) {
            chain.extend(fallbacks.iter().map(String::as_str));
        }
        chain
    }
}

#[async_trait]
impl Provider for ReliableProvider {
    fn name(&self) -> &str {
        "reliable"
    }

    fn models(&self) -> Vec<String> {
        // Aggregate models from all wrapped providers
        let mut models = Vec::new();
        for (_, provider) in &self.providers {
            models.extend(provider.models());
        }
        models.sort();
        models.dedup();
        models
    }

    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let models = self.model_chain(&request.model);
        let mut last_err = None;

        // Outer loop: model fallback chain
        for model in &models {
            // Skip O(N) clone when model matches the request.
            let model_request_owned;
            let model_request: &ProviderRequest = if *model == request.model.as_str() {
                request
            } else {
                model_request_owned = ProviderRequest {
                    model: model.to_string(),
                    ..request.clone()
                };
                &model_request_owned
            };

            // Middle loop: provider chain
            for (name, provider) in &self.providers {
                debug!(
                    model = %model,
                    provider = %name,
                    "attempting completion"
                );

                match self.try_with_retries(provider.as_ref(), model_request).await {
                    Ok(resp) => return Ok(resp),
                    Err(err) => {
                        if is_abort_all(&err) {
                            return Err(err);
                        }

                        if is_skip_provider(&err) {
                            warn!(
                                provider = %name,
                                error = %err,
                                "skipping provider due to non-retryable error"
                            );
                        }

                        last_err = Some(err);
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| ProviderError::server_error("reliable", 503)))
    }

    async fn stream(
        &self,
        request: &ProviderRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), ProviderError> {
        let models = self.model_chain(&request.model);
        let mut last_err = None;

        for model in &models {
            // Skip O(N) clone when model matches the request.
            let model_request_owned;
            let model_request: &ProviderRequest = if *model == request.model.as_str() {
                request
            } else {
                model_request_owned = ProviderRequest {
                    model: model.to_string(),
                    ..request.clone()
                };
                &model_request_owned
            };

            for (name, provider) in &self.providers {
                debug!(
                    model = %model,
                    provider = %name,
                    "attempting stream"
                );

                match self
                    .try_stream_with_retries(
                        provider.as_ref(),
                        model_request,
                        chunk_tx.clone(),
                    )
                    .await
                {
                    Ok(()) => return Ok(()),
                    Err(err) => {
                        if is_abort_all(&err) {
                            return Err(err);
                        }
                        last_err = Some(err);
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| ProviderError::server_error("reliable", 503)))
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        // Health check passes if at least one provider is healthy
        let mut last_err = None;
        for (name, provider) in &self.providers {
            match provider.health_check().await {
                Ok(()) => {
                    debug!(provider = %name, "health check passed");
                    return Ok(());
                }
                Err(err) => {
                    warn!(provider = %name, error = %err, "health check failed");
                    last_err = Some(err);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| ProviderError::server_error("reliable", 503)))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        assert!(is_retryable(&ProviderError::rate_limit("test", None)));

        assert!(is_retryable(&ProviderError::timeout("test", "m", Duration::from_secs(1))));

        assert!(is_retryable(&ProviderError::server_error("test", 500)));

        assert!(!is_retryable(&ProviderError::auth_failure("test", String::new())));

        assert!(is_abort_all(&ProviderError::context_length_exceeded("test", "m", "too long")));

        assert!(is_skip_provider(&ProviderError::billing("test")));
    }

    #[test]
    fn test_backoff_duration() {
        let config = ReliableConfig {
            max_retries: 5,
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
            model_fallbacks: HashMap::new(),
            jitter: JitterStrategy::None,
        };

        // Use a dummy provider
        let reliable = ReliableProvider {
            providers: Vec::new(),
            config,
        };

        assert_eq!(reliable.backoff_duration(0), Duration::from_millis(500));
        assert_eq!(reliable.backoff_duration(1), Duration::from_millis(1000));
        assert_eq!(reliable.backoff_duration(2), Duration::from_millis(2000));
        assert_eq!(reliable.backoff_duration(3), Duration::from_millis(4000));
        assert_eq!(reliable.backoff_duration(4), Duration::from_millis(8000));
        // Capped at max_backoff
        assert_eq!(reliable.backoff_duration(5), Duration::from_secs(10));
    }

    #[test]
    fn test_model_chain() {
        let config = ReliableConfig::default().with_model_fallback(
            "claude-sonnet-4-20250514",
            vec!["gpt-4o".into(), "gemini-2.0-flash".into()],
        );

        let reliable = ReliableProvider {
            providers: Vec::new(),
            config,
        };

        let chain = reliable.model_chain("claude-sonnet-4-20250514");
        assert_eq!(chain, vec!["claude-sonnet-4-20250514", "gpt-4o", "gemini-2.0-flash"]);

        // Unknown model: no fallbacks
        let chain2 = reliable.model_chain("unknown-model");
        assert_eq!(chain2, vec!["unknown-model"]);
    }

    #[test]
    fn test_config_defaults() {
        let config = ReliableConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_backoff, Duration::from_millis(500));
        assert_eq!(config.max_backoff, Duration::from_secs(10));
        assert!(config.model_fallbacks.is_empty());
    }
}
