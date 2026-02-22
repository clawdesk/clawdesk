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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Overflow strategy when a subscriber's channel is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowStrategy {
    /// Drop the newest event (the one being delivered). Default.
    DropNewest,
    /// Drop the oldest event in the channel (not trivially possible with mpsc;
    /// approximate by dropping the new event and incrementing a counter).
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
    config: SubscriberConfig,
}

/// Manages per-subscriber backpressure channels.
pub struct BackpressureManager {
    subscribers: HashMap<String, RegisteredSubscriber>,
}

impl BackpressureManager {
    pub fn new() -> Self {
        Self {
            subscribers: HashMap::new(),
        }
    }

    /// Register a new subscriber with a bounded channel.
    ///
    /// Returns the receiving end of the mpsc channel and a metrics handle.
    pub fn register(
        &mut self,
        subscriber_id: impl Into<String>,
        config: SubscriberConfig,
    ) -> (mpsc::Receiver<Vec<u8>>, Arc<SubscriberMetrics>) {
        let (tx, rx) = mpsc::channel(config.capacity);
        let metrics = Arc::new(SubscriberMetrics::new());

        let sub = RegisteredSubscriber {
            sender: tx,
            metrics: metrics.clone(),
            config,
        };

        self.subscribers.insert(subscriber_id.into(), sub);
        (rx, metrics)
    }

    /// Unregister a subscriber, dropping its channel.
    pub fn unregister(&mut self, subscriber_id: &str) -> bool {
        self.subscribers.remove(subscriber_id).is_some()
    }

    /// Deliver an event (serialised as bytes) to a specific subscriber.
    ///
    /// Returns `true` if delivered, `false` if dropped.
    pub fn try_deliver(&self, subscriber_id: &str, event: Vec<u8>) -> bool {
        let sub = match self.subscribers.get(subscriber_id) {
            Some(s) => s,
            None => return false,
        };

        match sub.sender.try_send(event) {
            Ok(()) => {
                sub.metrics.delivered.fetch_add(1, Ordering::Relaxed);
                sub.metrics.pending.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                match sub.config.overflow {
                    OverflowStrategy::DropNewest | OverflowStrategy::DropOldest => {
                        sub.metrics.dropped.fetch_add(1, Ordering::Relaxed);
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
        let sub = match self.subscribers.get(subscriber_id) {
            Some(s) => s,
            None => return false,
        };

        match sub.config.overflow {
            OverflowStrategy::Block => {
                match sub.sender.send(event).await {
                    Ok(()) => {
                        sub.metrics.delivered.fetch_add(1, Ordering::Relaxed);
                        sub.metrics.pending.fetch_add(1, Ordering::Relaxed);
                        true
                    }
                    Err(_) => {
                        sub.metrics.dropped.fetch_add(1, Ordering::Relaxed);
                        false
                    }
                }
            }
            _ => self.try_deliver(subscriber_id, event),
        }
    }

    /// Broadcast an event to all registered subscribers.
    ///
    /// Returns the number of successful deliveries.
    pub fn broadcast(&self, event: &[u8]) -> usize {
        let mut delivered = 0;
        for id in self.subscribers.keys() {
            if self.try_deliver(id, event.to_vec()) {
                delivered += 1;
            }
        }
        delivered
    }

    /// Get metrics for a specific subscriber.
    pub fn metrics(&self, subscriber_id: &str) -> Option<Arc<SubscriberMetrics>> {
        self.subscribers
            .get(subscriber_id)
            .map(|s| s.metrics.clone())
    }

    /// Get metrics snapshots for all subscribers.
    pub fn all_metrics(&self) -> Vec<SubscriberMetricsSnapshot> {
        self.subscribers
            .iter()
            .map(|(id, sub)| SubscriberMetricsSnapshot {
                subscriber_id: id.clone(),
                delivered: sub.metrics.delivered_count(),
                dropped: sub.metrics.dropped_count(),
                pending: sub.metrics.pending_count(),
                drop_rate: sub.metrics.drop_rate(),
            })
            .collect()
    }

    /// Number of registered subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    /// Mark events as consumed by a subscriber (decrement pending).
    pub fn mark_consumed(&self, subscriber_id: &str, count: u64) {
        if let Some(sub) = self.subscribers.get(subscriber_id) {
            let current = sub.metrics.pending.load(Ordering::Relaxed);
            let new = current.saturating_sub(count);
            sub.metrics.pending.store(new, Ordering::Relaxed);
        }
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

    #[test]
    fn test_register_and_deliver() {
        let mut mgr = BackpressureManager::new();
        let (mut rx, metrics) = mgr.register("sub-1", SubscriberConfig::default());

        assert!(mgr.try_deliver("sub-1", b"hello".to_vec()));
        assert_eq!(metrics.delivered_count(), 1);

        let msg = rx.try_recv().unwrap();
        assert_eq!(msg, b"hello");
    }

    #[test]
    fn test_overflow_drop_newest() {
        let mut mgr = BackpressureManager::new();
        let config = SubscriberConfig {
            capacity: 2,
            overflow: OverflowStrategy::DropNewest,
        };
        let (_rx, metrics) = mgr.register("sub-1", config);

        assert!(mgr.try_deliver("sub-1", b"msg1".to_vec()));
        assert!(mgr.try_deliver("sub-1", b"msg2".to_vec()));
        // Channel full — should drop
        assert!(!mgr.try_deliver("sub-1", b"msg3".to_vec()));

        assert_eq!(metrics.delivered_count(), 2);
        assert_eq!(metrics.dropped_count(), 1);
    }

    #[test]
    fn test_broadcast() {
        let mut mgr = BackpressureManager::new();
        let (_rx1, _) = mgr.register("sub-1", SubscriberConfig::default());
        let (_rx2, _) = mgr.register("sub-2", SubscriberConfig::default());

        let delivered = mgr.broadcast(b"event");
        assert_eq!(delivered, 2);
    }

    #[test]
    fn test_unregister() {
        let mut mgr = BackpressureManager::new();
        mgr.register("sub-1", SubscriberConfig::default());
        assert_eq!(mgr.subscriber_count(), 1);

        assert!(mgr.unregister("sub-1"));
        assert_eq!(mgr.subscriber_count(), 0);
        assert!(!mgr.try_deliver("sub-1", b"msg".to_vec()));
    }

    #[test]
    fn test_drop_rate() {
        let mut mgr = BackpressureManager::new();
        let config = SubscriberConfig {
            capacity: 1,
            overflow: OverflowStrategy::DropNewest,
        };
        let (_rx, metrics) = mgr.register("sub-1", config);

        mgr.try_deliver("sub-1", b"msg1".to_vec()); // delivered
        mgr.try_deliver("sub-1", b"msg2".to_vec()); // dropped

        assert!((metrics.drop_rate() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_metrics_snapshot() {
        let mut mgr = BackpressureManager::new();
        let (_rx1, _) = mgr.register("sub-1", SubscriberConfig::default());
        mgr.try_deliver("sub-1", b"msg".to_vec());

        let snapshots = mgr.all_metrics();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].delivered, 1);
    }

    #[tokio::test]
    async fn test_async_deliver() {
        let mut mgr = BackpressureManager::new();
        let config = SubscriberConfig {
            capacity: 256,
            overflow: OverflowStrategy::Block,
        };
        let (mut rx, metrics) = mgr.register("sub-1", config);

        assert!(mgr.deliver("sub-1", b"async-msg".to_vec()).await);
        assert_eq!(metrics.delivered_count(), 1);

        let msg = rx.recv().await.unwrap();
        assert_eq!(msg, b"async-msg");
    }
}
