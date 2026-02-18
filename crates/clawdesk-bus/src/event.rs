//! Typed event envelope for the reactive bus.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Priority class for event dispatch.
///
/// Weighted fair queuing assigns each class a weight:
/// - Urgent: w=8  (email triage, security alerts)
/// - Standard: w=4 (action items, approval queues)
/// - Batch: w=1   (morning briefings, social snapshots)
///
/// Virtual finish time: F_i = max(F_{i-1}, arrival_i) + size_i / w_k
/// O(log K) dispatch per event via min-heap on virtual finish times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Priority {
    /// Security alerts, urgent email triage — weight 8
    Urgent = 0,
    /// Action items, approval queues — weight 4
    Standard = 1,
    /// Morning briefings, social snapshots, digest compilation — weight 1
    Batch = 2,
}

impl Priority {
    /// WFQ weight for this priority class.
    pub const fn weight(self) -> u64 {
        match self {
            Priority::Urgent => 8,
            Priority::Standard => 4,
            Priority::Batch => 1,
        }
    }
}

/// Typed event categories emitted by subsystems.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    /// Inbound message on any channel
    MessageReceived,
    /// Outbound message delivered
    MessageSent,
    /// Email fetched from IMAP/Gmail API
    EmailIngested,
    /// Calendar event discovered
    CalendarEvent,
    /// Meeting transcript available (Fathom, etc.)
    TranscriptReady,
    /// Contact interaction recorded
    ContactInteraction,
    /// Social metric snapshot collected
    SocialMetricSnapshot,
    /// Approval request created
    ApprovalRequested,
    /// Approval resolved (approved/rejected/expired)
    ApprovalResolved,
    /// Pipeline completed
    PipelineCompleted,
    /// Agent heartbeat fired
    HeartbeatFired,
    /// Cron job executed
    CronExecuted,
    /// Memory stored
    MemoryStored,
    /// Relationship health alert (decay below threshold)
    RelationshipAlert,
    /// Digest window closed, ready for compilation
    DigestWindowClosed,
    /// Backup completed
    BackupCompleted,
    /// Custom event from plugin or skill
    Custom(String),
}

/// The core event envelope. Immutable once created.
///
/// Every event has a unique ID, a producing topic, a kind, a priority,
/// a timestamp, and an arbitrary JSON payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique event identifier
    pub id: Uuid,
    /// Topic this event was published to
    pub topic: String,
    /// Semantic category
    pub kind: EventKind,
    /// Dispatch priority class
    pub priority: Priority,
    /// Wall-clock time of event creation
    pub timestamp: DateTime<Utc>,
    /// Monotonic offset within the topic ring buffer
    pub offset: u64,
    /// Arbitrary structured payload
    pub payload: serde_json::Value,
    /// Optional correlation ID to link related events
    pub correlation_id: Option<Uuid>,
    /// Source subsystem that produced this event
    pub source: String,
}

impl Event {
    /// Create a new event with auto-generated ID and current timestamp.
    pub fn new(
        topic: impl Into<String>,
        kind: EventKind,
        priority: Priority,
        payload: serde_json::Value,
        source: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            topic: topic.into(),
            kind,
            priority,
            timestamp: Utc::now(),
            offset: 0, // assigned by topic on publish
            payload,
            correlation_id: None,
            source: source.into(),
        }
    }

    /// Attach a correlation ID to link this event to a causal chain.
    pub fn with_correlation(mut self, id: Uuid) -> Self {
        self.correlation_id = Some(id);
        self
    }
}
