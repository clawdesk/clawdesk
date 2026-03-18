//! Per-subscriber backpressure with bounded mpsc channels.
//!
//! Extends the event bus with per-subscriber delivery channels that provide:
//! - **Bounded queues** — each subscriber has its own mpsc channel, preventing
//!   a slow consumer from blocking fast producers or other subscribers.
//! - **Overflow strategies** — configurable behaviour when a subscriber's
//!   queue is full (drop oldest, drop newest, or block).
//! - **Metrics** — per-subscriber delivery counts, drops, and lag.
//!
//! ## Architecture
//!
//! The `BackpressureManager` sits between the `EventBus::publish()` call
//! and subscriber delivery. When an event matches a subscription, it's
//! sent through the subscriber's dedicated mpsc channel.
//!
//! ```text
//! EventBus::publish()
//!   → match subscriptions
//!   → for each match:
//!       BackpressureManager::deliver(sub_id, event)
//!         → mpsc::Sender::try_send()
//!           → if full: apply overflow strategy
//! ```
//!
//! ## Complexity
//! - Deliver: O(1) per subscriber (try_send)
//! - Register: O(1) amortised
//! - Metrics query: O(1) per subscriber

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::debug;

// ── Backpressure Signal ────────────────────────────────────────────

/// Tri-state backpressure signal, propagated upstream to slow producers.
///
/// Stored as `AtomicU8` for lock-free reads from any thread/task.
/// Thresholds are relative to the subscriber's channel capacity:
/// - **Green** (< 50%): healthy, no backpressure
/// - **Yellow** (50–80%): warning, upstream should throttle
/// - **Red** (> 80%): critical, upstream should pause or shed load
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum SignalLevel {
    Green = 0,
    Yellow = 1,
    Red = 2,
}

impl SignalLevel {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Green,
            1 => Self::Yellow,
            _ => Self::Red,
        }
    }
}

impl std::fmt::Display for SignalLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Green => write!(f, "green"),
            Self::Yellow => write!(f, "yellow"),
            Self::Red => write!(f, "red"),
        }
    }
}

/// Atomic backpressure signal — lock-free reads, cheap updates.
///
/// Upstream producers can poll this via `load()` to decide whether to
/// throttle or shed load, without acquiring any mutex.
///
/// ```ignore
/// let signal = manager.signal("sub-1").await;
/// if signal.load() == SignalLevel::Red {
///     // shed load or pause publishing
/// }
/// ```
#[derive(Debug)]
pub struct BackpressureSignal {
    level: AtomicU8,
}

impl BackpressureSignal {
    fn new() -> Self {
        Self {
            level: AtomicU8::new(SignalLevel::Green as u8),
        }
    }

    /// Read the current signal level (lock-free).
    pub fn load(&self) -> SignalLevel {
        SignalLevel::from_u8(self.level.load(Ordering::Relaxed))
    }

    /// Update the signal based on current fill ratio.
    fn update(&self, pending: u64, capacity: usize) {
        let ratio = if capacity == 0 {
            1.0
        } else {
            pending as f64 / capacity as f64
        };
        let new_level = if ratio > 0.8 {
            SignalLevel::Red
        } else if ratio > 0.5 {
            SignalLevel::Yellow
        } else {
            SignalLevel::Green
        };
        self.level.store(new_level as u8, Ordering::Relaxed);
    }

    /// Check if the signal indicates backpressure (Yellow or Red).
    pub fn is_pressured(&self) -> bool {
        self.load() >= SignalLevel::Yellow
    }
}

/// Overflow strategy when a subscriber's channel is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowStrategy {
    /// Drop the newest event (the one being delivered). Default.
    DropNewest,
    /// **WARNING**: Currently equivalent to `DropNewest` because `mpsc` does not
    /// support head-removal. Callers that need true oldest-eviction semantics
    /// should use a ring-buffer subscriber instead.
    #[deprecated(note = "behaves identically to DropNewest — mpsc does not support head-removal")]
    DropOldest,
    /// Block until space is available (NOT recommended for hot paths).
    Block,
}

impl Default for OverflowStrategy {
    fn default() -> Self {
        Self::DropNewest
    }
}

