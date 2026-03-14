//! # Event sink — transport-agnostic event delivery
//!
//! Instead of coupling to `tauri::AppHandle::emit()`, the core emits
//! typed events through a trait. Each transport implements the trait.

use serde::Serialize;

/// A typed event emitted by the core service.
///
/// Transports map these to their native event system:
/// - Tauri: `app.emit("event_name", payload)`
/// - CLI: `eprintln!("[event] ...")`
/// - Gateway: WebSocket JSON frame
/// - TMUX: `tmux display-message`
#[derive(Debug, Clone, Serialize)]
pub enum CoreEvent {
    /// Agent started processing a message.
    AgentStarted {
        chat_id: String,
        agent_id: String,
    },
    /// A tool was called by the agent.
    ToolCall {
        chat_id: String,
        tool_name: String,
        tool_call_id: String,
        arguments: serde_json::Value,
    },
    /// Tool execution completed.
    ToolResult {
        chat_id: String,
        tool_call_id: String,
        result: String,
        is_error: bool,
    },
    /// Agent produced a text chunk (streaming).
    TextChunk {
        chat_id: String,
        chunk: String,
    },
    /// Agent finished processing.
    AgentFinished {
        chat_id: String,
        agent_id: String,
        content: String,
        input_tokens: u64,
        output_tokens: u64,
        duration_ms: u64,
    },
    /// A new chat session was created.
    ChatCreated {
        chat_id: String,
        agent_id: String,
        title: String,
    },
    /// System-level alert (errors, security, etc.).
    SystemAlert {
        level: String,
        title: String,
        message: String,
    },
    /// Project files changed (for IDE mode file watchers).
    ProjectFilesChanged {
        chat_id: String,
        paths: Vec<String>,
    },
}

/// Trait for receiving core events.
///
/// Each transport implements this to bridge events to its native system.
/// The trait is `Send + Sync` to allow sharing across async tasks.
#[async_trait::async_trait]
pub trait EventSink: Send + Sync {
    /// Emit a core event. Implementations should not block.
    async fn emit(&self, event: CoreEvent);
}

/// No-op event sink for testing and headless operation.
pub struct NullEventSink;

#[async_trait::async_trait]
impl EventSink for NullEventSink {
    async fn emit(&self, _event: CoreEvent) {}
}
