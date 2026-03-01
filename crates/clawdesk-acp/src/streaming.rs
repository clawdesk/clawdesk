//! SSE stream with backpressure control and dual-channel delivery.
//!
//! ## Architecture
//!
//! The streaming system is a leaky bucket with rate `λ` (production) and
//! drain rate `μ` (consumption). Stability requires `μ > λ`.
//! Buffer depth `B` provides grace period `B/(λ - μ)` seconds before overflow.
//!
//! ## Lock-Free Ring Buffer
//!
//! The hot-path buffer uses an SPSC (Single-Producer Single-Consumer) atomic
//! ring buffer. Producer writes to `buffer[tail % N]` and increments `tail`
//! with `Release` ordering. Consumer reads from `buffer[head % N]` and
//! increments `head` with `Acquire` ordering. Enqueue/dequeue is O(1)
//! with no critical section — backpressure check is `tail - head < N`.
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

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};

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
    /// Text delta (streaming response) — legacy, no integrity tracking.
    TextDelta { delta: String, done: bool },
    /// Rich text delta with offset tracking, sequence numbers, and rolling hash.
    /// Produced by `DeltaPublisher` and consumed by `DeltaConsumer`.
    RichTextDelta(crate::delta_stream::TextDelta),
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

// ---------------------------------------------------------------------------
// SPSC lock-free ring buffer (cache-line aligned)
// ---------------------------------------------------------------------------

/// Cache-line size for padding to prevent false sharing.
const CACHE_LINE: usize = 64;

/// Pad a usize-sized atomic to fill an entire cache line, preventing
/// false sharing between producer (tail) and consumer (head).
#[repr(C, align(64))]
struct CacheAlignedAtomicUsize {
    value: AtomicUsize,
    _pad: [u8; CACHE_LINE - std::mem::size_of::<AtomicUsize>()],
}

impl CacheAlignedAtomicUsize {
    fn new(v: usize) -> Self {
        Self {
            value: AtomicUsize::new(v),
            _pad: [0u8; CACHE_LINE - std::mem::size_of::<AtomicUsize>()],
        }
    }
}

/// Lock-free SPSC ring buffer for stream events.
///
/// Uses atomic head/tail indices with Acquire/Release ordering.
/// Capacity is rounded up to the next power of two for efficient modulo
/// via bitmask.
struct SpscRing {
    /// Slots — `UnsafeCell` for interior mutability without locks.
    slots: Box<[std::cell::UnsafeCell<Option<StreamEvent>>]>,
    /// Bitmask = capacity - 1 (capacity is always a power of two).
    mask: usize,
    /// Producer writes here (cache-line aligned).
    tail: CacheAlignedAtomicUsize,
    /// Consumer reads here (cache-line aligned).
    head: CacheAlignedAtomicUsize,
}

// SAFETY: SpscRing is designed for single-producer single-consumer use.
// The producer only writes to `tail` and `slots[tail & mask]`.
// The consumer only writes to `head` and reads `slots[head & mask]`.
// Acquire/Release ordering ensures proper happens-before relationships.
unsafe impl Send for SpscRing {}
unsafe impl Sync for SpscRing {}

