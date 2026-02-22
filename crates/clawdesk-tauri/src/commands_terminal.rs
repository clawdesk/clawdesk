//! Terminal commands — execute shell commands from the ClawDesk terminal panel.

use serde::{Deserialize, Serialize};
use std::process::Command as StdCommand;

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

/// Execute a shell command and return its output.
///
/// Runs the command via the user's default shell (`zsh` on macOS, `sh` fallback)
/// and captures stdout, stderr, and exit code.
#[tauri::command]
pub async fn run_shell_command(request: RunCommandRequest) -> Result<RunCommandResponse, String> {
    let shell = if cfg!(target_os = "windows") {
        "cmd"
    } else {
        // Prefer zsh on macOS, fall back to sh
        if std::path::Path::new("/bin/zsh").exists() {
            "/bin/zsh"
        } else {
            "/bin/sh"
        }
    };

    let shell_flag = if cfg!(target_os = "windows") {
        "/C"
    } else {
        "-c"
    };

    let mut cmd = StdCommand::new(shell);
    cmd.arg(shell_flag).arg(&request.command);

    // Set working directory if provided
    if let Some(ref cwd) = request.cwd {
        let path = std::path::Path::new(cwd);
        if path.is_dir() {
            cmd.current_dir(path);
        }
    }

    // Set HOME so commands like `cd ~` work
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
