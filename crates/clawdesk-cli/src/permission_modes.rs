//! Permission modes for tool execution control.
//!
//! Three modes matching Claude Code's progressive trust model:
//!
//! 1. **Interactive** (default) — prompt user, cache per session
//! 2. **Allowlist** — config-driven glob patterns for auto-approved commands
//! 3. **Unattended** — all tools auto-approved (CI/CD, trusted environments)
//!
//! Config loaded from `~/.clawdesk/config.toml`:
//!
//! ```toml
//! [tools.permissions]
//! mode = "allowlist"
//!
//! [tools.allowlist]
//! shell_exec = ["git *", "cargo *", "npm *", "ls", "cat", "grep"]
//! file_write = ["*.rs", "*.toml", "*.md"]
//! message_send = ["telegram", "slack"]
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Permission mode for tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Prompt user for each dangerous tool, cache per session.
    Interactive,
    /// Auto-approve commands matching allowlist patterns; prompt for the rest.
    Allowlist,
    /// All tools auto-approved (requires explicit opt-in).
    Unattended,
}

impl Default for PermissionMode {
    fn default() -> Self {
        Self::Interactive
    }
}

/// Compiled allowlist entry: a set of glob patterns compiled to regex.
#[derive(Debug, Clone)]
pub struct CompiledAllowlist {
    /// Tool name → list of compiled regex patterns.
    tool_patterns: HashMap<String, Vec<CompiledPattern>>,
}

/// A single compiled pattern with the original glob for display.
#[derive(Debug, Clone)]
struct CompiledPattern {
    /// Original glob pattern string.
    glob: String,
    /// Compiled regex for matching.
    regex: regex::Regex,
}

/// Configuration for the permission system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionConfig {
    /// Active permission mode.
    #[serde(default)]
    pub mode: PermissionMode,
    /// Glob patterns per tool name for allowlist mode.
    #[serde(default)]
    pub allowlist: HashMap<String, Vec<String>>,
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Interactive,
            allowlist: HashMap::new(),
        }
    }
}

/// Full tools config section from config.toml.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ToolsConfig {
    #[serde(default)]
    permissions: PermissionConfig,
}

/// Top-level config wrapper.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    tools: ToolsConfig,
}

impl PermissionConfig {
    /// Load permission config from the standard config path.
    ///
    /// Searches: `~/.clawdesk/config.toml` → `$XDG_CONFIG_HOME/clawdesk/config.toml`.
    /// Returns default config if the file doesn't exist.
    pub fn load_from_default_path() -> Self {
        let candidates = config_paths();
        for path in &candidates {
            if path.exists() {
                match Self::load_from_file(path) {
                    Ok(config) => {
                        info!(path = %path.display(), mode = ?config.mode, "loaded permission config");
                        return config;
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "failed to parse config, using defaults");
                    }
                }
            }
        }
        debug!("no config file found, using default permission config");
        Self::default()
    }

    /// Load permission config from a specific file path.
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        let config: ConfigFile = toml::from_str(&content)
            .map_err(|e| format!("failed to parse {}: {}", path.display(), e))?;
        Ok(config.tools.permissions)
    }

    /// Compile glob patterns into a `CompiledAllowlist` for fast matching.
    ///
    /// Glob → regex conversion at load time (O(|pattern|) per pattern).
    /// Runtime matching is O(|command|) per pattern, or O(|command|)
    /// amortized when patterns are combined.
    pub fn compile_allowlist(&self) -> CompiledAllowlist {
        let mut tool_patterns = HashMap::new();

        for (tool, patterns) in &self.allowlist {
            let compiled: Vec<CompiledPattern> = patterns
                .iter()
                .filter_map(|glob| {
                    let regex_str = glob_to_regex(glob);
                    match regex::Regex::new(&regex_str) {
                        Ok(re) => Some(CompiledPattern {
                            glob: glob.clone(),
                            regex: re,
                        }),
                        Err(e) => {
                            warn!(
                                tool = %tool,
                                glob = %glob,
                                error = %e,
                                "invalid allowlist glob pattern, skipping"
                            );
                            None
                        }
                    }
                })
                .collect();
            if !compiled.is_empty() {
                tool_patterns.insert(tool.clone(), compiled);
            }
        }

        CompiledAllowlist { tool_patterns }
    }
}

