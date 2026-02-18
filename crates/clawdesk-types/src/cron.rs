//! Cron & scheduling types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A scheduled task definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTask {
    pub id: String,
    pub name: String,
    /// Cron expression (standard 5-field or extended 6-field).
    pub schedule: String,
    /// The prompt to send to the agent.
    pub prompt: String,
    /// Which agent to run.
    pub agent_id: Option<String>,
    /// Where to deliver results.
    pub delivery_targets: Vec<DeliveryTarget>,
    /// Whether to skip if previous run is still active.
    pub skip_if_running: bool,
    /// Maximum execution time in seconds.
    pub timeout_secs: u64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Where to deliver cron task results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeliveryTarget {
    /// Send to a specific channel + conversation.
    Channel {
        channel_id: String,
        conversation_id: String,
    },
    /// Store in session only.
    Session { session_key: String },
    /// Call a webhook URL.
    Webhook { url: String },
}

/// Record of a cron task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronRunLog {
    pub task_id: String,
    pub run_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: CronRunStatus,
    pub result_preview: Option<String>,
    pub error: Option<String>,
    pub tokens_used: Option<u64>,
}

/// Status of a cron run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CronRunStatus {
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Skipped,
    Cancelled,
}

/// Parsed cron schedule with next execution time.
#[derive(Debug, Clone)]
pub struct ParsedSchedule {
    pub expression: String,
    pub next_run: DateTime<Utc>,
    pub timezone: String,
}
