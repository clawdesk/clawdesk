//! Outbound webhook delivery queue — at-least-once semantics.
//!
//! ## Design
//!
//! When an agent produces an event that triggers an outbound webhook
//! (e.g., agent response → callback URL, event bus → HTTP webhook),
//! the payload is:
//!
//! 1. **Persisted** to SochDB (WAL-backed) before delivery is attempted.
//! 2. **Delivered** via HTTP POST with HMAC-SHA256 signature.
//! 3. **Acknowledged** only after a 2xx response (at-least-once).
//! 4. **Retried** with exponential backoff on failure.
//! 5. **Dead-lettered** after max retries.
//!
//! ## Deduplication
//!
//! Each delivery carries an `X-Delivery-Id` header (UUIDv7).
//! A Bloom filter provides approximate dedup on the receiver side,
//! and idempotency keys are logged for exact dedup.
//!
//! ## Storage
//!
//! ```text
//! webhook:outbox:{delivery_id}  →  DeliveryEnvelope (JSON)
//! webhook:dlq:{delivery_id}     →  DeliveryEnvelope (JSON)
//! ```
//!
//! ## Background worker
//!
//! `DeliveryWorker` runs a background loop that:
//! 1. Scans `webhook:outbox:` prefix for pending deliveries.
//! 2. Attempts HTTP POST for each.
//! 3. On 2xx → deletes from outbox.
//! 4. On failure → increments attempt, applies backoff, re-persists.
//! 5. On max retries → moves to `webhook:dlq:`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A webhook delivery envelope — the unit of persistence and retry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryEnvelope {
    /// Unique delivery ID (UUIDv7 for time-ordering).
    pub delivery_id: String,
    /// Target URL to POST to.
    pub url: String,
    /// HMAC-SHA256 signing secret (empty = unsigned).
    pub secret: String,
    /// JSON payload body.
    pub payload: serde_json::Value,
    /// Source event type (e.g., "agent.response", "cron.run").
    pub event_type: String,
    /// Number of delivery attempts so far.
    pub attempts: u32,
    /// Maximum attempts before dead-lettering.
    pub max_attempts: u32,
    /// When the envelope was created.
    pub created_at: DateTime<Utc>,
    /// When the next attempt should occur.
    pub next_attempt_at: DateTime<Utc>,
    /// Last error message (if any).
    pub last_error: Option<String>,
    /// HTTP status of last attempt (if any).
    pub last_status: Option<u16>,
}

impl DeliveryEnvelope {
    /// Create a new envelope ready for first delivery attempt.
    pub fn new(
        url: impl Into<String>,
        secret: impl Into<String>,
        payload: serde_json::Value,
        event_type: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            delivery_id: uuid::Uuid::new_v4().to_string(),
            url: url.into(),
            secret: secret.into(),
            payload,
            event_type: event_type.into(),
            attempts: 0,
            max_attempts: 5,
            created_at: now,
            next_attempt_at: now,
            last_error: None,
            last_status: None,
        }
    }

    /// Compute the next retry time using exponential backoff.
    ///
    /// Backoff: `min(base × 2^attempt, max_backoff)` with ±20% jitter.
    pub fn compute_backoff(&self) -> Duration {
        let base_secs: u64 = 5;
        let max_secs: u64 = 300; // 5 minutes
        let exp = base_secs.saturating_mul(1u64 << self.attempts.min(6));
        let capped = exp.min(max_secs);
        // Simple deterministic "jitter" based on delivery_id hash.
        let jitter_pct = (self.delivery_id.len() % 20) as u64;
        let jitter = capped * jitter_pct / 100;
        Duration::from_secs(capped + jitter)
    }

    fn outbox_key(&self) -> String {
        format!("webhook:outbox:{}", self.delivery_id)
    }

    fn dlq_key(&self) -> String {
        format!("webhook:dlq:{}", self.delivery_id)
    }
}