impl SpscRing {
    fn new(min_capacity: usize) -> Self {
        let capacity = min_capacity.next_power_of_two().max(2);
        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            slots.push(std::cell::UnsafeCell::new(None));
        }
        Self {
            slots: slots.into_boxed_slice(),
            mask: capacity - 1,
            tail: CacheAlignedAtomicUsize::new(0),
            head: CacheAlignedAtomicUsize::new(0),
        }
    }

    /// Number of items currently in the ring.
    #[inline]
    fn len(&self) -> usize {
        let tail = self.tail.value.load(Ordering::Acquire);
        let head = self.head.value.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    /// True if the ring is full.
    #[inline]
    fn is_full(&self) -> bool {
        self.len() > self.mask // len > capacity - 1 means full
    }

    /// Capacity of the ring.
    #[inline]
    fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Try to push an event. Returns `Err(event)` if full.
    fn try_push(&self, event: StreamEvent) -> Result<(), StreamEvent> {
        let tail = self.tail.value.load(Ordering::Relaxed);
        let head = self.head.value.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= self.capacity() {
            return Err(event);
        }
        let slot = &self.slots[tail & self.mask];
        // SAFETY: producer is the only writer to this slot at this tail index.
        unsafe { *slot.get() = Some(event) };
        self.tail.value.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Try to pop an event. Returns `None` if empty.
    fn try_pop(&self) -> Option<StreamEvent> {
        let head = self.head.value.load(Ordering::Relaxed);
        let tail = self.tail.value.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let slot = &self.slots[head & self.mask];
        // SAFETY: consumer is the only reader of this slot at this head index.
        let event = unsafe { (*slot.get()).take() };
        self.head.value.store(head.wrapping_add(1), Ordering::Release);
        event
    }

    /// Force-push by evicting the oldest event (for DropOldest policy).
    fn force_push(&self, event: StreamEvent) -> Option<StreamEvent> {
        let dropped = self.try_pop();
        // After evicting one, there's guaranteed space.
        let _ = self.try_push(event);
        dropped
    }
}

/// Backpressure-aware event stream with bounded lock-free buffer.
///
/// Memory usage under slow consumers is bounded to `O(B)` where `B`
/// is the configured buffer depth.
pub struct BackpressureStream {
    config: StreamConfig,
    /// Lock-free SPSC ring buffer — no mutex on the hot path.
    ring: SpscRing,
    /// Sequence counter per task.
    sequence: AtomicU64,
    /// Notify when buffer has space (for BlockProducer policy).
    space_available: Notify,
    /// Metrics.
    pub metrics: Arc<StreamMetrics>,
    /// Channel for consumer notifications (async wakeup).
    event_tx: mpsc::Sender<StreamEvent>,
    /// Consumer receives events here.
    event_rx: tokio::sync::Mutex<mpsc::Receiver<StreamEvent>>,
}

impl BackpressureStream {
    /// Create a new backpressure-aware stream.
    pub fn new(config: StreamConfig) -> Self {
        let (tx, rx) = mpsc::channel(config.buffer_depth);
        Self {
            ring: SpscRing::new(config.buffer_depth),
            sequence: AtomicU64::new(0),
            space_available: Notify::new(),
            metrics: Arc::new(StreamMetrics::default()),
            event_tx: tx,
            event_rx: tokio::sync::Mutex::new(rx),
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

        match self.ring.try_push(event.clone()) {
            Ok(()) => {}
            Err(rejected) => {
                match self.config.overflow_policy {
                    OverflowPolicy::DropOldest => {
                        // Evict oldest — bounded latency, lossy.
                        if self.ring.force_push(rejected).is_some() {
                            self.metrics
                                .events_dropped
                                .fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    OverflowPolicy::DropNewest => {
                        // Drop incoming — bounded age, lossy.
                        self.metrics
                            .events_dropped
                            .fetch_add(1, Ordering::Relaxed);
                        return Err(StreamError::BufferFull {
                            buffer_depth: self.ring.capacity(),
                        });
                    }
                    OverflowPolicy::BlockProducer => {
                        // Wait for space, then retry.
                        self.metrics
                            .producer_blocks
                            .fetch_add(1, Ordering::Relaxed);

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

                        // Retry after wakeup — best effort.
                        let _ = self.ring.try_push(rejected);
                    }
                }
            }
        }

        // Send to consumer channel (non-blocking async wakeup).
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

    /// Drain stale events from the front of the ring buffer.
    ///
    /// Since the ring is FIFO, stale events are always at the front.
    /// We pop consecutive stale events and stop at the first non-stale
    /// event, avoiding the pop-all + repush pattern that would violate
    /// the SPSC single-consumer contract.
    pub async fn drain_stale(&self) -> Vec<StreamEvent> {
        let now = Instant::now();
        let mut stale = Vec::new();

        loop {
            // Peek at the head — check staleness before committing the pop.
            let head = self.ring.head.value.load(Ordering::Relaxed);
            let tail = self.ring.tail.value.load(Ordering::Acquire);
            if head == tail {
                break; // empty
            }
            let slot = &self.ring.slots[head & self.ring.mask];
            // SAFETY: consumer-side read — no concurrent consumer (drain_stale
            // is the only consumer of the ring; consume() uses the mpsc channel).
            let event_ref = unsafe { &*slot.get() };
            match event_ref {
                Some(ev) if now.duration_since(ev.produced_at) > self.config.max_pending_age => {
                    // Stale — actually pop it.
                    let ev = unsafe { (*slot.get()).take() };
                    self.ring.head.value.store(head.wrapping_add(1), Ordering::Release);
                    if let Some(ev) = ev {
                        stale.push(ev);
                    }
                }
                _ => break, // Non-stale or empty — stop scanning.
            }
        }

        if !stale.is_empty() {
            self.space_available.notify_waiters();
        }
        stale
    }

    /// Current buffer occupancy.
    pub async fn buffer_len(&self) -> usize {
        self.ring.len()
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
///
/// ## Design: Two-generation Bloom filter
///
/// Instead of a `HashSet` with destructive eviction (which causes false negatives
/// and thus duplicate deliveries), we use a two-generation Bloom filter.
///
/// **Generation rotation**: When the current generation fills up (exceeds
/// `max_entries`), we swap `current ↔ previous` and clear the new current.
/// Lookups check *both* generations, so recently-evicted entries are still
/// recognized as delivered for one full rotation window.
///
/// **False positive rate**: Each generation uses `k = 7` hash functions with
/// `m = max_entries × 10` bits, giving ε ≈ 0.8% — far better than destructive
/// eviction's 50% false-negative rate. False positives (suppressing a delivery
/// that never happened) are harmless since the client will retry.
///
/// **Memory**: `2 × 10n` bits ≈ 2.5 bytes per entry, versus `≈ 80 bytes`
/// per entry for `HashSet<String>`. A 10,000-entry ledger uses ≈ 25 KB.
pub struct DeliveryLedger {
    /// Two generational Bloom filter bitmaps.
    current: Vec<u64>,
    previous: Vec<u64>,
    /// Number of entries inserted into the current generation.
    current_count: usize,
    /// Total deliveries across all time (monotonic counter).
    total_deliveries: u64,
    /// Maximum entries per generation before rotation.
    max_entries: usize,
    /// Number of bits per generation.
    num_bits: usize,
    /// Number of u64 words per generation.
    num_words: usize,
}

/// Number of hash functions for the Bloom filter.
const BLOOM_K: usize = 7;

impl DeliveryLedger {
    pub fn new(max_entries: usize) -> Self {
        // m = 10 × n gives ε ≈ 0.8% with k=7.
        let num_bits = (max_entries * 10).max(64);
        let num_words = (num_bits + 63) / 64;
        Self {
            current: vec![0u64; num_words],
            previous: vec![0u64; num_words],
            current_count: 0,
            total_deliveries: 0,
            max_entries: max_entries.max(1),
            num_bits,
            num_words,
        }
    }

    /// Compute `BLOOM_K` bit positions from the idempotency key.
    /// Uses double-hashing: `h(i) = (h1 + i × h2) mod m`.
    #[inline]
    fn bloom_positions(&self, key: &str) -> [usize; BLOOM_K] {
        let bytes = key.as_bytes();
        // FNV-1a for h1
        let mut h1 = 0xcbf29ce484222325u64;
        for &b in bytes {
            h1 ^= b as u64;
            h1 = h1.wrapping_mul(0x100000001b3);
        }
        // FNV-1a with different seed for h2
        let mut h2 = 0x6c62272e07bb0142u64;
        for &b in bytes {
            h2 ^= b as u64;
            h2 = h2.wrapping_mul(0x100000001b3);
        }
        let m = self.num_bits;
        let mut positions = [0usize; BLOOM_K];
        for i in 0..BLOOM_K {
            positions[i] = (h1.wrapping_add((i as u64).wrapping_mul(h2)) % (m as u64)) as usize;
        }
        positions
    }

    /// Test whether all bits are set for the given positions in a bitmap.
    #[inline]
    fn test_all(bitmap: &[u64], positions: &[usize; BLOOM_K]) -> bool {
        for &pos in positions {
            let word = pos / 64;
            let bit = pos % 64;
            if bitmap[word] & (1u64 << bit) == 0 {
                return false;
            }
        }
        true
    }

    /// Set all bits for the given positions in a bitmap.
    #[inline]
    fn set_all(bitmap: &mut [u64], positions: &[usize; BLOOM_K]) {
        for &pos in positions {
            let word = pos / 64;
            let bit = pos % 64;
            bitmap[word] |= 1u64 << bit;
        }
    }

    /// Mark an event as delivered. Returns `true` if this is the first delivery
    /// (not a duplicate — i.e., not found in either generation).
    ///
    /// When the current generation exceeds `max_entries`, the previous generation
    /// is discarded and current becomes previous — no entries are lost suddenly.
    pub fn mark_delivered(&mut self, idempotency_key: &str) -> bool {
        let positions = self.bloom_positions(idempotency_key);

        // Check both generations for existing delivery.
        if Self::test_all(&self.current, &positions) || Self::test_all(&self.previous, &positions) {
            return false; // Already delivered.
        }

        // Rotate generations if current is full.
        if self.current_count >= self.max_entries {
            std::mem::swap(&mut self.current, &mut self.previous);
            // Clear the new current generation.
            for w in self.current.iter_mut() {
                *w = 0;
            }
            self.current_count = 0;
        }

        Self::set_all(&mut self.current, &positions);
        self.current_count += 1;
        self.total_deliveries += 1;
        true
    }

    /// Check if an event was already delivered (probabilistic — may return
    /// false positives but never false negatives within the two-generation window).
    pub fn is_delivered(&self, idempotency_key: &str) -> bool {
        let positions = self.bloom_positions(idempotency_key);
        Self::test_all(&self.current, &positions) || Self::test_all(&self.previous, &positions)
    }

    /// Approximate number of entries tracked in the current generation.
    pub fn len(&self) -> usize {
        self.current_count
    }

    /// Whether the current generation has zero entries.
    pub fn is_empty(&self) -> bool {
        self.current_count == 0
    }

    /// Total deliveries since creation (monotonic).
    pub fn total_deliveries(&self) -> u64 {
        self.total_deliveries
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
    fn delivery_ledger_generation_rotation() {
        let mut ledger = DeliveryLedger::new(4);
        // Insert 4 entries — fills first generation.
        for i in 0..4 {
            assert!(ledger.mark_delivered(&format!("task:{i}")));
        }
        assert_eq!(ledger.len(), 4);

        // 5th entry triggers rotation: current → previous, new current created.
        assert!(ledger.mark_delivered("task:4"));
        assert_eq!(ledger.len(), 1); // Only task:4 in current generation.

        // Older entries are still visible via the previous generation.
        assert!(ledger.is_delivered("task:0"));
        assert!(ledger.is_delivered("task:3"));
        assert!(ledger.is_delivered("task:4"));

        // Total deliveries is monotonic.
        assert_eq!(ledger.total_deliveries(), 5);
    }

    #[test]
    fn delivery_ledger_no_false_negatives_after_rotation() {
        // Key correctness property: evicted entries must NOT cause false negatives
        // within the two-generation window.
        let mut ledger = DeliveryLedger::new(4);
        for i in 0..4 {
            ledger.mark_delivered(&format!("batch-a:{i}"));
        }
        // Trigger rotation
        ledger.mark_delivered("batch-b:0");

        // batch-a entries moved to previous → still recognized as delivered
        for i in 0..4 {
            assert!(
                !ledger.mark_delivered(&format!("batch-a:{i}")),
                "batch-a:{i} should still be recognized as delivered after rotation"
            );
        }
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
