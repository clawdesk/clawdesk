//! Async batching exporter for ClawDesk traces
//!
//! Spans are collected in memory and sent in batches either when
//! `batch_size` is reached or `batch_timeout` expires.
//!
//! Target: <50 ms P99 latency overhead on application code.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{debug, error, warn};

// ─────────────────────────────────────────────────────────────────────────────
// Span types
// ─────────────────────────────────────────────────────────────────────────────

/// Link to another span for distributed tracing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanLink {
    pub trace_id: String,
    pub span_id: String,
    pub relationship: String,
    pub attributes: Option<std::collections::HashMap<String, String>>,
}

/// Simplified span structure with W3C Trace Context support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClawdeskSpan {
    pub span_id: String,
    pub trace_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub start_time: u64,
    pub end_time: Option<u64>,
    pub attributes: std::collections::HashMap<String, String>,

    // W3C Trace Context fields
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
    /// Span flags: 0x01 = sampled, 0x02 = random trace id.
    pub span_flags: u8,
    /// Links to other spans (batch processing, async operations).
    pub span_links: Option<Vec<SpanLink>>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Batcher configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the async batch exporter.
#[derive(Debug, Clone)]
pub struct BatcherConfig {
    /// Maximum spans per batch before flushing.
    pub batch_size: usize,
    /// Maximum time to wait before flushing.
    pub batch_timeout: Duration,
    /// Internal channel buffer size.
    pub channel_buffer_size: usize,
    /// Maximum in-memory buffer size (OOM protection).
    pub max_buffer_size: usize,
    /// ClawDesk endpoint (e.g., `http://localhost:47100`).
    pub clawdesk_endpoint: String,
    /// API key for authentication.
    pub api_key: Option<String>,
    /// Priority span types that should never be dropped.
    pub priority_span_types: Vec<String>,
    /// Enable adaptive sampling under load.
    pub adaptive_sampling: bool,
    /// Target sampling rate when under load (0.0–1.0).
    pub load_sampling_rate: f32,
}

