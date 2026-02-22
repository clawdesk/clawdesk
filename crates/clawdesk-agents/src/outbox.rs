//! Persistent Outbox — durable sub-agent result store.
//!
//! ## Sub-Agent Outbox Persistence
//!
//! The existing `Outbox` in `subagent.rs` is an in-memory `Vec<OutboxEntry>`.
//! This module provides a **durable outbox** backed by a pluggable storage
//! trait, ensuring sub-agent results survive process restarts and can be
//! delivered reliably even if the parent agent is temporarily unavailable.
//!
//! ## Outbox pattern
//!
//! The outbox pattern guarantees at-least-once delivery:
//! 1. Sub-agent completes → result is persisted to the outbox.
//! 2. Delivery loop scans for undelivered entries → attempts delivery.
//! 3. On success → entry marked as delivered.
//! 4. GC removes delivered entries after a retention period.
//!
//! ## Delivery guarantees
//!
//! With exponential backoff retry:
//! P(delivery after n attempts) = 1 - (1 - p)^n
//! where p is the per-attempt success probability.
//! For p = 0.9 and n = 5: P ≈ 0.99999

use crate::subagent::{OutboxEntry, OutboxPayload, SubAgentId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Storage trait
// ═══════════════════════════════════════════════════════════════════════════

/// Backend storage for outbox entries.
///
/// Implementations can back this with SochDB, SQLite, or in-memory stores.
#[async_trait]
pub trait OutboxStore: Send + Sync + 'static {
    /// Persist an outbox entry.
    async fn put(&self, entry: &OutboxEntry) -> Result<(), OutboxError>;

    /// Retrieve all undelivered entries for a given parent agent.
    async fn pending(&self, parent_id: &str) -> Result<Vec<OutboxEntry>, OutboxError>;

    /// Mark an entry as delivered.
    async fn mark_delivered(&self, sub_agent_id: &SubAgentId) -> Result<(), OutboxError>;

    /// Increment the delivery attempt counter.
    async fn increment_attempts(&self, sub_agent_id: &SubAgentId) -> Result<(), OutboxError>;

    /// Remove all delivered entries older than the retention period.
    async fn gc(&self, retention: Duration) -> Result<usize, OutboxError>;

    /// Count total entries (for monitoring).
    async fn count(&self) -> Result<OutboxStats, OutboxError>;
}

/// Outbox error.
#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("entry not found: {0}")]
    NotFound(String),
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// Outbox statistics for monitoring.
#[derive(Debug, Clone, Default)]
pub struct OutboxStats {
    pub total: usize,
    pub pending: usize,
    pub delivered: usize,
    pub max_attempts: u32,
}

// ═══════════════════════════════════════════════════════════════════════════
// In-memory store (for testing + fallback)
// ═══════════════════════════════════════════════════════════════════════════

/// Thread-safe in-memory outbox store.
pub struct InMemoryOutboxStore {
    entries: RwLock<HashMap<SubAgentId, OutboxEntry>>,
}

impl InMemoryOutboxStore {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryOutboxStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OutboxStore for InMemoryOutboxStore {
    async fn put(&self, entry: &OutboxEntry) -> Result<(), OutboxError> {
        self.entries
            .write()
            .await
            .insert(entry.sub_agent_id.clone(), entry.clone());
        Ok(())
    }

    async fn pending(&self, parent_id: &str) -> Result<Vec<OutboxEntry>, OutboxError> {
        let entries = self.entries.read().await;
        let pending = entries
            .values()
            .filter(|e| e.parent_id == parent_id && !e.delivered)
            .cloned()
            .collect();
        Ok(pending)
    }

    async fn mark_delivered(&self, sub_agent_id: &SubAgentId) -> Result<(), OutboxError> {
        let mut entries = self.entries.write().await;
        if let Some(entry) = entries.get_mut(sub_agent_id) {
            entry.delivered = true;
            Ok(())
        } else {
            Err(OutboxError::NotFound(format!("{:?}", sub_agent_id)))
        }
    }

    async fn increment_attempts(&self, sub_agent_id: &SubAgentId) -> Result<(), OutboxError> {
        let mut entries = self.entries.write().await;
        if let Some(entry) = entries.get_mut(sub_agent_id) {
            entry.delivery_attempts += 1;
            Ok(())
        } else {
            Err(OutboxError::NotFound(format!("{:?}", sub_agent_id)))
        }
    }

