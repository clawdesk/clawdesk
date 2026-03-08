//! Terminal commands — PTY-backed terminal sessions for ClawDesk.
//!
//! Replaces the old one-shot `run_shell_command` with full interactive
//! terminal sessions inspired by open-terminal's architecture:
//!
//! - `pty_create_session`  — spawn a new PTY shell session
//! - `pty_write_input`     — send keystrokes to a session
//! - `pty_resize`          — resize the terminal
//! - `pty_kill_session`    — kill a session
//! - `pty_list_sessions`   — list active sessions
//!
//! Output is streamed to the frontend via the `terminal-output` Tauri event.
//! The old `run_shell_command` is kept for backward compatibility (agent tools).

use crate::pty_session::{PtySessionManager, TerminalSession};
use serde::{Deserialize, Serialize};
use std::process::Command as StdCommand;
use tauri::State;

// ─── PTY session commands ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub cwd: Option<String>,
}

#[tauri::command]
pub async fn pty_create_session(
    app: tauri::AppHandle,
    pty_manager: State<'_, PtySessionManager>,
    request: CreateSessionRequest,
) -> Result<TerminalSession, String> {
    pty_manager
        .create_session(
            app,
            request.cols.unwrap_or(80),
            request.rows.unwrap_or(24),
            request.cwd,
        )
        .await
}

#[derive(Debug, Deserialize)]
pub struct WriteInputRequest {
    pub session_id: String,
    pub data: String,
}

#[tauri::command]
pub async fn pty_write_input(
    pty_manager: State<'_, PtySessionManager>,
    request: WriteInputRequest,
) -> Result<(), String> {
    pty_manager
        .write_input(&request.session_id, request.data.as_bytes())
        .await
}

#[derive(Debug, Deserialize)]
pub struct ResizeRequest {
    pub session_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[tauri::command]
pub async fn pty_resize(
    pty_manager: State<'_, PtySessionManager>,
    request: ResizeRequest,
) -> Result<(), String> {
    pty_manager
        .resize(&request.session_id, request.cols, request.rows)
        .await
}

#[tauri::command]
pub async fn pty_kill_session(
    pty_manager: State<'_, PtySessionManager>,
    session_id: String,
) -> Result<(), String> {
    pty_manager.kill_session(&session_id).await
}

#[tauri::command]
pub async fn pty_list_sessions(
    pty_manager: State<'_, PtySessionManager>,
) -> Result<Vec<TerminalSession>, String> {
    Ok(pty_manager.list_sessions().await)
}

// ─── Legacy: one-shot shell command (kept for agent tool usage) ─────────────

#[derive(Debug, Deserialize)]
pub struct RunCommandRequest {
    pub command: String,
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunCommandResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
}

/// Execute a shell command and return its output (one-shot, non-interactive).
#[tauri::command]
pub async fn run_shell_command(request: RunCommandRequest) -> Result<RunCommandResponse, String> {
    let shell = if cfg!(target_os = "windows") {
        "cmd"
    } else if std::path::Path::new("/bin/zsh").exists() {
        "/bin/zsh"
    } else {
        "/bin/sh"
    };

    let shell_flag = if cfg!(target_os = "windows") { "/C" } else { "-c" };

    let mut cmd = StdCommand::new(shell);
    cmd.arg(shell_flag).arg(&request.command);

    if let Some(ref cwd) = request.cwd {
        let path = std::path::Path::new(cwd);
        if path.is_dir() {
            cmd.current_dir(path);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }

    match cmd.output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Ok(RunCommandResponse {
                stdout,
                stderr,
                exit_code: output.status.code(),
                success: output.status.success(),
            })
        }
        Err(e) => Err(format!("Failed to execute command: {}", e)),
    }
}
