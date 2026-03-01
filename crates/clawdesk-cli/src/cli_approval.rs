//! CLI Approval Gate — Terminal-Based Permission Prompts.
//!
//! Implements the `ApprovalGate` trait for interactive CLI sessions.
//! When the agent calls a dangerous tool (shell_exec, file_write, http,
//! message_send, etc.), this gate prompts the user in the terminal:
//!
//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │  Agent wants to execute: shell_exec              │
//! │  Command: git push origin main                   │
//! │                                                  │
//! │  [a] Allow  [s] Allow for session  [d] Deny      │
//! └─────────────────────────────────────────────────┘
//! ```
//!
//! Session-scoped decisions are cached by the runner in its
//! `approval_session_cache` — once approved for a session, subsequent calls
//! to the same tool skip the prompt entirely (O(1) HashMap lookup).

use async_trait::async_trait;
use clawdesk_agents::runner::{ApprovalDecision, ApprovalGate};
use std::io::{self, BufRead, Write};
use tracing::{debug, info};

/// Terminal-based approval gate for CLI agent execution.
///
/// Prompts the user via stdin/stdout when a dangerous tool is invoked.
/// Supports three decisions:
/// - **Allow** — permit this single invocation
/// - **Allow for session** — auto-approve this tool for the rest of the session
/// - **Deny** — block this invocation
pub struct CliApprovalGate {
    /// When true, all tools are auto-approved without prompting.
    /// Activated via `--allow-all-tools` CLI flag.
    auto_approve_all: bool,
}

impl CliApprovalGate {
    /// Create a new interactive CLI approval gate.
    pub fn new() -> Self {
        Self {
            auto_approve_all: false,
        }
    }

    /// Create an auto-approve gate (for CI/CD or `--allow-all-tools` mode).
    pub fn permissive() -> Self {
        Self {
            auto_approve_all: true,
        }
    }

    /// Format a human-readable preview of the tool arguments.
    fn format_args_preview(tool_name: &str, arguments: &str) -> String {
        // Try to parse as JSON for pretty display
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) {
            match tool_name {
                "shell_exec" | "shell" => {
                    if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                        return format!("  Command: {}", cmd);
                    }
                }
                "file_write" => {
                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                    let content_len = args
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(|s| s.len())
                        .unwrap_or(0);
                    return format!("  Path: {}\n  Content: {} bytes", path, content_len);
                }
                "http" | "http_fetch" => {
                    let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("?");
                    let method = args.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
                    return format!("  {} {}", method, url);
                }
                "message_send" => {
                    let channel = args.get("channel").and_then(|v| v.as_str()).unwrap_or("?");
                    let content_preview = args
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(|s| {
                            if s.len() > 80 {
                                format!("{}...", &s[..80])
                            } else {
                                s.to_string()
                            }
                        })
                        .unwrap_or_default();
                    return format!("  Channel: {}\n  Content: {}", channel, content_preview);
                }
                "email_send" => {
                    let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("?");
                    let subject = args.get("subject").and_then(|v| v.as_str()).unwrap_or("(no subject)");
                    return format!("  To: {}\n  Subject: {}", to, subject);
                }
                "spawn_subagent" | "dynamic_spawn" => {
                    let task = args
                        .get("task")
                        .and_then(|v| v.as_str())
                        .map(|s| {
                            if s.len() > 120 {
                                format!("{}...", &s[..120])
                            } else {
                                s.to_string()
                            }
                        })
                        .unwrap_or_default();
                    return format!("  Task: {}", task);
                }
                _ => {}
            }
            // Fallback: pretty-print JSON (truncated)
            let pretty = serde_json::to_string_pretty(&args).unwrap_or_else(|_| arguments.to_string());
            if pretty.len() > 300 {
                return format!("  Args: {}...", &pretty[..300]);
            }
            return format!("  Args: {}", pretty);
        }
        // Not valid JSON — show raw (truncated)
        if arguments.len() > 300 {
            format!("  Args: {}...", &arguments[..300])
        } else {
            format!("  Args: {}", arguments)
        }
    }
}

