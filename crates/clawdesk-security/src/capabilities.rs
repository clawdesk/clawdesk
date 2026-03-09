//! Capability-based security enforcement for ClawDesk agents.
//!
//! Each agent declares its capabilities in its TOML config. The `CapabilityGuard`
//! enforces these declarations at runtime with O(1) per-check cost using `HashSet`
//! membership tests.
//!
//! ## Security Model
//!
//! Access control matrix `A[agent, capability] → {allow, deny}`.
//! Default-deny: if a capability is not explicitly allowed, it is denied.
//!
//! ```text
//! check(agent, cap) =
//!   if cap ∈ deny_set(agent) → Deny
//!   if cap ∈ allow_set(agent) → Allow
//!   else → Deny  (default-deny)
//! ```
//!
//! ## Capability Categories
//!
//! - **Tools**: Named tool access (e.g., "shell", "file_read", "browser")
//! - **Network**: Host-level network access with glob patterns
//! - **Filesystem**: Path-level filesystem access with glob patterns
//! - **Concurrent Tools**: Max parallel tool invocations

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::{debug, warn};

// ─────────────────────────────────────────────────────────────────────────────
// Policy types
// ─────────────────────────────────────────────────────────────────────────────

/// A set of allow/deny rules for a named capability domain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicySet {
    #[serde(default)]
    pub allow: HashSet<String>,
    #[serde(default)]
    pub deny: HashSet<String>,
}

/// Per-agent capability policy loaded from TOML config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityPolicy {
    /// Agent identifier.
    pub agent_id: String,
    /// Tool-level access control.
    #[serde(default)]
    pub tools: PolicySet,
    /// Network host access control (supports glob: "*.openai.com").
    #[serde(default)]
    pub network: PolicySet,
    /// Filesystem path access control (supports glob: "/tmp/**").
    #[serde(default)]
    pub filesystem: PolicySet,
    /// Maximum concurrent tool invocations.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_tools: usize,
}

fn default_max_concurrent() -> usize {
    5
}

impl Default for CapabilityPolicy {
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            tools: PolicySet::default(),
            network: PolicySet::default(),
            filesystem: PolicySet::default(),
            max_concurrent_tools: default_max_concurrent(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Capability guard
// ─────────────────────────────────────────────────────────────────────────────

/// What kind of capability is being checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityKind {
    Tool(String),
    Network(String),
    Filesystem(String),
}

impl std::fmt::Display for CapabilityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tool(name) => write!(f, "tool:{name}"),
            Self::Network(host) => write!(f, "network:{host}"),
            Self::Filesystem(path) => write!(f, "filesystem:{path}"),
        }
    }
}

/// Result of a capability check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityDecision {
    /// Access is allowed.
    Allow,
    /// Access is denied with a reason.
    Deny { reason: String },
}

impl CapabilityDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    pub fn is_denied(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }
}

/// Enforces capability policies for agents.
///
/// Thread-safe: all methods take `&self` and operate on immutable policy data.
/// Each agent gets its own `CapabilityGuard` instance, constructed from its
/// `CapabilityPolicy` at agent boot time.
pub struct CapabilityGuard {
    policy: CapabilityPolicy,
}

impl CapabilityGuard {
    /// Create a new guard from a capability policy.
    pub fn new(policy: CapabilityPolicy) -> Self {
        Self { policy }
    }

    /// Check whether the agent is allowed to exercise the given capability.
    ///
    /// Complexity: O(1) amortized for exact matches, O(n) for glob patterns
    /// where n = number of patterns in the deny/allow set.
    pub fn check(&self, capability: &CapabilityKind) -> CapabilityDecision {
        match capability {
            CapabilityKind::Tool(name) => self.check_tool(name),
            CapabilityKind::Network(host) => self.check_network(host),
            CapabilityKind::Filesystem(path) => self.check_filesystem(path),
        }
    }

    /// Check tool access — exact match in HashSet (O(1)).
    fn check_tool(&self, tool_name: &str) -> CapabilityDecision {
        // Deny takes precedence
        if self.policy.tools.deny.contains(tool_name) || self.policy.tools.deny.contains("*") {
            debug!(agent = %self.policy.agent_id, tool = tool_name, "tool access denied");
            return CapabilityDecision::Deny {
                reason: format!(
                    "agent '{}' is denied tool '{}'",
                    self.policy.agent_id, tool_name
                ),
            };
        }
        // Check allow
        if self.policy.tools.allow.contains(tool_name) || self.policy.tools.allow.contains("*") {
            return CapabilityDecision::Allow;
        }
        // Default deny
        debug!(agent = %self.policy.agent_id, tool = tool_name, "tool access default-denied");
        CapabilityDecision::Deny {
            reason: format!(
                "agent '{}' has no explicit allow for tool '{}'",
                self.policy.agent_id, tool_name
            ),
        }
    }

    /// Check network access — supports glob patterns (e.g., "*.openai.com").
    fn check_network(&self, host: &str) -> CapabilityDecision {
        if self.matches_any(&self.policy.network.deny, host) {
            warn!(agent = %self.policy.agent_id, host = host, "network access denied");
            return CapabilityDecision::Deny {
                reason: format!(
                    "agent '{}' is denied network access to '{}'",
                    self.policy.agent_id, host
                ),
            };
        }
        if self.matches_any(&self.policy.network.allow, host) {
            return CapabilityDecision::Allow;
        }
        CapabilityDecision::Deny {
            reason: format!(
                "agent '{}' has no explicit allow for network host '{}'",
                self.policy.agent_id, host
            ),
        }
    }

