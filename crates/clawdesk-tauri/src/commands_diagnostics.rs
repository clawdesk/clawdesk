//! Tauri commands for the Diagnostic Engine (Phase 1.5).
//!
//! Surfaces the CLI doctor as a GUI-accessible self-healing system.
//! Note: Uses dedicated diagnostic types to avoid circular dependency on clawdesk-cli.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

/// Serializable diagnostic result for GUI consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagResultInfo {
    pub name: String,
    pub status: String,
    pub detail: String,
    pub duration_ms: u64,
    pub fix_action: Option<FixActionInfo>,
}

/// Fix action for one-click remediation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixActionInfo {
    pub label: String,
    pub action_id: String,
}

/// Complete diagnostic report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagReport {
    pub results: Vec<DiagResultInfo>,
    pub ok_count: usize,
    pub warn_count: usize,
    pub fail_count: usize,
}

/// Run all diagnostics.
#[tauri::command]
pub async fn run_diagnostics(
    _state: State<'_, AppState>,
) -> Result<DiagReport, String> {
    let mut results = Vec::new();

    // Platform check
    results.push(DiagResultInfo {
        name: "Platform".into(),
        status: "ok".into(),
        detail: format!("v{} ({}/{})", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH),
        duration_ms: 0,
        fix_action: None,
    });

    // Data directory check
    let data_dir = clawdesk_types::dirs::data();
    let data_exists = data_dir.exists();
    results.push(DiagResultInfo {
        name: "Data directory".into(),
        status: if data_exists { "ok" } else { "warn" }.into(),
        detail: format!("{}", data_dir.display()),
        duration_ms: 0,
        fix_action: if !data_exists {
            Some(FixActionInfo { label: "Create Directories".into(), action_id: "create_dirs".into() })
        } else { None },
    });

    // Gateway check — since Tauri embeds the gateway, it's always running
    results.push(DiagResultInfo {
        name: "Gateway".into(),
        status: "ok".into(),
        detail: "embedded (running)".into(),
        duration_ms: 0,
        fix_action: None,
    });

    // Ollama check — try TCP connect
    let ollama_ok = tokio::net::TcpStream::connect("127.0.0.1:11434").await.is_ok();
    results.push(DiagResultInfo {
        name: "Ollama".into(),
        status: if ollama_ok { "ok" } else { "skip" }.into(),
        detail: if ollama_ok { "running" } else { "not running (optional)" }.into(),
        duration_ms: 0,
        fix_action: if !ollama_ok {
            Some(FixActionInfo { label: "Install Ollama".into(), action_id: "install_ollama".into() })
        } else { None },
    });

    let ok_count = results.iter().filter(|r| r.status == "ok").count();
    let warn_count = results.iter().filter(|r| r.status == "warn").count();
    let fail_count = results.iter().filter(|r| r.status == "fail").count();

    Ok(DiagReport { results, ok_count, warn_count, fail_count })
}

/// Execute a one-click fix action.
#[tauri::command]
pub async fn execute_diagnostic_fix(
    action_id: String,
    _state: State<'_, AppState>,
) -> Result<String, String> {
    match action_id.as_str() {
        "create_dirs" => {
            let data = clawdesk_types::dirs::data();
            let dot = clawdesk_types::dirs::dot_clawdesk();
            for dir in &[data.join("skills"), dot.join("sochdb"), dot.join("threads"), dot.join("agents")] {
                let _ = std::fs::create_dir_all(dir);
            }
            Ok("Directories created".into())
        }
        "start_gateway" => {
            tokio::process::Command::new("clawdesk")
                .args(["gateway", "run"])
                .spawn()
                .map_err(|e| format!("Failed: {}", e))?;
            Ok("Gateway starting...".into())
        }
        _ => Err(format!("Unknown action: {}", action_id)),
    }
}