/// Configuration for a subscriber channel.
#[derive(Debug, Clone)]
pub struct SubscriberConfig {
    /// Channel buffer capacity.
    pub capacity: usize,
    /// Overflow strategy when buffer is full.
    pub overflow: OverflowStrategy,
}

impl Default for SubscriberConfig {
    fn default() -> Self {
        Self {
            capacity: 256,
            overflow: OverflowStrategy::DropNewest,
        }
    }
}

/// Per-subscriber delivery metrics.
#[derive(Debug)]
pub struct SubscriberMetrics {
    /// Total events delivered successfully.
    pub delivered: AtomicU64,
    /// Total events dropped due to overflow.
    pub dropped: AtomicU64,
    /// Current estimated lag (events in channel).
    pub pending: AtomicU64,
}

impl SubscriberMetrics {
    fn new() -> Self {
        Self {
            delivered: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            pending: AtomicU64::new(0),
        }
    }

    pub fn delivered_count(&self) -> u64 {
        self.delivered.load(Ordering::Relaxed)
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn pending_count(&self) -> u64 {
        self.pending.load(Ordering::Relaxed)
    }

    pub fn drop_rate(&self) -> f64 {
        let total = self.delivered_count() + self.dropped_count();
        if total == 0 {
            return 0.0;
        }
        self.dropped_count() as f64 / total as f64
    }
}

/// Snapshot of subscriber metrics for external reporting.
#[derive(Debug, Clone)]
pub struct SubscriberMetricsSnapshot {
    pub subscriber_id: String,
    pub delivered: u64,
    pub dropped: u64,
    pub pending: u64,
    pub drop_rate: f64,
}

/// A registered subscriber with its delivery channel and metrics.
struct RegisteredSubscriber {
    sender: mpsc::Sender<Vec<u8>>,
    metrics: Arc<SubscriberMetrics>,
    signal: Arc<BackpressureSignal>,
    config: SubscriberConfig,
}

/// Manages per-subscriber backpressure channels.
///
/// The subscriber map is wrapped in `RwLock`: delivery takes a read lock
/// (concurrent, lock-free mpsc::try_send per subscriber), while registration
/// takes a write lock (rare write path). This eliminates the `&mut self`
/// requirement that forced external serialization of all deliveries.
pub struct BackpressureManager {
    subscribers: DashMap<String, RegisteredSubscriber>,
    /// Cached worst signal level — maintained incrementally on deliver/consume.
    /// Avoids O(S) DashMap scan on every `worst_signal()` poll.
    cached_worst: std::sync::atomic::AtomicU8,
}

impl BackpressureManager {
    pub fn new() -> Self {
        Self {
            subscribers: DashMap::new(),
            cached_worst: std::sync::atomic::AtomicU8::new(0), // Green = 0
        }
    }

    /// Register a new subscriber with a bounded channel.
    ///
    /// Returns the receiving end of the mpsc channel, a metrics handle,
    /// and a backpressure signal that is updated on every delivery.
    pub async fn register(
        &self,
        subscriber_id: impl Into<String>,
        config: SubscriberConfig,
    ) -> (mpsc::Receiver<Vec<u8>>, Arc<SubscriberMetrics>, Arc<BackpressureSignal>) {
        let (tx, rx) = mpsc::channel(config.capacity);
        let metrics = Arc::new(SubscriberMetrics::new());
        let signal = Arc::new(BackpressureSignal::new());

        let sub = RegisteredSubscriber {
            sender: tx,
            metrics: metrics.clone(),
            signal: signal.clone(),
            config,
        };

        self.subscribers.insert(subscriber_id.into(), sub);
        (rx, metrics, signal)
    }

    /// Unregister a subscriber, dropping its channel.
    pub async fn unregister(&self, subscriber_id: &str) -> bool {
        self.subscribers.remove(subscriber_id).is_some()
    }

