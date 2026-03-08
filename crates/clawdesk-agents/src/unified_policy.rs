//! # Unified Policy — Compositional policy lattice across execution boundaries.
//!
//! Ensures policy composes as a single scenario-level contract across local
//! execution, remote A2A delegation, browser actions, and post-processing.
//! Resolution cost is O(depth) in the policy stack — trivial compared with
//! model/tool execution latency.
//!
//! ## Design
//!
//! Policy is modeled as a lattice where more restrictive policies dominate.
//! When policies from different boundaries (local, browser, A2A) compose,
//! the result is the meet (greatest lower bound) — ensuring no boundary
//! can weaken another's restrictions.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A policy boundary — defines the execution context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyBoundary {
    /// Local agent execution (same process).
    Local,
    /// Browser automation context.
    Browser,
    /// Remote A2A delegation.
    RemoteAgent,
    /// Pipeline step execution.
    Pipeline,
    /// Cron/scheduled execution.
    Scheduled,
}

/// A capability permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Allow,
    Deny,
    RequireApproval,
}

impl Permission {
    /// Meet operation on the permission lattice.
    /// More restrictive wins: Deny > RequireApproval > Allow.
    pub fn meet(self, other: Self) -> Self {
        match (self, other) {
            (Self::Deny, _) | (_, Self::Deny) => Self::Deny,
            (Self::RequireApproval, _) | (_, Self::RequireApproval) => Self::RequireApproval,
            _ => Self::Allow,
        }
    }
}

/// A policy layer — one level in the policy stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedPolicyLayer {
    /// Name of this layer (for audit logging).
    pub name: String,
    /// Which boundary this layer applies to.
    pub boundary: PolicyBoundary,
    /// Tools that are explicitly allowed.
    pub allowed_tools: HashSet<String>,
    /// Tools that are explicitly denied.
    pub denied_tools: HashSet<String>,
    /// Tools that require human approval.
    pub approval_required: HashSet<String>,
    /// Maximum concurrent tool executions.
    pub max_concurrent: usize,
    /// Maximum execution timeout in seconds.
    pub timeout_secs: u64,
    /// Whether this layer allows sub-agent spawning.
    pub allow_spawn: bool,
    /// Maximum spawn depth.
    pub max_spawn_depth: u32,
}

impl Default for UnifiedPolicyLayer {
    fn default() -> Self {
        Self {
            name: "default".into(),
            boundary: PolicyBoundary::Local,
            allowed_tools: HashSet::new(),
            denied_tools: HashSet::new(),
            approval_required: HashSet::new(),
            max_concurrent: 8,
            timeout_secs: 30,
            allow_spawn: true,
            max_spawn_depth: 3,
        }
    }
}

/// A composed policy stack — multiple layers resolved via lattice meet.
#[derive(Debug, Clone)]
pub struct UnifiedPolicyStack {
    layers: Vec<UnifiedPolicyLayer>,
}

impl UnifiedPolicyStack {
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    /// Push a new layer onto the stack.
    pub fn push(&mut self, layer: UnifiedPolicyLayer) {
        self.layers.push(layer);
    }

    /// Resolve a tool permission across all layers.
    /// Uses lattice meet: most restrictive wins.
    pub fn resolve_tool(&self, tool_name: &str) -> Permission {
        let mut result = Permission::Allow;

        for layer in &self.layers {
            // Check denied first (most restrictive)
            if layer.denied_tools.contains(tool_name) {
                result = result.meet(Permission::Deny);
                continue;
            }

            // Check approval required
            if layer.approval_required.contains(tool_name) {
                result = result.meet(Permission::RequireApproval);
                continue;
            }

            // If allowed_tools is non-empty, tool must be in the set
            if !layer.allowed_tools.is_empty() && !layer.allowed_tools.contains(tool_name) {
                result = result.meet(Permission::Deny);
            }
        }

        result
    }

    /// Resolve the effective maximum concurrency (minimum across layers).
    pub fn effective_max_concurrent(&self) -> usize {
        self.layers
            .iter()
            .map(|l| l.max_concurrent)
            .min()
            .unwrap_or(8)
    }

    /// Resolve the effective timeout (minimum across layers).
    pub fn effective_timeout_secs(&self) -> u64 {
        self.layers
            .iter()
            .map(|l| l.timeout_secs)
            .min()
            .unwrap_or(30)
    }

