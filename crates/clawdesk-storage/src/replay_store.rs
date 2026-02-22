//! Chat replay store — paired input/response turn storage for replay.
//!
//! The `ConversationStore` stores individual `AgentMessage` records as a
//! flat list. This module adds a higher-level abstraction: the **`ChatTurn`**,
//! which pairs a user input with its assistant response, tool calls, and
//! per-turn metadata (tokens, model, timing, finish reason).
//!
//! ## Why this exists
//!
//! Without turn-level grouping, replaying a conversation requires
//! reconstructing input→response associations from temporal adjacency
//! in the flat message list — brittle and lossy (tool calls are
//! interleaved, multi-round loops are invisible).
//!
//! `ChatTurn` makes each exchange a first-class entity with:
//! - **Deterministic replay**: load turns in sequence, each self-contained.
//! - **Full metadata**: token counts, model, latency, finish reason.
//! - **Tool call capture**: intermediate tool use/result pairs preserved.
//! - **Turn ID**: stable identifier for linking to other systems.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use clawdesk_types::error::StorageError;
use clawdesk_types::session::SessionKey;
use serde::{Deserialize, Serialize};

/// Unique identifier for a turn within a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(pub String);

impl TurnId {
    pub fn new(session_key: &str, sequence: u64) -> Self {
        Self(format!("{}:turn:{}", session_key, sequence))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A tool call made during a turn (request + response pair).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExchange {
    /// Tool call ID (matches LLM tool_use id).
    pub call_id: String,
    /// Tool name.
    pub tool_name: String,
    /// Tool input (JSON arguments).
    pub input: serde_json::Value,
    /// Tool output.
    pub output: String,
    /// Time taken by the tool call.
    pub duration_ms: u64,
}

/// A single paired exchange: user input → assistant response.
///
/// This is the fundamental replayable unit. Loading a session's turns
/// in sequence reconstructs the full conversation with all metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    /// Unique turn identifier.
    pub id: TurnId,
    /// Session this turn belongs to.
    pub session_id: String,
    /// Sequence number within the session (0-indexed, monotonic).
    pub sequence: u64,

    // ── Input ────────────────────────────────────────────────
    /// The user's input text.
    pub user_input: String,
    /// System prompt active during this turn (if changed from prior turn).
    pub system_prompt: Option<String>,

    // ── Output ───────────────────────────────────────────────
    /// The assistant's final response text.
    pub assistant_output: String,
    /// Intermediate tool exchanges (ordered).
    pub tool_exchanges: Vec<ToolExchange>,
    /// Number of LLM rounds (1 = direct response, >1 = tool loop).
    pub rounds: u32,
    /// Finish reason from the LLM.
    pub finish_reason: Option<String>,

    // ── Metadata ─────────────────────────────────────────────
    /// Model used for this turn.
    pub model: Option<String>,
    /// Input tokens consumed.
    pub input_tokens: u64,
    /// Output tokens generated.
    pub output_tokens: u64,
    /// Wall-clock duration of the full turn.
    pub duration_ms: u64,
    /// When the turn started (user message received).
    pub started_at: DateTime<Utc>,
    /// When the turn completed (assistant response delivered).
    pub completed_at: DateTime<Utc>,

    // ── Extensible metadata ──────────────────────────────────
    /// Arbitrary key-value metadata for integrations.
    pub metadata: serde_json::Value,
}

impl ChatTurn {
    /// Total tokens (input + output).
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Whether this turn involved tool calls.
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_exchanges.is_empty()
    }
}

/// Query parameters for loading turns.
#[derive(Debug, Clone)]
pub struct TurnQuery {
    /// Session to query.
    pub session_key: SessionKey,
    /// Maximum number of turns to return.
    pub limit: usize,
    /// Start from this sequence number (inclusive). None = from beginning.
    pub from_sequence: Option<u64>,
    /// End at this sequence number (inclusive). None = to end.
    pub to_sequence: Option<u64>,
}

/// Aggregate statistics for a session's turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnStats {
    /// Total turns in the session.
    pub total_turns: u64,
    /// Total input tokens across all turns.
    pub total_input_tokens: u64,
    /// Total output tokens across all turns.
    pub total_output_tokens: u64,
    /// Total tool calls across all turns.
    pub total_tool_calls: u64,
    /// Average turn duration in ms.
    pub avg_duration_ms: f64,
    /// Most used model.
    pub primary_model: Option<String>,
}

/// Trait for storing and retrieving paired chat turns.
///
/// Implementations should persist turns durably and support
/// sequential loading for conversation replay.
#[async_trait]
pub trait ChatReplayStore: Send + Sync + 'static {
    /// Store a completed chat turn.
    async fn store_turn(&self, turn: &ChatTurn) -> Result<(), StorageError>;

    /// Load turns for a session in sequence order.
    ///
    /// Returns turns ordered by `sequence` ascending.
    async fn load_turns(
        &self,
        session_key: &SessionKey,
        limit: usize,
    ) -> Result<Vec<ChatTurn>, StorageError>;

    /// Load a specific turn by ID.
    async fn get_turn(&self, turn_id: &TurnId) -> Result<Option<ChatTurn>, StorageError>;

    /// Load turns within a sequence range.
    async fn load_turn_range(
        &self,
        session_key: &SessionKey,
        from_sequence: u64,
        to_sequence: u64,
    ) -> Result<Vec<ChatTurn>, StorageError>;

    /// Count the number of turns in a session.
    async fn turn_count(&self, session_key: &SessionKey) -> Result<u64, StorageError>;

    /// Delete all turns for a session.
    async fn delete_session_turns(&self, session_key: &SessionKey) -> Result<u64, StorageError>;

    /// Get aggregate statistics for a session.
    async fn turn_stats(&self, session_key: &SessionKey) -> Result<TurnStats, StorageError>;
}
