//! Domain types for chat threads and messages.

use serde::{Deserialize, Serialize};

/// Metadata for a chat thread (the "header" stored at `threads/{id}`).
///
/// Every thread is an **agent** in the A2A protocol. The `agent_id` field
/// identifies which agent owns this thread, and the optional `spawn_mode`
/// / `parent_thread_id` fields model sub-agent thread hierarchies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadMeta {
    /// Thread identifier (UUID as u128, stored hex-encoded in the key).
    pub id: u128,
    /// Agent that owns this thread.
    pub agent_id: String,
    /// Human-readable title (auto-generated from first message or user-set).
    pub title: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 last-updated timestamp (updated on every new message).
    pub updated_at: String,
    /// Total number of messages in this thread (maintained incrementally).
    pub message_count: u64,
    /// Optional model override used for this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whether the thread is pinned by the user.
    #[serde(default)]
    pub pinned: bool,
    /// Whether the thread is archived (hidden from default listing).
    #[serde(default)]
    pub archived: bool,
    /// Arbitrary user-defined tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    // ── A2A thread-as-agent fields ────────────────────────────────────

    /// Spawn mode: how this thread-agent was created.
    /// - `"standalone"` (default) — user-created top-level thread
    /// - `"run"` — fire-and-forget sub-agent task (result announced to parent)
    /// - `"session"` — persistent sub-agent session (stays active)
    #[serde(default = "default_spawn_mode", skip_serializing_if = "is_standalone")]
    pub spawn_mode: String,

    /// Parent thread ID if this is a sub-agent thread.
    /// Set when `spawn_mode` is `"run"` or `"session"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<u128>,

    /// Per-agent capability overrides.
    /// Lists capability strings this thread-agent advertises (e.g. "code-review",
    /// "summarize"). Used to generate the thread's A2A `AgentCard`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,

    /// Per-agent skill filter.
    /// If non-empty, only these skills are available to this thread-agent
    /// (overrides the global skill registry).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

fn default_spawn_mode() -> String { "standalone".to_string() }
fn is_standalone(s: &str) -> bool { s == "standalone" }

/// A single chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Message identifier (UUID as u128).
    pub id: u128,
    /// Thread this message belongs to.
    pub thread_id: u128,
    /// Role of the message author.
    pub role: MessageRole,
    /// Text content.
    pub content: String,
    /// Microsecond Unix timestamp.
    pub timestamp_us: u64,
    /// Optional structured metadata (model, tokens, cost, tool usage, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// If true, a separate attachment blob exists at `attachments/{msg_id}`.
    #[serde(default)]
    pub has_attachment: bool,
}

/// Message role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

impl MessageRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

/// Lightweight summary returned by list operations (avoids deserializing
/// every message in every thread).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub id: u128,
    pub agent_id: String,
    pub title: String,
    pub updated_at: String,
    pub message_count: u64,
    pub pinned: bool,
    pub archived: bool,
}

impl From<&ThreadMeta> for ThreadSummary {
    fn from(m: &ThreadMeta) -> Self {
        Self {
            id: m.id,
            agent_id: m.agent_id.clone(),
            title: m.title.clone(),
            updated_at: m.updated_at.clone(),
            message_count: m.message_count,
            pinned: m.pinned,
            archived: m.archived,
        }
    }
}

/// Query parameters for listing threads.
#[derive(Debug, Clone, Default)]
pub struct ThreadQuery {
    /// Filter by agent_id (uses secondary index).
    pub agent_id: Option<String>,
    /// Include archived threads (default: false).
    pub include_archived: bool,
    /// Maximum number of threads to return.
    pub limit: Option<usize>,
    /// Offset for pagination.
    pub offset: usize,
    /// Sort order for the results.
    pub sort: SortOrder,
}

/// Sort order for thread listings.
#[derive(Debug, Clone, Copy, Default)]
pub enum SortOrder {
    /// Most recently updated first (default).
    #[default]
    UpdatedDesc,
    /// Oldest first by creation time.
    CreatedAsc,
    /// Newest first by creation time.
    CreatedDesc,
}
