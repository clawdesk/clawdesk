//! Shell dispatch for process_manager — routes commands to the correct shell.
//!
//! Thin wrapper that picks `sh -c` / `zsh -c` on Unix and `cmd /C` on Windows.

use tokio::process::Command;

/// Build a `Command` for running a shell command string.
pub fn build_command(command: &str) -> Command {
    #[cfg(unix)]
    {
        let shell = detect_shell();
        let mut cmd = Command::new(&shell);
        cmd.arg("-c").arg(command);
        cmd
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    }
}

#[cfg(unix)]
fn detect_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() && std::path::Path::new(&shell).exists() {
            return shell;
        }
    }
    for candidate in &["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if std::path::Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    "/bin/sh".to_string()
}
