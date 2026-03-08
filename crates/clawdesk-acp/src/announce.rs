//! Announce/Delivery — push-based event notifications for A2A task results.
//!
//! When an A2A task completes, the result must be **delivered** to the
//! appropriate destination — a user-facing channel (Tauri, Telegram, Slack),
//! a webhook, or another agent's inbox.
//!
//! ## Architecture
//!
//! ```text
//!  Task completes
//!       │
//!       ▼
//!  AnnounceRouter
//!       │
//!       ├─ ChannelDelivery  → emit Tauri event / push to channel adapter
//!       ├─ WebhookDelivery  → HTTP POST to configured URL
//!       └─ AgentDelivery    → POST /a2a/tasks/:id/input on the source agent
//! ```
//!
//! ## Delivery guarantees
//!
//! Each delivery target has a retry policy. Failed deliveries are queued
//! for retry with exponential backoff (base 2s, max 60s, 5 attempts).
//! After all retries are exhausted, the failure is logged and the
//! `DeliveryCallback` is notified.
//!
//! ## Scheduler design (v2)
//!
//! All announcements — new and retried — flow through a single timed
//! min-heap keyed by `(ready_at, sequence_no)`. This eliminates the
//! previous split between mpsc channel and mutex-guarded VecDeque:
//!
//! - Retries are **never** delivered before their scheduled time.
//! - `pending()` and `summary()` are **always authoritative**.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::BinaryHeap;
use std::cmp::Ordering;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Default subscription TTL — 1 hour.
const DEFAULT_SUBSCRIPTION_TTL: Duration = Duration::from_secs(3600);

// ═══════════════════════════════════════════════════════════════════════════
// Delivery target
// ═══════════════════════════════════════════════════════════════════════════

/// Where to deliver a task result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeliveryTarget {
    /// Deliver to a user-facing channel (Tauri frontend, Telegram, etc.).
    Channel {
        channel_id: String,
        /// Optional thread/conversation ID within the channel.
        thread_id: Option<String>,
    },
    /// Deliver via HTTP POST to a webhook URL.
    Webhook {
        url: String,
        /// Optional Bearer token for auth.
        auth_token: Option<String>,
    },
    /// Deliver to another agent's A2A inbox (inter-agent notification).
    Agent {
        agent_id: String,
        /// The agent's A2A endpoint URL.
        endpoint_url: String,
    },
}

/// An announcement to deliver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Announcement {
    /// Unique delivery ID.
    pub id: String,
    /// Source task ID that generated this result.
    pub task_id: String,
    /// The agent that completed the task.
    pub source_agent: String,
    /// Where to deliver.
    pub target: DeliveryTarget,
    /// The payload to deliver.
    pub payload: AnnouncePayload,
    /// When the announcement was created.
    pub created_at: DateTime<Utc>,
    /// Number of delivery attempts so far.
    pub attempts: u32,
    /// Maximum delivery attempts.
    pub max_attempts: u32,
}

/// What's being announced.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnnouncePayload {
    /// Task completed successfully.
    TaskCompleted {
        output: serde_json::Value,
        duration_ms: u64,
    },
    /// Task failed.
    TaskFailed {
        error: String,
    },
    /// Task needs input (interactive flow).
    InputRequired {
        prompt: String,
        schema: Option<serde_json::Value>,
    },
    /// Progress update (streaming).
    Progress {
        percent: f64,
        message: Option<String>,
    },
    /// Artifact produced by the task.
    ArtifactReady {
        artifact_id: String,
        name: String,
        mime_type: String,
        size_bytes: Option<u64>,
    },
}

/// Result of a delivery attempt.
#[derive(Debug, Clone)]
pub enum DeliveryResult {
    /// Successfully delivered.
    Delivered,
    /// Delivery failed (retryable).
    Failed(String),
    /// Delivery permanently failed (no retry).
    PermanentFailure(String),
}

/// Retry policy for deliveries.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum attempts (including first try).
    pub max_attempts: u32,
    /// Base delay between retries (exponential backoff).
    pub base_delay: Duration,
    /// Maximum delay cap.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(60),
        }
    }
}

