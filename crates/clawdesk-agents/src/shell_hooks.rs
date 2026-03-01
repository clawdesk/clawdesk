//! User-Configurable Shell Hooks — Pre/Post Tool Execution Automation.
//!
//! Enables shell commands to run before/after specific tool calls, loaded
//! from `.clawdesk/hooks.toml`:
//!
//! ```toml
//! [[hooks]]
//! phase = "after"
//! tool = "file_write"
//! pattern = "*.rs"
//! command = "rustfmt {path}"
//!
//! [[hooks]]
//! phase = "before"
//! tool = "shell_exec"
//! pattern = "git commit*"
//! command = "cargo clippy --all-targets"
//! ```
//!
//! Integrates with the existing `HookManager` via the `Hook` trait.

use async_trait::async_trait;
use clawdesk_plugin::hooks::{Hook, HookContext, HookResult, Phase, Priority};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{debug, info, warn, error};

// ── Configuration types ──────────────────────────────────────

/// Top-level hooks configuration file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HooksConfig {
    #[serde(default)]
    pub hooks: Vec<ShellHookConfig>,
}

/// Configuration for a single shell hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellHookConfig {
    /// When to run: "before" or "after" the tool call.
    pub phase: HookPhase,
    /// Tool name to match (e.g., "file_write", "shell_exec").
    pub tool: String,
    /// Optional glob pattern to match against tool arguments.
    /// For file_write: matches the file path.
    /// For shell_exec: matches the command string.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Shell command to execute. Supports variable substitution:
    /// - `{path}` — file path (for file_write)
    /// - `{command}` — shell command (for shell_exec)
    /// - `{tool}` — tool name
    /// - `{args}` — raw arguments JSON
    pub command: String,
    /// Timeout in seconds (default: 30).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Whether a failing before-hook should cancel the tool call.
    /// Default: true for before hooks, false for after hooks.
    #[serde(default)]
    pub cancel_on_failure: Option<bool>,
    /// Working directory for the command (default: workspace or cwd).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Optional human-readable label for logging.
    #[serde(default)]
    pub label: Option<String>,
}

/// Phase configuration in TOML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPhase {
    Before,
    After,
}

fn default_timeout() -> u64 { 30 }

// ── Shell Hook implementation ────────────────────────────────

/// A shell hook that implements the `Hook` trait.
///
/// Loaded from user config and registered with the HookManager
/// at startup. Dispatched via the standard hook pipeline.
pub struct ShellHook {
    /// Unique name for this hook.
    name: String,
    /// Phase to fire at.
    phase: Phase,
    /// Tool name to match.
    tool: String,
    /// Compiled pattern for argument matching.
    pattern: Option<regex::Regex>,
    /// Original glob pattern string.
    pattern_glob: Option<String>,
    /// Shell command template.
    command: String,
    /// Timeout.
    timeout: Duration,
    /// Whether failure cancels the tool call (before-hooks only).
    cancel_on_failure: bool,
    /// Working directory.
    cwd: Option<PathBuf>,
}

impl ShellHook {
    /// Create a ShellHook from config.
    pub fn from_config(config: &ShellHookConfig, index: usize) -> Result<Self, String> {
        let phase = match config.phase {
            HookPhase::Before => Phase::BeforeToolCall,
            HookPhase::After => Phase::AfterToolCall,
        };

        let pattern = if let Some(ref glob) = config.pattern {
            let regex_str = glob_to_regex(glob);
            let re = regex::Regex::new(&regex_str)
                .map_err(|e| format!("invalid pattern '{}': {}", glob, e))?;
            Some(re)
        } else {
            None
        };

        let cancel_on_failure = config.cancel_on_failure.unwrap_or(
            config.phase == HookPhase::Before
        );

        let name = config
            .label
            .clone()
            .unwrap_or_else(|| format!("shell-hook-{}-{}-{}", config.phase_str(), config.tool, index));

        Ok(Self {
            name,
            phase,
            tool: config.tool.clone(),
            pattern,
            pattern_glob: config.pattern.clone(),
            command: config.command.clone(),
            timeout: Duration::from_secs(config.timeout_secs),
            cancel_on_failure,
            cwd: config.cwd.as_ref().map(PathBuf::from),
        })
    }

