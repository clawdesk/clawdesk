//! A2A Policy Engine — config-driven allow/deny for inter-agent communication.
//!
//! Provides a layered policy engine that controls:
//! - **Agent delegation**: which agents may delegate tasks to which other agents
//! - **Skill access**: which skills can be invoked across agent boundaries
//! - **Rate limits**: per-agent task quotas (window-based)
//! - **Source restrictions**: which `AgentSource` types are permitted
//!
//! ## Policy evaluation
//!
//! Policies are evaluated as an ordered rule list with first-match semantics:
//!
//! ```text
//! for rule in rules:
//!     if rule.matches(request):
//!         return rule.effect    // Allow or Deny
//! return default_effect         // Deny (closed by default)
//! ```
//!
//! ## Glob matching
//!
//! Agent IDs and skill IDs support glob patterns (`*`, `?`) for concise rules:
//! - `"worker-*"` matches `"worker-1"`, `"worker-code"`, etc.
//! - `"*"` matches everything (wildcard)
//! - `"code-review"` matches exactly `"code-review"`

use crate::session_router::AgentSource;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use rustc_hash::FxHashMap;

// ═══════════════════════════════════════════════════════════════════════════
// Policy types
// ═══════════════════════════════════════════════════════════════════════════

/// The effect of a policy rule: allow or deny the action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyEffect {
    Allow,
    Deny,
}

/// A single policy rule — matches a (requester, target, skill) triple.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Human-readable description of this rule.
    #[serde(default)]
    pub description: String,
    /// Glob pattern for the requester agent ID (who is asking).
    pub requester: String,
    /// Glob pattern for the target agent ID (who is being asked).
    pub target: String,
    /// Optional glob pattern for the skill being invoked.
    /// `None` means "any skill".
    #[serde(default)]
    pub skill: Option<String>,
    /// Which agent source types this rule applies to.
    /// Empty = all sources.
    #[serde(default)]
    pub source_types: Vec<SourceType>,
    /// The effect if this rule matches.
    pub effect: PolicyEffect,
}

/// Serializable agent source type (mirrors `AgentSource` variants without data).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    ClawDesk,
    OpenClaw,
    External,
}

impl SourceType {
    /// Check if an `AgentSource` matches this source type.
    pub fn matches_source(&self, source: &AgentSource) -> bool {
        match (self, source) {
            (SourceType::ClawDesk, AgentSource::ClawDesk) => true,
            (SourceType::OpenClaw, AgentSource::OpenClaw { .. }) => true,
            (SourceType::External, AgentSource::External { .. }) => true,
            _ => false,
        }
    }
}

/// Per-agent rate limit configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Maximum tasks allowed within the time window.
    pub max_tasks: u32,
    /// Time window in seconds.
    pub window_secs: u64,
}

/// A2A Policy configuration — the full policy document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2APolicy {
    /// Ordered list of policy rules (first match wins).
    pub rules: Vec<PolicyRule>,
    /// Default effect when no rules match.
    #[serde(default = "default_deny")]
    pub default_effect: PolicyEffect,
    /// Per-agent rate limits.
    #[serde(default)]
    pub rate_limits: FxHashMap<String, RateLimitConfig>,
    /// Global rate limit (applies to all agents not in `rate_limits`).
    #[serde(default)]
    pub global_rate_limit: Option<RateLimitConfig>,
}

fn default_deny() -> PolicyEffect {
    PolicyEffect::Deny
}

