//! # Policy DSL — Declarative tool execution policies
//!
//! The audit found: "No policy language DSL (hardcoded in code)".
//! This module adds a small, declarative policy language that can be
//! loaded from config files (TOML/YAML) instead of being hardcoded.
//!
//! ## Syntax
//!
//! ```toml
//! [[rules]]
//! name = "shell_requires_approval"
//! match_tool = "shell_exec"
//! action = "require_approval"
//! reason = "Shell commands are dangerous"
//!
//! [[rules]]
//! name = "file_write_in_workspace"
//! match_tool = "file_write"
//! condition = "path_starts_with('/tmp')"
//! action = "deny"
//! reason = "Cannot write outside workspace"
//!
//! [[rules]]
//! name = "web_search_always_allow"
//! match_tool = "web_search"
//! action = "allow"
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A complete policy document containing ordered rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDocument {
    /// Ordered list of rules. First match wins.
    pub rules: Vec<PolicyRule>,
    /// Default action when no rule matches.
    #[serde(default = "default_action")]
    pub default_action: PolicyAction,
    /// Policy version for tracking changes.
    #[serde(default)]
    pub version: String,
}

fn default_action() -> PolicyAction {
    PolicyAction::RequireApproval
}

/// A single policy rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Human-readable rule name.
    pub name: String,
    /// Tool name pattern to match (exact or glob).
    pub match_tool: String,
    /// Optional condition expression.
    #[serde(default)]
    pub condition: Option<String>,
    /// Action to take when the rule matches.
    pub action: PolicyAction,
    /// Human-readable reason (shown to user on deny/approval).
    #[serde(default)]
    pub reason: String,
}

/// What to do when a rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    /// Allow the tool call without approval.
    Allow,
    /// Require human approval before execution.
    RequireApproval,
    /// Block the tool call entirely.
    Deny,
    /// Allow but log for audit.
    AllowWithAudit,
}

/// Result of evaluating a policy.
#[derive(Debug, Clone)]
pub struct PolicyDecision {
    pub action: PolicyAction,
    pub matched_rule: Option<String>,
    pub reason: String,
}

/// Evaluates tool calls against a policy document.
pub struct PolicyEngine {
    document: PolicyDocument,
}

impl PolicyEngine {
    pub fn new(document: PolicyDocument) -> Self {
        Self { document }
    }

    /// Evaluate a tool call against the policy.
    pub fn evaluate(&self, tool_name: &str, args: &serde_json::Value) -> PolicyDecision {
        for rule in &self.document.rules {
            if !matches_tool(&rule.match_tool, tool_name) {
                continue;
            }

            if let Some(ref cond) = rule.condition {
                if !evaluate_condition(cond, tool_name, args) {
                    continue;
                }
            }

            return PolicyDecision {
                action: rule.action,
                matched_rule: Some(rule.name.clone()),
                reason: rule.reason.clone(),
            };
        }

        // No rule matched — use default.
        PolicyDecision {
            action: self.document.default_action,
            matched_rule: None,
            reason: "no matching rule — default policy applied".into(),
        }
    }

    /// Parse a TOML policy document.
    pub fn from_toml(toml_str: &str) -> Result<Self, String> {
        let doc: PolicyDocument =
            toml::from_str(toml_str).map_err(|e| format!("Invalid policy TOML: {}", e))?;
        Ok(Self::new(doc))
    }
}

/// Match tool name against a pattern (exact or glob with `*`).
fn matches_tool(pattern: &str, tool_name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.ends_with('*') {
        let prefix = &pattern[..pattern.len() - 1];
        return tool_name.starts_with(prefix);
    }
    if pattern.starts_with('*') {
        let suffix = &pattern[1..];
        return tool_name.ends_with(suffix);
    }
    pattern == tool_name
}

/// Evaluate a simple condition expression against tool arguments.
/// Supports: `path_starts_with('...')`, `arg_equals('key', 'value')`, `always`.
fn evaluate_condition(condition: &str, _tool_name: &str, args: &serde_json::Value) -> bool {
    let condition = condition.trim();

    if condition == "always" {
        return true;
    }

    // path_starts_with('/tmp')
    if let Some(rest) = condition.strip_prefix("path_starts_with(") {
        if let Some(path_pattern) = rest.strip_suffix(')') {
            let path_pattern = path_pattern.trim_matches('\'').trim_matches('"');
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                return path.starts_with(path_pattern);
            }
            return false;
        }
    }

    // arg_equals('key', 'value')
    if let Some(rest) = condition.strip_prefix("arg_equals(") {
        if let Some(inner) = rest.strip_suffix(')') {
            let parts: Vec<&str> = inner.splitn(2, ',').collect();
            if parts.len() == 2 {
                let key = parts[0].trim().trim_matches('\'').trim_matches('"');
                let val = parts[1].trim().trim_matches('\'').trim_matches('"');
                if let Some(actual) = args.get(key).and_then(|v| v.as_str()) {
                    return actual == val;
                }
            }
            return false;
        }
    }

    // arg_present('key')
    if let Some(rest) = condition.strip_prefix("arg_present(") {
        if let Some(inner) = rest.strip_suffix(')') {
            let key = inner.trim().trim_matches('\'').trim_matches('"');
            return args.get(key).is_some();
        }
    }

    // Unknown condition → fail-closed (don't match).
    false
}