    /// Check if this hook should fire for the given tool call.
    fn matches(&self, tool_name: &str, arguments: &str) -> bool {
        if tool_name != self.tool {
            return false;
        }

        if let Some(ref re) = self.pattern {
            let target = extract_match_target(tool_name, arguments);
            re.is_match(&target)
        } else {
            true // No pattern = match all calls to this tool
        }
    }

    /// Substitute variables in the command template.
    fn substitute_command(&self, tool_name: &str, arguments: &str) -> String {
        let mut cmd = self.command.clone();

        cmd = cmd.replace("{tool}", tool_name);
        cmd = cmd.replace("{args}", arguments);

        // Extract tool-specific values for substitution
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                cmd = cmd.replace("{path}", path);
            }
            if let Some(command) = args.get("command").and_then(|v| v.as_str()) {
                cmd = cmd.replace("{command}", command);
            }
            if let Some(url) = args.get("url").and_then(|v| v.as_str()) {
                cmd = cmd.replace("{url}", url);
            }
            if let Some(to) = args.get("to").and_then(|v| v.as_str()) {
                cmd = cmd.replace("{to}", to);
            }
        }

        cmd
    }

    /// Execute the shell command.
    async fn run_command(&self, tool_name: &str, arguments: &str) -> Result<String, String> {
        let cmd = self.substitute_command(tool_name, arguments);

        debug!(
            hook = %self.name,
            command = %cmd,
            "executing shell hook"
        );

        let mut proc = tokio::process::Command::new("sh");
        proc.arg("-c").arg(&cmd);

        if let Some(ref cwd) = self.cwd {
            proc.current_dir(cwd);
        }

        proc.stdout(std::process::Stdio::piped());
        proc.stderr(std::process::Stdio::piped());

        let child = proc.spawn()
            .map_err(|e| format!("failed to spawn shell hook '{}': {}", self.name, e))?;

        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| format!("shell hook '{}' timed out after {}s", self.name, self.timeout.as_secs()))?
            .map_err(|e| format!("shell hook '{}' failed: {}", self.name, e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            info!(
                hook = %self.name,
                exit_code = 0,
                stdout_len = stdout.len(),
                "shell hook completed"
            );
            Ok(stdout)
        } else {
            let code = output.status.code().unwrap_or(-1);
            warn!(
                hook = %self.name,
                exit_code = code,
                stderr = %stderr,
                "shell hook failed"
            );
            Err(format!(
                "shell hook '{}' exited with code {}: {}",
                self.name, code, stderr.trim()
            ))
        }
    }
}

impl ShellHookConfig {
    fn phase_str(&self) -> &str {
        match self.phase {
            HookPhase::Before => "before",
            HookPhase::After => "after",
        }
    }
}

#[async_trait]
impl Hook for ShellHook {
    fn name(&self) -> &str {
        &self.name
    }

    fn phases(&self) -> Vec<Phase> {
        vec![self.phase]
    }

    fn priority(&self) -> Priority {
        // Shell hooks run at priority 200 (after built-in hooks at 100)
        200
    }

    async fn execute(&self, ctx: HookContext) -> HookResult {
        // Extract tool_name and arguments from hook context data
        let tool_name = ctx.data.get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let arguments = ctx.data.get("arguments")
            .map(|v| v.to_string())
            .unwrap_or_default();

        // Check if this hook matches the tool call
        if !self.matches(tool_name, &arguments) {
            return HookResult::Continue(ctx);
        }

        debug!(
            hook = %self.name,
            tool = tool_name,
            "shell hook matched"
        );

        match self.run_command(tool_name, &arguments).await {
            Ok(_output) => HookResult::Continue(ctx),
            Err(e) => {
                if self.cancel_on_failure && self.phase == Phase::BeforeToolCall {
                    let mut cancelled_ctx = ctx;
                    cancelled_ctx.cancelled = true;
                    cancelled_ctx.data["block_reason"] =
                        serde_json::json!(format!("shell hook '{}' failed: {}", self.name, e));
                    HookResult::ShortCircuit(cancelled_ctx)
                } else {
                    // Log error but continue
                    error!(hook = %self.name, error = %e, "shell hook error (non-blocking)");
                    HookResult::Continue(ctx)
                }
            }
        }
    }
}

// ── Configuration loading ────────────────────────────────────

