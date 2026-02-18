//! Auto-reply and send policy types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Trigger type — why the agent was invoked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerType {
    /// Direct mention of the bot.
    Mention,
    /// Direct message (1:1).
    DirectMessage,
    /// Command prefix (e.g., /ask, !bot).
    Command { command: String },
    /// Scheduled cron invocation.
    Scheduled { task_id: String },
    /// Channel-specific trigger (e.g., Telegram bot command).
    ChannelSpecific { channel: String, trigger: String },
    /// API-initiated (from gateway HTTP/WS).
    Api,
    /// Webhook-initiated.
    Webhook { source: String },
    /// Voice wake word.
    VoiceWake,
}

/// Classification result for an inbound message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerClassification {
    pub trigger: TriggerType,
    pub confidence: f64,
    pub should_reply: bool,
    pub priority: ReplyPriority,
}

/// Reply priority levels for queue ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReplyPriority {
    Low = 0,
    Normal = 1,
    High = 2,
    Urgent = 3,
}

/// Send policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendPolicy {
    /// Token bucket capacity.
    pub bucket_capacity: u32,
    /// Refill rate (messages per second).
    pub refill_rate: f64,
    /// Queue max depth before backpressure.
    pub max_queue_depth: usize,
    /// Per-channel overrides.
    pub channel_overrides: Vec<ChannelSendPolicy>,
}

impl Default for SendPolicy {
    fn default() -> Self {
        Self {
            bucket_capacity: 30,
            refill_rate: 1.0,
            max_queue_depth: 100,
            channel_overrides: Vec::new(),
        }
    }
}

/// Per-channel send policy override.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSendPolicy {
    pub channel_id: String,
    pub bucket_capacity: u32,
    pub refill_rate: f64,
}

/// Reply pipeline stage metadata.
#[derive(Debug, Clone)]
pub struct PipelineStage {
    pub name: &'static str,
    pub started_at: DateTime<Utc>,
    pub duration_ms: Option<u64>,
    pub skipped: bool,
}

/// Delivery receipt from outbound message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryStatus {
    pub message_id: String,
    pub channel: String,
    pub status: DeliveryState,
    pub timestamp: DateTime<Utc>,
    pub retry_count: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryState {
    Queued,
    Sending,
    Delivered,
    Failed,
    DeadLettered,
}
