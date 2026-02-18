//! Skill definition types — the algebraic specification of a skill.
//!
//! A skill is a morphism in the category of agent capabilities:
//! `Skill: (Context, Parameters) → (PromptFragment, ToolBindings)`
//!
//! The manifest is the static description; the runtime instantiation
//! produces concrete prompt text and tool definitions.

use clawdesk_types::estimate_tokens;
use serde::{Deserialize, Serialize};

/// Unique skill identifier — namespaced to prevent collisions.
/// Format: `namespace/name` (e.g., `core/web-search`, `community/code-review`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SkillId(pub String);

impl SkillId {
    pub fn new(namespace: &str, name: &str) -> Self {
        Self(format!("{}/{}", namespace, name))
    }

    pub fn namespace(&self) -> &str {
        self.0.split('/').next().unwrap_or("unknown")
    }

    pub fn name(&self) -> &str {
        self.0.split('/').nth(1).unwrap_or(&self.0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SkillId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SkillId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Skill manifest — static definition loaded from disk or registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    /// Unique identifier.
    pub id: SkillId,
    /// Human-readable display name.
    pub display_name: String,
    /// Description shown in skill listings.
    pub description: String,
    /// Version string (semver).
    pub version: String,
    /// Author or organization.
    pub author: Option<String>,
    /// Skills this skill depends on (must be loaded first).
    pub dependencies: Vec<SkillId>,
    /// Tool names this skill requires from the ToolRegistry.
    pub required_tools: Vec<String>,
    /// Parameter schema for runtime configuration.
    pub parameters: Vec<SkillParameter>,
    /// When this skill should be auto-activated.
    pub triggers: Vec<SkillTrigger>,
    /// Estimated token cost of the prompt fragment.
    pub estimated_tokens: usize,
    /// Priority weight for knapsack selection (higher = more valuable).
    pub priority_weight: f64,
    /// Tags for categorization and search.
    pub tags: Vec<String>,

    // ── Cryptographic signing ─────────────────────────────────
    /// Ed25519 signature over the canonical manifest bytes (excluding this field).
    /// Present when the skill is published to a registry; absent for local-only skills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Ed25519 public key of the publisher (hex-encoded, 64 chars).
    /// Used to verify `signature`. Must match a trusted key in the gateway's
    /// `[security.trusted_publishers]` config section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_key: Option<String>,

    // ── Content addressing (T-06) ────────────────────────────
    /// SHA-256 hash of the skill's content (prompt + tool bindings).
    /// Computed at load time for deduplication and integrity verification.
    /// Format: hex-encoded 64-char string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,

    // ── Schema versioning (T-11) ─────────────────────────────
    /// Schema version of this manifest format. Enables two-phase
    /// deserialization for backward compatibility.
    /// Current version: 1
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

/// Default schema version for manifests that don't specify one.
fn default_schema_version() -> u32 {
    1
}

/// Parameter definition for a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillParameter {
    pub name: String,
    pub description: String,
    pub param_type: ParameterType,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
}

/// Parameter type enumeration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterType {
    String,
    Integer,
    Float,
    Boolean,
    Enum { values: Vec<String> },
    Json,
}

/// Trigger condition for auto-activation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillTrigger {
    /// Always active for this agent.
    Always,
    /// Active when a specific command is used.
    Command { command: String },
    /// Active when keywords are detected in the message.
    Keywords { words: Vec<String>, threshold: f64 },
    /// Active for specific channels.
    Channel { channel_ids: Vec<String> },
    /// Active during specific time windows.
    Schedule { cron_expression: String },
    /// Active when explicitly requested by another skill.
    OnDemand,
}

/// A fully instantiated skill ready for agent consumption.
#[derive(Debug, Clone)]
pub struct Skill {
    pub manifest: SkillManifest,
    /// The prompt fragment to inject into the system prompt.
    pub prompt_fragment: String,
    /// Tool definitions this skill provides (beyond required_tools).
    pub provided_tools: Vec<SkillToolBinding>,
    /// Runtime parameter values.
    pub parameter_values: serde_json::Value,
    /// Source location on disk (if loaded from filesystem).
    pub source_path: Option<String>,
}

impl Skill {
    /// Estimated tokens for the fully rendered prompt fragment.
    /// Delegates to the canonical LUT-accelerated estimator in clawdesk-types.
    pub fn token_cost(&self) -> usize {
        estimate_tokens(&self.prompt_fragment)
    }

    /// Value-to-weight ratio for greedy knapsack selection.
    /// Higher ratio = more value per token consumed.
    pub fn value_density(&self) -> f64 {
        let cost = self.token_cost() as f64;
        if cost == 0.0 {
            return 0.0;
        }
        self.manifest.priority_weight / cost
    }
}

/// Tool binding provided by a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillToolBinding {
    pub tool_name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

/// Skill source — where it was loaded from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillSource {
    /// Bundled with the application.
    Builtin,
    /// Loaded from `~/.clawdesk/skills/`.
    Local { path: String },
    /// Downloaded from the skill registry.
    Remote { url: String, checksum: String },
}

/// Skill state in the lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillState {
    /// Discovered on disk but not loaded.
    Discovered,
    /// Manifest loaded and validated.
    Loaded,
    /// Dependencies resolved, ready to activate.
    Resolved,
    /// Active and contributing to agent prompts.
    Active,
    /// Failed to load or activate.
    Failed,
    /// Explicitly disabled by user.
    Disabled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_id_parsing() {
        let id = SkillId::new("core", "web-search");
        assert_eq!(id.namespace(), "core");
        assert_eq!(id.name(), "web-search");
        assert_eq!(id.as_str(), "core/web-search");
    }

    #[test]
    fn token_cost_heuristic() {
        let skill = Skill {
            manifest: SkillManifest {
                id: SkillId::from("test/skill"),
                display_name: "Test".into(),
                description: "A test skill".into(),
                version: "0.1.0".into(),
                author: None,
                dependencies: vec![],
                required_tools: vec![],
                parameters: vec![],
                triggers: vec![SkillTrigger::Always],
                estimated_tokens: 100,
                priority_weight: 1.0,
                tags: vec![],
                signature: None,
                publisher_key: None,
                content_hash: None,
                schema_version: 1,
            },
            prompt_fragment: "You are a helpful assistant that can search the web.".into(),
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: None,
        };
        // ~51 chars, mostly alnum+space → roughly 10-15 tokens
        let cost = skill.token_cost();
        assert!(cost > 5 && cost < 25, "token cost {} out of range", cost);
    }
}