impl CompiledAllowlist {
    /// Check if a tool call matches any allowlist pattern.
    ///
    /// For `shell_exec`: matches against the command string.
    /// For `file_write`: matches against the file path.
    /// For `message_send`: matches against the channel name.
    /// For other tools: matches against the full arguments string.
    ///
    /// Returns `true` if any pattern matches (auto-approve).
    pub fn matches(&self, tool_name: &str, arguments: &str) -> bool {
        let patterns = match self.tool_patterns.get(tool_name) {
            Some(p) => p,
            None => return false,
        };

        // Extract the relevant matching target from arguments
        let target = extract_match_target(tool_name, arguments);

        for pattern in patterns {
            if pattern.regex.is_match(&target) {
                debug!(
                    tool = tool_name,
                    pattern = %pattern.glob,
                    target = %target,
                    "allowlist match — auto-approving"
                );
                return true;
            }
        }

        false
    }

    /// Returns true if the allowlist has any patterns for this tool.
    pub fn has_patterns_for(&self, tool_name: &str) -> bool {
        self.tool_patterns.contains_key(tool_name)
    }
}

/// Decision from the permission engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Auto-approved (by mode or allowlist match).
    AutoApprove,
    /// Needs interactive prompt.
    NeedsPrompt,
    /// Denied (by explicit deny rule).
    Denied(String),
}

/// The unified permission engine.
///
/// Resolves the layered decision:
///   config_allowlist → session_cache → interactive_prompt → default_deny
pub struct PermissionEngine {
    mode: PermissionMode,
    allowlist: CompiledAllowlist,
}

impl PermissionEngine {
    /// Create a new permission engine from config.
    pub fn new(config: &PermissionConfig) -> Self {
        Self {
            mode: config.mode.clone(),
            allowlist: config.compile_allowlist(),
        }
    }

    /// Create a fully permissive engine (unattended mode).
    pub fn permissive() -> Self {
        Self {
            mode: PermissionMode::Unattended,
            allowlist: CompiledAllowlist {
                tool_patterns: HashMap::new(),
            },
        }
    }

    /// Evaluate whether a tool call needs approval.
    ///
    /// This is the first layer in the decision chain. The runner then
    /// checks its session cache and finally falls back to the interactive
    /// prompt (CliApprovalGate).
    pub fn evaluate(&self, tool_name: &str, arguments: &str) -> PermissionDecision {
        match &self.mode {
            PermissionMode::Unattended => {
                debug!(tool = tool_name, "auto-approved (unattended mode)");
                PermissionDecision::AutoApprove
            }
            PermissionMode::Allowlist => {
                if self.allowlist.matches(tool_name, arguments) {
                    PermissionDecision::AutoApprove
                } else {
                    // Fall through to interactive prompt for non-matching tools
                    PermissionDecision::NeedsPrompt
                }
            }
            PermissionMode::Interactive => PermissionDecision::NeedsPrompt,
        }
    }

    /// Get the active permission mode.
    pub fn mode(&self) -> &PermissionMode {
        &self.mode
    }
}

// ── Helpers ──────────────────────────────────────────────────

/// Convert a glob pattern to a regex string.
///
/// Supported glob features:
/// - `*` matches any sequence of characters (except `/`)
/// - `?` matches any single character
/// - `[abc]` matches any character in the set
/// - Literal characters are regex-escaped
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
                // Pass through character classes as-is
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
                // Escape regex metacharacters
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

/// Extract the relevant matching string from tool arguments.
///
/// Different tools expose different "important" values:
/// - shell_exec → the command text
/// - file_write → the file path
/// - message_send → the channel name
/// - email_send → the recipient address
/// - http / http_fetch → the URL
/// - Others → full arguments string
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
            "message_send" => {
                if let Some(channel) = args.get("channel").and_then(|v| v.as_str()) {
                    return channel.to_string();
                }
            }
            "email_send" => {
                if let Some(to) = args.get("to").and_then(|v| v.as_str()) {
                    return to.to_string();
                }
            }
            "http" | "http_fetch" => {
                if let Some(url) = args.get("url").and_then(|v| v.as_str()) {
                    return url.to_string();
                }
            }
            _ => {}
        }
    }
    arguments.to_string()
}