impl Default for A2APolicy {
    fn default() -> Self {
        Self {
            rules: vec![
                // Default: allow ClawDesk-to-ClawDesk delegation
                PolicyRule {
                    description: "Allow ClawDesk agents to delegate to each other".into(),
                    requester: "*".into(),
                    target: "*".into(),
                    skill: None,
                    source_types: vec![SourceType::ClawDesk],
                    effect: PolicyEffect::Allow,
                },
            ],
            default_effect: PolicyEffect::Deny,
            rate_limits: FxHashMap::default(),
            global_rate_limit: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Rate limit tracker
// ═══════════════════════════════════════════════════════════════════════════

/// Sliding-window rate limit tracker.
///
/// Uses `VecDeque` instead of `Vec` for O(1) front eviction.
/// Both `is_allowed()` and `record()` evict expired entries, preventing
/// unbounded growth from denied-only check patterns.
#[derive(Debug)]
struct RateWindow {
    /// Timestamps of recent task submissions (sorted, oldest at front).
    timestamps: std::collections::VecDeque<Instant>,
    /// Maximum allowed in window.
    max_tasks: u32,
    /// Window duration.
    window: Duration,
}

impl RateWindow {
    fn new(max_tasks: u32, window: Duration) -> Self {
        Self {
            timestamps: std::collections::VecDeque::with_capacity(max_tasks as usize),
            max_tasks,
            window,
        }
    }

    /// Evict expired entries from the front of the deque.
    /// O(E) where E = expired entries (amortized O(1) per call).
    fn evict_expired(&mut self, now: Instant) {
        let cutoff = now - self.window;
        while let Some(&front) = self.timestamps.front() {
            if front <= cutoff {
                self.timestamps.pop_front();
            } else {
                break;
            }
        }
    }

    /// Check if a new task is allowed (does not record it).
    ///
    /// Also evicts expired entries so denied-only patterns
    /// don't accumulate stale timestamps.
    fn is_allowed(&mut self, now: Instant) -> bool {
        self.evict_expired(now);
        (self.timestamps.len() as u32) < self.max_tasks
    }

    /// Record a new task submission.
    fn record(&mut self, now: Instant) {
        self.evict_expired(now);
        self.timestamps.push_back(now);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Policy engine
// ═══════════════════════════════════════════════════════════════════════════

/// Result of a policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Request is allowed.
    Allow,
    /// Request is denied, with reason.
    Deny { reason: String },
    /// Request is rate-limited (retry later).
    RateLimited { retry_after_secs: u64 },
}

impl PolicyDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, PolicyDecision::Allow)
    }
}

/// A2A Policy Engine — evaluates delegation requests against the policy.
pub struct PolicyEngine {
    /// The active policy configuration.
    policy: A2APolicy,
    /// Per-agent rate windows.
    rate_windows: FxHashMap<String, RateWindow>,
}

impl PolicyEngine {
    /// Create a new policy engine with the given configuration.
    pub fn new(policy: A2APolicy) -> Self {
        Self {
            policy,
            rate_windows: FxHashMap::default(),
        }
    }

    /// Create a permissive policy engine (allow-all, no rate limits).
    pub fn permissive() -> Self {
        Self::new(A2APolicy {
            rules: vec![PolicyRule {
                description: "Allow all".into(),
                requester: "*".into(),
                target: "*".into(),
                skill: None,
                source_types: vec![],
                effect: PolicyEffect::Allow,
            }],
            default_effect: PolicyEffect::Allow,
            rate_limits: FxHashMap::default(),
            global_rate_limit: None,
        })
    }

    /// Evaluate a delegation request.
    ///
    /// # Arguments
    /// - `requester_id`: The agent requesting delegation
    /// - `target_id`: The target agent
    /// - `skill_id`: Optional skill being invoked
    /// - `source`: The agent source type of the target (if known)
    pub fn evaluate(
        &mut self,
        requester_id: &str,
        target_id: &str,
        skill_id: Option<&str>,
        source: Option<&AgentSource>,
    ) -> PolicyDecision {
        // 1. Check rate limits first
        if let Some(decision) = self.check_rate_limit(requester_id) {
            return decision;
        }

        // 2. Evaluate rules (first match wins)
        for rule in &self.policy.rules {
            if self.rule_matches(rule, requester_id, target_id, skill_id, source) {
                return match rule.effect {
                    PolicyEffect::Allow => {
                        // Record for rate limiting
                        self.record_task(requester_id);
                        PolicyDecision::Allow
                    }
                    PolicyEffect::Deny => PolicyDecision::Deny {
                        reason: if rule.description.is_empty() {
                            format!(
                                "denied by policy: {} → {} (skill: {})",
                                requester_id,
                                target_id,
                                skill_id.unwrap_or("*")
                            )
                        } else {
                            rule.description.clone()
                        },
                    },
                };
            }
        }

        // 3. Default effect
        match self.policy.default_effect {
            PolicyEffect::Allow => {
                self.record_task(requester_id);
                PolicyDecision::Allow
            }
            PolicyEffect::Deny => PolicyDecision::Deny {
                reason: format!(
                    "no matching policy rule for {} → {} (default: deny)",
                    requester_id, target_id
                ),
            },
        }
    }