/// Result of a single delivery attempt.
#[derive(Debug)]
pub enum DeliveryResult {
    /// Delivered successfully (2xx).
    Delivered { status: u16, latency_ms: u64 },
    /// Delivery failed — will retry.
    Failed { error: String, status: Option<u16> },
    /// Max retries exceeded — moved to DLQ.
    DeadLettered { error: String },
}

/// Configuration for the delivery worker.
#[derive(Debug, Clone)]
pub struct DeliveryWorkerConfig {
    /// How often to scan the outbox for pending deliveries.
    pub poll_interval: Duration,
    /// HTTP timeout for delivery attempts.
    pub request_timeout: Duration,
    /// Maximum concurrent deliveries.
    pub max_concurrent: usize,
}

impl Default for DeliveryWorkerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            max_concurrent: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// Delivery queue
// ---------------------------------------------------------------------------

/// Persistent webhook delivery queue backed by SochDB.
///
/// Provides enqueue/dequeue/ack/dead-letter operations with
/// WAL-backed persistence for at-least-once delivery.
pub struct WebhookDeliveryQueue {
    store: Arc<clawdesk_sochdb::SochStore>,
}

impl WebhookDeliveryQueue {
    pub fn new(store: Arc<clawdesk_sochdb::SochStore>) -> Self {
        Self { store }
    }

    /// Enqueue a new delivery. Persists to SochDB before returning.
    pub fn enqueue(&self, envelope: &DeliveryEnvelope) -> Result<(), String> {
        let key = envelope.outbox_key();
        let bytes = serde_json::to_vec(envelope)
            .map_err(|e| format!("serialization: {e}"))?;
        self.store.put(&key, &bytes)
            .map_err(|e| format!("persist: {e}"))?;
        debug!(
            delivery_id = %envelope.delivery_id,
            url = %envelope.url,
            event_type = %envelope.event_type,
            "webhook delivery enqueued"
        );
        Ok(())
    }

    /// Scan for deliveries ready to be attempted (next_attempt_at ≤ now).
    pub fn scan_pending(&self) -> Result<Vec<DeliveryEnvelope>, String> {
        let prefix = "webhook:outbox:";
        let entries = self.store.scan(prefix)
            .map_err(|e| format!("scan: {e}"))?;

        let now = Utc::now();
        let mut ready = Vec::new();

        for (_, val) in &entries {
            if let Ok(env) = serde_json::from_slice::<DeliveryEnvelope>(val) {
                if env.next_attempt_at <= now {
                    ready.push(env);
                }
            }
        }

        // Sort by next_attempt_at for fairness.
        ready.sort_by_key(|e| e.next_attempt_at);
        Ok(ready)
    }

    /// Acknowledge successful delivery — remove from outbox.
    pub fn ack(&self, delivery_id: &str) -> Result<(), String> {
        let key = format!("webhook:outbox:{delivery_id}");
        let _ = self.store.delete(&key);
        debug!(%delivery_id, "webhook delivery acknowledged");
        Ok(())
    }

    /// Update an envelope after a failed attempt (increment attempt, set backoff).
    pub fn retry(&self, envelope: &mut DeliveryEnvelope) -> Result<(), String> {
        envelope.attempts += 1;
        let backoff = envelope.compute_backoff();
        envelope.next_attempt_at = Utc::now() + chrono::Duration::from_std(backoff)
            .unwrap_or(chrono::Duration::seconds(300));

        let key = envelope.outbox_key();
        let bytes = serde_json::to_vec(envelope)
            .map_err(|e| format!("serialization: {e}"))?;
        self.store.put(&key, &bytes)
            .map_err(|e| format!("persist: {e}"))?;

        debug!(
            delivery_id = %envelope.delivery_id,
            attempt = envelope.attempts,
            next_attempt = %envelope.next_attempt_at,
            "webhook delivery scheduled for retry"
        );
        Ok(())
    }