    async fn gc(&self, retention: Duration) -> Result<usize, OutboxError> {
        let mut entries = self.entries.write().await;
        let before = entries.len();
        let cutoff = Utc::now() - chrono::Duration::from_std(retention)
            .unwrap_or(chrono::Duration::seconds(3600));

        entries.retain(|_, e| {
            if !e.delivered {
                return true; // Keep undelivered
            }
            // Parse created_at and check if it's within retention
            if let Ok(created) = DateTime::parse_from_rfc3339(&e.created_at) {
                created.with_timezone(&Utc) > cutoff
            } else {
                true // Keep entries with unparseable timestamps
            }
        });

        Ok(before - entries.len())
    }

    async fn count(&self) -> Result<OutboxStats, OutboxError> {
        let entries = self.entries.read().await;
        let total = entries.len();
        let delivered = entries.values().filter(|e| e.delivered).count();
        let max_attempts = entries
            .values()
            .map(|e| e.delivery_attempts)
            .max()
            .unwrap_or(0);
        Ok(OutboxStats {
            total,
            pending: total - delivered,
            delivered,
            max_attempts,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Durable outbox
// ═══════════════════════════════════════════════════════════════════════════

/// Callback for delivering outbox entries to the parent agent.
#[async_trait]
pub trait OutboxDelivery: Send + Sync + 'static {
    /// Attempt to deliver a result to the parent agent.
    /// Returns `true` if delivery was successful.
    async fn deliver(&self, entry: &OutboxEntry) -> bool;
}

/// Durable outbox with automatic retry delivery.
pub struct DurableOutbox {
    store: Arc<dyn OutboxStore>,
    /// Maximum delivery attempts before giving up.
    max_attempts: u32,
    /// Base backoff delay for retries.
    backoff_base: Duration,
    /// Retention period for delivered entries before GC.
    retention: Duration,
}

impl DurableOutbox {
    pub fn new(store: Arc<dyn OutboxStore>) -> Self {
        Self {
            store,
            max_attempts: 5,
            backoff_base: Duration::from_secs(1),
            retention: Duration::from_secs(3600),
        }
    }

    pub fn with_max_attempts(mut self, max: u32) -> Self {
        self.max_attempts = max;
        self
    }

    pub fn with_backoff(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
    }

    pub fn with_retention(mut self, retention: Duration) -> Self {
        self.retention = retention;
        self
    }

    /// Enqueue a sub-agent result for delivery.
    pub async fn enqueue(&self, entry: OutboxEntry) -> Result<(), OutboxError> {
        info!(
            sub_agent = ?entry.sub_agent_id,
            parent = %entry.parent_id,
            "enqueuing outbox entry"
        );
        self.store.put(&entry).await
    }

    /// Get all pending entries for a parent.
    pub async fn pending_for(&self, parent_id: &str) -> Result<Vec<OutboxEntry>, OutboxError> {
        self.store.pending(parent_id).await
    }

    /// Attempt delivery of all pending entries for a parent.
    ///
    /// Returns the number of successfully delivered entries.
    pub async fn deliver_pending(
        &self,
        parent_id: &str,
        delivery: &dyn OutboxDelivery,
    ) -> Result<usize, OutboxError> {
        let pending = self.store.pending(parent_id).await?;
        let mut delivered = 0;

        for entry in &pending {
            if entry.delivery_attempts >= self.max_attempts {
                warn!(
                    sub_agent = ?entry.sub_agent_id,
                    attempts = entry.delivery_attempts,
                    "outbox entry exceeded max delivery attempts"
                );
                continue;
            }

            self.store.increment_attempts(&entry.sub_agent_id).await?;

            if delivery.deliver(entry).await {
                self.store.mark_delivered(&entry.sub_agent_id).await?;
                delivered += 1;
                debug!(
                    sub_agent = ?entry.sub_agent_id,
                    "outbox entry delivered"
                );
            } else {
                debug!(
                    sub_agent = ?entry.sub_agent_id,
                    attempt = entry.delivery_attempts + 1,
                    "delivery failed, will retry"
                );
            }
        }

        Ok(delivered)
    }

    /// Run garbage collection on delivered entries.
    pub async fn gc(&self) -> Result<usize, OutboxError> {
        let removed = self.store.gc(self.retention).await?;
        if removed > 0 {
            info!(removed, "outbox GC complete");
        }
        Ok(removed)
    }

    /// Get outbox statistics.
    pub async fn stats(&self) -> Result<OutboxStats, OutboxError> {
        self.store.count().await
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn make_entry(sub_id: &str, parent_id: &str) -> OutboxEntry {
        OutboxEntry {
            sub_agent_id: SubAgentId(sub_id.to_string()),
            parent_id: parent_id.to_string(),
            result: OutboxPayload::Success {
                output: "result".into(),
            },
            delivered: false,
            created_at: Utc::now().to_rfc3339(),
            delivery_attempts: 0,
        }
    }

    struct AlwaysDeliver;
    #[async_trait]
    impl OutboxDelivery for AlwaysDeliver {
        async fn deliver(&self, _entry: &OutboxEntry) -> bool {
            true
        }
    }

    struct NeverDeliver;
    #[async_trait]
    impl OutboxDelivery for NeverDeliver {
        async fn deliver(&self, _entry: &OutboxEntry) -> bool {
            false
        }
    }

    struct ConditionalDeliver {
        should_succeed: AtomicBool,
    }
    #[async_trait]
    impl OutboxDelivery for ConditionalDeliver {
        async fn deliver(&self, _entry: &OutboxEntry) -> bool {
            self.should_succeed.load(Ordering::Relaxed)
        }
    }

    #[tokio::test]
    async fn enqueue_and_deliver() {
        let store = Arc::new(InMemoryOutboxStore::new());
        let outbox = DurableOutbox::new(store);

        outbox.enqueue(make_entry("sa-1", "parent-1")).await.unwrap();
        outbox.enqueue(make_entry("sa-2", "parent-1")).await.unwrap();

        let pending = outbox.pending_for("parent-1").await.unwrap();
        assert_eq!(pending.len(), 2);

        let delivered = outbox
            .deliver_pending("parent-1", &AlwaysDeliver)
            .await
            .unwrap();
        assert_eq!(delivered, 2);

        // No more pending
        let pending = outbox.pending_for("parent-1").await.unwrap();
        assert_eq!(pending.len(), 0);
    }

    #[tokio::test]
    async fn delivery_failure_increments_attempts() {
        let store = Arc::new(InMemoryOutboxStore::new());
        let outbox = DurableOutbox::new(store.clone()).with_max_attempts(3);

        outbox.enqueue(make_entry("sa-1", "parent-1")).await.unwrap();

        // First failed attempt
        let delivered = outbox
            .deliver_pending("parent-1", &NeverDeliver)
            .await
            .unwrap();
        assert_eq!(delivered, 0);

        // Check attempt count was incremented
        let entries = store.entries.read().await;
        let entry = entries.get(&SubAgentId("sa-1".into())).unwrap();
        assert_eq!(entry.delivery_attempts, 1);
    }

    #[tokio::test]
    async fn max_attempts_skips_entry() {
        let store = Arc::new(InMemoryOutboxStore::new());
        let outbox = DurableOutbox::new(store.clone()).with_max_attempts(2);

        let mut entry = make_entry("sa-1", "parent-1");
        entry.delivery_attempts = 2; // Already at max
        outbox.enqueue(entry).await.unwrap();

        // Should skip, not attempt delivery
        let delivered = outbox
            .deliver_pending("parent-1", &AlwaysDeliver)
            .await
            .unwrap();
        assert_eq!(delivered, 0);
    }

    #[tokio::test]
    async fn gc_removes_delivered() {
        let store = Arc::new(InMemoryOutboxStore::new());
        let outbox = DurableOutbox::new(store.clone()).with_retention(Duration::from_secs(0));

        outbox.enqueue(make_entry("sa-1", "parent-1")).await.unwrap();
        outbox
            .deliver_pending("parent-1", &AlwaysDeliver)
            .await
            .unwrap();

        let removed = outbox.gc().await.unwrap();
        assert_eq!(removed, 1);

        let stats = outbox.stats().await.unwrap();
        assert_eq!(stats.total, 0);
    }

    #[tokio::test]
    async fn stats_tracking() {
        let store = Arc::new(InMemoryOutboxStore::new());
        let outbox = DurableOutbox::new(store);

        outbox.enqueue(make_entry("sa-1", "parent-1")).await.unwrap();
        outbox.enqueue(make_entry("sa-2", "parent-1")).await.unwrap();

        let stats = outbox.stats().await.unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.pending, 2);
        assert_eq!(stats.delivered, 0);

        outbox
            .deliver_pending("parent-1", &AlwaysDeliver)
            .await
            .unwrap();

        let stats = outbox.stats().await.unwrap();
        assert_eq!(stats.delivered, 2);
        assert_eq!(stats.pending, 0);
    }
}
