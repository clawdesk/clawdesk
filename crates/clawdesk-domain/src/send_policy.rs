//! Send policy — token-bucket rate limiter with priority queuing and backpressure.
//!
//! Token bucket algorithm: bucket capacity B, refill rate R tokens/second.
//! Priority queue with k levels: O(log n) insertion, O(1) dequeue.

use std::collections::BinaryHeap;
use std::time::{Duration, Instant};
use std::cmp::Ordering;

/// Token bucket rate limiter.
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            capacity,
            tokens: capacity,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume `cost` tokens. Returns true if allowed.
    pub fn try_consume(&mut self, cost: f64) -> bool {
        self.refill();
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }

    /// Time until `cost` tokens are available.
    pub fn time_until_available(&mut self, cost: f64) -> Duration {
        self.refill();
        if self.tokens >= cost {
            return Duration::ZERO;
        }
        let deficit = cost - self.tokens;
        Duration::from_secs_f64(deficit / self.refill_rate)
    }

    /// Current token count.
    pub fn available(&mut self) -> f64 {
        self.refill();
        self.tokens
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = now;
    }
}

/// Priority level for queued messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendPriority {
    Low = 0,
    Normal = 1,
    High = 2,
    Urgent = 3,
}

/// A queued outbound message.
#[derive(Debug)]
pub struct QueuedMessage {
    pub id: String,
    pub priority: SendPriority,
    pub channel_id: String,
    pub content: String,
    pub enqueued_at: Instant,
    pub cost: f64,
}

impl PartialEq for QueuedMessage {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.id == other.id
    }
}

impl Eq for QueuedMessage {}

impl PartialOrd for QueuedMessage {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueuedMessage {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first, then older messages first
        match (self.priority as u8).cmp(&(other.priority as u8)) {
            Ordering::Equal => other.enqueued_at.cmp(&self.enqueued_at), // older first
            other => other,
        }
    }
}

/// Rate-limited priority send queue with backpressure signaling.
pub struct SendQueue {
    bucket: TokenBucket,
    queue: BinaryHeap<QueuedMessage>,
    max_depth: usize,
}

/// Backpressure signal from the send queue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackpressureSignal {
    /// Queue has capacity, proceed normally.
    Ok,
    /// Queue is filling up, reduce send rate.
    SlowDown { queue_utilization: f64 },
    /// Queue is full, reject new messages.
    Full,
}

impl SendQueue {
    pub fn new(bucket_capacity: f64, refill_rate: f64, max_depth: usize) -> Self {
        Self {
            bucket: TokenBucket::new(bucket_capacity, refill_rate),
            queue: BinaryHeap::new(),
            max_depth,
        }
    }

    /// Enqueue a message. Returns backpressure signal.
    pub fn enqueue(&mut self, msg: QueuedMessage) -> BackpressureSignal {
        if self.queue.len() >= self.max_depth {
            return BackpressureSignal::Full;
        }

        self.queue.push(msg);

        let utilization = self.queue.len() as f64 / self.max_depth as f64;
        if utilization > 0.8 {
            BackpressureSignal::SlowDown {
                queue_utilization: utilization,
            }
        } else {
            BackpressureSignal::Ok
        }
    }

    /// Try to dequeue and send the highest-priority message.
    /// Returns the message if rate limit allows, None otherwise.
    pub fn try_dequeue(&mut self) -> Option<QueuedMessage> {
        if let Some(msg) = self.queue.peek() {
            if self.bucket.try_consume(msg.cost) {
                self.queue.pop()
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Number of messages waiting in the queue.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// Time until the next message can be sent.
    pub fn time_until_next(&mut self) -> Duration {
        if let Some(msg) = self.queue.peek() {
            self.bucket.time_until_available(msg.cost)
        } else {
            Duration::MAX
        }
    }

    /// Current backpressure status.
    pub fn backpressure(&self) -> BackpressureSignal {
        let utilization = self.queue.len() as f64 / self.max_depth as f64;
        if utilization >= 1.0 {
            BackpressureSignal::Full
        } else if utilization > 0.8 {
            BackpressureSignal::SlowDown {
                queue_utilization: utilization,
            }
        } else {
            BackpressureSignal::Ok
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_bucket_allows_within_capacity() {
        let mut bucket = TokenBucket::new(10.0, 1.0);
        assert!(bucket.try_consume(5.0));
        assert!(bucket.try_consume(5.0));
        assert!(!bucket.try_consume(1.0)); // Empty
    }

    #[test]
    fn test_priority_queue_ordering() {
        let mut queue = SendQueue::new(100.0, 10.0, 100);

        queue.enqueue(QueuedMessage {
            id: "low".into(),
            priority: SendPriority::Low,
            channel_id: "ch1".into(),
            content: "low priority".into(),
            enqueued_at: Instant::now(),
            cost: 1.0,
        });
        queue.enqueue(QueuedMessage {
            id: "urgent".into(),
            priority: SendPriority::Urgent,
            channel_id: "ch1".into(),
            content: "urgent message".into(),
            enqueued_at: Instant::now(),
            cost: 1.0,
        });

        let msg = queue.try_dequeue().unwrap();
        assert_eq!(msg.id, "urgent"); // Urgent comes first
    }

    #[test]
    fn test_backpressure_signal() {
        let mut queue = SendQueue::new(100.0, 10.0, 4);
        for i in 0..4 {
            let signal = queue.enqueue(QueuedMessage {
                id: format!("msg-{i}"),
                priority: SendPriority::Normal,
                channel_id: "ch1".into(),
                content: "test".into(),
                enqueued_at: Instant::now(),
                cost: 1.0,
            });
            if i >= 4 {
                assert!(matches!(signal, BackpressureSignal::SlowDown { .. }));
            }
        }

        let signal = queue.enqueue(QueuedMessage {
            id: "msg-5".into(),
            priority: SendPriority::Normal,
            channel_id: "ch1".into(),
            content: "test".into(),
            enqueued_at: Instant::now(),
            cost: 1.0,
        });
        assert!(matches!(signal, BackpressureSignal::Full));
    }
}