impl Default for BatcherConfig {
    fn default() -> Self {
        Self {
            batch_size: 100,
            batch_timeout: Duration::from_secs(1),
            channel_buffer_size: 1000,
            max_buffer_size: 10_000,
            clawdesk_endpoint: "http://localhost:47100".to_string(),
            api_key: None,
            priority_span_types: vec!["error".to_string(), "root".to_string()],
            adaptive_sampling: true,
            load_sampling_rate: 0.1,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Async batch exporter
// ─────────────────────────────────────────────────────────────────────────────

/// Async batch exporter for spans.
///
/// Spawns a background Tokio task that collects spans and exports them
/// in batches. The task runs until the sender is dropped.
pub struct AsyncBatchExporter {
    sender: mpsc::Sender<ClawdeskSpan>,
    config: BatcherConfig,
}

impl AsyncBatchExporter {
    /// Create a new exporter, spawning a background worker.
    pub fn new(config: BatcherConfig) -> Self {
        let (sender, receiver) = mpsc::channel(config.channel_buffer_size);

        let worker_config = config.clone();
        tokio::spawn(async move {
            if let Err(e) = batch_worker(receiver, worker_config).await {
                error!("Batch worker error: {}", e);
            }
        });

        Self { sender, config }
    }

    /// Record a span asynchronously with priority handling and adaptive sampling.
    ///
    /// Non-blocking — adds <1 ms overhead.
    pub async fn record_span(&self, span: ClawdeskSpan) {
        let is_priority = self
            .config
            .priority_span_types
            .iter()
            .any(|pt| span.name.to_lowercase().contains(&pt.to_lowercase()));

        // Adaptive sampling for non-priority spans
        if !is_priority && self.config.adaptive_sampling {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};

            let mut hasher = DefaultHasher::new();
            span.span_id.hash(&mut hasher);
            let hash_val = hasher.finish();
            let threshold = (self.config.load_sampling_rate * (u64::MAX as f32)) as u64;
            if hash_val > threshold {
                return; // sampled out
            }
        }

        match self.sender.try_send(span) {
            Ok(_) => {}
            Err(mpsc::error::TrySendError::Full(dropped)) => {
                if is_priority {
                    warn!("Span buffer full, dropping priority span: {}", dropped.name);
                }
                // Non-priority drops are silent
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                error!("Span channel closed, cannot record span");
            }
        }
    }

    /// Get batch configuration.
    pub fn config(&self) -> &BatcherConfig {
        &self.config
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Background worker
// ─────────────────────────────────────────────────────────────────────────────

async fn batch_worker(
    mut receiver: mpsc::Receiver<ClawdeskSpan>,
    config: BatcherConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buffer = Vec::with_capacity(config.batch_size);
    let mut flush_interval = interval(config.batch_timeout);
    let client = reqwest::Client::new();

    loop {
        tokio::select! {
            Some(span) = receiver.recv() => {
                buffer.push(span);
                if buffer.len() >= config.batch_size {
                    flush_batch(&mut buffer, &client, &config).await;
                }
                if buffer.capacity() > config.max_buffer_size {
                    warn!("Buffer exceeds max size ({}), flushing early", config.max_buffer_size);
                    flush_batch(&mut buffer, &client, &config).await;
                }
            }
            _ = flush_interval.tick() => {
                if !buffer.is_empty() {
                    debug!("Timeout reached, flushing {} spans", buffer.len());
                    flush_batch(&mut buffer, &client, &config).await;
                }
            }
            else => {
                if !buffer.is_empty() {
                    debug!("Channel closed, flushing final {} spans", buffer.len());
                    flush_batch(&mut buffer, &client, &config).await;
                }
                break;
            }
        }
    }

    Ok(())
}

async fn flush_batch(
    buffer: &mut Vec<ClawdeskSpan>,
    client: &reqwest::Client,
    config: &BatcherConfig,
) {
    if buffer.is_empty() {
        return;
    }

    let batch_size = buffer.len();
    debug!("Flushing batch of {} spans to ClawDesk", batch_size);

    let endpoint = format!("{}/api/v1/traces", config.clawdesk_endpoint);
    let mut request = client.post(&endpoint).json(&buffer);

    if let Some(api_key) = &config.api_key {
        request = request.header("X-ClawDesk-API-Key", api_key);
    }

    match request.send().await {
        Ok(response) => {
            if response.status().is_success() {
                debug!("Successfully exported {} spans", batch_size);
            } else {
                warn!("Failed to export spans: HTTP {}", response.status());
            }
        }
        Err(e) => {
            warn!("Error sending spans: {}", e);
        }
    }

    buffer.clear();
}

// ─────────────────────────────────────────────────────────────────────────────
// Circuit breaker
// ─────────────────────────────────────────────────────────────────────────────

/// Circuit breaker state for graceful degradation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation.
    Closed,
    /// Backend is down — don't send.
    Open,
    /// Testing if backend recovered.
    HalfOpen,
}

/// Circuit breaker for export failures.
pub struct CircuitBreaker {
    state: Arc<Mutex<CircuitState>>,
    failure_threshold: usize,
    failures: Arc<Mutex<usize>>,
    #[allow(dead_code)]
    timeout: Duration,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: usize, timeout: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(CircuitState::Closed)),
            failure_threshold,
            failures: Arc::new(Mutex::new(0)),
            timeout,
        }
    }

    /// Record a successful export.
    pub async fn record_success(&self) {
        *self.state.lock().await = CircuitState::Closed;
        *self.failures.lock().await = 0;
    }

    /// Record a failed export.
    pub async fn record_failure(&self) {
        let mut failures = self.failures.lock().await;
        *failures += 1;
        if *failures >= self.failure_threshold {
            *self.state.lock().await = CircuitState::Open;
            warn!(
                "Circuit breaker opened after {} failures",
                self.failure_threshold
            );
        }
    }

    /// Check if requests should be allowed.
    pub async fn should_allow_request(&self) -> bool {
        matches!(*self.state.lock().await, CircuitState::Closed | CircuitState::HalfOpen)
    }

    /// Current state.
    pub async fn state(&self) -> CircuitState {
        *self.state.lock().await
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_batcher_config_default() {
        let config = BatcherConfig::default();
        assert_eq!(config.batch_size, 100);
        assert_eq!(config.batch_timeout, Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_circuit_breaker() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(10));
        assert_eq!(cb.state().await, CircuitState::Closed);

        cb.record_failure().await;
        cb.record_failure().await;
        assert_eq!(cb.state().await, CircuitState::Closed);

        cb.record_failure().await;
        assert_eq!(cb.state().await, CircuitState::Open);

        cb.record_success().await;
        assert_eq!(cb.state().await, CircuitState::Closed);
    }
}
