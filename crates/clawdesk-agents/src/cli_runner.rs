//! CLI Agent Runner — delegates to external agent runtimes (Claude Code CLI, Codex CLI).
//!
//! ## Architecture
//!
//! The `CliAgentRunner` integrates with ClawDesk's hexagonal architecture as a
//! logical peer to the API-based `AgentRunner`. It manages external CLI processes
//! with:
//! - Session ID persistence for conversation continuity across tool rounds
//! - Structured JSON output parsing
//! - Two-tier timeout (overall + no-output watchdog)
//! - Failover on session expiry
//! - Serialized execution via semaphore for single-threaded CLIs
//!
//! ## Session Resume
//! Without resume, turn n replays all n-1 previous turns: O(n²) total tokens.
//! With session resume, each turn sends only the new message: O(n) total tokens.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

/// Configuration for CLI backend integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliBackendConfig {
    /// Path to the CLI binary (e.g., "claude", "codex").
    pub binary_path: String,
    /// Default arguments passed to every invocation.
    #[serde(default)]
    pub default_args: Vec<String>,
    /// Whether to request structured JSON output.
    #[serde(default = "default_true")]
    pub json_output: bool,
    /// Overall timeout for a single CLI invocation (seconds).
    #[serde(default = "default_overall_timeout")]
    pub overall_timeout_secs: u64,
    /// No-output watchdog timeout (seconds). Process is killed if no output
    /// is produced for this duration.
    #[serde(default = "default_watchdog_timeout")]
    pub watchdog_timeout_secs: u64,
    /// Whether concurrent requests should be serialized (queued).
    /// Required for single-threaded CLIs like Claude Code.
    #[serde(default = "default_true")]
    pub serialize: bool,
    /// Environment variables to set for the CLI process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Working directory for the CLI process.
    pub working_dir: Option<String>,
}

fn default_true() -> bool { true }
fn default_overall_timeout() -> u64 { 300 }
fn default_watchdog_timeout() -> u64 { 60 }

impl Default for CliBackendConfig {
    fn default() -> Self {
        Self {
            binary_path: "claude".to_string(),
            default_args: vec!["--print".to_string()],
            json_output: true,
            overall_timeout_secs: 300,
            watchdog_timeout_secs: 60,
            serialize: true,
            env: HashMap::new(),
            working_dir: None,
        }
    }
}

/// Result from a CLI agent invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliAgentResult {
    /// The response text from the CLI agent.
    pub response: String,
    /// Session ID for resume (if the CLI supports it).
    pub session_id: Option<String>,
    /// Whether the session expired and a new one was started.
    pub session_renewed: bool,
    /// Exit code of the CLI process.
    pub exit_code: Option<i32>,
    /// Total elapsed time in seconds.
    pub elapsed_secs: f64,
}

/// CLI Agent Runner — manages external CLI agent processes.
pub struct CliAgentRunner {
    config: CliBackendConfig,
    /// Active session ID for conversation continuity.
    session_id: tokio::sync::Mutex<Option<String>>,
    /// Serialization semaphore: permits=1 when serialize=true.
    serialize_semaphore: Arc<Semaphore>,
}

impl CliAgentRunner {
    /// Create a new CLI agent runner with the given configuration.
    pub fn new(config: CliBackendConfig) -> Self {
        let permits = if config.serialize { 1 } else { 16 };
        Self {
            config,
            session_id: tokio::sync::Mutex::new(None),
            serialize_semaphore: Arc::new(Semaphore::new(permits)),
        }
    }

    /// Run the CLI agent with a user message.
    ///
    /// Handles session resume, structured output parsing, and failover.
    pub async fn run(&self, message: &str) -> Result<CliAgentResult, String> {
        let _permit = self
            .serialize_semaphore
            .acquire()
            .await
            .map_err(|e| format!("semaphore closed: {}", e))?;

        let start = std::time::Instant::now();

        // Build command with session resume
        let session_id = self.session_id.lock().await.clone();
        let result = self.execute_cli(message, session_id.as_deref()).await;

        match result {
            Ok(mut result) => {
                result.elapsed_secs = start.elapsed().as_secs_f64();

                // Update session ID for next turn
                if let Some(ref new_session) = result.session_id {
                    *self.session_id.lock().await = Some(new_session.clone());
                }

                Ok(result)
            }
            Err(e) => {
                // Check if this is a session expiry error
                if Self::is_session_expired(&e) {
                    warn!("CLI session expired, retrying with new session");
                    *self.session_id.lock().await = None;
                    let mut result = self.execute_cli(message, None).await?;
                    result.session_renewed = true;
                    result.elapsed_secs = start.elapsed().as_secs_f64();

                    if let Some(ref new_session) = result.session_id {
                        *self.session_id.lock().await = Some(new_session.clone());
                    }

                    Ok(result)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Execute the CLI process with watchdog and overall timeout.
    async fn execute_cli(
        &self,
        message: &str,
        session_id: Option<&str>,
    ) -> Result<CliAgentResult, String> {
        let mut cmd = Command::new(&self.config.binary_path);
        cmd.args(&self.config.default_args);

        // Session resume argument
        if let Some(sid) = session_id {
            cmd.arg("--resume").arg(sid);
        }

        // JSON output mode
        if self.config.json_output {
            cmd.arg("--output-format").arg("json");
        }

        // The user message as the final positional argument
        cmd.arg("--").arg(message);

        // Environment
        for (k, v) in &self.config.env {
            cmd.env(k, v);
        }
        if let Some(ref dir) = self.config.working_dir {
            cmd.current_dir(dir);
        }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| format!("failed to spawn CLI: {}", e))?;

        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;

        // Collect output with timer-reset watchdog
        let watchdog_timeout =
            std::time::Duration::from_secs(self.config.watchdog_timeout_secs);
        let overall_timeout =
            std::time::Duration::from_secs(self.config.overall_timeout_secs);

        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<String>(256);

        // Spawn stdout reader
        let stdout_tx = output_tx.clone();
        let stdout_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            let mut collected = String::new();
            while let Ok(Some(line)) = reader.next_line().await {
                collected.push_str(&line);
                collected.push('\n');
                let _ = stdout_tx.send(line).await;
            }
            collected
        });

        // Spawn stderr reader
        let stderr_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            let mut collected = String::new();
            while let Ok(Some(line)) = reader.next_line().await {
                collected.push_str(&line);
                collected.push('\n');
            }
            collected
        });

