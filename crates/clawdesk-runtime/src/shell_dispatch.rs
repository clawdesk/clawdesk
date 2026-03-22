//! Platform-aware shell dispatch — routes commands to the correct shell
//! binary based on the current OS and available interpreters.
//!
//! This module exists because `sh -c` doesn't work on native Windows,
//! `cmd /C` doesn't understand Unix paths, and PowerShell has different
//! quoting rules. The dispatcher detects the platform and builds the
//! correct `Command` for each.

use tokio::process::Command;

/// Which shell+flag to use for running a command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellTarget {
    /// Unix shell: `sh -c` or `zsh -c` or `bash -c`
    Unix(String),
    /// Windows CMD: `cmd /C`
    Cmd,
    /// PowerShell: `powershell -NoProfile -Command`
    PowerShell,
    /// WSL bridge: `wsl -- sh -c`
    Wsl,
}

/// Build a `Command` for running a shell command string on the current platform.
pub fn build_command(command: &str) -> Command {
    #[cfg(unix)]
    {
        let shell = detect_user_shell();
        let mut cmd = Command::new(&shell);
        cmd.arg("-c").arg(command);
        cmd
    }
    #[cfg(windows)]
    {
        build_windows_command(command)
    }
}

/// Detect the user's login shell on Unix systems.
///
/// Checks `$SHELL` first (set by login), then probes for common shells.
#[cfg(unix)]
pub fn detect_user_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() && std::path::Path::new(&shell).exists() {
            return shell;
        }
    }
    // Probe common shells
    for candidate in &["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if std::path::Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    "/bin/sh".to_string()
}

/// Build a command for Windows, routing to cmd.exe, PowerShell, or WSL.
#[cfg(windows)]
fn build_windows_command(command: &str) -> Command {
    if is_wsl_environment() && looks_like_unix_command(command) {
        // Route through WSL for Unix-style commands
        let mut cmd = Command::new("wsl");
        cmd.args(["--", "sh", "-c", command]);
        cmd
    } else if needs_powershell(command) {
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", command]);
        cmd
    } else {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    }
}

/// Detect whether we're running inside WSL (Windows Subsystem for Linux).
///
/// Checks `/proc/version` for Microsoft/WSL markers — this file only exists
/// on Linux and WSL, not on native Windows.
pub fn is_wsl_environment() -> bool {
    #[cfg(unix)]
    {
        std::fs::read_to_string("/proc/version")
            .map(|v| {
                let lower = v.to_lowercase();
                lower.contains("microsoft") || lower.contains("wsl")
            })
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        // On native Windows, check if wsl.exe is available
        std::process::Command::new("wsl")
            .arg("--status")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// Heuristic: does this command look like it needs PowerShell?
#[cfg(windows)]
fn needs_powershell(command: &str) -> bool {
    let lower = command.to_lowercase();
    lower.starts_with("get-")
        || lower.starts_with("set-")
        || lower.starts_with("invoke-")
        || lower.starts_with("new-")
        || lower.starts_with("remove-")
        || lower.contains("$env:")
        || lower.contains("| select-")
        || lower.contains("| where-")
        || lower.contains("| format-")
}

/// Heuristic: does this command look like a Unix command?
#[cfg(windows)]
fn looks_like_unix_command(command: &str) -> bool {
    let first_word = command.split_whitespace().next().unwrap_or("");
    let unix_commands = [
        "ls", "cat", "grep", "find", "sed", "awk", "head", "tail",
        "wc", "sort", "uniq", "cut", "tr", "chmod", "chown", "mkdir",
        "rm", "cp", "mv", "ln", "diff", "curl", "wget", "tar", "gzip",
        "ssh", "scp", "rsync", "make", "cmake", "gcc", "g++",
        "python3", "pip3", "node", "npm", "cargo", "rustc",
    ];
    unix_commands.contains(&first_word)
        || command.contains(" | ")
        || command.starts_with("./")
        || command.starts_with("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn detects_user_shell() {
        let shell = detect_user_shell();
        assert!(
            shell.contains("sh") || shell.contains("zsh") || shell.contains("bash"),
            "unexpected shell: {}",
            shell
        );
    }

    #[test]
    #[cfg(unix)]
    fn build_command_uses_shell() {
        let cmd = build_command("echo hello");
        let prog = cmd.as_std().get_program().to_string_lossy().to_string();
        assert!(
            prog.contains("sh") || prog.contains("zsh") || prog.contains("bash"),
            "unexpected program: {}",
            prog
        );
    }
}