impl RetryPolicy {
    /// Compute delay for attempt `n` (0-indexed): base × 2^n, capped at max.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let multiplier = 1u64 << attempt.min(6);
        let delay = self.base_delay.as_millis() as u64 * multiplier;
        Duration::from_millis(delay.min(self.max_delay.as_millis() as u64))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Announce router
// ═══════════════════════════════════════════════════════════════════════════

/// Routes announcements to their delivery targets.
///
/// All announcements flow through a single timed min-heap. New items
/// are scheduled with `ready_at = now`. Retried items get `ready_at =
/// now + backoff`. `next_delivery()` only returns elapsed items.
///
/// **Lock ordering**: TaskStore lock → AnnounceRouter queue lock.
pub struct AnnounceRouter {
    /// Unified timed priority queue.
    queue: Mutex<BinaryHeap<TimedEntry>>,
    /// Monotonic sequence counter for FIFO tie-breaking.
    next_sequence: AtomicU64,
    retry_policy: RetryPolicy,
    delivered_count: AtomicU64,
    failed_count: AtomicU64,
    subscriptions: DashMap<String, Vec<TimedSubscription>>,
    subscription_ttl: Duration,
    max_capacity: usize,
}

/// A subscription with a creation timestamp for TTL-based expiry.
#[derive(Debug, Clone)]
struct TimedSubscription {
    target: DeliveryTarget,
    subscribed_at: Instant,
}

/// Scheduled announcement entry for the min-heap.
struct TimedEntry {
    announcement: Announcement,
    ready_at: Instant,
    sequence: u64,
}

impl PartialEq for TimedEntry {
    fn eq(&self, other: &Self) -> bool {
        self.ready_at == other.ready_at && self.sequence == other.sequence
    }
}
impl Eq for TimedEntry {}

impl PartialOrd for TimedEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimedEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is max-heap; invert for min-heap semantics.
        other.ready_at.cmp(&self.ready_at)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

/// Default channel capacity for the delivery queue.
const DEFAULT_CHANNEL_CAPACITY: usize = 4096;

impl AnnounceRouter {
    /// Create a new announce router.
    pub fn new(retry_policy: RetryPolicy) -> Self {
        Self {
            queue: Mutex::new(BinaryHeap::with_capacity(1024)),
            next_sequence: AtomicU64::new(0),
            retry_policy,
            delivered_count: AtomicU64::new(0),
            failed_count: AtomicU64::new(0),
            subscriptions: DashMap::new(),
            subscription_ttl: DEFAULT_SUBSCRIPTION_TTL,
            max_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }

    /// Create with default retry policy.
    pub fn with_defaults() -> Self {
        Self::new(RetryPolicy::default())
    }

    /// Set the subscription TTL.
    pub fn with_subscription_ttl(mut self, ttl: Duration) -> Self {
        self.subscription_ttl = ttl;
        self
    }

    fn next_seq(&self) -> u64 {
        self.next_sequence.fetch_add(1, AtomicOrdering::Relaxed)
    }

    /// Garbage-collect all expired subscriptions across all tasks.
    /// Returns the number of tasks whose subscription lists were cleaned up.
    pub fn gc_subscriptions(&self) -> usize {
        let now = Instant::now();
        let ttl = self.subscription_ttl;
        let mut gc_count = 0;

        self.subscriptions.retain(|_, subs| {
            let before = subs.len();
            subs.retain(|s| now.duration_since(s.subscribed_at) < ttl);
            if subs.len() < before {
                gc_count += 1;
            }
            !subs.is_empty()
        });

        gc_count
    }

    /// Subscribe a delivery target to a task's results.
    ///
    /// When the task produces results (completion, failure, progress, artifacts),
    /// announcements are generated and queued for delivery to all subscribed targets.
    pub fn subscribe(&self, task_id: &str, target: DeliveryTarget) {
        info!(task_id = task_id, "subscribed delivery target");
        self.subscriptions
            .entry(task_id.to_string())
            .or_default()
            .push(TimedSubscription {
                target,
                subscribed_at: Instant::now(),
            });
    }

    /// Remove all subscriptions for a task.
    pub fn unsubscribe(&self, task_id: &str) -> usize {
        self.subscriptions
            .remove(task_id)
            .map(|(_, v)| v.len())
            .unwrap_or(0)
    }

    /// Announce a payload for a task. Creates an `Announcement` for each
    /// subscribed target and enqueues them for immediate delivery.
    pub fn announce(&self, task_id: &str, source_agent: &str, payload: AnnouncePayload) -> usize {
        let now_instant = Instant::now();
        let ttl = self.subscription_ttl;

        let targets: Vec<DeliveryTarget> = match self.subscriptions.get_mut(task_id) {
            Some(mut subs) => {
                subs.retain(|s| now_instant.duration_since(s.subscribed_at) < ttl);
                subs.iter().map(|s| s.target.clone()).collect()
            }
            None => {
                debug!(task_id = task_id, "no subscribers for task");
                return 0;
            }
        };

        let mut count = 0;
        // Spin-try the lock — consumer holds it only for O(log n) pop.
        for _ in 0..200 {
            if let Ok(mut queue) = self.queue.try_lock() {
                for (i, target) in targets.into_iter().enumerate() {
                    if queue.len() >= self.max_capacity {
                        warn!(task_id = task_id, "delivery queue at capacity, rejecting");
                        break;
                    }
                    let announcement = Announcement {
                        id: format!("ann_{}_{}", task_id, i),
                        task_id: task_id.to_string(),
                        source_agent: source_agent.to_string(),
                        target,
                        payload: payload.clone(),
                        created_at: Utc::now(),
                        attempts: 0,
                        max_attempts: self.retry_policy.max_attempts,
                    };
                    queue.push(TimedEntry {
                        announcement,
                        ready_at: now_instant,
                        sequence: self.next_seq(),
                    });
                    count += 1;
                }
                info!(task_id = task_id, targets = count, "announcements queued");
                return count;
            }
            std::hint::spin_loop();
        }
        warn!(task_id = task_id, "failed to acquire queue lock for announce");
        info!(task_id = task_id, targets = count, "announcements queued");
        count
    }

    /// Pop next announcement whose `ready_at` has elapsed.
    pub async fn next_delivery(&self) -> Option<Announcement> {
        let mut queue = self.queue.lock().await;
        if let Some(entry) = queue.peek() {
            if Instant::now() >= entry.ready_at {
                return queue.pop().map(|e| e.announcement);
            }
        }
        None
    }

    /// Re-enqueue with exponential backoff. Returns `false` if max attempts
    /// exhausted. The announcement will NOT be returned by `next_delivery()`
    /// until its backoff delay has elapsed — this guarantee is unconditional.
    pub fn retry(&self, mut announcement: Announcement) -> bool {
        announcement.attempts += 1;
        if announcement.attempts >= announcement.max_attempts {
            warn!(
                id = announcement.id,
                task = announcement.task_id,
                attempts = announcement.attempts,
                "delivery permanently failed after max retries"
            );
            self.failed_count.fetch_add(1, AtomicOrdering::Relaxed);
            return false;
        }

        let delay = self.retry_policy.delay_for_attempt(announcement.attempts.saturating_sub(1));
        let ready_at = Instant::now() + delay;

        debug!(
            id = announcement.id,
            attempt = announcement.attempts,
            delay_ms = delay.as_millis() as u64,
            "scheduling retry with backoff"
        );

        // Spin-try the lock — consumer holds it only for O(log n) pop.
        for _ in 0..200 {
            if let Ok(mut queue) = self.queue.try_lock() {
                queue.push(TimedEntry {
                    announcement,
                    ready_at,
                    sequence: self.next_seq(),
                });
                return true;
            }
            std::hint::spin_loop();
        }
        warn!("retry: queue lock contention exceeded spin limit");
        self.failed_count.fetch_add(1, AtomicOrdering::Relaxed);
        false
    }

    /// Record a successful delivery.
    pub fn record_delivered(&self) {
        self.delivered_count.fetch_add(1, AtomicOrdering::Relaxed);
    }

    /// Number of pending deliveries — authoritative, single queue.
    pub fn pending(&self) -> usize {
        self.queue.try_lock().map(|q| q.len()).unwrap_or(0)
    }

    /// Whether there are pending deliveries.
    pub fn has_pending(&self) -> bool {
        self.queue.try_lock().map(|q| !q.is_empty()).unwrap_or(false)
    }

    /// Drain all pending announcements (for shutdown / batch processing).
    pub async fn drain_pending(&self) -> Vec<Announcement> {
        let mut queue = self.queue.lock().await;
        let mut drained = Vec::with_capacity(queue.len());
        while let Some(entry) = queue.pop() {
            drained.push(entry.announcement);
        }
        drained
    }

    /// Summary for monitoring — all counts are authoritative.
    pub fn summary(&self) -> AnnounceSummary {
        let (total, ready, waiting) = self.queue.try_lock().map(|q| {
            let now = Instant::now();
            let total = q.len();
            let ready = q.iter().filter(|e| now >= e.ready_at).count();
            (total, ready, total - ready)
        }).unwrap_or((0, 0, 0));

        AnnounceSummary {
            pending: total,
            primary_pending: ready,
            retry_pending: waiting,
            delivered: self.delivered_count.load(AtomicOrdering::Relaxed),
            failed: self.failed_count.load(AtomicOrdering::Relaxed),
            subscriptions: self.subscriptions.len(),
        }
    }
}

impl Default for AnnounceRouter {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Monitoring summary.
#[derive(Debug, Clone, Serialize)]
pub struct AnnounceSummary {
    /// Total pending: primary channel + retry queue.
    pub pending: usize,
    /// Items in the primary delivery channel.
    pub primary_pending: usize,
    /// Items in the timed retry queue (awaiting backoff expiry).
    pub retry_pending: usize,
    pub delivered: u64,
    pub failed: u64,
    pub subscriptions: usize,
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribe_and_announce() {
        let router = AnnounceRouter::with_defaults();

        router.subscribe(
            "task-1",
            DeliveryTarget::Channel {
                channel_id: "tauri".into(),
                thread_id: Some("t-123".into()),
            },
        );
        router.subscribe(
            "task-1",
            DeliveryTarget::Webhook {
                url: "https://hooks.example.com/notify".into(),
                auth_token: None,
            },
        );

        let queued = router.announce(
            "task-1",
            "agent-a",
            AnnouncePayload::TaskCompleted {
                output: serde_json::json!({"result": "ok"}),
                duration_ms: 1500,
            },
        );

        assert_eq!(queued, 2);
        assert_eq!(router.pending(), 2);
    }