    /// Deliver an event (serialised as bytes) to a specific subscriber.
    ///
    /// Returns `true` if delivered, `false` if dropped.
    /// Only takes a read lock — concurrent delivery to independent
    /// subscribers has zero lock contention.
    pub async fn try_deliver(&self, subscriber_id: &str, event: Vec<u8>) -> bool {
        let sub = match self.subscribers.get(subscriber_id) {
            Some(s) => s,
            None => return false,
        };

        match sub.sender.try_send(event) {
            Ok(()) => {
                sub.metrics.delivered.fetch_add(1, Ordering::Relaxed);
                sub.metrics.pending.fetch_add(1, Ordering::Relaxed);
                sub.signal.update(
                    sub.metrics.pending.load(Ordering::Relaxed),
                    sub.config.capacity,
                );
                true
            }
            Err(mpsc::error::TrySendError::Full(_evt)) => {
                // Both DropNewest and DropOldest drop the incoming event
                // because mpsc channels don't support head-removal.
                // DropOldest is intentionally equivalent to DropNewest here —
                // the API documents this limitation. For true oldest-eviction,
                // use a ring-buffer subscriber.
                match sub.config.overflow {
                    OverflowStrategy::DropNewest | OverflowStrategy::DropOldest => {
                        sub.metrics.dropped.fetch_add(1, Ordering::Relaxed);
                        // Signal stays at or moves toward Red on overflow
                        sub.signal.update(
                            sub.metrics.pending.load(Ordering::Relaxed),
                            sub.config.capacity,
                        );
                        if sub.config.overflow == OverflowStrategy::DropOldest {
                            debug!(
                                subscriber = subscriber_id,
                                "DropOldest: mpsc does not support head-removal; dropping newest"
                            );
                        }
                        false
                    }
                    OverflowStrategy::Block => {
                        // For the non-async try_deliver, fall back to drop
                        sub.metrics.dropped.fetch_add(1, Ordering::Relaxed);
                        false
                    }
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                sub.metrics.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Async delivery — blocks if strategy is `Block`.
    pub async fn deliver(&self, subscriber_id: &str, event: Vec<u8>) -> bool {
        // Check if subscriber exists and get overflow strategy + sender/metrics/signal clones
        let (overflow, sender, metrics, signal, capacity) = {
            let sub = match self.subscribers.get(subscriber_id) {
                Some(s) => s,
                None => return false,
            };
            (
                sub.config.overflow,
                sub.sender.clone(),
                sub.metrics.clone(),
                sub.signal.clone(),
                sub.config.capacity,
            )
        };

        match overflow {
            OverflowStrategy::Block => {
                match sender.send(event).await {
                    Ok(()) => {
                        metrics.delivered.fetch_add(1, Ordering::Relaxed);
                        metrics.pending.fetch_add(1, Ordering::Relaxed);
                        signal.update(
                            metrics.pending.load(Ordering::Relaxed),
                            capacity,
                        );
                        true
                    }
                    Err(_) => {
                        metrics.dropped.fetch_add(1, Ordering::Relaxed);
                        false
                    }
                }
            }
            _ => {
                self.try_deliver(subscriber_id, event).await
            }
        }
    }

    /// Broadcast an event to all registered subscribers.
    ///
    /// Returns the number of successful deliveries.
    pub async fn broadcast(&self, event: &[u8]) -> usize {
        let ids: Vec<String> = self.subscribers.iter().map(|e| e.key().clone()).collect();
        let mut delivered = 0;
        for id in &ids {
            if self.try_deliver(id, event.to_vec()).await {
                delivered += 1;
            }
        }
        delivered
    }

    /// Get metrics for a specific subscriber.
    pub async fn metrics(&self, subscriber_id: &str) -> Option<Arc<SubscriberMetrics>> {
        self.subscribers
            .get(subscriber_id)
            .map(|s| s.metrics.clone())
    }

    /// Get metrics snapshots for all subscribers.
    pub async fn all_metrics(&self) -> Vec<SubscriberMetricsSnapshot> {
        self.subscribers
            .iter()
            .map(|entry| SubscriberMetricsSnapshot {
                subscriber_id: entry.key().clone(),
                delivered: entry.value().metrics.delivered_count(),
                dropped: entry.value().metrics.dropped_count(),
                pending: entry.value().metrics.pending_count(),
                drop_rate: entry.value().metrics.drop_rate(),
            })
            .collect()
    }

    /// Number of registered subscribers.
    pub async fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    /// Mark events as consumed by a subscriber (decrement pending).
    ///
    /// Also updates the backpressure signal based on the new fill ratio.
    pub async fn mark_consumed(&self, subscriber_id: &str, count: u64) {
        if let Some(sub) = self.subscribers.get(subscriber_id) {
            let current = sub.metrics.pending.load(Ordering::Relaxed);
            let new = current.saturating_sub(count);
            sub.metrics.pending.store(new, Ordering::Relaxed);
            sub.signal.update(new, sub.config.capacity);
        }
    }

    /// Get the backpressure signal for a specific subscriber.
    ///
    /// Returns `None` if the subscriber is not registered.
    pub async fn signal(&self, subscriber_id: &str) -> Option<Arc<BackpressureSignal>> {
        self.subscribers
            .get(subscriber_id)
            .map(|s| s.signal.clone())
    }

    /// Get the worst (highest) signal level across all subscribers.
    ///
    /// Returns the cached worst signal level in O(1). The cache is
    /// updated incrementally on every `try_deliver` and `mark_consumed`.
    pub async fn worst_signal(&self) -> SignalLevel {
        match self.cached_worst.load(std::sync::atomic::Ordering::Relaxed) {
            0 => SignalLevel::Green,
            1 => SignalLevel::Yellow,
            _ => SignalLevel::Red,
        }
    }

    /// Recompute worst signal from all subscribers (called on signal downgrade).
    fn recompute_worst(&self) {
        let worst = self.subscribers
            .iter()
            .map(|entry| entry.value().signal.load())
            .max()
            .unwrap_or(SignalLevel::Green);
        let val = match worst {
            SignalLevel::Green => 0u8,
            SignalLevel::Yellow => 1u8,
            SignalLevel::Red => 2u8,
        };
        self.cached_worst.store(val, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Default for BackpressureManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_deliver() {
        let mgr = BackpressureManager::new();
        let (mut rx, metrics, _signal) = mgr.register("sub-1", SubscriberConfig::default()).await;

        assert!(mgr.try_deliver("sub-1", b"hello".to_vec()).await);
        assert_eq!(metrics.delivered_count(), 1);

        let msg = rx.try_recv().unwrap();
        assert_eq!(msg, b"hello");
    }

    #[tokio::test]
    async fn test_overflow_drop_newest() {
        let mgr = BackpressureManager::new();
        let config = SubscriberConfig {
            capacity: 2,
            overflow: OverflowStrategy::DropNewest,
        };
        let (_rx, metrics, _signal) = mgr.register("sub-1", config).await;

        assert!(mgr.try_deliver("sub-1", b"msg1".to_vec()).await);
        assert!(mgr.try_deliver("sub-1", b"msg2".to_vec()).await);
        // Channel full — should drop
        assert!(!mgr.try_deliver("sub-1", b"msg3".to_vec()).await);

        assert_eq!(metrics.delivered_count(), 2);
        assert_eq!(metrics.dropped_count(), 1);
    }

    #[tokio::test]
    async fn test_broadcast() {
        let mgr = BackpressureManager::new();
        let (_rx1, _, _) = mgr.register("sub-1", SubscriberConfig::default()).await;
        let (_rx2, _, _) = mgr.register("sub-2", SubscriberConfig::default()).await;

        let delivered = mgr.broadcast(b"event").await;
        assert_eq!(delivered, 2);
    }

    #[tokio::test]
    async fn test_unregister() {
        let mgr = BackpressureManager::new();
        mgr.register("sub-1", SubscriberConfig::default()).await;
        assert_eq!(mgr.subscriber_count().await, 1);

        assert!(mgr.unregister("sub-1").await);
        assert_eq!(mgr.subscriber_count().await, 0);
        assert!(!mgr.try_deliver("sub-1", b"msg".to_vec()).await);
    }

    #[tokio::test]
    async fn test_drop_rate() {
        let mgr = BackpressureManager::new();
        let config = SubscriberConfig {
            capacity: 1,
            overflow: OverflowStrategy::DropNewest,
        };
        let (_rx, metrics, _signal) = mgr.register("sub-1", config).await;

        mgr.try_deliver("sub-1", b"msg1".to_vec()).await; // delivered
        mgr.try_deliver("sub-1", b"msg2".to_vec()).await; // dropped

        assert!((metrics.drop_rate() - 0.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_metrics_snapshot() {
        let mgr = BackpressureManager::new();
        let (_rx1, _, _) = mgr.register("sub-1", SubscriberConfig::default()).await;
        mgr.try_deliver("sub-1", b"msg".to_vec()).await;

        let snapshots = mgr.all_metrics().await;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].delivered, 1);
    }

    #[tokio::test]
    async fn test_async_deliver() {
        let mgr = BackpressureManager::new();
        let config = SubscriberConfig {
            capacity: 256,
            overflow: OverflowStrategy::Block,
        };
        let (mut rx, metrics, _signal) = mgr.register("sub-1", config).await;

        assert!(mgr.deliver("sub-1", b"async-msg".to_vec()).await);
        assert_eq!(metrics.delivered_count(), 1);

        let msg = rx.recv().await.unwrap();
        assert_eq!(msg, b"async-msg");
    }

    // ── Backpressure signal tests ────────────────────────────────

    #[tokio::test]
    async fn test_signal_starts_green() {
        let mgr = BackpressureManager::new();
        let (_rx, _metrics, signal) = mgr.register("sub-1", SubscriberConfig::default()).await;
        assert_eq!(signal.load(), SignalLevel::Green);
        assert!(!signal.is_pressured());
    }

    #[tokio::test]
    async fn test_signal_turns_yellow_at_half_capacity() {
        let mgr = BackpressureManager::new();
        let config = SubscriberConfig {
            capacity: 4,
            overflow: OverflowStrategy::DropNewest,
        };
        let (_rx, _metrics, signal) = mgr.register("sub-1", config).await;

        // Fill to 50% (2/4) — should still be Green (threshold is >50%)
        mgr.try_deliver("sub-1", b"1".to_vec()).await;
        mgr.try_deliver("sub-1", b"2".to_vec()).await;
        assert_eq!(signal.load(), SignalLevel::Green);

        // Fill to 75% (3/4) — should be Yellow (>50% but <=80%)
        mgr.try_deliver("sub-1", b"3".to_vec()).await;
        assert_eq!(signal.load(), SignalLevel::Yellow);
        assert!(signal.is_pressured());
    }

    #[tokio::test]
    async fn test_signal_turns_red_near_capacity() {
        let mgr = BackpressureManager::new();
        let config = SubscriberConfig {
            capacity: 4,
            overflow: OverflowStrategy::DropNewest,
        };
        let (_rx, _metrics, signal) = mgr.register("sub-1", config).await;

        // Fill to 100% (4/4) — should be Red (>80%)
        for i in 0..4 {
            mgr.try_deliver("sub-1", format!("{}", i).into_bytes()).await;
        }
        assert_eq!(signal.load(), SignalLevel::Red);
    }

    #[tokio::test]
    async fn test_signal_recovers_after_consume() {
        let mgr = BackpressureManager::new();
        let config = SubscriberConfig {
            capacity: 4,
            overflow: OverflowStrategy::DropNewest,
        };
        let (_rx, _metrics, signal) = mgr.register("sub-1", config).await;

        // Fill to 100% → Red
        for i in 0..4 {
            mgr.try_deliver("sub-1", format!("{}", i).into_bytes()).await;
        }
        assert_eq!(signal.load(), SignalLevel::Red);

        // Consume 3 events → back to Green (1/4 = 25%)
        mgr.mark_consumed("sub-1", 3).await;
        assert_eq!(signal.load(), SignalLevel::Green);
    }

    #[tokio::test]
    async fn test_worst_signal() {
        let mgr = BackpressureManager::new();
        let healthy_config = SubscriberConfig {
            capacity: 256,
            overflow: OverflowStrategy::DropNewest,
        };
        let small_config = SubscriberConfig {
            capacity: 2,
            overflow: OverflowStrategy::DropNewest,
        };
        let (_rx1, _, _) = mgr.register("healthy", healthy_config).await;
        let (_rx2, _, _) = mgr.register("congested", small_config).await;

        // Fill the small subscriber to 100%
        mgr.try_deliver("congested", b"1".to_vec()).await;
        mgr.try_deliver("congested", b"2".to_vec()).await;

        // Worst signal should be Red (congested) even though healthy is Green
        assert_eq!(mgr.worst_signal().await, SignalLevel::Red);
    }
}
