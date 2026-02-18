//! Layered tool-policy resolution.
//!
//! Policy lookup follows a **chain of responsibility** pattern:
//!
//! ```text
//! subagent → agent → global → default
//! ```
//!
//! Each layer can **allow** or **deny** specific tools. The first layer
//! that contains the tool wins (O(1) per tool via `FxHashSet`).
//!
//! Policy layers are composed via `PolicyStack::resolve(tool_name)`.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Policy types
// ---------------------------------------------------------------------------

/// Decision for a single tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDecision {
    Allow,
    Deny,
    /// No explicit rule — pass to next layer.
    Unset,
}

/// A single policy layer (e.g. agent-level, global-level).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicyLayer {
    /// Layer name for audit trail.
    pub name: String,
    /// Explicitly allowed tools.
    #[serde(default)]
    pub allow: HashSet<String>,
    /// Explicitly denied tools.
    #[serde(default)]
    pub deny: HashSet<String>,
    /// Wildcard allow-all flag.
    #[serde(default)]
    pub allow_all: bool,
    /// Wildcard deny-all flag.
    #[serde(default)]
    pub deny_all: bool,
}

impl PolicyLayer {
    /// Create a new named layer.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Resolve a tool against this single layer.
    pub fn resolve(&self, tool: &str) -> ToolDecision {
        // Explicit deny takes precedence within a layer.
        if self.deny.contains(tool) {
            return ToolDecision::Deny;
        }
        if self.deny_all {
            return ToolDecision::Deny;
        }
        if self.allow.contains(tool) {
            return ToolDecision::Allow;
        }
        if self.allow_all {
            return ToolDecision::Allow;
        }
        ToolDecision::Unset
    }
}

// ---------------------------------------------------------------------------
// Policy stack
// ---------------------------------------------------------------------------

/// Ordered stack of policy layers. The first layer that returns a non-Unset
/// decision wins.
#[derive(Debug, Clone, Default)]
pub struct PolicyStack {
    /// Layers ordered from most-specific (subagent) to least-specific (default).
    pub layers: Vec<PolicyLayer>,
}

impl PolicyStack {
    /// Create an empty policy stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a layer onto the stack (added at the end = least specific).
    pub fn push(&mut self, layer: PolicyLayer) {
        self.layers.push(layer);
    }

    /// Resolve a tool. Returns `(decision, layer_name)`.
    pub fn resolve(&self, tool: &str) -> (ToolDecision, &str) {
        for layer in &self.layers {
            let decision = layer.resolve(tool);
            if decision != ToolDecision::Unset {
                return (decision, &layer.name);
            }
        }
        // Default: deny if no layer permits.
        (ToolDecision::Deny, "default-deny")
    }

    /// Resolve multiple tools, returning a map of tool → (decision, layer).
    pub fn resolve_batch<'a>(&'a self, tools: &[&str]) -> Vec<ToolResolution<'a>> {
        tools
            .iter()
            .map(|t| {
                let (decision, layer) = self.resolve(t);
                ToolResolution {
                    tool: t.to_string(),
                    decision,
                    resolved_by: layer,
                }
            })
            .collect()
    }
}

/// Result of resolving a single tool.
#[derive(Debug, Clone)]
pub struct ToolResolution<'a> {
    pub tool: String,
    pub decision: ToolDecision,
    pub resolved_by: &'a str,
}

// ---------------------------------------------------------------------------
// Conflict detection
// ---------------------------------------------------------------------------

/// A conflict where the same tool is allowed in one layer and denied in another.
#[derive(Debug, Clone)]
pub struct PolicyConflict {
    pub tool: String,
    pub allow_layer: String,
    pub deny_layer: String,
}

/// Detect conflicts across layers (informational — not errors, since layering
/// resolves them — but useful for UI warnings).
pub fn detect_conflicts(stack: &PolicyStack) -> Vec<PolicyConflict> {
    let mut conflicts = Vec::new();

    for i in 0..stack.layers.len() {
        for j in (i + 1)..stack.layers.len() {
            let layer_a = &stack.layers[i];
            let layer_b = &stack.layers[j];

            // Tools allowed in A but denied in B
            for tool in &layer_a.allow {
                if layer_b.deny.contains(tool) {
                    conflicts.push(PolicyConflict {
                        tool: tool.clone(),
                        allow_layer: layer_a.name.clone(),
                        deny_layer: layer_b.name.clone(),
                    });
                }
            }

            // Tools allowed in B but denied in A
            for tool in &layer_b.allow {
                if layer_a.deny.contains(tool) {
                    conflicts.push(PolicyConflict {
                        tool: tool.clone(),
                        allow_layer: layer_b.name.clone(),
                        deny_layer: layer_a.name.clone(),
                    });
                }
            }
        }
    }

    conflicts
}

// ---------------------------------------------------------------------------
// TOML policy parsing
// ---------------------------------------------------------------------------