    /// Whether spawning is allowed (AND across layers).
    pub fn can_spawn(&self) -> bool {
        self.layers.iter().all(|l| l.allow_spawn)
    }

    /// Effective max spawn depth (minimum across layers).
    pub fn effective_max_spawn_depth(&self) -> u32 {
        self.layers
            .iter()
            .map(|l| l.max_spawn_depth)
            .min()
            .unwrap_or(3)
    }

    /// Generate a human-readable policy summary for audit logging.
    pub fn summary(&self) -> PolicySummary {
        let boundaries: HashSet<PolicyBoundary> =
            self.layers.iter().map(|l| l.boundary).collect();

        let all_denied: HashSet<String> = self
            .layers
            .iter()
            .flat_map(|l| l.denied_tools.iter().cloned())
            .collect();

        let all_approval: HashSet<String> = self
            .layers
            .iter()
            .flat_map(|l| l.approval_required.iter().cloned())
            .collect();

        PolicySummary {
            layer_count: self.layers.len(),
            boundaries: boundaries.into_iter().collect(),
            denied_tool_count: all_denied.len(),
            approval_required_count: all_approval.len(),
            effective_max_concurrent: self.effective_max_concurrent(),
            effective_timeout_secs: self.effective_timeout_secs(),
            can_spawn: self.can_spawn(),
        }
    }
}

impl Default for UnifiedPolicyStack {
    fn default() -> Self {
        Self::new()
    }
}

/// Human-readable policy summary for audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicySummary {
    pub layer_count: usize,
    pub boundaries: Vec<PolicyBoundary>,
    pub denied_tool_count: usize,
    pub approval_required_count: usize,
    pub effective_max_concurrent: usize,
    pub effective_timeout_secs: u64,
    pub can_spawn: bool,
}

