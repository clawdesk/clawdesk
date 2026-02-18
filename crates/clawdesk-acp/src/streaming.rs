//! SSE stream with backpressure control and dual-channel delivery.
//!
//! ## Architecture
//!
//! The streaming system is a leaky bucket with rate `λ` (production) and
//! drain rate `μ` (consumption). Stability requires `μ > λ`.
//! Buffer depth `B` provides grace period `B/(λ - μ)` seconds before overflow.
//!
//! ## Overflow Policies
//!
//! - `DropOldest`: Bounded latency, lossy — evicts oldest undelivered event.
//! - `DropNewest`: Bounded age, lossy — drops incoming events when full.
//! - `BlockProducer`: Lossless — introduces upstream backpressure.
//!
//! ## Dual-Channel Delivery
//!
//! Each message has state in {pending, sse_acked, push_sent, delivered}.
//! Idempotency key = `(task_id, sequence_number)`.
//! Dedup via Bloom filter with `ε < 10⁻⁶` at `≈ 30n` bits.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Notify};

/// Overflow policy when the buffer is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Drop the oldest undelivered event (bounded latency, lossy).
    DropOldest,
    /// Drop the newest incoming event (bounded age, lossy).
    DropNewest,
    /// Block the producer until space is available (lossless).
    BlockProducer,
}

/// Delivery state for a stream event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    /// Event is in the buffer, not yet delivered.
    Pending,
    /// Event was delivered via SSE and acknowledged.
    SseAcked,
    /// Event was sent via push notification (fallback).
    PushSent,
    /// Event was confirmed delivered (either channel).
    Delivered,
}

/// A stream event with delivery tracking.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    /// Task this event belongs to.
    pub task_id: String,
    /// Sequence number within the task (monotonically increasing).
    pub sequence: u64,
    /// Event payload.
    pub payload: StreamPayload,
    /// Delivery state.
    pub state: DeliveryState,
    /// When this event was produced.
    pub produced_at: Instant,
    /// Idempotency key = `(task_id, sequence)`.
    pub idempotency_key: String,
}

impl StreamEvent {
    pub fn new(task_id: String, sequence: u64, payload: StreamPayload) -> Self {
        let idempotency_key = format!("{}:{}", task_id, sequence);
        Self {
            task_id,
            sequence,
            payload,
            state: DeliveryState::Pending,
            produced_at: Instant::now(),
            idempotency_key,
        }
    }
}

/// Stream event payloads.
#[derive(Debug, Clone)]
pub enum StreamPayload {
    /// Text delta (streaming response).
    TextDelta { delta: String, done: bool },
    /// Task status change.
    StatusChange { state: String, progress: Option<f64> },
    /// Artifact delivery notification.
    ArtifactReady { artifact_id: String },
    /// Error notification.
    Error { code: String, message: String },
    /// Keepalive/ping.
    Ping { nonce: u64 },
}

/// Configuration for the backpressure-aware stream.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Maximum number of buffered events before overflow policy kicks in.
    pub buffer_depth: usize,
    /// Overflow policy when buffer is full.
    pub overflow_policy: OverflowPolicy,
    /// Timeout for SSE delivery before falling back to push notification.
    pub sse_timeout: Duration,
    /// Maximum time an event can remain pending before push fallback.
    pub max_pending_age: Duration,
    /// Keepalive interval for SSE connections.
    pub keepalive_interval: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            buffer_depth: 1024,
            overflow_policy: OverflowPolicy::DropOldest,
            sse_timeout: Duration::from_secs(30),
            max_pending_age: Duration::from_secs(60),
            keepalive_interval: Duration::from_secs(15),
        }
    }
}

/// Stream metrics.
#[derive(Debug, Default)]
pub struct StreamMetrics {
    /// Total events produced.
    pub events_produced: AtomicU64,
    /// Events delivered via SSE.
    pub sse_delivered: AtomicU64,
    /// Events delivered via push notification.
    pub push_delivered: AtomicU64,
    /// Events dropped due to overflow.
    pub events_dropped: AtomicU64,
    /// Producer blocks due to backpressure.
    pub producer_blocks: AtomicU64,
}

/// Backpressure-aware event stream with bounded buffer.
///
/// Memory usage under slow consumers is bounded to `O(B)` where `B`
/// is the configured buffer depth.
pub struct BackpressureStream {
    config: StreamConfig,
    /// Bounded buffer — ring buffer semantics via VecDeque.
    buffer: Mutex<VecDeque<StreamEvent>>,
    /// Sequence counter per task.
    sequence: AtomicU64,
    /// Notify when buffer has space (for BlockProducer policy).
    space_available: Notify,
    /// Metrics.
    pub metrics: Arc<StreamMetrics>,
    /// Channel for consumer notifications.
    event_tx: mpsc::Sender<StreamEvent>,
    /// Consumer receives events here.
    event_rx: Mutex<mpsc::Receiver<StreamEvent>>,
}

