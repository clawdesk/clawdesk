//! Notification Priority Queue with Aging and Throttling
//!
//! Extends the proactive orchestrator with priority-based delivery
//! and notification fatigue prevention.
//!
//! ## Priority Model
//!
//! ```text
//! p(n) = w_urgency × u(n) + w_relevance × r(n) + w_age × age(n)/τ
//! ```
//!
//! Notifications with `p(n) > θ_push` delivered immediately;
//! others batch at digest intervals.
//!
//! ## Token Bucket Throttling
//!
//! Capacity B = max_notifications_per_hour, refill B/3600 tokens/sec.
//! When throughput exceeds user-defined max, θ_push increases dynamically.

use serde::{Deserialize, Serialize};
use std::collections::BinaryHeap;
use std::cmp::Ordering;
use std::time::{Duration, Instant};

/// A notification with computed priority for the queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrioritizedNotification {
    /// Notification content
    pub type_id: String,
    pub type_name: String,
    pub title: String,
    pub body: String,
    /// Priority score (higher = more urgent)
    pub priority: f64,
    /// Urgency component (0.0 – 1.0)
    pub urgency: f64,
    /// Relevance component (0.0 – 1.0)
    pub relevance: f64,
    /// Age in seconds since creation
    pub age_secs: u64,
    /// Whether to deliver immediately or batch
    pub immediate: bool,
    /// Preferred delivery channel
    pub channel: DeliveryChannel,
    /// Creation timestamp (epoch ms)
    pub created_at: u64,
}

impl PartialEq for PrioritizedNotification {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}
impl Eq for PrioritizedNotification {}

impl PartialOrd for PrioritizedNotification {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrioritizedNotification {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority.partial_cmp(&other.priority)
            .unwrap_or(Ordering::Equal)
    }
}

/// Delivery channel preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryChannel {
    Desktop,
    Mobile,
    WhatsApp,
    Telegram,
    Email,
    Voice,
}

/// Priority queue configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorityQueueConfig {
    /// Weight for urgency component
    pub w_urgency: f64,
    /// Weight for relevance component
    pub w_relevance: f64,
    /// Weight for age component
    pub w_age: f64,
    /// Age normalization factor (seconds)
    pub age_tau: f64,
    /// Push threshold — immediate delivery above this
    pub push_threshold: f64,
    /// Maximum notifications per hour
    pub max_per_hour: u32,
    /// Digest interval for batched notifications (seconds)
    pub digest_interval_secs: u64,
}

impl Default for PriorityQueueConfig {
    fn default() -> Self {
        Self {
            w_urgency: 0.4,
            w_relevance: 0.35,
            w_age: 0.25,
            age_tau: 3600.0, // 1 hour
            push_threshold: 0.6,
            max_per_hour: 10,
            digest_interval_secs: 3600,
        }
    }
}