impl HooksConfig {
    /// Load hooks configuration from the standard path.
    pub fn load_from_default_path() -> Self {
        let candidates = hooks_config_paths();
        for path in &candidates {
            if path.exists() {
                match Self::load_from_file(path) {
                    Ok(config) => {
                        info!(
                            path = %path.display(),
                            count = config.hooks.len(),
                            "loaded shell hooks config"
                        );
                        return config;
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "failed to parse hooks config");
                    }
                }
            }
        }
        debug!("no hooks config found, using empty hooks");
        Self::default()
    }

    /// Load from a specific file.
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        toml::from_str(&content)
            .map_err(|e| format!("failed to parse {}: {}", path.display(), e))
    }

    /// Compile all hook configs into ShellHook instances.
    pub fn compile(&self) -> Vec<ShellHook> {
        self.hooks
            .iter()
            .enumerate()
            .filter_map(|(i, config)| {
                match ShellHook::from_config(config, i) {
                    Ok(hook) => {
                        debug!(
                            name = %hook.name,
                            tool = %hook.tool,
                            "compiled shell hook"
                        );
                        Some(hook)
                    }
                    Err(e) => {
                        warn!(index = i, error = %e, "invalid shell hook config, skipping");
                        None
                    }
                }
            })
            .collect()
    }
}

/// Register all shell hooks from config with the HookManager.
pub async fn register_shell_hooks(
    hook_manager: &clawdesk_plugin::hooks::HookManager,
    config: &HooksConfig,
) {
    let hooks = config.compile();
    let count = hooks.len();

    for hook in hooks {
        let name = hook.name.clone();
        hook_manager.register(std::sync::Arc::new(hook)).await;
        debug!(hook = %name, "registered shell hook with hook manager");
    }

    if count > 0 {
        info!(count, "registered shell hooks from user config");
    }
}

// ── Helpers ──────────────────────────────────────────────────

/// Return candidate paths for hooks config file.
fn hooks_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Project-local: .clawdesk/hooks.toml
    paths.push(PathBuf::from(".clawdesk").join("hooks.toml"));

    // Home: ~/.clawdesk/hooks.toml
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        paths.push(PathBuf::from(home).join(".clawdesk").join("hooks.toml"));
    }

    paths
}

/// Convert a glob pattern to a regex.
fn glob_to_regex(glob: &str) -> String {
    let mut regex = String::with_capacity(glob.len() * 2 + 2);
    regex.push('^');

    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '[' => {
                regex.push('[');
                i += 1;
                while i < chars.len() && chars[i] != ']' {
                    regex.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() {
                    regex.push(']');
                }
            }
            c => {
                if "\\^$.|+(){}".contains(c) {
                    regex.push('\\');
                }
                regex.push(c);
            }
        }
        i += 1;
    }

    regex.push('$');
    regex
}