    /// Check if a rule matches the given request parameters.
    fn rule_matches(
        &self,
        rule: &PolicyRule,
        requester_id: &str,
        target_id: &str,
        skill_id: Option<&str>,
        source: Option<&AgentSource>,
    ) -> bool {
        // Check requester pattern
        if !glob_match(&rule.requester, requester_id) {
            return false;
        }

        // Check target pattern
        if !glob_match(&rule.target, target_id) {
            return false;
        }

        // Check skill pattern (if rule specifies one)
        if let Some(ref rule_skill) = rule.skill {
            match skill_id {
                Some(sid) => {
                    if !glob_match(rule_skill, sid) {
                        return false;
                    }
                }
                None => return false, // Rule requires skill but none specified
            }
        }

        // Check source type (if rule specifies any)
        if !rule.source_types.is_empty() {
            match source {
                Some(src) => {
                    if !rule.source_types.iter().any(|st| st.matches_source(src)) {
                        return false;
                    }
                }
                None => return false, // Rule requires source but none specified
            }
        }

        true
    }

    /// Check rate limit for a requester. Returns `Some(RateLimited)` if exceeded.
    ///
    /// Takes `&mut self` so `is_allowed()` can evict expired entries.
    fn check_rate_limit(&mut self, requester_id: &str) -> Option<PolicyDecision> {
        let now = Instant::now();

        // Check per-agent limit first, then global
        let limit = self
            .policy
            .rate_limits
            .get(requester_id)
            .or(self.policy.global_rate_limit.as_ref());

        if let Some(config) = limit {
            if let Some(window) = self.rate_windows.get_mut(requester_id) {
                if !window.is_allowed(now) {
                    return Some(PolicyDecision::RateLimited {
                        retry_after_secs: config.window_secs,
                    });
                }
            }
        }

        None
    }

    /// Record a task submission for rate limiting.
    fn record_task(&mut self, requester_id: &str) {
        let now = Instant::now();

        // Get or create the rate window
        let limit = self
            .policy
            .rate_limits
            .get(requester_id)
            .or(self.policy.global_rate_limit.as_ref());

        if let Some(config) = limit {
            let window = self
                .rate_windows
                .entry(requester_id.to_string())
                .or_insert_with(|| {
                    RateWindow::new(config.max_tasks, Duration::from_secs(config.window_secs))
                });
            window.record(now);
        }
    }

    /// Get the current policy configuration.
    pub fn policy(&self) -> &A2APolicy {
        &self.policy
    }

    /// Replace the active policy (hot-reload).
    pub fn set_policy(&mut self, policy: A2APolicy) {
        self.policy = policy;
        // Reset rate windows on policy change
        self.rate_windows.clear();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Glob matching
// ═══════════════════════════════════════════════════════════════════════════

/// Simple glob matching for agent/skill IDs.
/// Iterative two-pointer glob match — O(P×T) worst case, O(P+T) typical.
///
/// Supports:
/// - `*` matches any sequence of characters (including empty)
/// - `?` matches exactly one character
/// - All other characters match literally (case-sensitive)
///
/// Uses an iterative NFA approach instead of recursive backtracking,
/// eliminating stack growth and exponential worst-case in patterns
/// like `*a*a*a*a`.
fn glob_match(pattern: &str, text: &str) -> bool {
    // Fast paths
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return pattern == text;
    }

    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();

    let mut pi = 0; // pattern index
    let mut ti = 0; // text index
    let mut star_pi: Option<usize> = None; // position of last `*` in pattern
    let mut star_ti = 0; // text position when we matched last `*`

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            // Record `*` position for backtracking
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1; // try matching `*` against empty first
        } else if let Some(sp) = star_pi {
            // Backtrack: extend last `*` by one more character
            star_ti += 1;
            ti = star_ti;
            pi = sp + 1;
        } else {
            return false;
        }
    }

