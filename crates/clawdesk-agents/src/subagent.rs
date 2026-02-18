//! Sub-agent spawn, supervision, and cleanup.
//!
//! A sub-agent is a child agent spawned by a parent during execution.
//! This module provides:
//!
//! - `SubAgentHandle` — lightweight reference to a running sub-agent
//! - `SpawnConfig` — configuration for spawning
//! - `CleanupPolicy` — when/how to reap sub-agents
//! - Durable outbox pattern for result delivery
//!
//! Lifecycle:
//! ```text
//! spawn → running → (completed | failed | timed_out) → cleanup
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Sub-agent identity
// ---------------------------------------------------------------------------

/// Unique identifier for a sub-agent instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubAgentId(pub String);

impl SubAgentId {
    /// Create a new sub-agent ID.
    pub fn new(parent_id: &str, child_agent: &str, seq: u64) -> Self {
        Self(format!("{parent_id}::{child_agent}::{seq}"))
    }

    /// Parse a sub-agent ID to extract (parent, child_agent, seq).
    pub fn parse(&self) -> Option<(&str, &str, u64)> {
        let parts: Vec<&str> = self.0.splitn(3, "::").collect();
        if parts.len() == 3 {
            let seq = parts[2].parse().ok()?;
            Some((parts[0], parts[1], seq))
        } else {
            None
        }
    }

    /// The parent agent's ID.
    pub fn parent_id(&self) -> Option<&str> {
        self.parse().map(|(p, _, _)| p)
    }
}

// ---------------------------------------------------------------------------
// Spawn configuration
// ---------------------------------------------------------------------------

/// Configuration for spawning a sub-agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnConfig {
    /// Agent definition ID to spawn.
    pub agent_id: String,
    /// Task to assign.
    pub task: String,
    /// Maximum execution time.
    #[serde(default = "default_spawn_timeout")]
    pub timeout_secs: u64,
    /// Maximum recursion depth (sub-agents spawning sub-agents).
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Maximum concurrent sub-agents for this parent.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// How to deliver results.
    #[serde(default)]
    pub result_format: ResultFormat,
    /// Where to announce completion.
    #[serde(default)]
    pub announce_target: AnnounceTarget,
    /// Cleanup policy.
    #[serde(default)]
    pub cleanup: CleanupPolicy,
}

fn default_spawn_timeout() -> u64 { 300 }
fn default_max_depth() -> u32 { 3 }
fn default_max_concurrent() -> usize { 5 }

/// How sub-agent results are formatted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultFormat {
    /// Plain text.
    Text,
    /// Structured JSON.
    Json,
    /// Markdown.
    Markdown,
}

impl Default for ResultFormat {
    fn default() -> Self { Self::Text }
}

/// Where to announce sub-agent completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnounceTarget {
    /// Send back to parent agent only.
    Parent,
    /// Post to a specific channel.
    Channel(String),
    /// No announcement (results are polled).
    None,
}

impl Default for AnnounceTarget {
    fn default() -> Self { Self::Parent }
}

/// When to clean up sub-agent state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicy {
    /// Remove state immediately after result delivery.
    Immediate,
    /// Keep for N seconds after completion.
    RetainSecs(u64),
    /// Never auto-clean (manual cleanup required).
    Manual,
}

impl Default for CleanupPolicy {
    fn default() -> Self { Self::RetainSecs(3600) }
}

// ---------------------------------------------------------------------------
// Sub-agent state machine
// ---------------------------------------------------------------------------

/// Lifecycle state of a sub-agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentState {
    /// Queued, waiting for a slot.
    Queued,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Completed,
    /// Failed with an error.
    Failed,
    /// Exceeded timeout.
    TimedOut,
    /// Cancelled by parent or operator.
    Cancelled,
    /// State cleaned up.
    Reaped,
}

impl SubAgentState {
    /// Whether the sub-agent is in a terminal state.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled | Self::Reaped
        )
    }
}

// ---------------------------------------------------------------------------
// Sub-agent handle
// ---------------------------------------------------------------------------

/// Lightweight handle to a running or completed sub-agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentHandle {
    pub id: SubAgentId,
    pub config: SpawnConfig,
    pub state: SubAgentState,
    pub depth: u32,
    pub output: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

impl SubAgentHandle {
    /// Create a new handle in Queued state.
    pub fn new(id: SubAgentId, config: SpawnConfig, depth: u32) -> Self {
        Self {
            id,
            config,
            state: SubAgentState::Queued,
            depth,
            output: None,
            error: None,
            started_at: None,
            completed_at: None,
        }
    }

    /// Transition to Running.
    pub fn start(&mut self, timestamp: &str) {
        self.state = SubAgentState::Running;
        self.started_at = Some(timestamp.to_string());
    }