/// Build a cross-boundary policy for a complete scenario.
///
/// Composes policies from Explorer/Planner/Executor profiles with
/// boundary-specific restrictions for browser, A2A, and pipeline contexts.
pub fn build_scenario_policy(
    base_profile: &str,
    boundaries: &[PolicyBoundary],
) -> UnifiedPolicyStack {
    let mut stack = UnifiedPolicyStack::new();

    // Base layer from agent profile
    let base_layer = match base_profile {
        "explorer" => UnifiedPolicyLayer {
            name: "explorer_base".into(),
            boundary: PolicyBoundary::Local,
            allowed_tools: [
                "file_read", "file_list", "web_search", "memory_search",
                "memory_store", "agents_list", "workspace_search", "workspace_grep",
            ].iter().map(|s| s.to_string()).collect(),
            max_concurrent: 4,
            timeout_secs: 15,
            allow_spawn: false,
            ..Default::default()
        },
        "planner" => UnifiedPolicyLayer {
            name: "planner_base".into(),
            boundary: PolicyBoundary::Local,
            allowed_tools: [
                "file_read", "file_list", "web_search", "memory_search",
                "memory_store", "agents_list", "workspace_search", "workspace_grep",
                "spawn_subagent",
            ].iter().map(|s| s.to_string()).collect(),
            approval_required: ["spawn_subagent"].iter().map(|s| s.to_string()).collect(),
            max_concurrent: 6,
            timeout_secs: 30,
            allow_spawn: true,
            max_spawn_depth: 2,
            ..Default::default()
        },
        "executor" | _ => UnifiedPolicyLayer {
            name: "executor_base".into(),
            boundary: PolicyBoundary::Local,
            approval_required: [
                "shell_exec", "shell", "file_write", "http", "http_fetch",
                "message_send", "sessions_send", "spawn_subagent", "dynamic_spawn",
                "email_send", "process_start",
            ].iter().map(|s| s.to_string()).collect(),
            max_concurrent: 8,
            timeout_secs: 30,
            allow_spawn: true,
            max_spawn_depth: 3,
            ..Default::default()
        },
    };

    stack.push(base_layer);

    // Add boundary-specific restriction layers
    for boundary in boundaries {
        match boundary {
            PolicyBoundary::Browser => {
                stack.push(UnifiedPolicyLayer {
                    name: "browser_restriction".into(),
                    boundary: PolicyBoundary::Browser,
                    denied_tools: [
                        "shell_exec", "shell", "file_write", "process_start",
                        "email_send", "spawn_subagent",
                    ].iter().map(|s| s.to_string()).collect(),
                    max_concurrent: 2,
                    timeout_secs: 60,
                    allow_spawn: false,
                    ..Default::default()
                });
            }
            PolicyBoundary::RemoteAgent => {
                stack.push(UnifiedPolicyLayer {
                    name: "remote_agent_restriction".into(),
                    boundary: PolicyBoundary::RemoteAgent,
                    denied_tools: [
                        "file_write", "shell_exec", "shell", "process_start",
                    ].iter().map(|s| s.to_string()).collect(),
                    approval_required: [
                        "http", "http_fetch", "message_send",
                    ].iter().map(|s| s.to_string()).collect(),
                    max_concurrent: 4,
                    timeout_secs: 120,
                    allow_spawn: true,
                    max_spawn_depth: 1,
                    ..Default::default()
                });
            }
            _ => {}
        }
    }

    stack
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_lattice_meet() {
        assert_eq!(Permission::Allow.meet(Permission::Allow), Permission::Allow);
        assert_eq!(Permission::Allow.meet(Permission::Deny), Permission::Deny);
        assert_eq!(
            Permission::Allow.meet(Permission::RequireApproval),
            Permission::RequireApproval
        );
        assert_eq!(
            Permission::RequireApproval.meet(Permission::Deny),
            Permission::Deny
        );
    }

    #[test]
    fn test_resolve_tool_deny_wins() {
        let mut stack = UnifiedPolicyStack::new();
        stack.push(UnifiedPolicyLayer {
            name: "layer1".into(),
            // shell_exec allowed at this layer
            ..Default::default()
        });
        stack.push(UnifiedPolicyLayer {
            name: "layer2".into(),
            denied_tools: ["shell_exec"].iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        });

        assert_eq!(stack.resolve_tool("shell_exec"), Permission::Deny);
    }

    #[test]
    fn test_resolve_tool_approval_required() {
        let mut stack = UnifiedPolicyStack::new();
        stack.push(UnifiedPolicyLayer {
            name: "base".into(),
            approval_required: ["file_write"].iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        });

        assert_eq!(stack.resolve_tool("file_write"), Permission::RequireApproval);
        assert_eq!(stack.resolve_tool("file_read"), Permission::Allow);
    }

    #[test]
    fn test_effective_concurrency() {
        let mut stack = UnifiedPolicyStack::new();
        stack.push(UnifiedPolicyLayer {
            name: "a".into(),
            max_concurrent: 8,
            ..Default::default()
        });
        stack.push(UnifiedPolicyLayer {
            name: "b".into(),
            max_concurrent: 2,
            ..Default::default()
        });

        assert_eq!(stack.effective_max_concurrent(), 2);
    }

    #[test]
    fn test_can_spawn_all_must_agree() {
        let mut stack = UnifiedPolicyStack::new();
        stack.push(UnifiedPolicyLayer {
            name: "a".into(),
            allow_spawn: true,
            ..Default::default()
        });
        stack.push(UnifiedPolicyLayer {
            name: "b".into(),
            allow_spawn: false,
            ..Default::default()
        });

        assert!(!stack.can_spawn());
    }

    #[test]
    fn test_build_scenario_explorer_browser() {
        let stack = build_scenario_policy(
            "explorer",
            &[PolicyBoundary::Browser],
        );

        // Explorer + Browser: file_write denied by explorer (not in allowed),
        // shell_exec denied by browser restriction
        assert_eq!(stack.resolve_tool("shell_exec"), Permission::Deny);
        assert_eq!(stack.resolve_tool("file_read"), Permission::Allow);
        assert!(!stack.can_spawn()); // browser disallows spawn
    }

    #[test]
    fn test_build_scenario_executor_remote() {
        let stack = build_scenario_policy(
            "executor",
            &[PolicyBoundary::RemoteAgent],
        );

        // Executor: shell_exec requires approval
        // Remote: shell_exec denied → Deny wins
        assert_eq!(stack.resolve_tool("shell_exec"), Permission::Deny);

        // http: executor requires approval, remote requires approval → RequireApproval
        assert_eq!(stack.resolve_tool("http"), Permission::RequireApproval);
    }

    #[test]
    fn test_policy_summary() {
        let stack = build_scenario_policy("explorer", &[PolicyBoundary::Browser]);
        let summary = stack.summary();
        assert_eq!(summary.layer_count, 2);
        assert!(!summary.can_spawn);
        assert!(summary.boundaries.contains(&PolicyBoundary::Browser));
    }
}