    // Consume trailing `*`s in pattern
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }

    pi == p.len()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_exact_match() {
        assert!(glob_match("agent-1", "agent-1"));
        assert!(!glob_match("agent-1", "agent-2"));
    }

    #[test]
    fn glob_wildcard() {
        assert!(glob_match("worker-*", "worker-1"));
        assert!(glob_match("worker-*", "worker-code-review"));
        assert!(!glob_match("worker-*", "manager-1"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn glob_question_mark() {
        assert!(glob_match("agent-?", "agent-1"));
        assert!(!glob_match("agent-?", "agent-12"));
    }

    #[test]
    fn glob_combined() {
        assert!(glob_match("*-agent-?", "worker-agent-1"));
        assert!(!glob_match("*-agent-?", "worker-agent-12"));
    }

    #[test]
    fn policy_allow_clawdesk_default() {
        let policy = A2APolicy::default();
        let mut engine = PolicyEngine::new(policy);

        let decision = engine.evaluate(
            "self",
            "worker-1",
            Some("code-review"),
            Some(&AgentSource::ClawDesk),
        );
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn policy_deny_external_by_default() {
        let policy = A2APolicy::default();
        let mut engine = PolicyEngine::new(policy);

        let decision = engine.evaluate(
            "self",
            "external-agent",
            Some("code-review"),
            Some(&AgentSource::External {
                discovery_url: "http://example.com".into(),
            }),
        );
        // Default policy only allows ClawDesk source, so external falls to default deny
        assert!(matches!(decision, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn policy_custom_rules() {
        let policy = A2APolicy {
            rules: vec![
                // Deny specific agent pair
                PolicyRule {
                    description: "Block untrusted agent".into(),
                    requester: "*".into(),
                    target: "untrusted-*".into(),
                    skill: None,
                    source_types: vec![],
                    effect: PolicyEffect::Deny,
                },
                // Allow everything else
                PolicyRule {
                    description: "Allow all".into(),
                    requester: "*".into(),
                    target: "*".into(),
                    skill: None,
                    source_types: vec![],
                    effect: PolicyEffect::Allow,
                },
            ],
            default_effect: PolicyEffect::Deny,
            rate_limits: FxHashMap::default(),
            global_rate_limit: None,
        };

        let mut engine = PolicyEngine::new(policy);

        // Allowed
        assert_eq!(
            engine.evaluate("self", "trusted-worker", None, None),
            PolicyDecision::Allow
        );

        // Denied by first rule
        assert!(matches!(
            engine.evaluate("self", "untrusted-agent", None, None),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn policy_rate_limiting() {
        let mut limits = FxHashMap::default();
        limits.insert(
            "greedy-agent".to_string(),
            RateLimitConfig {
                max_tasks: 2,
                window_secs: 60,
            },
        );

        let policy = A2APolicy {
            rules: vec![PolicyRule {
                description: "Allow all".into(),
                requester: "*".into(),
                target: "*".into(),
                skill: None,
                source_types: vec![],
                effect: PolicyEffect::Allow,
            }],
            default_effect: PolicyEffect::Allow,
            rate_limits: limits,
            global_rate_limit: None,
        };

        let mut engine = PolicyEngine::new(policy);

        // First two should be allowed
        assert_eq!(
            engine.evaluate("greedy-agent", "worker", None, None),
            PolicyDecision::Allow
        );
        assert_eq!(
            engine.evaluate("greedy-agent", "worker", None, None),
            PolicyDecision::Allow
        );

        // Third should be rate-limited
        assert!(matches!(
            engine.evaluate("greedy-agent", "worker", None, None),
            PolicyDecision::RateLimited { .. }
        ));

        // Different agent should still be allowed
        assert_eq!(
            engine.evaluate("other-agent", "worker", None, None),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn policy_skill_filtering() {
        let policy = A2APolicy {
            rules: vec![
                // Only allow code-* skills
                PolicyRule {
                    description: "Only code skills".into(),
                    requester: "*".into(),
                    target: "*".into(),
                    skill: Some("code-*".into()),
                    source_types: vec![],
                    effect: PolicyEffect::Allow,
                },
            ],
            default_effect: PolicyEffect::Deny,
            rate_limits: FxHashMap::default(),
            global_rate_limit: None,
        };

        let mut engine = PolicyEngine::new(policy);

        // Allowed: code-review matches code-*
        assert_eq!(
            engine.evaluate("self", "worker", Some("code-review"), None),
            PolicyDecision::Allow
        );

        // Denied: web-search doesn't match code-*
        assert!(matches!(
            engine.evaluate("self", "worker", Some("web-search"), None),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn policy_permissive() {
        let mut engine = PolicyEngine::permissive();
        assert_eq!(
            engine.evaluate("anyone", "anywhere", Some("anything"), None),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn policy_hot_reload() {
        let mut engine = PolicyEngine::permissive();

        // Initially allows everything
        assert_eq!(
            engine.evaluate("a", "b", None, None),
            PolicyDecision::Allow
        );

        // Hot-reload to deny-all
        engine.set_policy(A2APolicy {
            rules: vec![],
            default_effect: PolicyEffect::Deny,
            rate_limits: FxHashMap::default(),
            global_rate_limit: None,
        });

        assert!(matches!(
            engine.evaluate("a", "b", None, None),
            PolicyDecision::Deny { .. }
        ));
    }
}