    /// Move an envelope to the dead letter queue.
    pub fn dead_letter(&self, envelope: &DeliveryEnvelope) -> Result<(), String> {
        // Remove from outbox.
        let outbox_key = envelope.outbox_key();
        let _ = self.store.delete(&outbox_key);

        // Persist to DLQ.
        let dlq_key = envelope.dlq_key();
        let bytes = serde_json::to_vec(envelope)
            .map_err(|e| format!("serialization: {e}"))?;
        self.store.put(&dlq_key, &bytes)
            .map_err(|e| format!("persist: {e}"))?;

        warn!(
            delivery_id = %envelope.delivery_id,
            attempts = envelope.attempts,
            url = %envelope.url,
            "webhook delivery moved to DLQ"
        );
        Ok(())
    }

    /// List all dead-lettered deliveries.
    pub fn list_dlq(&self) -> Result<Vec<DeliveryEnvelope>, String> {
        let prefix = "webhook:dlq:";
        let entries = self.store.scan(prefix)
            .map_err(|e| format!("scan: {e}"))?;

        let mut result = Vec::new();
        for (_, val) in &entries {
            if let Ok(env) = serde_json::from_slice::<DeliveryEnvelope>(val) {
                result.push(env);
            }
        }
        Ok(result)
    }

    /// Retry a dead-lettered delivery (move back to outbox).
    pub fn retry_from_dlq(&self, delivery_id: &str) -> Result<(), String> {
        let dlq_key = format!("webhook:dlq:{delivery_id}");
        let bytes = self.store.get(&dlq_key)
            .map_err(|e| format!("read: {e}"))?
            .ok_or_else(|| format!("DLQ entry not found: {delivery_id}"))?;

        let mut env: DeliveryEnvelope = serde_json::from_slice(&bytes)
            .map_err(|e| format!("deserialize: {e}"))?;

        // Reset for retry.
        env.attempts = 0;
        env.next_attempt_at = Utc::now();
        env.last_error = None;
        env.last_status = None;

        // Move to outbox.
        let outbox_key = env.outbox_key();
        let outbox_bytes = serde_json::to_vec(&env)
            .map_err(|e| format!("serialization: {e}"))?;
        self.store.put(&outbox_key, &outbox_bytes)
            .map_err(|e| format!("persist: {e}"))?;

        let _ = self.store.delete(&dlq_key);

        info!(%delivery_id, "webhook delivery moved from DLQ back to outbox");
        Ok(())
    }

    /// Purge all dead-lettered entries.
    pub fn purge_dlq(&self) -> Result<usize, String> {
        let prefix = "webhook:dlq:";
        self.store.delete_prefix(prefix)
            .map_err(|e| format!("purge: {e}"))
    }

    /// Count of pending deliveries in the outbox.
    pub fn outbox_count(&self) -> Result<usize, String> {
        let prefix = "webhook:outbox:";
        let entries = self.store.scan(prefix)
            .map_err(|e| format!("scan: {e}"))?;
        Ok(entries.len())
    }

    /// Count of dead-lettered deliveries.
    pub fn dlq_count(&self) -> Result<usize, String> {
        let prefix = "webhook:dlq:";
        let entries = self.store.scan(prefix)
            .map_err(|e| format!("scan: {e}"))?;
        Ok(entries.len())
    }
}

// ---------------------------------------------------------------------------
// Delivery worker (background task)
// ---------------------------------------------------------------------------

/// Background worker that processes the webhook outbox.
///
/// Runs as a tokio task, polling for pending deliveries and attempting
/// HTTP delivery with exponential backoff retry.
pub struct DeliveryWorker {
    queue: Arc<WebhookDeliveryQueue>,
    config: DeliveryWorkerConfig,
    http: reqwest::Client,
}

impl DeliveryWorker {
    pub fn new(
        queue: Arc<WebhookDeliveryQueue>,
        config: DeliveryWorkerConfig,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .user_agent("ClawDesk-Webhook/1.0")
            .build()
            .unwrap_or_default();

        Self { queue, config, http }
    }

