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
    /// Model fallback chains: primary model → list of fallbacks.
    pub model_fallbacks: HashMap<String, Vec<String>>,
}

impl Default for ReliableConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
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
    matches!(err, ProviderError::RateLimit { .. } | ProviderError::Timeout { .. })
        || matches!(err, ProviderError::ServerError { status, .. } if *status >= 500)
}

/// Classify whether an error should abort ALL retries and fallbacks.
fn is_abort_all(err: &ProviderError) -> bool {
    matches!(err, ProviderError::ContextLengthExceeded { .. })
}

/// Classify whether an error should skip to the next provider.
fn is_skip_provider(err: &ProviderError) -> bool {
    matches!(
        err,
        ProviderError::AuthFailure { .. }
            | ProviderError::Billing { .. }
            | ProviderError::ModelNotFound { .. }
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

    /// Calculate exponential backoff duration for a given attempt.
    fn backoff_duration(&self, attempt: u32) -> Duration {
        let backoff = self.config.base_backoff.as_millis() as u64
            * 2u64.saturating_pow(attempt);
        Duration::from_millis(backoff.min(self.config.max_backoff.as_millis() as u64))
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

        Err(last_err.unwrap_or_else(|| ProviderError::ServerError {
            provider: provider.name().to_string(),
            status: 500,
        }))
    }

    /// Try a streaming request against a single provider with retries.
    async fn try_stream_with_retries(
        &self,
        provider: &dyn Provider,
        request: &ProviderRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), ProviderError> {
        let mut last_err = None;

        for attempt in 0..=self.config.max_retries {
            match provider.stream(request, chunk_tx.clone()).await {
                Ok(()) => return Ok(()),
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
                        "retrying stream after transient error"
                    );
                    tokio::time::sleep(backoff).await;
                    last_err = Some(err);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| ProviderError::ServerError {
            provider: provider.name().to_string(),
            status: 500,
        }))
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
            let mut model_request = request.clone();
            model_request.model = model.to_string();

            // Middle loop: provider chain
            for (name, provider) in &self.providers {
                debug!(
                    model = %model,
                    provider = %name,
                    "attempting completion"
                );

                match self.try_with_retries(provider.as_ref(), &model_request).await {
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

        Err(last_err.unwrap_or_else(|| ProviderError::ServerError {
            provider: "reliable".into(),
            status: 503,
        }))
    }

    async fn stream(
        &self,
        request: &ProviderRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), ProviderError> {
        let models = self.model_chain(&request.model);
        let mut last_err = None;

        for model in &models {
            let mut model_request = request.clone();
            model_request.model = model.to_string();

            for (name, provider) in &self.providers {
                debug!(
                    model = %model,
                    provider = %name,
                    "attempting stream"
                );

                match self
                    .try_stream_with_retries(
                        provider.as_ref(),
                        &model_request,
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

        Err(last_err.unwrap_or_else(|| ProviderError::ServerError {
            provider: "reliable".into(),
            status: 503,
        }))
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

        Err(last_err.unwrap_or_else(|| ProviderError::ServerError {
            provider: "reliable".into(),
            status: 503,
        }))
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
        assert!(is_retryable(&ProviderError::RateLimit {
            provider: "test".into(),
            retry_after: None,
        }));

        assert!(is_retryable(&ProviderError::Timeout {
            provider: "test".into(),
            model: "m".into(),
            after: Duration::from_secs(1),
        }));

        assert!(is_retryable(&ProviderError::ServerError {
            provider: "test".into(),
            status: 500,
        }));

        assert!(!is_retryable(&ProviderError::AuthFailure {
            provider: "test".into(),
            profile_id: String::new(),
        }));

        assert!(is_abort_all(&ProviderError::ContextLengthExceeded {
            model: "m".into(),
            detail: "too long".into(),
        }));

        assert!(is_skip_provider(&ProviderError::Billing {
            provider: "test".into(),
        }));
    }

    #[test]
    fn test_backoff_duration() {
        let config = ReliableConfig {
            max_retries: 5,
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
            model_fallbacks: HashMap::new(),
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