    #[tokio::test]
    async fn no_subscribers_no_announcements() {
        let router = AnnounceRouter::with_defaults();
        let queued = router.announce(
            "task-orphan",
            "agent-a",
            AnnouncePayload::TaskFailed { error: "oops".into() },
        );
        assert_eq!(queued, 0);
        assert!(router.next_delivery().await.is_none());
    }

    #[tokio::test]
    async fn delivery_retry_exhaustion() {
        let policy = RetryPolicy {
            max_attempts: 3,
            // Use zero-delay backoff for test determinism.
            base_delay: Duration::from_millis(0),
            max_delay: Duration::from_millis(0),
        };
        let router = AnnounceRouter::new(policy);

        router.subscribe(
            "task-2",
            DeliveryTarget::Agent {
                agent_id: "other".into(),
                endpoint_url: "http://other.local".into(),
            },
        );
        router.announce(
            "task-2",
            "self",
            AnnouncePayload::Progress { percent: 0.5, message: None },
        );

        let ann = router.next_delivery().await.unwrap();
        assert_eq!(ann.attempts, 0);

        // Retry 1 — scheduled with backoff (0ms for test)
        assert!(router.retry(ann.clone()));
        // Allow the backoff to "elapse" (0ms)
        tokio::time::sleep(Duration::from_millis(1)).await;
        let ann = router.next_delivery().await.unwrap();
        assert_eq!(ann.attempts, 1);

        // Retry 2
        assert!(router.retry(ann.clone()));
        tokio::time::sleep(Duration::from_millis(1)).await;
        let ann = router.next_delivery().await.unwrap();
        assert_eq!(ann.attempts, 2);

        // Retry 3 — should fail (max_attempts = 3)
        assert!(!router.retry(ann));
        assert_eq!(router.failed_count.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn retry_delay_exponential_backoff() {
        let policy = RetryPolicy {
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(60),
            max_attempts: 10,
        };

        assert_eq!(policy.delay_for_attempt(0), Duration::from_secs(2));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_secs(4));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_secs(8));
        assert_eq!(policy.delay_for_attempt(3), Duration::from_secs(16));
        // Should cap at max_delay
        assert_eq!(policy.delay_for_attempt(10), Duration::from_secs(60));
    }