impl Default for PolicyDocument {
    fn default() -> Self {
        Self {
            rules: vec![
                PolicyRule {
                    name: "allow_read_tools".into(),
                    match_tool: "file_read".into(),
                    condition: None,
                    action: PolicyAction::Allow,
                    reason: "Read operations are safe".into(),
                },
                PolicyRule {
                    name: "allow_list_tools".into(),
                    match_tool: "file_list".into(),
                    condition: None,
                    action: PolicyAction::Allow,
                    reason: "List operations are safe".into(),
                },
                PolicyRule {
                    name: "allow_grep".into(),
                    match_tool: "grep".into(),
                    condition: None,
                    action: PolicyAction::Allow,
                    reason: "Search operations are safe".into(),
                },
                PolicyRule {
                    name: "allow_web_search".into(),
                    match_tool: "web_search".into(),
                    condition: None,
                    action: PolicyAction::Allow,
                    reason: "Web search is safe".into(),
                },
                PolicyRule {
                    name: "audit_file_writes".into(),
                    match_tool: "file_write".into(),
                    condition: None,
                    action: PolicyAction::AllowWithAudit,
                    reason: "File writes are logged for audit".into(),
                },
                PolicyRule {
                    name: "approve_shell".into(),
                    match_tool: "shell_exec".into(),
                    condition: None,
                    action: PolicyAction::RequireApproval,
                    reason: "Shell commands require human approval".into(),
                },
            ],
            default_action: PolicyAction::RequireApproval,
            version: "1.0.0".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let engine = PolicyEngine::new(PolicyDocument::default());
        let decision = engine.evaluate("file_read", &serde_json::json!({}));
        assert_eq!(decision.action, PolicyAction::Allow);
    }

    #[test]
    fn test_shell_requires_approval() {
        let engine = PolicyEngine::new(PolicyDocument::default());
        let decision = engine.evaluate("shell_exec", &serde_json::json!({"command": "rm -rf /"}));
        assert_eq!(decision.action, PolicyAction::RequireApproval);
    }

    #[test]
    fn test_default_action() {
        let engine = PolicyEngine::new(PolicyDocument::default());
        let decision = engine.evaluate("unknown_tool", &serde_json::json!({}));
        assert_eq!(decision.action, PolicyAction::RequireApproval);
    }

    #[test]
    fn test_condition_path_starts_with() {
        let doc = PolicyDocument {
            rules: vec![PolicyRule {
                name: "deny_tmp".into(),
                match_tool: "file_write".into(),
                condition: Some("path_starts_with('/tmp')".into()),
                action: PolicyAction::Deny,
                reason: "no /tmp writes".into(),
            }],
            default_action: PolicyAction::Allow,
            version: "1".into(),
        };
        let engine = PolicyEngine::new(doc);

        let inside = engine.evaluate("file_write", &serde_json::json!({"path": "/tmp/evil.sh"}));
        assert_eq!(inside.action, PolicyAction::Deny);

        let outside = engine.evaluate("file_write", &serde_json::json!({"path": "/workspace/app.js"}));
        assert_eq!(outside.action, PolicyAction::Allow); // falls through to default
    }

    #[test]
    fn test_glob_match() {
        assert!(matches_tool("mcp_*", "mcp_github_create_issue"));
        assert!(matches_tool("*_search", "web_search"));
        assert!(!matches_tool("file_*", "shell_exec"));
    }

    #[test]
    fn test_toml_parsing() {
        let toml = r#"
version = "2.0"
default_action = "deny"

[[rules]]
name = "allow_reads"
match_tool = "file_read"
action = "allow"
reason = "reads are safe"

[[rules]]
name = "approve_writes"
match_tool = "file_write"
action = "require_approval"
reason = "writes need approval"
"#;
        let engine = PolicyEngine::from_toml(toml).unwrap();
        let r = engine.evaluate("file_read", &serde_json::json!({}));
        assert_eq!(r.action, PolicyAction::Allow);

        let w = engine.evaluate("file_write", &serde_json::json!({}));
        assert_eq!(w.action, PolicyAction::RequireApproval);

        let u = engine.evaluate("unknown", &serde_json::json!({}));
        assert_eq!(u.action, PolicyAction::Deny);
    }
}
