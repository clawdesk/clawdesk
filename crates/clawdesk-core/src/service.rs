//! # CoreService — the unified entry point
//!
//! Composes all sub-services into a single service handle that
//! transports (Tauri, CLI, Gateway, TMUX) can hold and invoke.
//!
//! ## Usage
//!
//! ```rust,ignore
//! // In Tauri setup:
//! let core = CoreService::new(workspace_root);
//! app.manage(core);
//!
//! // In CLI:
//! let core = CoreService::new(workspace_root);
//! core.chat().create_chat("coder", &NullEventSink).await?;
//!
//! // In Gateway HTTP handler:
//! let core = CoreService::new(workspace_root);
//! let (chat_id, _) = core.chat().create_chat("coder", &ws_sink).await?;
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use crate::chat::ChatService;
use crate::project::ProjectService;

/// The unified core service — all business logic, zero transport coupling.
///
/// This is the single object that Tauri, CLI, Gateway, and TMUX hold.
/// It owns the sub-services and provides access to them.
pub struct CoreService {
    project: Arc<ProjectService>,
    chat: ChatService,
}

impl CoreService {
    /// Create a new core service with the given workspace root.
    ///
    /// The workspace root is typically `~/.clawdesk/workspace/`.
    /// Per-chat project directories will be created under `{root}/projects/`.
    pub fn new(workspace_root: PathBuf) -> Self {
        let project = Arc::new(ProjectService::new(workspace_root));
        let chat = ChatService::new(Arc::clone(&project));
        Self { project, chat }
    }

    /// Access the chat service (session lifecycle, messaging).
    pub fn chat(&self) -> &ChatService {
        &self.chat
    }

    /// Access the project service (workspace isolation, file operations).
    pub fn project(&self) -> &ProjectService {
        &self.project
    }
}
