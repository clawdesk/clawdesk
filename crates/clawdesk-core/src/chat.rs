//! # Chat service — session lifecycle and message orchestration
//!
//! Transport-agnostic chat/session management. Handles:
//! - Chat creation with per-project workspace isolation
//! - Message sending with agent execution
//! - Session listing, deletion, persistence
//!
//! This is the core logic that was previously embedded in
//! `clawdesk-tauri/src/commands.rs::send_message()` (2000+ lines).

use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

use crate::event::{CoreEvent, EventSink};
use crate::project::ProjectService;

/// Request to send a message in a chat.
#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageRequest {
    pub agent_id: String,
    pub content: String,
    #[serde(default)]
    pub model_override: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub provider_override: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Response from sending a message.
#[derive(Debug, Serialize)]
pub struct SendMessageResponse {
    pub chat_id: String,
    pub content: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_title: Option<String>,
}

/// Summary of a chat session.
#[derive(Debug, Clone, Serialize)]
pub struct ChatSummary {
    pub chat_id: String,
    pub agent_id: String,
    pub title: String,
    pub created_at: String,
    pub last_activity: String,
    pub message_count: usize,
    pub project_path: Option<String>,
}

/// Chat service — manages chat lifecycle.
///
/// This is the transport-agnostic core of chat management.
/// Tauri/CLI/Gateway each call these methods instead of
/// reimplementing the orchestration logic.
pub struct ChatService {
    project_service: Arc<ProjectService>,
}

impl ChatService {
    pub fn new(project_service: Arc<ProjectService>) -> Self {
        Self { project_service }
    }

    /// Create a new chat session with an isolated project directory.
    ///
    /// Returns the chat_id and project directory path.
    pub async fn create_chat(
        &self,
        agent_id: &str,
        event_sink: &dyn EventSink,
    ) -> Result<(String, std::path::PathBuf), String> {
        let chat_id = Uuid::new_v4().to_string();
        let project_dir = self.project_service.project_dir(&chat_id);

        info!(
            chat_id = %chat_id,
            agent_id = %agent_id,
            project_dir = ?project_dir,
            "Created new chat with isolated project workspace"
        );

        event_sink.emit(CoreEvent::ChatCreated {
            chat_id: chat_id.clone(),
            agent_id: agent_id.to_string(),
            title: format!("New chat with {}", agent_id),
        }).await;

        Ok((chat_id, project_dir))
    }

    /// Resolve the workspace path for a chat session.
    /// Returns the per-chat project directory.
    pub fn resolve_workspace(&self, chat_id: &str) -> std::path::PathBuf {
        self.project_service.project_dir(chat_id)
    }

    /// Build a scoped tool registry for a specific chat.
    pub fn scoped_tools(
        &self,
        chat_id: &str,
        base_registry: &clawdesk_agents::tools::ToolRegistry,
    ) -> clawdesk_agents::tools::ToolRegistry {
        self.project_service.scoped_tool_registry(chat_id, base_registry)
    }

    /// Get the project service reference.
    pub fn project_service(&self) -> &ProjectService {
        &self.project_service
    }
}