impl BackpressureStream {
    /// Create a new backpressure-aware stream.
    pub fn new(config: StreamConfig) -> Self {
        let (tx, rx) = mpsc::channel(config.buffer_depth);
        Self {
            buffer: Mutex::new(VecDeque::with_capacity(config.buffer_depth)),
            sequence: AtomicU64::new(0),
            space_available: Notify::new(),
            metrics: Arc::new(StreamMetrics::default()),
            event_tx: tx,
            event_rx: Mutex::new(rx),
            config,
        }
    }

    /// Publish an event to the stream with backpressure handling.
    ///
    /// Returns the sequence number assigned, or an error if the event
    /// was dropped (only under DropNewest policy).
    pub async fn publish(
        &self,
        task_id: String,
        payload: StreamPayload,
    ) -> Result<u64, StreamError> {
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        let event = StreamEvent::new(task_id, seq, payload);

        self.metrics
            .events_produced
            .fetch_add(1, Ordering::Relaxed);

        let mut buf = self.buffer.lock().await;

        if buf.len() >= self.config.buffer_depth {
            match self.config.overflow_policy {
                OverflowPolicy::DropOldest => {
                    // Evict oldest — bounded latency, lossy.
                    if let Some(_dropped) = buf.pop_front() {
                        self.metrics
                            .events_dropped
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    buf.push_back(event.clone());
                }
                OverflowPolicy::DropNewest => {
                    // Drop incoming — bounded age, lossy.
                    self.metrics
                        .events_dropped
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(StreamError::BufferFull {
                        buffer_depth: self.config.buffer_depth,
                    });
                }
                OverflowPolicy::BlockProducer => {
                    // Drop lock, wait for space, re-acquire.
                    drop(buf);
                    self.metrics
                        .producer_blocks
                        .fetch_add(1, Ordering::Relaxed);

                    // Wait with timeout to prevent deadlock.
                    let wait_result = tokio::time::timeout(
                        self.config.sse_timeout,
                        self.space_available.notified(),
                    )
                    .await;

                    if wait_result.is_err() {
                        return Err(StreamError::BackpressureTimeout {
                            timeout: self.config.sse_timeout,
                        });
                    }

                    let mut buf = self.buffer.lock().await;
                    buf.push_back(event.clone());
                }
            }
        } else {
            buf.push_back(event.clone());
        }

        // Try to send to consumer channel (non-blocking).
        let _ = self.event_tx.try_send(event);

        Ok(seq)
    }

    /// Consume the next event from the stream.
    ///
    /// Returns `None` if the stream is closed.
    pub async fn consume(&self) -> Option<StreamEvent> {
        let mut rx = self.event_rx.lock().await;
        let event = rx.recv().await;

        if event.is_some() {
            // Notify producer that space is available.
            self.space_available.notify_one();
            self.metrics.sse_delivered.fetch_add(1, Ordering::Relaxed);
        }

        event
    }

    /// Consume with timeout — for SSE keepalive.
    pub async fn consume_timeout(&self, timeout: Duration) -> Option<StreamEvent> {
        match tokio::time::timeout(timeout, self.consume()).await {
            Ok(event) => event,
            Err(_) => None, // Timeout — caller should send keepalive.
        }
    }

    /// Drain all pending events older than `max_pending_age`.
    /// These should be re-routed to push notification channel.
    pub async fn drain_stale(&self) -> Vec<StreamEvent> {
        let mut buf = self.buffer.lock().await;
        let now = Instant::now();
        let mut stale = Vec::new();

        buf.retain(|event| {
            if now.duration_since(event.produced_at) > self.config.max_pending_age {
                stale.push(event.clone());
                false
            } else {
                true
            }
        });

        // Notify producer of freed space.
        if !stale.is_empty() {
            self.space_available.notify_waiters();
        }

        stale
    }

    /// Current buffer occupancy.
    pub async fn buffer_len(&self) -> usize {
        self.buffer.lock().await.len()
    }

    /// Snapshot of current metrics.
    pub fn metrics_snapshot(&self) -> StreamMetricsSnapshot {
        StreamMetricsSnapshot {
            events_produced: self.metrics.events_produced.load(Ordering::Relaxed),
            sse_delivered: self.metrics.sse_delivered.load(Ordering::Relaxed),
            push_delivered: self.metrics.push_delivered.load(Ordering::Relaxed),
            events_dropped: self.metrics.events_dropped.load(Ordering::Relaxed),
            producer_blocks: self.metrics.producer_blocks.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of stream metrics (non-atomic, for display/logging).
#[derive(Debug, Clone)]
pub struct StreamMetricsSnapshot {
    pub events_produced: u64,
    pub sse_delivered: u64,
    pub push_delivered: u64,
    pub events_dropped: u64,
    pub producer_blocks: u64,
}

/// Delivery ledger for exactly-once semantics across SSE and push channels.
///
/// Uses idempotency keys `(task_id, sequence)` to deduplicate.
/// Implemented as a simple HashSet for correctness; in production, can be
/// replaced with a Bloom filter (`ε < 10⁻⁶` at `≈ 30n` bits, `k = 20`).
pub struct DeliveryLedger {
    /// Set of delivered idempotency keys.
    delivered: std::collections::HashSet<String>,
    /// Maximum entries before cleanup.
    max_entries: usize,
}

impl DeliveryLedger {
    pub fn new(max_entries: usize) -> Self {
        Self {
            delivered: std::collections::HashSet::with_capacity(max_entries / 4),
            max_entries,
        }
    }

    /// Mark an event as delivered. Returns `true` if this is the first delivery
    /// (not a duplicate).
    pub fn mark_delivered(&mut self, idempotency_key: &str) -> bool {
        if self.delivered.len() >= self.max_entries {
            // Evict oldest entries (simple approach — clear half).
            let drain_count = self.max_entries / 2;
            let keys: Vec<String> = self.delivered.iter().take(drain_count).cloned().collect();
            for k in keys {
                self.delivered.remove(&k);
            }
        }
        self.delivered.insert(idempotency_key.to_string())
    }

    /// Check if an event was already delivered.
    pub fn is_delivered(&self, idempotency_key: &str) -> bool {
        self.delivered.contains(idempotency_key)
    }

    /// Number of tracked deliveries.
    pub fn len(&self) -> usize {
        self.delivered.len()
    }

    /// Whether the ledger is empty.
    pub fn is_empty(&self) -> bool {
        self.delivered.is_empty()
    }
}

/// Stream errors.
#[derive(Debug)]
pub enum StreamError {
    /// Buffer is full and overflow policy is DropNewest.
    BufferFull { buffer_depth: usize },
    /// Producer blocked too long waiting for buffer space.
    BackpressureTimeout { timeout: Duration },
    /// Stream has been closed.
    Closed,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BufferFull { buffer_depth } => {
                write!(f, "stream buffer full (depth={})", buffer_depth)
            }
            Self::BackpressureTimeout { timeout } => {
                write!(f, "backpressure timeout after {:?}", timeout)
            }
            Self::Closed => write!(f, "stream closed"),
        }
    }
}

impl std::error::Error for StreamError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_and_consume() {
        let stream = BackpressureStream::new(StreamConfig::default());
        let seq = stream
            .publish("task-1".into(), StreamPayload::TextDelta {
                delta: "hello".into(),
                done: false,
            })
            .await
            .unwrap();
        assert_eq!(seq, 0);

        let event = stream.consume().await.unwrap();
        assert_eq!(event.task_id, "task-1");
        assert_eq!(event.sequence, 0);
    }

    #[tokio::test]
    async fn drop_oldest_policy() {
        let config = StreamConfig {
            buffer_depth: 2,
            overflow_policy: OverflowPolicy::DropOldest,
            ..Default::default()
        };
        let stream = BackpressureStream::new(config);

        // Fill buffer.
        stream.publish("t".into(), StreamPayload::Ping { nonce: 1 }).await.unwrap();
        stream.publish("t".into(), StreamPayload::Ping { nonce: 2 }).await.unwrap();

        // This should drop the oldest.
        stream.publish("t".into(), StreamPayload::Ping { nonce: 3 }).await.unwrap();

        let m = stream.metrics_snapshot();
        assert_eq!(m.events_dropped, 1);
    }

    #[tokio::test]
    async fn drop_newest_policy() {
        let config = StreamConfig {
            buffer_depth: 2,
            overflow_policy: OverflowPolicy::DropNewest,
            ..Default::default()
        };
        let stream = BackpressureStream::new(config);

        stream.publish("t".into(), StreamPayload::Ping { nonce: 1 }).await.unwrap();
        stream.publish("t".into(), StreamPayload::Ping { nonce: 2 }).await.unwrap();

        let result = stream.publish("t".into(), StreamPayload::Ping { nonce: 3 }).await;
        assert!(result.is_err());
    }

    #[test]
    fn delivery_ledger_deduplication() {
        let mut ledger = DeliveryLedger::new(100);
        assert!(ledger.mark_delivered("task-1:0"));
        assert!(!ledger.mark_delivered("task-1:0")); // duplicate
        assert!(ledger.is_delivered("task-1:0"));
        assert!(!ledger.is_delivered("task-1:1"));
    }

    #[test]
    fn delivery_ledger_eviction() {
        let mut ledger = DeliveryLedger::new(4);
        for i in 0..10 {
            ledger.mark_delivered(&format!("task:{i}"));
        }
        // Should have evicted some entries.
        assert!(ledger.len() <= 6);
    }

    #[tokio::test]
    async fn metrics_tracking() {
        let stream = BackpressureStream::new(StreamConfig::default());
        for i in 0..5 {
            stream
                .publish("t".into(), StreamPayload::TextDelta {
                    delta: format!("chunk-{i}"),
                    done: i == 4,
                })
                .await
                .unwrap();
        }
        let m = stream.metrics_snapshot();
        assert_eq!(m.events_produced, 5);
    }
}
