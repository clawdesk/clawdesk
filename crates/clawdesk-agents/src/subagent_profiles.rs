//! Subagent Permission Profiles — Role-Based Least-Privilege Tool Access.
//!
//! Defines predefined permission profiles for common subagent roles,
//! matching Claude Code's approach where different subagent types get
//! different tool access levels:
//!
//! - **Explorer** — read-only tools (file_read, search, web_search)
//! - **Planner** — read-only + spawn_subagent (can delegate to explorers)
//! - **Executor** — full tool access with approval gate
//! - **Custom** — user-defined in `.clawdesk/agents/*.toml`
//!
//! Uses the existing `PolicyStack` and `ToolPolicy` infrastructure.

use crate::tool_policy::{PolicyLayer, PolicyStack};
use crate::tools::ToolPolicy;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Predefined subagent permission profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentProfile {
    /// Read-only tools: file_read, file_list, web_search, memory_search,
    /// workspace_search, workspace_grep, agents_list.
    /// Model recommendation: fastest/cheapest (e.g., Haiku).
    Explorer,

    /// Same as Explorer + spawn_subagent (can delegate to explorers).
    /// Model recommendation: reasoning-capable (e.g., Sonnet/Opus).
    Planner,

    /// Full tool access with approval gate.
    /// Model recommendation: capable (e.g., Sonnet).
    Executor,

    /// User-defined profile loaded from `.clawdesk/agents/<name>.toml`.
    Custom(String),
}

impl std::fmt::Display for SubagentProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Explorer => write!(f, "explorer"),
            Self::Planner => write!(f, "planner"),
            Self::Executor => write!(f, "executor"),
            Self::Custom(name) => write!(f, "custom:{}", name),
        }
    }
}

/// Recommended model tier for a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Fastest/cheapest (Haiku, GPT-4o-mini).
    Fast,
    /// Balanced (Sonnet, GPT-4o).
    Balanced,
    /// Maximum capability (Opus, o1).
    Capable,
}

/// Resolved profile: tool policy + model recommendation.
#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    /// The profile that was resolved.
    pub profile: SubagentProfile,
    /// Tool policy for this profile.
    pub policy: ToolPolicy,
    /// Recommended model tier.
    pub model_tier: ModelTier,
    /// Human-readable description.
    pub description: String,
}

// ── Read-only tool sets ──────────────────────────────────────