        // Drop original sender so channel closes when readers finish
        drop(output_tx);

        // Timer-reset watchdog + overall timeout
        let start = std::time::Instant::now();
        let mut last_output = std::time::Instant::now();

        loop {
            let time_since_output = last_output.elapsed();
            let overall_elapsed = start.elapsed();

            if overall_elapsed >= overall_timeout {
                warn!("CLI overall timeout reached, killing process");
                let _ = child.kill().await;
                return Err("CLI agent overall timeout exceeded".to_string());
            }

            if time_since_output >= watchdog_timeout {
                warn!("CLI watchdog timeout (no output), killing process");
                let _ = child.kill().await;
                return Err("CLI agent watchdog timeout: no output".to_string());
            }

            let remaining_watchdog = watchdog_timeout.saturating_sub(time_since_output);
            let remaining_overall = overall_timeout.saturating_sub(overall_elapsed);
            let wait_time = remaining_watchdog.min(remaining_overall);

            tokio::select! {
                result = output_rx.recv() => {
                    match result {
                        Some(_line) => {
                            last_output = std::time::Instant::now();
                        }
                        None => {
                            // Channel closed — readers are done
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(wait_time) => {
                    // Will re-check timeouts at top of loop
                    continue;
                }
            }
        }

        // Wait for process to finish
        let status = child
            .wait()
            .await
            .map_err(|e| format!("wait error: {}", e))?;

        let stdout_output = stdout_handle.await.unwrap_or_default();
        let stderr_output = stderr_handle.await.unwrap_or_default();

        if !status.success() && !stderr_output.is_empty() {
            return Err(format!(
                "CLI exited with code {:?}: {}",
                status.code(),
                stderr_output.trim()
            ));
        }

        // Parse output for session ID and response
        let (response, session_id) = Self::parse_output(&stdout_output, self.config.json_output);

        Ok(CliAgentResult {
            response,
            session_id,
            session_renewed: false,
            exit_code: status.code(),
            elapsed_secs: 0.0, // Set by caller
        })
    }

    /// Parse CLI output, extracting response text and session ID.
    fn parse_output(output: &str, json_mode: bool) -> (String, Option<String>) {
        if json_mode {
            // Try to parse as JSON for session_id extraction
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output) {
                let response = parsed
                    .get("result")
                    .or_else(|| parsed.get("response"))
                    .or_else(|| parsed.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(output)
                    .to_string();

                let session_id = parsed
                    .get("session_id")
                    .or_else(|| parsed.get("sessionId"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                return (response, session_id);
            }
        }

        // Fallback: treat entire output as the response
        (output.trim().to_string(), None)
    }

    /// Check if an error indicates session expiry.
    fn is_session_expired(error: &str) -> bool {
        let lower = error.to_lowercase();
        lower.contains("session expired")
            || lower.contains("session not found")
            || lower.contains("invalid session")
            || lower.contains("conversation not found")
    }

    /// Get the current session ID (if any).
    pub async fn current_session_id(&self) -> Option<String> {
        self.session_id.lock().await.clone()
    }

    /// Clear the current session, forcing a fresh start on next run.
    pub async fn clear_session(&self) {
        *self.session_id.lock().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_output() {
        let json = r#"{"result": "Hello!", "session_id": "sess_abc123"}"#;
        let (response, session) = CliAgentRunner::parse_output(json, true);
        assert_eq!(response, "Hello!");
        assert_eq!(session, Some("sess_abc123".to_string()));
    }

    #[test]
    fn parse_plain_output() {
        let plain = "Hello, I'm Claude!\n";
        let (response, session) = CliAgentRunner::parse_output(plain, false);
        assert_eq!(response, "Hello, I'm Claude!");
        assert!(session.is_none());
    }

    #[test]
    fn session_expired_detection() {
        assert!(CliAgentRunner::is_session_expired("Error: session expired"));
        assert!(CliAgentRunner::is_session_expired("Session not found"));
        assert!(!CliAgentRunner::is_session_expired("rate limit exceeded"));
    }

    #[test]
    fn default_config() {
        let config = CliBackendConfig::default();
        assert_eq!(config.binary_path, "claude");
        assert!(config.serialize);
        assert_eq!(config.overall_timeout_secs, 300);
        assert_eq!(config.watchdog_timeout_secs, 60);
    }
}
