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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Duration;
use tracing::{debug, info, warn};

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
/// The router holds a delivery queue and processes announcements in order.
/// Actual delivery is callback-based (via `DeliveryHandler`) so the router
/// is transport-agnostic.
pub struct AnnounceRouter {
    /// Pending deliveries.
    queue: VecDeque<Announcement>,
    /// Retry policy.
    retry_policy: RetryPolicy,
    /// Successfully delivered count.
    pub delivered_count: u64,
    /// Failed delivery count (after exhausting retries).
    pub failed_count: u64,
    /// Active subscriptions: task_id → list of targets.
    subscriptions: std::collections::HashMap<String, Vec<DeliveryTarget>>,
}

impl AnnounceRouter {
    /// Create a new announce router.
    pub fn new(retry_policy: RetryPolicy) -> Self {
        Self {
            queue: VecDeque::new(),
            retry_policy,
            delivered_count: 0,
            failed_count: 0,
            subscriptions: std::collections::HashMap::new(),
        }
    }

    /// Create with default retry policy.
    pub fn with_defaults() -> Self {
        Self::new(RetryPolicy::default())
    }

    /// Subscribe a delivery target to a task's results.
    ///
    /// When the task produces results (completion, failure, progress, artifacts),
    /// announcements are generated and queued for delivery to all subscribed targets.
    pub fn subscribe(&mut self, task_id: &str, target: DeliveryTarget) {
        info!(task_id = task_id, "subscribed delivery target");
        self.subscriptions
            .entry(task_id.to_string())
            .or_default()
            .push(target);
    }

    /// Remove all subscriptions for a task.
    pub fn unsubscribe(&mut self, task_id: &str) -> usize {
        self.subscriptions
            .remove(task_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Announce a payload for a task. Creates an `Announcement` for each
    /// subscribed target and enqueues them for delivery.
    pub fn announce(&mut self, task_id: &str, source_agent: &str, payload: AnnouncePayload) -> usize {
        let targets = match self.subscriptions.get(task_id) {
            Some(targets) => targets.clone(),
            None => {
                debug!(task_id = task_id, "no subscribers for task");
                return 0;
            }
        };

        let mut count = 0;
        for target in targets {
            let announcement = Announcement {
                id: format!("ann_{}_{}", task_id, count),
                task_id: task_id.to_string(),
                source_agent: source_agent.to_string(),
                target,
                payload: payload.clone(),
                created_at: Utc::now(),
                attempts: 0,
                max_attempts: self.retry_policy.max_attempts,
            };
            self.queue.push_back(announcement);
            count += 1;
        }

        info!(task_id = task_id, targets = count, "announcements queued");
        count
    }

    /// Pop the next announcement for delivery. Returns `None` if the queue is empty.
    pub fn next_delivery(&mut self) -> Option<Announcement> {
        self.queue.pop_front()
    }

    /// Re-enqueue an announcement for retry after a failed delivery.
    /// Returns `false` if max attempts exhausted.
    pub fn retry(&mut self, mut announcement: Announcement) -> bool {
        announcement.attempts += 1;
        if announcement.attempts >= announcement.max_attempts {
            warn!(
                id = announcement.id,
                task = announcement.task_id,
                attempts = announcement.attempts,
                "delivery permanently failed after max retries"
            );
            self.failed_count += 1;
            return false;
        }

        debug!(
            id = announcement.id,
            attempt = announcement.attempts,
            "re-enqueuing for retry"
        );
        self.queue.push_back(announcement);
        true
    }

    /// Record a successful delivery.
    pub fn record_delivered(&mut self) {
        self.delivered_count += 1;
    }

    /// Number of pending deliveries.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// Whether there are pending deliveries.
    pub fn has_pending(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Drain all pending announcements (for shutdown / batch processing).
    pub fn drain_pending(&mut self) -> Vec<Announcement> {
        self.queue.drain(..).collect()
    }

    /// Summary for monitoring.
    pub fn summary(&self) -> AnnounceSummary {
        AnnounceSummary {
            pending: self.queue.len(),
            delivered: self.delivered_count,
            failed: self.failed_count,
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
    pub pending: usize,
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

    #[test]
    fn subscribe_and_announce() {
        let mut router = AnnounceRouter::with_defaults();

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

    #[test]
    fn no_subscribers_no_announcements() {
        let mut router = AnnounceRouter::with_defaults();
        let queued = router.announce(
            "task-orphan",
            "agent-a",
            AnnouncePayload::TaskFailed { error: "oops".into() },
        );
        assert_eq!(queued, 0);
        assert!(router.next_delivery().is_none());
    }

    #[test]
    fn delivery_retry_exhaustion() {
        let policy = RetryPolicy {
            max_attempts: 3,
            ..Default::default()
        };
        let mut router = AnnounceRouter::new(policy);

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

        let ann = router.next_delivery().unwrap();
        assert_eq!(ann.attempts, 0);

        // Retry 1
        assert!(router.retry(ann.clone()));
        let ann = router.next_delivery().unwrap();
        assert_eq!(ann.attempts, 1);

        // Retry 2
        assert!(router.retry(ann.clone()));
        let ann = router.next_delivery().unwrap();
        assert_eq!(ann.attempts, 2);

        // Retry 3 — should fail (max_attempts = 3)
        assert!(!router.retry(ann));
        assert_eq!(router.failed_count, 1);
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

    #[test]
    fn unsubscribe_removes_targets() {
        let mut router = AnnounceRouter::with_defaults();
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

    #[test]
    fn drain_pending_empties_queue() {
        let mut router = AnnounceRouter::with_defaults();
        router.subscribe("t", DeliveryTarget::Channel { channel_id: "c".into(), thread_id: None });
        router.announce("t", "a", AnnouncePayload::Progress { percent: 1.0, message: None });
        let drained = router.drain_pending();
        assert_eq!(drained.len(), 1);
        assert!(router.next_delivery().is_none());
    }

    #[test]
    fn summary_reflects_state() {
        let mut router = AnnounceRouter::with_defaults();
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
}
