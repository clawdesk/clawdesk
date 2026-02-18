//! # Approval Queue Protocol — Human-Gate with Item-Level Granularity
//!
//! Extends the Gate mechanism with channel-delivered approval queues,
//! per-item approval/rejection, and a persistent approval index.
//!
//! ## State Machine (per item)
//!
//! ```text
//! Pending → Approved | Rejected | Expired
//! ```
//!
//! ## Quorum Functions
//!
//! ```text
//! AllApproved  → all items ∈ {Approved}
//! AnyApproved  → ∃ item ∈ {Approved}
//! Threshold(k) → |{Approved}| ≥ k
//! ItemLevel    → pass approved, drop rejected (most useful for Life OS)
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Approval Item State Machine ─────────────────────────────────────────────

/// State of a single approval item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalState {
    Pending,
    Approved,
    Rejected,
    Expired,
}

/// A single item within an approval queue requiring human review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalItem {
    /// Unique item identifier
    pub id: String,
    /// Human-readable description
    pub description: String,
    /// Current state
    pub state: ApprovalState,
    /// Structured data (e.g., action item details)
    pub data: serde_json::Value,
    /// When this item was created
    pub created_at: DateTime<Utc>,
    /// When this item expires if not acted upon
    pub expires_at: DateTime<Utc>,
    /// Default action if expired
    pub default_on_expire: ApprovalState,
    /// Category tag (e.g., "action-item", "email-send", "task-create")
    pub category: String,
}

impl ApprovalItem {
    /// Check if this item has expired and transition if so.
    pub fn check_expired(&mut self, now: DateTime<Utc>) -> bool {
        if self.state == ApprovalState::Pending && now >= self.expires_at {
            self.state = self.default_on_expire;
            true
        } else {
            false
        }
    }

    /// Approve this item. Only transitions from Pending.
    pub fn approve(&mut self) -> bool {
        if self.state == ApprovalState::Pending {
            self.state = ApprovalState::Approved;
            true
        } else {
            false
        }
    }

    /// Reject this item. Only transitions from Pending.
    pub fn reject(&mut self) -> bool {
        if self.state == ApprovalState::Pending {
            self.state = ApprovalState::Rejected;
            true
        } else {
            false
        }
    }
}

// ── Quorum Policy ───────────────────────────────────────────────────────────

/// How item states are aggregated to resolve the overall gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QuorumPolicy {
    /// All items must be approved
    AllApproved,
    /// At least one item must be approved
    AnyApproved,
    /// At least k items must be approved
    Threshold(usize),
    /// Pass approved items, drop rejected — most flexible
    ItemLevel,
}

// ── Approval Queue ──────────────────────────────────────────────────────────

/// A complete approval queue associated with a pipeline gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalQueue {
    /// Unique queue identifier
    pub id: String,
    /// Associated pipeline run ID (for resumption after resolution)
    pub pipeline_run_id: String,
    /// Gate step index within the pipeline
    pub gate_step_index: usize,
    /// Human-readable title for the approval request
    pub title: String,
    /// Items requiring approval
    pub items: Vec<ApprovalItem>,
    /// How to aggregate item decisions
    pub policy: QuorumPolicy,
    /// Channel to deliver the approval request to (e.g., "telegram", "slack")
    pub delivery_channel: Option<String>,
    /// When the queue was created
    pub created_at: DateTime<Utc>,
    /// When the queue was resolved (all items decided or expired)
    pub resolved_at: Option<DateTime<Utc>>,
    /// User who submitted approvals
    pub decided_by: Option<String>,
}

impl ApprovalQueue {
    /// Create a new approval queue.
    pub fn new(
        id: impl Into<String>,
        pipeline_run_id: impl Into<String>,
        gate_step_index: usize,
        title: impl Into<String>,
        items: Vec<ApprovalItem>,
        policy: QuorumPolicy,
    ) -> Self {
        Self {
            id: id.into(),
            pipeline_run_id: pipeline_run_id.into(),
            gate_step_index,
            title: title.into(),
            items,
            policy,
            delivery_channel: None,
            created_at: Utc::now(),
            resolved_at: None,
            decided_by: None,
        }
    }

    /// Approve a specific item by ID.
    pub fn approve_item(&mut self, item_id: &str) -> bool {
        self.items
            .iter_mut()
            .find(|i| i.id == item_id)
            .map(|i| i.approve())
            .unwrap_or(false)
    }

    /// Reject a specific item by ID.
    pub fn reject_item(&mut self, item_id: &str) -> bool {
        self.items
            .iter_mut()
            .find(|i| i.id == item_id)
            .map(|i| i.reject())
            .unwrap_or(false)
    }

    /// Approve all pending items.
    pub fn approve_all(&mut self) {
        for item in &mut self.items {
            item.approve();
        }
    }

