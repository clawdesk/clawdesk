//! CLI surface for tool-policy auditing.
//!
//! ```text
//! clawdesk policy show <agent-id>    # show resolved policy
//! clawdesk policy check <agent-id> <tool>  # check a single tool
//! clawdesk policy conflicts          # show cross-layer conflicts
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Audit report types
// ---------------------------------------------------------------------------

/// Full audit report for an agent's tool policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyAuditReport {
    pub agent_id: String,
    pub layers: Vec<LayerSummary>,
    pub tool_resolutions: Vec<ToolAuditEntry>,
    pub conflicts: Vec<ConflictEntry>,
}

/// Summary of a single policy layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerSummary {
    pub name: String,
    pub allowed_count: usize,
    pub denied_count: usize,
    pub allow_all: bool,
    pub deny_all: bool,
}

/// Audit entry for a single tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAuditEntry {
    pub tool: String,
    pub decision: String,
    pub resolved_by: String,
}

/// A conflict between layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictEntry {
    pub tool: String,
    pub allow_layer: String,
    pub deny_layer: String,
}

// ---------------------------------------------------------------------------
// Report generation
// ---------------------------------------------------------------------------

/// Generate a policy audit report from layer summaries and tool list.
pub fn generate_audit_report(
    agent_id: &str,
    layers: Vec<LayerSummary>,
    tools: &[&str],
    tool_resolutions: Vec<(String, String, String)>, // (tool, decision, layer)
    conflicts: Vec<(String, String, String)>,         // (tool, allow_layer, deny_layer)
) -> PolicyAuditReport {
    PolicyAuditReport {
        agent_id: agent_id.to_string(),
        layers,
        tool_resolutions: tool_resolutions
            .into_iter()
            .map(|(tool, decision, resolved_by)| ToolAuditEntry {
                tool,
                decision,
                resolved_by,
            })
            .collect(),
        conflicts: conflicts
            .into_iter()
            .map(|(tool, allow_layer, deny_layer)| ConflictEntry {
                tool,
                allow_layer,
                deny_layer,
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Pretty-print
// ---------------------------------------------------------------------------

/// Format an audit report as a human-readable table.
pub fn format_audit_report(report: &PolicyAuditReport) -> String {
    let mut out = String::new();

    out.push_str(&format!("Policy Audit: {}\n", report.agent_id));
    out.push_str(&"=".repeat(50));
    out.push('\n');

    // Layers
    out.push_str("\nLayers (most specific → least specific):\n");
    for (i, layer) in report.layers.iter().enumerate() {
        out.push_str(&format!(
            "  {}. {} — {} allowed, {} denied",
            i + 1,
            layer.name,
            layer.allowed_count,
            layer.denied_count,
        ));
        if layer.allow_all {
            out.push_str(" [ALLOW ALL]");
        }
        if layer.deny_all {
            out.push_str(" [DENY ALL]");
        }
        out.push('\n');
    }

    // Resolutions
    if !report.tool_resolutions.is_empty() {
        out.push_str("\nTool Resolutions:\n");
        let max_tool = report
            .tool_resolutions
            .iter()
            .map(|t| t.tool.len())
            .max()
            .unwrap_or(10);
        for entry in &report.tool_resolutions {
            let icon = if entry.decision == "allow" { "✓" } else { "✗" };
            out.push_str(&format!(
                "  {icon} {:<width$}  {:<8}  ({})\n",
                entry.tool,
                entry.decision,
                entry.resolved_by,
                width = max_tool,
            ));
        }
    }

    // Conflicts
    if !report.conflicts.is_empty() {
        out.push_str("\n⚠ Conflicts:\n");
        for c in &report.conflicts {
            out.push_str(&format!(
                "  {} — allowed by '{}', denied by '{}'\n",
                c.tool, c.allow_layer, c.deny_layer
            ));
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Policy diff
// ---------------------------------------------------------------------------

/// Compare two agents' effective policies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDiff {
    pub agent_a: String,
    pub agent_b: String,
    pub only_a_allows: Vec<String>,
    pub only_b_allows: Vec<String>,
    pub both_allow: Vec<String>,
    pub both_deny: Vec<String>,
}

/// Compute a diff given two sets of tool resolutions (tool → decision).
pub fn compute_policy_diff(
    agent_a: &str,
    agent_b: &str,
    resolutions_a: &HashMap<String, String>, // tool → "allow"/"deny"
    resolutions_b: &HashMap<String, String>,
) -> PolicyDiff {
    let all_tools: std::collections::BTreeSet<_> = resolutions_a
        .keys()
        .chain(resolutions_b.keys())
        .cloned()
        .collect();

    let mut only_a_allows = Vec::new();
    let mut only_b_allows = Vec::new();
    let mut both_allow = Vec::new();
    let mut both_deny = Vec::new();

    for tool in &all_tools {
        let a = resolutions_a.get(tool).map(|s| s.as_str()).unwrap_or("deny");
        let b = resolutions_b.get(tool).map(|s| s.as_str()).unwrap_or("deny");

        match (a, b) {
            ("allow", "allow") => both_allow.push(tool.clone()),
            ("deny", "deny") => both_deny.push(tool.clone()),
            ("allow", _) => only_a_allows.push(tool.clone()),
            (_, "allow") => only_b_allows.push(tool.clone()),
            _ => both_deny.push(tool.clone()),
        }
    }

    PolicyDiff {
        agent_a: agent_a.to_string(),
        agent_b: agent_b.to_string(),
        only_a_allows,
        only_b_allows,
        both_allow,
        both_deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_audit_report() {
        let report = generate_audit_report(
            "design-agent",
            vec![LayerSummary {
                name: "agent".into(),
                allowed_count: 3,
                denied_count: 1,
                allow_all: false,
                deny_all: false,
            }],
            &["read_file", "execute_command"],
            vec![
                ("read_file".into(), "allow".into(), "agent".into()),
                ("execute_command".into(), "deny".into(), "agent".into()),
            ],
            vec![],
        );
        assert_eq!(report.agent_id, "design-agent");
        assert_eq!(report.tool_resolutions.len(), 2);
    }

    #[test]
    fn test_format_audit_report() {
        let report = generate_audit_report(
            "test-agent",
            vec![LayerSummary {
                name: "agent".into(),
                allowed_count: 2,
                denied_count: 1,
                allow_all: false,
                deny_all: false,
            }],
            &[],
            vec![("read_file".into(), "allow".into(), "agent".into())],
            vec![("exec".into(), "agent".into(), "global".into())],
        );
        let text = format_audit_report(&report);
        assert!(text.contains("Policy Audit: test-agent"));
        assert!(text.contains("read_file"));
        assert!(text.contains("Conflicts"));
    }

    #[test]
    fn test_compute_policy_diff() {
        let mut a = HashMap::new();
        a.insert("read_file".into(), "allow".into());
        a.insert("execute_command".into(), "allow".into());
        a.insert("delete_file".into(), "deny".into());

        let mut b = HashMap::new();
        b.insert("read_file".into(), "allow".into());
        b.insert("execute_command".into(), "deny".into());
        b.insert("delete_file".into(), "deny".into());

        let diff = compute_policy_diff("agent-a", "agent-b", &a, &b);
        assert!(diff.both_allow.contains(&"read_file".to_string()));
        assert!(diff.only_a_allows.contains(&"execute_command".to_string()));
        assert!(diff.both_deny.contains(&"delete_file".to_string()));
    }

    #[test]
    fn test_policy_diff_missing_tools() {
        let mut a = HashMap::new();
        a.insert("tool_a".into(), "allow".into());

        let mut b = HashMap::new();
        b.insert("tool_b".into(), "allow".into());

        let diff = compute_policy_diff("x", "y", &a, &b);
        assert!(diff.only_a_allows.contains(&"tool_a".to_string()));
        assert!(diff.only_b_allows.contains(&"tool_b".to_string()));
    }
}