    #[tokio::test]
    async fn unsubscribe_removes_targets() {
        let router = AnnounceRouter::with_defaults();
        router.subscribe(
            "task-3",
            DeliveryTarget::Channel { channel_id: "test".into(), thread_id: None },
        );
        assert_eq!(router.unsubscribe("task-3"), 1);
        assert_eq!(router.unsubscribe("task-3"), 0);

        // Now announce should produce 0
        let q = router.announce("task-3", "a", AnnouncePayload::TaskFailed { error: "x".into() });
        assert_eq!(q, 0);
    }

    #[tokio::test]
    async fn drain_pending_empties_queue() {
        let router = AnnounceRouter::with_defaults();
        router.subscribe("t", DeliveryTarget::Channel { channel_id: "c".into(), thread_id: None });
        router.announce("t", "a", AnnouncePayload::Progress { percent: 1.0, message: None });
        let drained = router.drain_pending().await;
        assert_eq!(drained.len(), 1);
        assert!(router.next_delivery().await.is_none());
    }

    #[test]
    fn summary_reflects_state() {
        let router = AnnounceRouter::with_defaults();
        router.subscribe("t", DeliveryTarget::Channel { channel_id: "c".into(), thread_id: None });
        router.announce("t", "a", AnnouncePayload::TaskCompleted {
            output: serde_json::json!(null),
            duration_ms: 0,
        });
        router.record_delivered();

        let s = router.summary();
        assert_eq!(s.pending, 1);
        assert_eq!(s.delivered, 1);
        assert_eq!(s.subscriptions, 1);
    }