    /// Reject all pending items.
    pub fn reject_all(&mut self) {
        for item in &mut self.items {
            item.reject();
        }
    }

    /// Check for expired items and transition them.
    pub fn check_expirations(&mut self, now: DateTime<Utc>) {
        for item in &mut self.items {
            item.check_expired(now);
        }
    }

    /// Whether all items have been decided (no more Pending).
    pub fn is_resolved(&self) -> bool {
        self.items.iter().all(|i| i.state != ApprovalState::Pending)
    }

    /// Resolve the queue and return the decision.
    pub fn resolve(&mut self, now: DateTime<Utc>) -> QueueResolution {
        self.check_expirations(now);

        if !self.is_resolved() {
            return QueueResolution::Pending;
        }

        self.resolved_at = Some(now);

        match &self.policy {
            QuorumPolicy::AllApproved => {
                if self.items.iter().all(|i| i.state == ApprovalState::Approved) {
                    QueueResolution::Proceed(self.items.iter().map(|i| i.id.clone()).collect())
                } else {
                    QueueResolution::Abort
                }
            }
            QuorumPolicy::AnyApproved => {
                let approved: Vec<String> = self
                    .items
                    .iter()
                    .filter(|i| i.state == ApprovalState::Approved)
                    .map(|i| i.id.clone())
                    .collect();
                if approved.is_empty() {
                    QueueResolution::Abort
                } else {
                    QueueResolution::Proceed(approved)
                }
            }
            QuorumPolicy::Threshold(k) => {
                let approved: Vec<String> = self
                    .items
                    .iter()
                    .filter(|i| i.state == ApprovalState::Approved)
                    .map(|i| i.id.clone())
                    .collect();
                if approved.len() >= *k {
                    QueueResolution::Proceed(approved)
                } else {
                    QueueResolution::Abort
                }
            }
            QuorumPolicy::ItemLevel => {
                let approved: Vec<String> = self
                    .items
                    .iter()
                    .filter(|i| i.state == ApprovalState::Approved)
                    .map(|i| i.id.clone())
                    .collect();
                if approved.is_empty() {
                    QueueResolution::Abort
                } else {
                    QueueResolution::PartialProceed {
                        approved_ids: approved,
                        rejected_ids: self
                            .items
                            .iter()
                            .filter(|i| i.state == ApprovalState::Rejected)
                            .map(|i| i.id.clone())
                            .collect(),
                        expired_ids: self
                            .items
                            .iter()
                            .filter(|i| i.state == ApprovalState::Expired)
                            .map(|i| i.id.clone())
                            .collect(),
                    }
                }
            }
        }
    }

    /// Count items by state.
    pub fn counts(&self) -> ApprovalCounts {
        let mut counts = ApprovalCounts::default();
        for item in &self.items {
            match item.state {
                ApprovalState::Pending => counts.pending += 1,
                ApprovalState::Approved => counts.approved += 1,
                ApprovalState::Rejected => counts.rejected += 1,
                ApprovalState::Expired => counts.expired += 1,
            }
        }
        counts
    }
}

/// Resolution outcome for an approval queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QueueResolution {
    /// Not all items decided yet
    Pending,
    /// All conditions met — proceed with the listed item IDs
    Proceed(Vec<String>),
    /// Conditions not met — abort the pipeline
    Abort,
    /// ItemLevel policy — partial proceed with segregated results
    PartialProceed {
        approved_ids: Vec<String>,
        rejected_ids: Vec<String>,
        expired_ids: Vec<String>,
    },
}

/// Summary counts for display.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApprovalCounts {
    pub pending: usize,
    pub approved: usize,
    pub rejected: usize,
    pub expired: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(id: &str, timeout_secs: i64) -> ApprovalItem {
        let now = Utc::now();
        ApprovalItem {
            id: id.to_string(),
            description: format!("Item {id}"),
            state: ApprovalState::Pending,
            data: serde_json::Value::Null,
            created_at: now,
            expires_at: now + chrono::Duration::seconds(timeout_secs),
            default_on_expire: ApprovalState::Rejected,
            category: "test".into(),
        }
    }

    #[test]
    fn item_level_partial() {
        let items = vec![
            make_item("a", 3600),
            make_item("b", 3600),
            make_item("c", 3600),
        ];
        let mut queue = ApprovalQueue::new("q1", "run1", 0, "Test", items, QuorumPolicy::ItemLevel);

        queue.approve_item("a");
        queue.reject_item("b");
        queue.reject_item("c");

        let result = queue.resolve(Utc::now());
        match result {
            QueueResolution::PartialProceed {
                approved_ids,
                rejected_ids,
                ..
            } => {
                assert_eq!(approved_ids, vec!["a"]);
                assert_eq!(rejected_ids.len(), 2);
            }
            _ => panic!("Expected PartialProceed"),
        }
    }
}
