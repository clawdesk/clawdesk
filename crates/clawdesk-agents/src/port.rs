//! Typed ports for tool callbacks — eliminates positional-arg closures.
//!
//! # Motivation
//!
//! Tool structs in `builtin_tools.rs` store callbacks like:
//! ```text
//! Arc<dyn Fn(String, String, String, Option<String>, u64, Option<String>)
//!     -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync>
//! ```
//! This is unreadable and fragile — swapping two `String` args compiles but
//! produces a silent bug. This module defines:
//!
//! 1. **`AsyncPort<Req, Res>`** — a generic type alias that wraps the closure pattern
//! 2. **Request DTOs** with named fields for tools with 3+ positional args
//!
//! After this refactor, the same callback looks like:
//! ```text
//! AsyncPort<CronScheduleRequest, Result<String, String>>
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// ─── Generic type alias ────────────────────────────────────────────────────

/// A type-erased async callback port with typed request and response.
///
/// Replaces the verbose pattern:
/// ```text
/// Arc<dyn Fn(A, B, C) -> Pin<Box<dyn Future<Output = R> + Send>> + Send + Sync>
/// ```
/// with:
/// ```text
/// AsyncPort<MyRequest, R>
/// ```
///
/// The request type bundles all arguments into a single struct with named fields,
/// preventing positional-arg swap bugs.
pub type AsyncPort<Req, Res> =
    Arc<dyn Fn(Req) -> Pin<Box<dyn Future<Output = Res> + Send>> + Send + Sync>;

// ─── Request DTOs ──────────────────────────────────────────────────────────
// Only tools with 3+ positional args get DTOs. Simple tools (0-2 args)
// keep their existing signatures since they're already unambiguous.

/// Request for `CronScheduleTool` — schedules a recurring agent task.
///
/// Previously: `Fn(String, String, String, Option<String>, u64, Option<String>)`
/// i.e. 6 positional args where 3 are `String` — impossible to distinguish.
#[derive(Debug, Clone)]
pub struct CronScheduleRequest {
    /// Human-readable task name.
    pub name: String,
    /// Cron expression (5-field, e.g. `*/5 * * * *`).
    pub schedule: String,
    /// The prompt to execute on each scheduled run.
    pub prompt: String,
    /// Target agent ID (optional, defaults to current agent).
    pub agent_id: Option<String>,
    /// Max execution time in seconds (default: 300).
    pub timeout_secs: u64,
    /// Existing task ID to update (omit to create new).
    pub task_id: Option<String>,
    /// Delivery targets: where to send results when the cron task completes.
    /// Each entry is a `(channel_id, conversation_id)` pair, e.g. `("telegram", "default")`.
    pub delivery_targets: Vec<(String, String)>,
}

/// Request for `MessageSendTool` — sends a message through channels.
///
/// Previously: `Fn(String, Option<String>, String, Vec<String>)`
#[derive(Debug, Clone)]
pub struct MessageSendRequest {
    /// Normalized target identifier (channel ID, user, thread).
    pub target: String,
    /// Channel provider to use for delivery (optional).
    pub channel: Option<String>,
    /// The message text to send.
    pub content: String,
    /// Media attachment URLs.
    pub media_urls: Vec<String>,
}

/// Request for `McpConnectTool` — connects to an MCP server.
///
/// Previously: `Fn(String, String, String, Vec<String>)`
#[derive(Debug, Clone)]
pub struct McpConnectRequest {
    /// Unique name for this server connection.
    pub server_name: String,
    /// Transport type: `"stdio"` or `"sse"`.
    pub transport: String,
    /// For stdio: the command to run. For SSE: the server URL.
    pub command_or_url: String,
    /// Command arguments for stdio transport.
    pub args: Vec<String>,
}

/// Request for `WorkspaceGrepTool` — searches file contents.
///
/// Previously: `Fn(String, bool, Option<String>, usize)`
#[derive(Debug, Clone)]
pub struct WorkspaceGrepRequest {
    /// Text or regex pattern to search for.
    pub query: String,
    /// Whether `query` is a regex.
    pub is_regex: bool,
    /// Optional glob pattern to filter files.
    pub include_glob: Option<String>,
    /// Maximum number of results.
    pub max_results: usize,
}

/// Request for `SendNotificationTool` — fan-out notification.
///
/// Previously: `Fn(Vec<String>, Option<String>, String, String)`
#[derive(Debug, Clone)]
pub struct SendNotificationRequest {
    /// Channel names to deliver to.
    pub channels: Vec<String>,
    /// Named target within channels (e.g. `#ops`, `@user`).
    pub target: Option<String>,
    /// Notification message text.
    pub message: String,
    /// Priority level (e.g. `"normal"`, `"high"`, `"critical"`).
    pub priority: String,
}

/// Request for `SpawnSubAgentTool` — delegates to a sub-agent.
///
/// Previously: `Fn(String, String, u64)`
#[derive(Debug, Clone)]
pub struct SpawnSubAgentRequest {
    /// Agent ID to delegate to.
    pub agent_id: String,
    /// Task description for the sub-agent.
    pub task: String,
    /// Maximum execution time in seconds.
    pub timeout_secs: u64,
}

/// Request for `SessionsSendTool` — A2A protocol message.
///
/// Previously: `Fn(String, String, Option<String>)`
#[derive(Debug, Clone)]
pub struct SessionsSendRequest {
    /// Target agent ID to send the task to.
    pub target_agent: String,
    /// Task description or message.
    pub message: String,
    /// Optional specific skill to invoke.
    pub skill_id: Option<String>,
}

/// Request for `AskHumanTool` — pause execution and ask the human for a decision.
#[derive(Debug, Clone)]
pub struct AskHumanRequest {
    /// The question to present to the human.
    pub question: String,
    /// Optional suggested options (e.g. ["Yes, proceed", "No, cancel", "Let me choose"]).
    pub options: Vec<String>,
    /// Whether this is blocking (agent waits for response) or informational.
    pub urgent: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_port_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AsyncPort<CronScheduleRequest, Result<String, String>>>();
        assert_send_sync::<AsyncPort<MessageSendRequest, Result<String, String>>>();
    }

    #[test]
    fn request_dtos_are_clone() {
        let req = CronScheduleRequest {
            name: "test".into(),
            schedule: "* * * * *".into(),
            prompt: "hello".into(),
            agent_id: None,
            timeout_secs: 300,
            task_id: None,
            delivery_targets: vec![],
        };
        let _cloned = req.clone();
    }
}
