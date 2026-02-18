//! CLI harness abstraction for external agent runtimes.
//!
//! This models process-level agent harnesses (Claude Code, Codex CLI, Gemini CLI)
//! separately from HTTP provider calls. Implementations can live in runtime-facing
//! crates and plug into the same orchestration surface.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Known harness kinds.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    Native,
    ClaudeCode,
    CodexCli,
    GeminiCli,
    OpenCode,
    Custom(String),
}

/// Scheduling priority for long-lived harness sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessPriority {
    Urgent,
    Standard,
    Background,
}

impl Default for HarnessPriority {
    fn default() -> Self {
        Self::Standard
    }
}

/// Capability declaration for a harness runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessCapabilities {
    pub supports_streaming: bool,
    pub supports_workspace_write: bool,
    pub supports_web_search: bool,
    pub supports_tools: bool,
    pub supports_tmux_persistence: bool,
    pub max_context_tokens: Option<usize>,
}

impl Default for HarnessCapabilities {
    fn default() -> Self {
        Self {
            supports_streaming: true,
            supports_workspace_write: true,
            supports_web_search: false,
            supports_tools: true,
            supports_tmux_persistence: false,
            max_context_tokens: None,
        }
    }
}

/// Spawn configuration for a new harness session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessSpawnConfig {
    pub harness: HarnessKind,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub task_prompt: String,
    pub timeout_secs: u64,
    pub priority: HarnessPriority,
    pub metadata: HashMap<String, String>,
}

impl HarnessSpawnConfig {
    pub fn new(harness: HarnessKind, command: impl Into<String>, task_prompt: impl Into<String>) -> Self {
        Self {
            harness,
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: HashMap::new(),
            task_prompt: task_prompt.into(),
            timeout_secs: 3600,
            priority: HarnessPriority::Standard,
            metadata: HashMap::new(),
        }
    }
}

/// Session lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessSessionState {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}

/// A live (or completed) harness session handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessSession {
    pub id: String,
    pub harness: HarnessKind,
    pub state: HarnessSessionState,
    pub pid: Option<u32>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub metadata: HashMap<String, String>,
}

/// Stream/event payload from a harness session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HarnessEvent {
    Output {
        session_id: String,
        chunk: String,
        is_stderr: bool,
    },
    Status {
        session_id: String,
        state: HarnessSessionState,
    },
    Completed {
        session_id: String,
        exit_code: Option<i32>,
    },
    Error {
        session_id: String,
        message: String,
    },
}

/// Harness-level errors.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("send failed: {0}")]
    SendFailed(String),
    #[error("poll failed: {0}")]
    PollFailed(String),
    #[error("kill failed: {0}")]
    KillFailed(String),
    #[error("harness unavailable: {0}")]
    Unavailable(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Process-level harness interface.
#[async_trait]
pub trait Harness: Send + Sync {
    /// Human-readable harness name.
    fn name(&self) -> &str;

    /// Harness kind.
    fn kind(&self) -> HarnessKind;

    /// Declared runtime capabilities.
    fn capabilities(&self) -> HarnessCapabilities;

    /// Spawn a new harness session.
    async fn spawn(&self, config: HarnessSpawnConfig) -> Result<HarnessSession, HarnessError>;

    /// Send additional input to a running session.
    async fn send(&self, session: &HarnessSession, input: &str) -> Result<(), HarnessError>;

    /// Poll the next event from a running session.
    async fn poll(&self, session: &HarnessSession) -> Result<HarnessEvent, HarnessError>;

    /// Kill a running session.
    async fn kill(&self, session: &HarnessSession) -> Result<(), HarnessError>;
}