    /// Check filesystem access — supports glob patterns (e.g., "/tmp/clawdesk/**").
    fn check_filesystem(&self, path: &str) -> CapabilityDecision {
        if self.matches_any(&self.policy.filesystem.deny, path) {
            warn!(agent = %self.policy.agent_id, path = path, "filesystem access denied");
            return CapabilityDecision::Deny {
                reason: format!(
                    "agent '{}' is denied filesystem access to '{}'",
                    self.policy.agent_id, path
                ),
            };
        }
        if self.matches_any(&self.policy.filesystem.allow, path) {
            return CapabilityDecision::Allow;
        }
        CapabilityDecision::Deny {
            reason: format!(
                "agent '{}' has no explicit allow for path '{}'",
                self.policy.agent_id, path
            ),
        }
    }

    /// Check if the current concurrent tool count is within the limit.
    pub fn check_concurrency(&self, active_tools: usize) -> CapabilityDecision {
        if active_tools >= self.policy.max_concurrent_tools {
            return CapabilityDecision::Deny {
                reason: format!(
                    "agent '{}' at max concurrent tools ({}/{})",
                    self.policy.agent_id, active_tools, self.policy.max_concurrent_tools
                ),
            };
        }
        CapabilityDecision::Allow
    }

    /// Return the underlying policy (read-only).
    pub fn policy(&self) -> &CapabilityPolicy {
        &self.policy
    }

    // ── Pattern matching ────────────────────────────────────────────────

    /// Check if any pattern in the set matches the target.
    ///
    /// Supports:
    /// - Exact match: "api.openai.com"
    /// - Wildcard prefix: "*.openai.com" matches "api.openai.com"
    /// - Wildcard suffix: "/tmp/**" matches "/tmp/clawdesk/data"
    /// - Global wildcard: "*" matches everything
    fn matches_any(&self, patterns: &HashSet<String>, target: &str) -> bool {
        for pattern in patterns {
            if pattern == "*" || pattern == target {
                return true;
            }
            // Prefix wildcard: "*.example.com"
            if let Some(suffix) = pattern.strip_prefix("*.") {
                if target.ends_with(suffix) && target.len() > suffix.len() {
                    return true;
                }
            }
            // Suffix wildcard: "/tmp/**"
            if let Some(prefix) = pattern.strip_suffix("/**") {
                if target.starts_with(prefix) {
                    return true;
                }
            }
        }
        false
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_policy() -> CapabilityPolicy {
        CapabilityPolicy {
            agent_id: "test-agent".into(),
            tools: PolicySet {
                allow: ["shell", "file_read", "browser"].iter().map(|s| s.to_string()).collect(),
                deny: ["file_write"].iter().map(|s| s.to_string()).collect(),
            },
            network: PolicySet {
                allow: ["api.openai.com", "*.anthropic.com"].iter().map(|s| s.to_string()).collect(),
                deny: ["evil.example.com"].iter().map(|s| s.to_string()).collect(),
            },
            filesystem: PolicySet {
                allow: ["/tmp/clawdesk/**"].iter().map(|s| s.to_string()).collect(),
                deny: ["~/.ssh/**"].iter().map(|s| s.to_string()).collect(),
            },
            max_concurrent_tools: 3,
        }
    }

    #[test]
    fn tool_allowed() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard.check(&CapabilityKind::Tool("shell".into())).is_allowed());
        assert!(guard.check(&CapabilityKind::Tool("file_read".into())).is_allowed());
    }

    #[test]
    fn tool_denied_explicit() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard.check(&CapabilityKind::Tool("file_write".into())).is_denied());
    }

    #[test]
    fn tool_denied_default() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard.check(&CapabilityKind::Tool("unknown_tool".into())).is_denied());
    }

    #[test]
    fn network_allowed_exact() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard.check(&CapabilityKind::Network("api.openai.com".into())).is_allowed());
    }

    #[test]
    fn network_allowed_glob() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard.check(&CapabilityKind::Network("api.anthropic.com".into())).is_allowed());
    }

    #[test]
    fn network_denied_explicit() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard.check(&CapabilityKind::Network("evil.example.com".into())).is_denied());
    }

    #[test]
    fn network_denied_default() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard.check(&CapabilityKind::Network("random.example.com".into())).is_denied());
    }

    #[test]
    fn filesystem_allowed_glob() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard
            .check(&CapabilityKind::Filesystem("/tmp/clawdesk/data".into()))
            .is_allowed());
    }

    #[test]
    fn filesystem_denied_glob() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard
            .check(&CapabilityKind::Filesystem("~/.ssh/id_rsa".into()))
            .is_denied());
    }

    #[test]
    fn concurrency_within_limit() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard.check_concurrency(2).is_allowed());
    }

    #[test]
    fn concurrency_at_limit() {
        let guard = CapabilityGuard::new(test_policy());
        assert!(guard.check_concurrency(3).is_denied());
    }

    #[test]
    fn wildcard_allow_all_tools() {
        let mut policy = test_policy();
        policy.tools.allow = ["*"].iter().map(|s| s.to_string()).collect();
        policy.tools.deny.clear();
        let guard = CapabilityGuard::new(policy);
        assert!(guard.check(&CapabilityKind::Tool("anything".into())).is_allowed());
    }

    #[test]
    fn deny_takes_precedence() {
        // Even if "*" is in allow, explicit deny wins
        let mut policy = test_policy();
        policy.tools.allow = ["*"].iter().map(|s| s.to_string()).collect();
        let guard = CapabilityGuard::new(policy);
        assert!(guard.check(&CapabilityKind::Tool("file_write".into())).is_denied());
    }
}