impl Default for CliApprovalGate {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ApprovalGate for CliApprovalGate {
    async fn request_approval(
        &self,
        tool_name: &str,
        arguments: &str,
    ) -> Result<ApprovalDecision, String> {
        // Auto-approve mode (--allow-all-tools / CI)
        if self.auto_approve_all {
            debug!(tool = tool_name, "auto-approved (permissive mode)");
            return Ok(ApprovalDecision::AllowForSession);
        }

        // Use spawn_blocking to avoid blocking the tokio runtime on stdin reads
        let tool_name = tool_name.to_string();
        let arguments = arguments.to_string();

        let decision = tokio::task::spawn_blocking(move || {
            let preview = Self::format_args_preview(&tool_name, &arguments);
            let stdin = io::stdin();
            let mut stdout = io::stdout();

            // Print the approval prompt
            let _ = writeln!(stdout);
            let _ = writeln!(stdout, "┌─────────────────────────────────────────────────────────┐");
            let _ = writeln!(stdout, "│  Agent wants to execute: {:<32}│", tool_name);
            for line in preview.lines() {
                let _ = writeln!(stdout, "│  {:<55}│", line);
            }
            let _ = writeln!(stdout, "│                                                         │");
            let _ = writeln!(stdout, "│  [a] Allow  [s] Allow for session  [d] Deny             │");
            let _ = writeln!(stdout, "└─────────────────────────────────────────────────────────┘");
            let _ = write!(stdout, "  Choice [a/s/d]: ");
            let _ = stdout.flush();

            let mut input = String::new();
            let reader = stdin.lock();
            match reader.lines().next() {
                Some(Ok(line)) => {
                    input = line.trim().to_lowercase();
                }
                Some(Err(e)) => {
                    eprintln!("  Error reading input: {}", e);
                    return ApprovalDecision::Deny;
                }
                None => {
                    // EOF — non-interactive, deny by default
                    eprintln!("  (non-interactive terminal — denying)");
                    return ApprovalDecision::Deny;
                }
            }

            match input.as_str() {
                "a" | "allow" | "y" | "yes" => {
                    info!(tool = tool_name.as_str(), "user approved (single)");
                    ApprovalDecision::Allow
                }
                "s" | "session" => {
                    info!(tool = tool_name.as_str(), "user approved (session)");
                    ApprovalDecision::AllowForSession
                }
                "d" | "deny" | "n" | "no" | "" => {
                    info!(tool = tool_name.as_str(), "user denied");
                    ApprovalDecision::Deny
                }
                _ => {
                    eprintln!("  Unrecognized input '{}' — denying", input);
                    ApprovalDecision::Deny
                }
            }
        })
        .await
        .map_err(|e| format!("approval prompt failed: {}", e))?;

        Ok(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_shell_args() {
        let preview = CliApprovalGate::format_args_preview(
            "shell_exec",
            r#"{"command":"git push origin main"}"#,
        );
        assert!(preview.contains("git push origin main"));
    }

    #[test]
    fn format_file_write_args() {
        let preview = CliApprovalGate::format_args_preview(
            "file_write",
            r#"{"path":"/tmp/test.rs","content":"fn main() {}"}"#,
        );
        assert!(preview.contains("/tmp/test.rs"));
        assert!(preview.contains("bytes"));
    }

    #[test]
    fn format_http_args() {
        let preview = CliApprovalGate::format_args_preview(
            "http",
            r#"{"url":"https://api.example.com","method":"POST"}"#,
        );
        assert!(preview.contains("POST"));
        assert!(preview.contains("https://api.example.com"));
    }

    #[test]
    fn format_email_args() {
        let preview = CliApprovalGate::format_args_preview(
            "email_send",
            r#"{"to":"john@example.com","subject":"Meeting update"}"#,
        );
        assert!(preview.contains("john@example.com"));
        assert!(preview.contains("Meeting update"));
    }

    #[tokio::test]
    async fn permissive_gate_auto_approves() {
        let gate = CliApprovalGate::permissive();
        let decision = gate
            .request_approval("shell_exec", r#"{"command":"ls"}"#)
            .await
            .unwrap();
        assert_eq!(decision, ApprovalDecision::AllowForSession);
    }
}