/// Tools that are always safe (read-only, informational).
fn safe_tools() -> HashSet<String> {
    [
        "file_read",
        "file_list",
        "web_search",
        "memory_search",
        "memory_store",
        "agents_list",
        "workspace_search",
        "workspace_grep",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Tools available to Planners (safe + delegation).
fn planner_tools() -> HashSet<String> {
    let mut tools = safe_tools();
    tools.insert("spawn_subagent".to_string());
    tools
}

/// All tool names that require approval in Executor mode.
fn executor_require_approval() -> HashSet<String> {
    [
        "shell_exec",
        "shell",
        "file_write",
        "http",
        "http_fetch",
        "message_send",
        "sessions_send",
        "spawn_subagent",
        "dynamic_spawn",
        "email_send",
        "process_start",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

// ── Profile resolution ───────────────────────────────────────

/// Resolve a profile into a concrete ToolPolicy + model recommendation.
pub fn resolve_profile(profile: &SubagentProfile) -> ResolvedProfile {
    match profile {
        SubagentProfile::Explorer => ResolvedProfile {
            profile: profile.clone(),
            policy: ToolPolicy {
                allowlist: safe_tools(),
                denylist: HashSet::new(),
                require_approval: HashSet::new(),
                granted_capabilities: HashSet::new(),
                max_concurrent: 4,
                tool_timeout_secs: 15,
            },
            model_tier: ModelTier::Fast,
            description: "Read-only explorer: file_read, search, web_search. \
                          Cannot write files, execute commands, or send messages."
                .to_string(),
        },

        SubagentProfile::Planner => ResolvedProfile {
            profile: profile.clone(),
            policy: ToolPolicy {
                allowlist: planner_tools(),
                denylist: HashSet::new(),
                require_approval: {
                    let mut s = HashSet::new();
                    s.insert("spawn_subagent".to_string());
                    s
                },
                granted_capabilities: HashSet::new(),
                max_concurrent: 4,
                tool_timeout_secs: 30,
            },
            model_tier: ModelTier::Capable,
            description: "Planner: read-only + can spawn sub-agents for delegation. \
                          Cannot directly write files or execute commands."
                .to_string(),
        },

        SubagentProfile::Executor => ResolvedProfile {
            profile: profile.clone(),
            policy: ToolPolicy {
                allowlist: HashSet::new(), // Empty = allow all
                denylist: HashSet::new(),
                require_approval: executor_require_approval(),
                granted_capabilities: HashSet::new(),
                max_concurrent: 8,
                tool_timeout_secs: 30,
            },
            model_tier: ModelTier::Balanced,
            description: "Executor: full tool access with approval gate for dangerous tools."
                .to_string(),
        },

        SubagentProfile::Custom(name) => {
            // Try to load from .clawdesk/agents/<name>.toml
            match load_custom_profile(name) {
                Ok(resolved) => resolved,
                Err(e) => {
                    warn!(name = %name, error = %e, "failed to load custom profile, falling back to Explorer");
                    // Fail safe → Explorer (read-only)
                    resolve_profile(&SubagentProfile::Explorer)
                }
            }
        }
    }
}

/// Convert a profile to a PolicyLayer for use in PolicyStack.
pub fn profile_to_policy_layer(profile: &SubagentProfile) -> PolicyLayer {
    let resolved = resolve_profile(profile);
    let mut layer = PolicyLayer::new(format!("subagent-{}", profile));

    if resolved.policy.allowlist.is_empty() {
        layer.allow_all = true;
    } else {
        layer.allow = resolved.policy.allowlist;
    }

    // For Explorer/Planner, deny dangerous tools explicitly
    match profile {
        SubagentProfile::Explorer => {
            layer.deny.insert("shell_exec".to_string());
            layer.deny.insert("shell".to_string());
            layer.deny.insert("file_write".to_string());
            layer.deny.insert("http".to_string());
            layer.deny.insert("http_fetch".to_string());
            layer.deny.insert("message_send".to_string());
            layer.deny.insert("email_send".to_string());
            layer.deny.insert("spawn_subagent".to_string());
            layer.deny.insert("dynamic_spawn".to_string());
            layer.deny.insert("process_start".to_string());
        }
        SubagentProfile::Planner => {
            layer.deny.insert("shell_exec".to_string());
            layer.deny.insert("shell".to_string());
            layer.deny.insert("file_write".to_string());
            layer.deny.insert("http".to_string());
            layer.deny.insert("http_fetch".to_string());
            layer.deny.insert("message_send".to_string());
            layer.deny.insert("email_send".to_string());
            layer.deny.insert("process_start".to_string());
        }
        _ => {}
    }

    layer
}

/// Build a full PolicyStack for a subagent, incorporating the profile layer
/// on top of the parent agent's global policy.
pub fn build_subagent_policy_stack(
    profile: &SubagentProfile,
    parent_global_layer: Option<PolicyLayer>,
) -> PolicyStack {
    let mut stack = PolicyStack::new();

    // Most specific: subagent profile layer
    stack.push(profile_to_policy_layer(profile));

    // Less specific: parent's global policy
    if let Some(global) = parent_global_layer {
        stack.push(global);
    }

    stack
}

// ── Custom profile loading ───────────────────────────────────

/// TOML format for custom agent profiles in `.clawdesk/agents/<name>.toml`.
///
/// ```toml
/// [agent]
/// description = "Code reviewer — read files and suggest changes"
/// model_tier = "balanced"
///
/// [tools]
/// allow = ["file_read", "file_list", "workspace_search", "workspace_grep"]
/// deny = ["shell_exec", "file_write"]
/// require_approval = []
/// max_concurrent = 4
/// timeout_secs = 20
/// ```
#[derive(Debug, Clone, Deserialize)]
struct CustomProfileToml {
    #[serde(default)]
    agent: AgentSection,
    #[serde(default)]
    tools: ToolsSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AgentSection {
    #[serde(default)]
    description: String,
    #[serde(default = "default_model_tier")]
    model_tier: String,
}

fn default_model_tier() -> String {
    "balanced".to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolsSection {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default)]
    require_approval: Vec<String>,
    #[serde(default)]
    allow_all: bool,
    #[serde(default = "default_max_concurrent")]
    max_concurrent: usize,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_max_concurrent() -> usize { 4 }
fn default_timeout() -> u64 { 30 }

/// Load a custom profile from `.clawdesk/agents/<name>.toml`.
fn load_custom_profile(name: &str) -> Result<ResolvedProfile, String> {
    let paths = custom_profile_paths(name);

    for path in &paths {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
            let parsed: CustomProfileToml = toml::from_str(&content)
                .map_err(|e| format!("failed to parse {}: {}", path.display(), e))?;

            let model_tier = match parsed.agent.model_tier.as_str() {
                "fast" => ModelTier::Fast,
                "capable" => ModelTier::Capable,
                _ => ModelTier::Balanced,
            };

            let policy = ToolPolicy {
                allowlist: if parsed.tools.allow_all {
                    HashSet::new() // Empty = allow all
                } else {
                    parsed.tools.allow.into_iter().collect()
                },
                denylist: parsed.tools.deny.into_iter().collect(),
                require_approval: parsed.tools.require_approval.into_iter().collect(),
                granted_capabilities: HashSet::new(),
                max_concurrent: parsed.tools.max_concurrent,
                tool_timeout_secs: parsed.tools.timeout_secs,
            };

            info!(
                name = name,
                path = %path.display(),
                "loaded custom subagent profile"
            );

            return Ok(ResolvedProfile {
                profile: SubagentProfile::Custom(name.to_string()),
                policy,
                model_tier,
                description: parsed.agent.description,
            });
        }
    }

    Err(format!("custom profile '{}' not found in search paths", name))
}

/// Return candidate paths for a custom profile TOML file.
fn custom_profile_paths(name: &str) -> Vec<PathBuf> {
    let filename = format!("{}.toml", name);
    let mut paths = Vec::new();

    // Project-local: .clawdesk/agents/<name>.toml
    paths.push(
        PathBuf::from(".clawdesk")
            .join("agents")
            .join(&filename),
    );

    // Home: ~/.clawdesk/agents/<name>.toml
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        paths.push(
            PathBuf::from(home)
                .join(".clawdesk")
                .join("agents")
                .join(&filename),
        );
    }

    paths
}

/// Compute attack surface metrics for a profile.
pub fn attack_surface(profile: &SubagentProfile) -> AttackSurface {
    let resolved = resolve_profile(profile);
    let all_dangerous = executor_require_approval();
    let allowed_count = if resolved.policy.allowlist.is_empty() {
        20 // approximate total tool count
    } else {
        resolved.policy.allowlist.len()
    };
    let dangerous_allowed = if resolved.policy.allowlist.is_empty() {
        all_dangerous.len() - resolved.policy.denylist.len()
    } else {
        resolved
            .policy
            .allowlist
            .intersection(&all_dangerous)
            .count()
    };

    AttackSurface {
        profile: profile.clone(),
        total_tools_exposed: allowed_count,
        dangerous_tools_exposed: dangerous_allowed,
        reduction_vs_full: if allowed_count < 20 {
            ((20 - allowed_count) as f64 / 20.0) * 100.0
        } else {
            0.0
        },
    }
}

/// Attack surface metrics for audit/reporting.
#[derive(Debug, Clone)]
pub struct AttackSurface {
    pub profile: SubagentProfile,
    pub total_tools_exposed: usize,
    pub dangerous_tools_exposed: usize,
    /// Percentage reduction vs. full access.
    pub reduction_vs_full: f64,
}

impl std::fmt::Display for AttackSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} tools exposed ({} dangerous), {:.0}% reduction vs full",
            self.profile,
            self.total_tools_exposed,
            self.dangerous_tools_exposed,
            self.reduction_vs_full
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_profile() {
        let resolved = resolve_profile(&SubagentProfile::Explorer);
        assert_eq!(resolved.model_tier, ModelTier::Fast);

        // Explorer should allow only safe tools
        assert!(resolved.policy.allowlist.contains("file_read"));
        assert!(resolved.policy.allowlist.contains("web_search"));
        assert!(resolved.policy.allowlist.contains("memory_search"));

        // Explorer should NOT allow dangerous tools
        assert!(!resolved.policy.allowlist.contains("shell_exec"));
        assert!(!resolved.policy.allowlist.contains("file_write"));
        assert!(!resolved.policy.allowlist.contains("http"));

        // No approval required (everything is either allowed or denied)
        assert!(resolved.policy.require_approval.is_empty());

        // is_allowed checks
        assert!(resolved.policy.is_allowed("file_read"));
        assert!(resolved.policy.is_allowed("web_search"));
        assert!(!resolved.policy.is_allowed("shell_exec"));
        assert!(!resolved.policy.is_allowed("file_write"));
    }

    #[test]
    fn planner_profile() {
        let resolved = resolve_profile(&SubagentProfile::Planner);
        assert_eq!(resolved.model_tier, ModelTier::Capable);

        // Planner has safe tools + spawn_subagent
        assert!(resolved.policy.allowlist.contains("file_read"));
        assert!(resolved.policy.allowlist.contains("spawn_subagent"));

        // But NOT shell/write
        assert!(!resolved.policy.allowlist.contains("shell_exec"));
        assert!(!resolved.policy.allowlist.contains("file_write"));

        // spawn_subagent requires approval
        assert!(resolved.policy.requires_approval("spawn_subagent"));
    }

    #[test]
    fn executor_profile() {
        let resolved = resolve_profile(&SubagentProfile::Executor);
        assert_eq!(resolved.model_tier, ModelTier::Balanced);

        // Executor has empty allowlist = allow all
        assert!(resolved.policy.allowlist.is_empty());
        assert!(resolved.policy.is_allowed("shell_exec"));
        assert!(resolved.policy.is_allowed("file_write"));
        assert!(resolved.policy.is_allowed("http"));

        // But dangerous tools require approval
        assert!(resolved.policy.requires_approval("shell_exec"));
        assert!(resolved.policy.requires_approval("file_write"));
        assert!(resolved.policy.requires_approval("email_send"));
    }

    #[test]
    fn explorer_attack_surface() {
        let surface = attack_surface(&SubagentProfile::Explorer);
        assert_eq!(surface.dangerous_tools_exposed, 0);
        assert!(surface.reduction_vs_full > 50.0); // >50% reduction
    }

    #[test]
    fn executor_attack_surface() {
        let surface = attack_surface(&SubagentProfile::Executor);
        assert!(surface.dangerous_tools_exposed > 0);
        assert_eq!(surface.reduction_vs_full, 0.0); // No reduction (full access)
    }

    #[test]
    fn policy_layer_explorer() {
        let layer = profile_to_policy_layer(&SubagentProfile::Explorer);
        assert!(layer.deny.contains("shell_exec"));
        assert!(layer.deny.contains("file_write"));
        assert!(layer.allow.contains("file_read"));
        assert!(layer.allow.contains("web_search"));
    }

    #[test]
    fn policy_stack_subagent() {
        let stack = build_subagent_policy_stack(&SubagentProfile::Explorer, None);
        let (decision, _) = stack.resolve("file_read");
        assert_eq!(decision, crate::tool_policy::ToolDecision::Allow);

        let (decision, _) = stack.resolve("shell_exec");
        assert_eq!(decision, crate::tool_policy::ToolDecision::Deny);
    }

    #[test]
    fn custom_profile_fallback() {
        // Custom profile that doesn't exist should fall back to Explorer
        let resolved = resolve_profile(&SubagentProfile::Custom("nonexistent".into()));
        // Should have Explorer's policy (safe tools only)
        assert!(resolved.policy.allowlist.contains("file_read"));
        assert!(!resolved.policy.allowlist.contains("shell_exec"));
    }

    #[test]
    fn subagent_profile_display() {
        assert_eq!(SubagentProfile::Explorer.to_string(), "explorer");
        assert_eq!(SubagentProfile::Planner.to_string(), "planner");
        assert_eq!(SubagentProfile::Executor.to_string(), "executor");
        assert_eq!(
            SubagentProfile::Custom("reviewer".into()).to_string(),
            "custom:reviewer"
        );
    }
}