/// Extract the match target from tool arguments (same logic as permission_modes).
fn extract_match_target(tool_name: &str, arguments: &str) -> String {
    if let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) {
        match tool_name {
            "shell_exec" | "shell" => {
                if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                    return cmd.to_string();
                }
            }
            "file_write" => {
                if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                    return path.to_string();
                }
            }
            _ => {}
        }
    }
    arguments.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hooks_config() {
        let toml_str = r#"
[[hooks]]
phase = "after"
tool = "file_write"
pattern = "*.rs"
command = "rustfmt {path}"

[[hooks]]
phase = "before"
tool = "shell_exec"
pattern = "git commit*"
command = "cargo clippy --all-targets"
timeout_secs = 60
"#;
        let config: HooksConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.hooks.len(), 2);
        assert_eq!(config.hooks[0].phase, HookPhase::After);
        assert_eq!(config.hooks[0].tool, "file_write");
        assert_eq!(config.hooks[1].phase, HookPhase::Before);
        assert_eq!(config.hooks[1].timeout_secs, 60);
    }

    #[test]
    fn compile_hooks() {
        let config = HooksConfig {
            hooks: vec![
                ShellHookConfig {
                    phase: HookPhase::After,
                    tool: "file_write".to_string(),
                    pattern: Some("*.rs".to_string()),
                    command: "rustfmt {path}".to_string(),
                    timeout_secs: 30,
                    cancel_on_failure: None,
                    cwd: None,
                    label: Some("auto-format-rs".to_string()),
                },
                ShellHookConfig {
                    phase: HookPhase::Before,
                    tool: "shell_exec".to_string(),
                    pattern: Some("git commit*".to_string()),
                    command: "cargo clippy".to_string(),
                    timeout_secs: 60,
                    cancel_on_failure: Some(true),
                    cwd: None,
                    label: None,
                },
            ],
        };

        let hooks = config.compile();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].name, "auto-format-rs");
        assert!(hooks[0].pattern.is_some());
        assert_eq!(hooks[1].phase, Phase::BeforeToolCall);
        assert!(hooks[1].cancel_on_failure);
    }

    #[test]
    fn hook_matching() {
        let hook = ShellHook::from_config(
            &ShellHookConfig {
                phase: HookPhase::After,
                tool: "file_write".to_string(),
                pattern: Some("*.rs".to_string()),
                command: "rustfmt {path}".to_string(),
                timeout_secs: 30,
                cancel_on_failure: None,
                cwd: None,
                label: None,
            },
            0,
        )
        .unwrap();

        assert!(hook.matches("file_write", r#"{"path":"src/main.rs"}"#));
        assert!(!hook.matches("file_write", r#"{"path":"README.md"}"#));
        assert!(!hook.matches("file_read", r#"{"path":"src/main.rs"}"#));
    }

    #[test]
    fn hook_no_pattern_matches_all() {
        let hook = ShellHook::from_config(
            &ShellHookConfig {
                phase: HookPhase::After,
                tool: "file_write".to_string(),
                pattern: None,
                command: "echo done".to_string(),
                timeout_secs: 30,
                cancel_on_failure: None,
                cwd: None,
                label: None,
            },
            0,
        )
        .unwrap();

        assert!(hook.matches("file_write", r#"{"path":"anything.txt"}"#));
        assert!(!hook.matches("file_read", r#"{"path":"anything.txt"}"#));
    }

    #[test]
    fn variable_substitution() {
        let hook = ShellHook::from_config(
            &ShellHookConfig {
                phase: HookPhase::After,
                tool: "file_write".to_string(),
                pattern: None,
                command: "rustfmt {path} && echo {tool}".to_string(),
                timeout_secs: 30,
                cancel_on_failure: None,
                cwd: None,
                label: None,
            },
            0,
        )
        .unwrap();

        let cmd = hook.substitute_command("file_write", r#"{"path":"src/main.rs"}"#);
        assert_eq!(cmd, "rustfmt src/main.rs && echo file_write");
    }

    #[test]
    fn default_cancel_behavior() {
        // Before hooks default to cancel_on_failure = true
        let before = ShellHook::from_config(
            &ShellHookConfig {
                phase: HookPhase::Before,
                tool: "shell_exec".to_string(),
                pattern: None,
                command: "true".to_string(),
                timeout_secs: 30,
                cancel_on_failure: None,
                cwd: None,
                label: None,
            },
            0,
        )
        .unwrap();
        assert!(before.cancel_on_failure);

        // After hooks default to cancel_on_failure = false
        let after = ShellHook::from_config(
            &ShellHookConfig {
                phase: HookPhase::After,
                tool: "file_write".to_string(),
                pattern: None,
                command: "true".to_string(),
                timeout_secs: 30,
                cancel_on_failure: None,
                cwd: None,
                label: None,
            },
            0,
        )
        .unwrap();
        assert!(!after.cancel_on_failure);
    }

    #[test]
    fn empty_config() {
        let config = HooksConfig::default();
        assert!(config.hooks.is_empty());
        let hooks = config.compile();
        assert!(hooks.is_empty());
    }

    #[tokio::test]
    async fn shell_hook_execute_echo() {
        let hook = ShellHook::from_config(
            &ShellHookConfig {
                phase: HookPhase::After,
                tool: "file_write".to_string(),
                pattern: None,
                command: "echo hello".to_string(),
                timeout_secs: 5,
                cancel_on_failure: None,
                cwd: None,
                label: Some("test-echo".to_string()),
            },
            0,
        )
        .unwrap();

        let result = hook.run_command("file_write", "{}").await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn shell_hook_execute_failure() {
        let hook = ShellHook::from_config(
            &ShellHookConfig {
                phase: HookPhase::Before,
                tool: "shell_exec".to_string(),
                pattern: None,
                command: "exit 1".to_string(),
                timeout_secs: 5,
                cancel_on_failure: Some(true),
                cwd: None,
                label: Some("test-fail".to_string()),
            },
            0,
        )
        .unwrap();

        let result = hook.run_command("shell_exec", "{}").await;
        assert!(result.is_err());
    }
}
