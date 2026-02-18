//! Persistent outbound dispatch queue with priority and retry semantics.
//!
//! Uses per-priority-level MPSC channels instead of a global `Mutex<BinaryHeap>`.
//! Enqueue is O(1) (tokio channel send, no lock contention between producers).
//! Dequeue polls priority levels in order: Critical → High → Normal → Low.
//!
//! ## Delay heap
//!
//! Items with `deliver_after` in the future are placed in a `BinaryHeap`
//! sorted by delivery time (min-heap). The dispatch loop uses `sleep_until`
//! on the soonest delayed item instead of busy-polling / re-enqueue spin.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, warn};
use uuid::Uuid;

/// Priority levels for outbound messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum OutboundPriority {
    Low = 0,
    Normal = 1,
    High = 2,
    Critical = 3,
}

/// An item in the outbound dispatch queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundItem {
    pub id: Uuid,
    pub channel: String,
    pub recipient: String,
    pub body: String,
    pub priority: OutboundPriority,
    pub created_at: DateTime<Utc>,
    pub deliver_after: DateTime<Utc>,
    pub attempts: u32,
    pub max_attempts: u32,
    pub last_error: Option<String>,
}

// BinaryHeap ordering removed for priority channels — FIFO within each
// priority is natural from MPSC. Inter-priority ordering handled by polling
// channels in priority order.
//
// Delayed items (deliver_after > now) use a separate min-heap sorted by
// delivery time, avoiding the re-enqueue spin loop.

/// Wrapper for items in the delay heap, ordered by `deliver_after` (soonest first).
struct DelayedItem(OutboundItem);

impl PartialEq for DelayedItem {
    fn eq(&self, other: &Self) -> bool {
        self.0.deliver_after == other.0.deliver_after
    }
}

impl Eq for DelayedItem {}

impl PartialOrd for DelayedItem {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for DelayedItem {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // Reverse: BinaryHeap is max-heap, we want min (soonest first).
        other.0.deliver_after.cmp(&self.0.deliver_after)
    }
}

/// Delivery handler function.
#[async_trait::async_trait]
pub trait DeliveryHandler: Send + Sync {
    async fn deliver(&self, item: &OutboundItem) -> Result<(), String>;
}

/// Configuration for the dispatch queue.
#[derive(Debug, Clone)]
pub struct DispatchConfig {
    /// Maximum queue depth before backpressure.
    pub max_depth: usize,
    /// Base retry delay (doubles each attempt).
    pub base_retry_delay: Duration,
    /// Maximum retry delay cap.
    pub max_retry_delay: Duration,
    /// How often to poll the queue (when not notified).
    pub poll_interval: Duration,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            max_depth: 10_000,
            base_retry_delay: Duration::from_secs(1),
            max_retry_delay: Duration::from_secs(300),
            poll_interval: Duration::from_secs(5),
        }
    }
}

/// Persistent outbound dispatch queue with per-priority channels.
///
/// Each priority level has its own unbounded MPSC channel:
/// - Enqueue: O(1) channel send (no global lock)
/// - Dequeue: polls Critical → High → Normal → Low in order
/// - Contention: zero between producers on different priorities,
///   minimal between producers on the same priority (lock-free MPSC)
pub struct DispatchQueue {
    /// Per-priority senders.
    tx_critical: mpsc::UnboundedSender<OutboundItem>,
    tx_high: mpsc::UnboundedSender<OutboundItem>,
    tx_normal: mpsc::UnboundedSender<OutboundItem>,
    tx_low: mpsc::UnboundedSender<OutboundItem>,
    /// Per-priority receivers (wrapped for dispatch loop).
    rx_critical: tokio::sync::Mutex<mpsc::UnboundedReceiver<OutboundItem>>,
    rx_high: tokio::sync::Mutex<mpsc::UnboundedReceiver<OutboundItem>>,
    rx_normal: tokio::sync::Mutex<mpsc::UnboundedReceiver<OutboundItem>>,
    rx_low: tokio::sync::Mutex<mpsc::UnboundedReceiver<OutboundItem>>,
    /// Delay heap for items with deliver_after > now.
    delayed: Mutex<BinaryHeap<DelayedItem>>,
    notify: Arc<Notify>,
    config: DispatchConfig,
    /// Approximate depth counter (not lock-protected; best-effort).
    depth: std::sync::atomic::AtomicUsize,
}