/// Parse a tools policy section from agent TOML.
///
/// Expected format:
/// ```toml
/// [tools]
/// allow = ["read_file", "write_file"]
/// deny = ["execute_command"]
/// ```
pub fn parse_tools_policy(toml_str: &str) -> Result<PolicyLayer, String> {
    #[derive(Deserialize)]
    struct ToolsToml {
        #[serde(default)]
        tools: ToolsPolicyToml,
    }

    #[derive(Default, Deserialize)]
    struct ToolsPolicyToml {
        #[serde(default)]
        allow: Vec<String>,
        #[serde(default)]
        deny: Vec<String>,
        #[serde(default)]
        allow_all: bool,
        #[serde(default)]
        deny_all: bool,
    }

    let parsed: ToolsToml =
        toml::from_str(toml_str).map_err(|e| format!("Invalid tool policy TOML: {e}"))?;

    Ok(PolicyLayer {
        name: "parsed".to_string(),
        allow: parsed.tools.allow.into_iter().collect(),
        deny: parsed.tools.deny.into_iter().collect(),
        allow_all: parsed.tools.allow_all,
        deny_all: parsed.tools.deny_all,
    })
}

// ---------------------------------------------------------------------------
// Default global policy
// ---------------------------------------------------------------------------

/// Construct the default global policy layer.
pub fn default_global_policy() -> PolicyLayer {
    let mut layer = PolicyLayer::new("global-default");
    // By default, allow safe read-only tools.
    layer.allow.insert("read_file".to_string());
    layer.allow.insert("list_directory".to_string());
    layer.allow.insert("search_files".to_string());
    layer.allow.insert("web_search".to_string());
    // Deny dangerous tools by default.
    layer.deny.insert("execute_command".to_string());
    layer.deny.insert("delete_file".to_string());
    layer.deny.insert("system_exec".to_string());
    layer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_layer_allow() {
        let mut layer = PolicyLayer::new("agent");
        layer.allow.insert("read_file".into());
        assert_eq!(layer.resolve("read_file"), ToolDecision::Allow);
    }

    #[test]
    fn test_single_layer_deny() {
        let mut layer = PolicyLayer::new("agent");
        layer.deny.insert("execute_command".into());
        assert_eq!(layer.resolve("execute_command"), ToolDecision::Deny);
    }

    #[test]
    fn test_deny_takes_precedence_within_layer() {
        let mut layer = PolicyLayer::new("agent");
        layer.allow.insert("execute_command".into());
        layer.deny.insert("execute_command".into());
        // Deny wins within a single layer.
        assert_eq!(layer.resolve("execute_command"), ToolDecision::Deny);
    }

    #[test]
    fn test_stack_resolution_order() {
        let mut stack = PolicyStack::new();

        let mut subagent = PolicyLayer::new("subagent");
        subagent.allow.insert("execute_command".into());
        stack.push(subagent);

        let mut global = PolicyLayer::new("global");
        global.deny.insert("execute_command".into());
        stack.push(global);

        // Subagent layer wins because it's first.
        let (decision, layer) = stack.resolve("execute_command");
        assert_eq!(decision, ToolDecision::Allow);
        assert_eq!(layer, "subagent");
    }

    #[test]
    fn test_stack_falls_through() {
        let mut stack = PolicyStack::new();

        let subagent = PolicyLayer::new("subagent");
        stack.push(subagent);

        let mut global = PolicyLayer::new("global");
        global.allow.insert("read_file".into());
        stack.push(global);

        // Subagent has no rule → falls to global.
        let (decision, layer) = stack.resolve("read_file");
        assert_eq!(decision, ToolDecision::Allow);
        assert_eq!(layer, "global");
    }

    #[test]
    fn test_default_deny() {
        let stack = PolicyStack::new();
        let (decision, layer) = stack.resolve("anything");
        assert_eq!(decision, ToolDecision::Deny);
        assert_eq!(layer, "default-deny");
    }

    #[test]
    fn test_resolve_batch() {
        let mut stack = PolicyStack::new();
        let mut layer = PolicyLayer::new("agent");
        layer.allow.insert("read_file".into());
        layer.deny.insert("delete_file".into());
        stack.push(layer);

        let results = stack.resolve_batch(&["read_file", "delete_file", "unknown"]);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].decision, ToolDecision::Allow);
        assert_eq!(results[1].decision, ToolDecision::Deny);
        assert_eq!(results[2].decision, ToolDecision::Deny); // default-deny
    }

    #[test]
    fn test_detect_conflicts() {
        let mut stack = PolicyStack::new();

        let mut agent = PolicyLayer::new("agent");
        agent.allow.insert("execute_command".into());
        stack.push(agent);

        let mut global = PolicyLayer::new("global");
        global.deny.insert("execute_command".into());
        stack.push(global);

        let conflicts = detect_conflicts(&stack);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].tool, "execute_command");
    }

    #[test]
    fn test_wildcard_allow_all() {
        let mut layer = PolicyLayer::new("permissive");
        layer.allow_all = true;
        assert_eq!(layer.resolve("anything"), ToolDecision::Allow);
    }

    #[test]
    fn test_wildcard_deny_all() {
        let mut layer = PolicyLayer::new("restrictive");
        layer.deny_all = true;
        assert_eq!(layer.resolve("anything"), ToolDecision::Deny);
    }

    #[test]
    fn test_parse_tools_policy() {
        let toml = r#"
[tools]
allow = ["read_file", "web_search"]
deny = ["execute_command"]
"#;
        let layer = parse_tools_policy(toml).unwrap();
        assert!(layer.allow.contains("read_file"));
        assert!(layer.allow.contains("web_search"));
        assert!(layer.deny.contains("execute_command"));
    }

    #[test]
    fn test_default_global_policy() {
        let policy = default_global_policy();
        assert!(policy.allow.contains("read_file"));
        assert!(policy.deny.contains("execute_command"));
    }
}