    /// Transition to Completed with output.
    pub fn complete(&mut self, output: String, timestamp: &str) {
        self.state = SubAgentState::Completed;
        self.output = Some(output);
        self.completed_at = Some(timestamp.to_string());
    }

    /// Transition to Failed with error.
    pub fn fail(&mut self, error: String, timestamp: &str) {
        self.state = SubAgentState::Failed;
        self.error = Some(error);
        self.completed_at = Some(timestamp.to_string());
    }

    /// Transition to TimedOut.
    pub fn timeout(&mut self, timestamp: &str) {
        self.state = SubAgentState::TimedOut;
        self.error = Some("Execution timed out".to_string());
        self.completed_at = Some(timestamp.to_string());
    }

    /// Cancel.
    pub fn cancel(&mut self, timestamp: &str) {
        self.state = SubAgentState::Cancelled;
        self.completed_at = Some(timestamp.to_string());
    }
}

// ---------------------------------------------------------------------------
// Durable outbox
// ---------------------------------------------------------------------------

/// A durable outbox entry for sub-agent result delivery.
///
/// The outbox pattern ensures results are not lost even if the parent
/// agent is unavailable at completion time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub sub_agent_id: SubAgentId,
    pub parent_id: String,
    pub result: OutboxPayload,
    pub delivered: bool,
    pub created_at: String,
    pub delivery_attempts: u32,
}

/// Outbox payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxPayload {
    Success { output: String },
    Failure { error: String },
    Timeout,
}

/// In-memory outbox (would be persisted in production).
#[derive(Debug, Default)]
pub struct Outbox {
    pub entries: Vec<OutboxEntry>,
}

impl Outbox {
    pub fn new() -> Self { Self::default() }

    /// Enqueue a result.
    pub fn enqueue(&mut self, entry: OutboxEntry) {
        self.entries.push(entry);
    }

    /// Get undelivered entries for a parent.
    pub fn pending_for_parent(&self, parent_id: &str) -> Vec<&OutboxEntry> {
        self.entries
            .iter()
            .filter(|e| e.parent_id == parent_id && !e.delivered)
            .collect()
    }

    /// Mark an entry as delivered.
    pub fn mark_delivered(&mut self, sub_agent_id: &SubAgentId) {
        for entry in &mut self.entries {
            if entry.sub_agent_id == *sub_agent_id {
                entry.delivered = true;
            }
        }
    }

    /// Remove delivered entries older than the retention period.
    pub fn gc(&mut self) {
        self.entries.retain(|e| !e.delivered);
    }
}

// ---------------------------------------------------------------------------
// Spawn validation
// ---------------------------------------------------------------------------