impl DispatchQueue {
    pub fn new(config: DispatchConfig) -> Self {
        let (tx_c, rx_c) = mpsc::unbounded_channel();
        let (tx_h, rx_h) = mpsc::unbounded_channel();
        let (tx_n, rx_n) = mpsc::unbounded_channel();
        let (tx_l, rx_l) = mpsc::unbounded_channel();
        Self {
            tx_critical: tx_c,
            tx_high: tx_h,
            tx_normal: tx_n,
            tx_low: tx_l,
            rx_critical: tokio::sync::Mutex::new(rx_c),
            rx_high: tokio::sync::Mutex::new(rx_h),
            rx_normal: tokio::sync::Mutex::new(rx_n),
            rx_low: tokio::sync::Mutex::new(rx_l),
            delayed: Mutex::new(BinaryHeap::new()),
            notify: Arc::new(Notify::new()),
            config,
            depth: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Enqueue an outbound item. Returns false if queue is at capacity.
    /// O(1) channel send — no global lock.
    pub async fn enqueue(&self, item: OutboundItem) -> bool {
        let current_depth = self.depth.load(std::sync::atomic::Ordering::Relaxed);
        if current_depth >= self.config.max_depth {
            warn!(
                queue_depth = current_depth,
                "dispatch queue at capacity, rejecting"
            );
            return false;
        }

        let tx = match item.priority {
            OutboundPriority::Critical => &self.tx_critical,
            OutboundPriority::High => &self.tx_high,
            OutboundPriority::Normal => &self.tx_normal,
            OutboundPriority::Low => &self.tx_low,
        };

        if tx.send(item).is_ok() {
            self.depth.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.notify.notify_one();
            true
        } else {
            false
        }
    }

    /// Enqueue a simple text message with default retry.
    pub async fn send(
        &self,
        channel: &str,
        recipient: &str,
        body: &str,
        priority: OutboundPriority,
    ) -> bool {
        let item = OutboundItem {
            id: Uuid::new_v4(),
            channel: channel.to_string(),
            recipient: recipient.to_string(),
            body: body.to_string(),
            priority,
            created_at: Utc::now(),
            deliver_after: Utc::now(),
            attempts: 0,
            max_attempts: 5,
            last_error: None,
        };
        self.enqueue(item).await
    }

    /// Take the next ready item, polling priority levels in order.
    /// Critical → High → Normal → Low (weighted fair queuing).
    /// Items with deliver_after in the future go to the delay heap.
    pub async fn take_ready(&self) -> Option<OutboundItem> {
        // First, drain any matured items from the delay heap back into channels.
        {
            let mut delayed = self.delayed.lock().await;
            let now = Utc::now();
            while let Some(top) = delayed.peek() {
                if top.0.deliver_after <= now {
                    let item = delayed.pop().unwrap().0;
                    let tx = match item.priority {
                        OutboundPriority::Critical => &self.tx_critical,
                        OutboundPriority::High => &self.tx_high,
                        OutboundPriority::Normal => &self.tx_normal,
                        OutboundPriority::Low => &self.tx_low,
                    };
                    let _ = tx.send(item);
                } else {
                    break;
                }
            }
        }

        // Try each priority level in order.
        let now = Utc::now();
        macro_rules! try_recv {
            ($rx:expr, $tx:expr) => {{
                let mut rx = $rx.lock().await;
                if let Ok(item) = rx.try_recv() {
                    if item.deliver_after <= now {
                        self.depth
                            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        return Some(item);
                    }
                    // Not ready — park in delay heap (no re-enqueue spin).
                    self.delayed.lock().await.push(DelayedItem(item));
                }
            }};
        }

        try_recv!(self.rx_critical, self.tx_critical);
        try_recv!(self.rx_high, self.tx_high);
        try_recv!(self.rx_normal, self.tx_normal);
        try_recv!(self.rx_low, self.tx_low);

        None
    }

    /// Duration until the soonest delayed item is ready, if any.
    /// Returns `None` if the delay heap is empty.
    pub async fn time_until_next_delayed(&self) -> Option<Duration> {
        let delayed = self.delayed.lock().await;
        delayed.peek().map(|top| {
            let now = Utc::now();
            if top.0.deliver_after <= now {
                Duration::ZERO
            } else {
                (top.0.deliver_after - now).to_std().unwrap_or(Duration::ZERO)
            }
        })
    }

    /// Requeue a failed item with exponential backoff.
    pub async fn retry(&self, mut item: OutboundItem, error: &str) {
        item.attempts += 1;
        item.last_error = Some(error.to_string());

        if item.attempts >= item.max_attempts {
            warn!(
                id = %item.id,
                attempts = item.attempts,
                "dispatch item exceeded max attempts, dropping"
            );
            return;
        }

        let delay_secs = self.config.base_retry_delay.as_secs() * 2u64.pow(item.attempts - 1);
        let delay = Duration::from_secs(delay_secs.min(self.config.max_retry_delay.as_secs()));
        item.deliver_after = Utc::now() + chrono::Duration::from_std(delay).unwrap_or_default();

        debug!(
            id = %item.id,
            attempt = item.attempts,
            delay_secs = delay.as_secs(),
            "requeuing dispatch item with backoff"
        );
        self.enqueue(item).await;
    }

    /// Run the dispatch loop, processing items as they become ready.
    /// Uses sleep_until on the soonest delayed item instead of fixed polling.
    pub async fn run(
        &self,
        handler: Arc<dyn DeliveryHandler>,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        loop {
            // Determine how long to sleep: either until next delayed item
            // or the default poll interval, whichever is sooner.
            let sleep_dur = self
                .time_until_next_delayed()
                .await
                .map(|d| d.min(self.config.poll_interval))
                .unwrap_or(self.config.poll_interval);

            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("dispatch queue shutting down");
                    break;
                }
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep(sleep_dur) => {}
            }

            while let Some(item) = self.take_ready().await {
                let id = item.id;
                match handler.deliver(&item).await {
                    Ok(()) => {
                        debug!(id = %id, "dispatch: delivered successfully");
                    }
                    Err(e) => {
                        warn!(id = %id, error = %e, "dispatch: delivery failed");
                        self.retry(item, &e).await;
                    }
                }
            }
        }
    }