/// Token bucket for rate limiting notifications.
pub struct TokenBucket {
    /// Current token count
    tokens: f64,
    /// Maximum capacity
    capacity: f64,
    /// Refill rate (tokens per second)
    refill_rate: f64,
    /// Last refill timestamp
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(max_per_hour: u32) -> Self {
        let capacity = max_per_hour as f64;
        Self {
            tokens: capacity,
            capacity,
            refill_rate: capacity / 3600.0,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume a token. Returns true if allowed.
    pub fn try_consume(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time.
    fn refill(&mut self) {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = Instant::now();
    }

    /// Current fill level (0.0 – 1.0).
    pub fn fill_level(&self) -> f64 {
        self.tokens / self.capacity
    }
}

/// The notification priority queue.
pub struct NotificationQueue {
    config: PriorityQueueConfig,
    /// Priority heap for notifications
    heap: BinaryHeap<PrioritizedNotification>,
    /// Batched notifications waiting for digest
    batch: Vec<PrioritizedNotification>,
    /// Token bucket throttle
    throttle: TokenBucket,
}

impl NotificationQueue {
    pub fn new(config: PriorityQueueConfig) -> Self {
        let throttle = TokenBucket::new(config.max_per_hour);
        Self {
            config,
            heap: BinaryHeap::new(),
            batch: Vec::new(),
            throttle,
        }
    }

    /// Compute priority for a notification.
    ///
    /// `p(n) = w_urgency × u(n) + w_relevance × r(n) + w_age × age(n)/τ`
    pub fn compute_priority(&self, urgency: f64, relevance: f64, age_secs: u64) -> f64 {
        self.config.w_urgency * urgency
            + self.config.w_relevance * relevance
            + self.config.w_age * (age_secs as f64 / self.config.age_tau)
    }

    /// Enqueue a notification.
    pub fn enqueue(&mut self, mut notification: PrioritizedNotification) {
        notification.priority = self.compute_priority(
            notification.urgency,
            notification.relevance,
            notification.age_secs,
        );
        notification.immediate = notification.priority > self.config.push_threshold;

        if notification.immediate {
            self.heap.push(notification);
        } else {
            self.batch.push(notification);
        }
    }

    /// Dequeue the highest-priority immediate notification.
    ///
    /// Returns None if throttled or no immediate notifications.
    pub fn dequeue(&mut self) -> Option<PrioritizedNotification> {
        if !self.throttle.try_consume() {
            return None; // Rate limited
        }
        self.heap.pop()
    }

    /// Get all batched notifications for digest delivery.
    pub fn drain_batch(&mut self) -> Vec<PrioritizedNotification> {
        let mut batch = Vec::new();
        std::mem::swap(&mut batch, &mut self.batch);
        batch.sort_by(|a, b| b.priority.partial_cmp(&a.priority).unwrap_or(Ordering::Equal));
        batch
    }

    /// Number of pending immediate notifications.
    pub fn immediate_count(&self) -> usize {
        self.heap.len()
    }

    /// Number of batched notifications.
    pub fn batch_count(&self) -> usize {
        self.batch.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_urgency_is_immediate() {
        let config = PriorityQueueConfig::default();
        let mut queue = NotificationQueue::new(config);

        let n = PrioritizedNotification {
            type_id: "urgent".into(), type_name: "Urgent".into(),
            title: "Alert".into(), body: "Something important".into(),
            priority: 0.0, urgency: 1.0, relevance: 0.8, age_secs: 0,
            immediate: false, channel: DeliveryChannel::Desktop,
            created_at: 0,
        };
        queue.enqueue(n);
        assert_eq!(queue.immediate_count(), 1);
        assert_eq!(queue.batch_count(), 0);
    }

    #[test]
    fn low_urgency_is_batched() {
        let config = PriorityQueueConfig::default();
        let mut queue = NotificationQueue::new(config);

        let n = PrioritizedNotification {
            type_id: "low".into(), type_name: "Low".into(),
            title: "Info".into(), body: "FYI".into(),
            priority: 0.0, urgency: 0.1, relevance: 0.2, age_secs: 0,
            immediate: false, channel: DeliveryChannel::Desktop,
            created_at: 0,
        };
        queue.enqueue(n);
        assert_eq!(queue.immediate_count(), 0);
        assert_eq!(queue.batch_count(), 1);
    }

    #[test]
    fn token_bucket_allows_within_limit() {
        let mut bucket = TokenBucket::new(10);
        for _ in 0..10 {
            assert!(bucket.try_consume());
        }
        // 11th should be denied (no refill yet)
        assert!(!bucket.try_consume());
    }

    #[test]
    fn dequeue_respects_priority_order() {
        let config = PriorityQueueConfig::default();
        let mut queue = NotificationQueue::new(config);

        for (urgency, id) in [(0.7, "a"), (0.9, "b"), (0.8, "c")] {
            queue.enqueue(PrioritizedNotification {
                type_id: id.into(), type_name: id.into(),
                title: id.into(), body: id.into(),
                priority: 0.0, urgency, relevance: 0.7, age_secs: 0,
                immediate: false, channel: DeliveryChannel::Desktop,
                created_at: 0,
            });
        }

        // Highest urgency first
        let first = queue.dequeue().unwrap();
        assert_eq!(first.type_id, "b");
    }
}