/// Validate a spawn request.
pub fn validate_spawn(
    config: &SpawnConfig,
    current_depth: u32,
    active_count: usize,
) -> Vec<String> {
    let mut errors = Vec::new();

    if config.agent_id.is_empty() {
        errors.push("agent_id is empty".into());
    }

    if config.task.is_empty() {
        errors.push("task is empty".into());
    }

    if current_depth >= config.max_depth {
        errors.push(format!(
            "Maximum spawn depth exceeded: {} >= {}",
            current_depth, config.max_depth
        ));
    }

    if active_count >= config.max_concurrent {
        errors.push(format!(
            "Maximum concurrent sub-agents exceeded: {} >= {}",
            active_count, config.max_concurrent
        ));
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sub_agent_id_parse() {
        let id = SubAgentId::new("parent", "child", 42);
        assert_eq!(id.0, "parent::child::42");
        let (p, c, s) = id.parse().unwrap();
        assert_eq!(p, "parent");
        assert_eq!(c, "child");
        assert_eq!(s, 42);
    }

    #[test]
    fn test_sub_agent_id_parent() {
        let id = SubAgentId::new("my-agent", "sub", 1);
        assert_eq!(id.parent_id(), Some("my-agent"));
    }

    #[test]
    fn test_handle_lifecycle() {
        let config = SpawnConfig {
            agent_id: "child".into(),
            task: "do stuff".into(),
            timeout_secs: 60,
            max_depth: 3,
            max_concurrent: 5,
            result_format: ResultFormat::Text,
            announce_target: AnnounceTarget::Parent,
            cleanup: CleanupPolicy::Immediate,
        };
        let id = SubAgentId::new("parent", "child", 1);
        let mut handle = SubAgentHandle::new(id, config, 1);

        assert_eq!(handle.state, SubAgentState::Queued);
        assert!(!handle.state.is_terminal());

        handle.start("2025-01-01T00:00:00Z");
        assert_eq!(handle.state, SubAgentState::Running);

        handle.complete("result".to_string(), "2025-01-01T00:01:00Z");
        assert_eq!(handle.state, SubAgentState::Completed);
        assert!(handle.state.is_terminal());
        assert_eq!(handle.output.as_deref(), Some("result"));
    }

    #[test]
    fn test_handle_failure() {
        let config = SpawnConfig {
            agent_id: "child".into(),
            task: "fail".into(),
            timeout_secs: 60,
            max_depth: 3,
            max_concurrent: 5,
            result_format: ResultFormat::default(),
            announce_target: AnnounceTarget::default(),
            cleanup: CleanupPolicy::default(),
        };
        let id = SubAgentId::new("parent", "child", 2);
        let mut handle = SubAgentHandle::new(id, config, 0);
        handle.start("t0");
        handle.fail("oops".to_string(), "t1");
        assert_eq!(handle.state, SubAgentState::Failed);
        assert_eq!(handle.error.as_deref(), Some("oops"));
    }

    #[test]
    fn test_handle_timeout() {
        let config = SpawnConfig {
            agent_id: "child".into(),
            task: "slow".into(),
            timeout_secs: 5,
            max_depth: 3,
            max_concurrent: 5,
            result_format: ResultFormat::default(),
            announce_target: AnnounceTarget::default(),
            cleanup: CleanupPolicy::default(),
        };
        let id = SubAgentId::new("parent", "child", 3);
        let mut handle = SubAgentHandle::new(id, config, 0);
        handle.start("t0");
        handle.timeout("t1");
        assert_eq!(handle.state, SubAgentState::TimedOut);
        assert!(handle.state.is_terminal());
    }

    #[test]
    fn test_outbox_enqueue_and_pending() {
        let mut outbox = Outbox::new();
        outbox.enqueue(OutboxEntry {
            sub_agent_id: SubAgentId::new("p", "c", 1),
            parent_id: "p".into(),
            result: OutboxPayload::Success { output: "done".into() },
            delivered: false,
            created_at: "t0".into(),
            delivery_attempts: 0,
        });

        let pending = outbox.pending_for_parent("p");
        assert_eq!(pending.len(), 1);

        outbox.mark_delivered(&SubAgentId::new("p", "c", 1));
        let pending = outbox.pending_for_parent("p");
        assert!(pending.is_empty());
    }

    #[test]
    fn test_outbox_gc() {
        let mut outbox = Outbox::new();
        outbox.enqueue(OutboxEntry {
            sub_agent_id: SubAgentId::new("p", "c", 1),
            parent_id: "p".into(),
            result: OutboxPayload::Timeout,
            delivered: true,
            created_at: "t0".into(),
            delivery_attempts: 1,
        });
        outbox.enqueue(OutboxEntry {
            sub_agent_id: SubAgentId::new("p", "c", 2),
            parent_id: "p".into(),
            result: OutboxPayload::Failure { error: "err".into() },
            delivered: false,
            created_at: "t1".into(),
            delivery_attempts: 0,
        });

        outbox.gc();
        assert_eq!(outbox.entries.len(), 1);
        assert!(!outbox.entries[0].delivered);
    }

    #[test]
    fn test_validate_spawn_ok() {
        let config = SpawnConfig {
            agent_id: "agent".into(),
            task: "do it".into(),
            timeout_secs: 60,
            max_depth: 3,
            max_concurrent: 5,
            result_format: ResultFormat::default(),
            announce_target: AnnounceTarget::default(),
            cleanup: CleanupPolicy::default(),
        };
        let errors = validate_spawn(&config, 0, 0);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_spawn_depth_exceeded() {
        let config = SpawnConfig {
            agent_id: "agent".into(),
            task: "deep".into(),
            timeout_secs: 60,
            max_depth: 2,
            max_concurrent: 5,
            result_format: ResultFormat::default(),
            announce_target: AnnounceTarget::default(),
            cleanup: CleanupPolicy::default(),
        };
        let errors = validate_spawn(&config, 2, 0);
        assert!(errors.iter().any(|e| e.contains("depth")));
    }

    #[test]
    fn test_validate_spawn_concurrency_exceeded() {
        let config = SpawnConfig {
            agent_id: "agent".into(),
            task: "busy".into(),
            timeout_secs: 60,
            max_depth: 3,
            max_concurrent: 2,
            result_format: ResultFormat::default(),
            announce_target: AnnounceTarget::default(),
            cleanup: CleanupPolicy::default(),
        };
        let errors = validate_spawn(&config, 0, 2);
        assert!(errors.iter().any(|e| e.contains("concurrent")));
    }

    #[test]
    fn test_result_format_default() {
        assert_eq!(ResultFormat::default(), ResultFormat::Text);
    }

    #[test]
    fn test_cleanup_policy_default() {
        assert_eq!(CleanupPolicy::default(), CleanupPolicy::RetainSecs(3600));
    }
}