    /// Current queue depth (approximate, lock-free read).
    pub async fn depth(&self) -> usize {
        self.depth.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_enqueue_and_take() {
        let q = DispatchQueue::new(DispatchConfig::default());
        q.send("telegram", "user-1", "hello", OutboundPriority::Normal)
            .await;
        assert_eq!(q.depth().await, 1);
        let item = q.take_ready().await;
        assert!(item.is_some());
        assert_eq!(item.unwrap().body, "hello");
        assert_eq!(q.depth().await, 0);
    }

    #[tokio::test]
    async fn test_priority_ordering() {
        let q = DispatchQueue::new(DispatchConfig::default());
        q.send("ch", "u", "low", OutboundPriority::Low).await;
        q.send("ch", "u", "critical", OutboundPriority::Critical)
            .await;
        q.send("ch", "u", "normal", OutboundPriority::Normal).await;

        let first = q.take_ready().await.unwrap();
        assert_eq!(first.body, "critical");
        let second = q.take_ready().await.unwrap();
        assert_eq!(second.body, "normal");
        let third = q.take_ready().await.unwrap();
        assert_eq!(third.body, "low");
    }

    #[tokio::test]
    async fn test_retry_backoff() {
        let q = DispatchQueue::new(DispatchConfig {
            base_retry_delay: Duration::from_millis(10),
            ..Default::default()
        });
        let item = OutboundItem {
            id: Uuid::new_v4(),
            channel: "test".to_string(),
            recipient: "u".to_string(),
            body: "retry me".to_string(),
            priority: OutboundPriority::Normal,
            created_at: Utc::now(),
            deliver_after: Utc::now(),
            attempts: 0,
            max_attempts: 3,
            last_error: None,
        };
        // First retry.
        q.retry(item, "connection error").await;
        assert_eq!(q.depth().await, 1);
    }

    #[tokio::test]
    async fn test_capacity_limit() {
        let q = DispatchQueue::new(DispatchConfig {
            max_depth: 2,
            ..Default::default()
        });
        assert!(q.send("ch", "u", "1", OutboundPriority::Normal).await);
        assert!(q.send("ch", "u", "2", OutboundPriority::Normal).await);
        assert!(!q.send("ch", "u", "3", OutboundPriority::Normal).await);
    }
}