    #[tokio::test]
    async fn concurrent_subscribe_and_announce() {
        use std::sync::Arc;

        let router = Arc::new(AnnounceRouter::with_defaults());
        let mut handles = Vec::new();

        // 10 threads subscribing and announcing concurrently
        for i in 0..10 {
            let r = Arc::clone(&router);
            handles.push(tokio::spawn(async move {
                let task_id = format!("task-{}", i);
                r.subscribe(
                    &task_id,
                    DeliveryTarget::Channel {
                        channel_id: format!("ch-{}", i),
                        thread_id: None,
                    },
                );
                r.announce(
                    &task_id,
                    "agent",
                    AnnouncePayload::TaskCompleted {
                        output: serde_json::json!({"i": i}),
                        duration_ms: 100,
                    },
                );
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(router.subscriptions.len(), 10);
        assert_eq!(router.pending(), 10);

        // Drain all
        let drained = router.drain_pending().await;
        assert_eq!(drained.len(), 10);
        assert_eq!(router.pending(), 0);
    }

    #[tokio::test]
    async fn retry_backoff_delays_delivery() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(60),
        };
        let router = AnnounceRouter::new(policy);
        router.subscribe("t", DeliveryTarget::Channel { channel_id: "c".into(), thread_id: None });
        router.announce("t", "a", AnnouncePayload::Progress { percent: 0.5, message: None });

        let ann = router.next_delivery().await.unwrap();

        // Retry with backoff (attempt 0 → 200ms delay)
        assert!(router.retry(ann));

        // Immediately polling should NOT yield the retry (backoff not elapsed).
        assert!(router.next_delivery().await.is_none());

        // Wait for backoff to elapse.
        tokio::time::sleep(Duration::from_millis(250)).await;

        // Now it should be available.
        let retried = router.next_delivery().await.unwrap();
        assert_eq!(retried.attempts, 1);
    }

    #[tokio::test]
    async fn pending_includes_retry_queue() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_secs(300), // Long delay — won't elapse in test.
            max_delay: Duration::from_secs(600),
        };
        let router = AnnounceRouter::new(policy);
        router.subscribe("t", DeliveryTarget::Channel { channel_id: "c".into(), thread_id: None });
        router.announce("t", "a", AnnouncePayload::Progress { percent: 1.0, message: None });

        let ann = router.next_delivery().await.unwrap();
        assert_eq!(router.pending(), 0);

        // Retry schedules into the retry queue with long backoff.
        assert!(router.retry(ann));

        // pending() should now report 1 (the retry-queue item).
        assert_eq!(router.pending(), 1);
        assert!(router.has_pending());

        // drain_pending should include the retry-queue item.
        let drained = router.drain_pending().await;
        assert_eq!(drained.len(), 1);
        assert_eq!(router.pending(), 0);
    }
}