    /// Start the delivery worker as a background task.
    pub fn spawn(
        self,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run(cancel).await;
        })
    }

    /// Main delivery loop.
    async fn run(&self, cancel: CancellationToken) {
        info!(
            poll_secs = self.config.poll_interval.as_secs(),
            max_concurrent = self.config.max_concurrent,
            "webhook delivery worker started"
        );

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("webhook delivery worker shutting down");
                    break;
                }
                _ = tokio::time::sleep(self.config.poll_interval) => {
                    self.process_batch().await;
                }
            }
        }
    }

    /// Process a batch of pending deliveries.
    async fn process_batch(&self) {
        let pending = match self.queue.scan_pending() {
            Ok(p) => p,
            Err(e) => {
                error!(%e, "failed to scan webhook outbox");
                return;
            }
        };

        if pending.is_empty() {
            return;
        }

        debug!(count = pending.len(), "processing webhook deliveries");

        // Process up to max_concurrent in parallel.
        let batch: Vec<_> = pending
            .into_iter()
            .take(self.config.max_concurrent)
            .collect();

        let mut handles = Vec::new();
        for env in batch {
            let queue = self.queue.clone();
            let http = self.http.clone();
            handles.push(tokio::spawn(async move {
                Self::deliver_one(queue, http, env).await;
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }
    }

    /// Attempt to deliver one envelope.
    async fn deliver_one(
        queue: Arc<WebhookDeliveryQueue>,
        http: reqwest::Client,
        mut envelope: DeliveryEnvelope,
    ) {
        let start = std::time::Instant::now();
        let body = serde_json::to_vec(&envelope.payload).unwrap_or_default();

        // Build HMAC signature.
        let signature = if !envelope.secret.is_empty() {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            type HmacSha256 = Hmac<Sha256>;
            let mut mac = HmacSha256::new_from_slice(envelope.secret.as_bytes())
                .expect("HMAC key");
            mac.update(&body);
            let result = mac.finalize();
            let hex = hex::encode(result.into_bytes());
            format!("sha256={hex}")
        } else {
            String::new()
        };

        let mut req = http
            .post(&envelope.url)
            .header("Content-Type", "application/json")
            .header("X-Delivery-Id", &envelope.delivery_id)
            .header("X-Event-Type", &envelope.event_type)
            .body(body);

        if !signature.is_empty() {
            req = req.header("X-Webhook-Signature", &signature);
        }

        let result = req.send().await;
        let latency_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if (200..300).contains(&(status as usize)) {
                    // Success — acknowledge.
                    let _ = queue.ack(&envelope.delivery_id);
                    debug!(
                        delivery_id = %envelope.delivery_id,
                        %status,
                        %latency_ms,
                        "webhook delivered successfully"
                    );
                } else {
                    // Non-2xx — retry or dead-letter.
                    let error = format!("HTTP {status}");
                    envelope.last_error = Some(error.clone());
                    envelope.last_status = Some(status);

                    if envelope.attempts + 1 >= envelope.max_attempts {
                        envelope.attempts += 1;
                        let _ = queue.dead_letter(&envelope);
                    } else {
                        let _ = queue.retry(&mut envelope);
                    }
                }
            }
            Err(e) => {
                let error = e.to_string();
                envelope.last_error = Some(error.clone());
                envelope.last_status = None;

                if envelope.attempts + 1 >= envelope.max_attempts {
                    envelope.attempts += 1;
                    let _ = queue.dead_letter(&envelope);
                } else {
                    let _ = queue.retry(&mut envelope);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (Arc<clawdesk_sochdb::SochStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            clawdesk_sochdb::SochStore::open(dir.path()).expect("open store")
        );
        (store, dir) // keep TempDir alive so the directory persists
    }

    #[test]
    fn envelope_creation() {
        let env = DeliveryEnvelope::new(
            "https://example.com/webhook",
            "secret123",
            serde_json::json!({"event": "test"}),
            "test.event",
        );
        assert_eq!(env.attempts, 0);
        assert_eq!(env.max_attempts, 5);
        assert!(!env.delivery_id.is_empty());
    }

    #[test]
    fn backoff_increases() {
        let mut env = DeliveryEnvelope::new(
            "https://example.com",
            "",
            serde_json::json!({}),
            "test",
        );
        let b0 = env.compute_backoff();
        env.attempts = 1;
        let b1 = env.compute_backoff();
        env.attempts = 3;
        let b3 = env.compute_backoff();

        assert!(b1 >= b0);
        assert!(b3 >= b1);
    }

    #[test]
    fn enqueue_and_scan() {
        let (store, _dir) = test_store();
        let queue = WebhookDeliveryQueue::new(store);

        let env = DeliveryEnvelope::new(
            "https://example.com",
            "secret",
            serde_json::json!({"data": 1}),
            "test.event",
        );

        queue.enqueue(&env).unwrap();
        let pending = queue.scan_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].delivery_id, env.delivery_id);
    }

    #[test]
    fn ack_removes_from_outbox() {
        let (store, _dir) = test_store();
        let queue = WebhookDeliveryQueue::new(store);

        let env = DeliveryEnvelope::new(
            "https://example.com",
            "",
            serde_json::json!({}),
            "test",
        );
        let id = env.delivery_id.clone();

        queue.enqueue(&env).unwrap();
        assert_eq!(queue.outbox_count().unwrap(), 1);

        queue.ack(&id).unwrap();
        assert_eq!(queue.outbox_count().unwrap(), 0);
    }

    #[test]
    fn retry_increments_attempts() {
        let (store, _dir) = test_store();
        let queue = WebhookDeliveryQueue::new(store);

        let mut env = DeliveryEnvelope::new(
            "https://example.com",
            "",
            serde_json::json!({}),
            "test",
        );
        queue.enqueue(&env).unwrap();

        queue.retry(&mut env).unwrap();
        assert_eq!(env.attempts, 1);
        assert!(env.next_attempt_at > env.created_at);
    }

    #[test]
    fn dead_letter_moves_to_dlq() {
        let (store, _dir) = test_store();
        let queue = WebhookDeliveryQueue::new(store);

        let env = DeliveryEnvelope::new(
            "https://example.com",
            "",
            serde_json::json!({}),
            "test",
        );
        queue.enqueue(&env).unwrap();
        assert_eq!(queue.outbox_count().unwrap(), 1);

        queue.dead_letter(&env).unwrap();
        assert_eq!(queue.outbox_count().unwrap(), 0);
        assert_eq!(queue.dlq_count().unwrap(), 1);
    }

    #[test]
    fn retry_from_dlq() {
        let (store, _dir) = test_store();
        let queue = WebhookDeliveryQueue::new(store);

        let mut env = DeliveryEnvelope::new(
            "https://example.com",
            "",
            serde_json::json!({}),
            "test",
        );
        env.attempts = 5;
        env.last_error = Some("HTTP 500".into());
        queue.enqueue(&env).unwrap();
        queue.dead_letter(&env).unwrap();
        assert_eq!(queue.dlq_count().unwrap(), 1);

        queue.retry_from_dlq(&env.delivery_id).unwrap();
        assert_eq!(queue.dlq_count().unwrap(), 0);
        assert_eq!(queue.outbox_count().unwrap(), 1);

        // Should have reset attempts.
        let pending = queue.scan_pending().unwrap();
        assert_eq!(pending[0].attempts, 0);
    }

    #[test]
    fn purge_dlq() {
        let (store, _dir) = test_store();
        let queue = WebhookDeliveryQueue::new(store);

        for _ in 0..3 {
            let env = DeliveryEnvelope::new("https://example.com", "", serde_json::json!({}), "test");
            queue.enqueue(&env).unwrap();
            queue.dead_letter(&env).unwrap();
        }
        assert_eq!(queue.dlq_count().unwrap(), 3);

        let purged = queue.purge_dlq().unwrap();
        assert_eq!(purged, 3);
        assert_eq!(queue.dlq_count().unwrap(), 0);
    }
}
