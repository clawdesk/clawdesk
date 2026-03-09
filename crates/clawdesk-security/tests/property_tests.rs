//! Property-based tests for clawdesk-security.
//!
//! Verifies security-critical invariants using proptest:
//! - Capability enforcement is default-deny
//! - Deny always overrides allow
//! - Pattern matching is consistent

use proptest::prelude::*;

use clawdesk_security::capabilities::*;

// ── Arbitrary generators ────────────────────────────────────────────────

fn arb_policy_set(items: Vec<String>) -> PolicySet {
    let mid = items.len() / 2;
    PolicySet {
        allow: items[..mid].iter().cloned().collect(),
        deny: items[mid..].iter().cloned().collect(),
    }
}

// ── Capability guard properties ─────────────────────────────────────────

proptest! {
    /// Default-deny: any tool not in the allow set is denied.
    #[test]
    fn default_deny_unknown_tools(tool_name in "[a-z_]{1,30}") {
        let policy = CapabilityPolicy {
            agent_id: "test".into(),
            tools: PolicySet {
                allow: std::collections::HashSet::new(),
                deny: std::collections::HashSet::new(),
            },
            ..Default::default()
        };
        let guard = CapabilityGuard::new(policy);
        let result = guard.check(&CapabilityKind::Tool(tool_name));
        prop_assert!(result.is_denied(), "empty allow set should deny all tools");
    }

    /// Deny takes precedence: if a tool is in both allow and deny, it's denied.
    #[test]
    fn deny_overrides_allow(tool_name in "[a-z_]{1,20}") {
        let mut allow = std::collections::HashSet::new();
        allow.insert(tool_name.clone());
        let mut deny = std::collections::HashSet::new();
        deny.insert(tool_name.clone());

        let policy = CapabilityPolicy {
            agent_id: "test".into(),
            tools: PolicySet { allow, deny },
            ..Default::default()
        };
        let guard = CapabilityGuard::new(policy);
        let result = guard.check(&CapabilityKind::Tool(tool_name));
        prop_assert!(result.is_denied(), "deny should override allow");
    }

    /// Wildcard deny blocks everything.
    #[test]
    fn wildcard_deny_blocks_all(tool_name in "[a-z_]{1,30}") {
        let policy = CapabilityPolicy {
            agent_id: "test".into(),
            tools: PolicySet {
                allow: ["*"].iter().map(|s| s.to_string()).collect(),
                deny: ["*"].iter().map(|s| s.to_string()).collect(),
            },
            ..Default::default()
        };
        let guard = CapabilityGuard::new(policy);
        let result = guard.check(&CapabilityKind::Tool(tool_name));
        prop_assert!(result.is_denied(), "wildcard deny should block all tools");
    }

    /// Wildcard allow permits everything (when no deny).
    #[test]
    fn wildcard_allow_permits_all(tool_name in "[a-z_]{1,30}") {
        let policy = CapabilityPolicy {
            agent_id: "test".into(),
            tools: PolicySet {
                allow: ["*"].iter().map(|s| s.to_string()).collect(),
                deny: std::collections::HashSet::new(),
            },
            ..Default::default()
        };
        let guard = CapabilityGuard::new(policy);
        let result = guard.check(&CapabilityKind::Tool(tool_name));
        prop_assert!(result.is_allowed(), "wildcard allow should permit all tools");
    }

    /// Concurrency check: active < max → allowed, active >= max → denied.
    #[test]
    fn concurrency_limit_enforced(
        max in 1usize..100,
        active in 0usize..200,
    ) {
        let policy = CapabilityPolicy {
            agent_id: "test".into(),
            max_concurrent_tools: max,
            ..Default::default()
        };
        let guard = CapabilityGuard::new(policy);
        let result = guard.check_concurrency(active);
        if active < max {
            prop_assert!(result.is_allowed());
        } else {
            prop_assert!(result.is_denied());
        }
    }

    /// Network glob pattern: "*.example.com" matches "sub.example.com"
    /// but not "example.com" itself.
    #[test]
    fn network_glob_prefix(subdomain in "[a-z]{1,10}") {
        let policy = CapabilityPolicy {
            agent_id: "test".into(),
            network: PolicySet {
                allow: ["*.example.com"].iter().map(|s| s.to_string()).collect(),
                deny: std::collections::HashSet::new(),
            },
            ..Default::default()
        };
        let guard = CapabilityGuard::new(policy);
        let host = format!("{subdomain}.example.com");
        let result = guard.check(&CapabilityKind::Network(host));
        prop_assert!(result.is_allowed());
    }

    /// Filesystem glob: "/tmp/**" matches any path under /tmp/.
    #[test]
    fn filesystem_glob_suffix(subpath in "[a-z/]{1,30}") {
        let policy = CapabilityPolicy {
            agent_id: "test".into(),
            filesystem: PolicySet {
                allow: ["/tmp/**"].iter().map(|s| s.to_string()).collect(),
                deny: std::collections::HashSet::new(),
            },
            ..Default::default()
        };
        let guard = CapabilityGuard::new(policy);
        let path = format!("/tmp/{subpath}");
        let result = guard.check(&CapabilityKind::Filesystem(path));
        prop_assert!(result.is_allowed());
    }
}