/// Return candidate config file paths in priority order.
fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // ~/.clawdesk/config.toml (primary)
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        paths.push(PathBuf::from(&home).join(".clawdesk").join("config.toml"));
    }

    // XDG config (Linux)
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(xdg).join("clawdesk").join("config.toml"));
    } else if let Ok(home) = std::env::var("HOME") {
        paths.push(
            PathBuf::from(home)
                .join(".config")
                .join("clawdesk")
                .join("config.toml"),
        );
    }

    // Project-local .clawdesk/config.toml
    paths.push(PathBuf::from(".clawdesk").join("config.toml"));

    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_to_regex_basic() {
        assert_eq!(glob_to_regex("git *"), "^git .*$");
        assert_eq!(glob_to_regex("*.rs"), "^.*\\.rs$");
        assert_eq!(glob_to_regex("cargo"), "^cargo$");
    }

    #[test]
    fn glob_pattern_matching() {
        let config = PermissionConfig {
            mode: PermissionMode::Allowlist,
            allowlist: {
                let mut m = HashMap::new();
                m.insert(
                    "shell_exec".to_string(),
                    vec!["git *".to_string(), "cargo *".to_string(), "ls".to_string()],
                );
                m.insert(
                    "file_write".to_string(),
                    vec!["*.rs".to_string(), "*.toml".to_string()],
                );
                m
            },
        };

        let allowlist = config.compile_allowlist();

        // Shell commands
        assert!(allowlist.matches("shell_exec", r#"{"command":"git push origin main"}"#));
        assert!(allowlist.matches("shell_exec", r#"{"command":"cargo build"}"#));
        assert!(allowlist.matches("shell_exec", r#"{"command":"ls"}"#));
        assert!(!allowlist.matches("shell_exec", r#"{"command":"rm -rf /"}"#));

        // File writes
        assert!(allowlist.matches("file_write", r#"{"path":"src/main.rs"}"#));
        assert!(allowlist.matches("file_write", r#"{"path":"Cargo.toml"}"#));
        assert!(!allowlist.matches("file_write", r#"{"path":"secrets.env"}"#));

        // Unknown tool — no patterns
        assert!(!allowlist.matches("http", r#"{"url":"https://example.com"}"#));
    }

    #[test]
    fn permission_engine_unattended() {
        let engine = PermissionEngine::permissive();
        assert_eq!(
            engine.evaluate("shell_exec", "{}"),
            PermissionDecision::AutoApprove
        );
        assert_eq!(
            engine.evaluate("file_write", "{}"),
            PermissionDecision::AutoApprove
        );
    }

    #[test]
    fn permission_engine_interactive() {
        let config = PermissionConfig::default();
        let engine = PermissionEngine::new(&config);
        assert_eq!(
            engine.evaluate("shell_exec", "{}"),
            PermissionDecision::NeedsPrompt
        );
    }

    #[test]
    fn permission_engine_allowlist_match() {
        let config = PermissionConfig {
            mode: PermissionMode::Allowlist,
            allowlist: {
                let mut m = HashMap::new();
                m.insert("shell_exec".to_string(), vec!["git *".to_string()]);
                m
            },
        };
        let engine = PermissionEngine::new(&config);

        // Matching command
        assert_eq!(
            engine.evaluate("shell_exec", r#"{"command":"git status"}"#),
            PermissionDecision::AutoApprove
        );

        // Non-matching command
        assert_eq!(
            engine.evaluate("shell_exec", r#"{"command":"rm -rf /"}"#),
            PermissionDecision::NeedsPrompt
        );
    }

    #[test]
    fn permission_config_default() {
        let config = PermissionConfig::default();
        assert_eq!(config.mode, PermissionMode::Interactive);
        assert!(config.allowlist.is_empty());
    }

    #[test]
    fn permission_config_toml_roundtrip() {
        let config = PermissionConfig {
            mode: PermissionMode::Allowlist,
            allowlist: {
                let mut m = HashMap::new();
                m.insert(
                    "shell_exec".to_string(),
                    vec!["git *".to_string(), "cargo *".to_string()],
                );
                m
            },
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: PermissionConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.mode, PermissionMode::Allowlist);
        assert_eq!(parsed.allowlist.len(), 1);
        assert_eq!(parsed.allowlist["shell_exec"].len(), 2);
    }

    #[test]
    fn extract_targets() {
        assert_eq!(
            extract_match_target("shell_exec", r#"{"command":"git push"}"#),
            "git push"
        );
        assert_eq!(
            extract_match_target("file_write", r#"{"path":"src/main.rs","content":"x"}"#),
            "src/main.rs"
        );
        assert_eq!(
            extract_match_target("email_send", r#"{"to":"john@example.com"}"#),
            "john@example.com"
        );
    }
}
