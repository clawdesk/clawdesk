//! Skill Pack — a versioned, content-addressed bundle of composable skills.
//!
//! A Skill Pack is the unit of distribution for agent capabilities. It replaces
//! monolithic agent definitions with composable, dynamically-selected bundles:
//!
//! ```text
//! SkillPack "legal-pro"
//!  ├── persona: 480 tokens (identity, tone, domain framing)
//!  ├── skills:
//!  │   ├── contract-review  w=0.9
//!  │   ├── compliance-check w=0.8
//!  │   └── doc-drafting     w=0.7
//!  ├── pipeline: review→draft→check (DAG template)
//!  ├── tool_policy: {allow: [...], deny: [...]}
//!  └── fallback: [claude→gpt→gemini]
//! ```
//!
//! ## Token Efficiency
//!
//! Monolithic: `T_static = |persona| + Σᵢ |skillᵢ|`  (all skills always loaded)
//! Skill Pack: `T_dynamic = |persona| + Σᵢ xᵢ·|skillᵢ|` where `xᵢ ∈ {0,1}`
//!
//! The knapsack selector solves per-turn:
//!     max  Σ wᵢ · relevance(skillᵢ, context) · xᵢ
//!     s.t. Σ |skillᵢ| · xᵢ ≤ B − |persona|

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::definition::SkillId;
use crate::federated_registry::ContentAddress;
use crate::verification::TrustLevel;

// ─── Pack Identity ───────────────────────────────────────────────────────────

/// Unique pack identifier — namespaced like skills.
/// Format: `tier/name` (e.g., `engineering/coder`, `business/analyst`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackId(pub String);

impl PackId {
    pub fn new(tier: &str, name: &str) -> Self {
        Self(format!("{}/{}", tier, name))
    }

    pub fn tier(&self) -> &str {
        self.0.split('/').next().unwrap_or("unknown")
    }

    pub fn name(&self) -> &str {
        self.0.split('/').nth(1).unwrap_or(&self.0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for PackId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

// ─── Skill Weight Entry ──────────────────────────────────────────────────────

/// A skill reference with a priority weight for the knapsack selector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillWeight {
    /// Skill to include in this pack.
    pub skill_id: SkillId,
    /// Priority weight ∈ (0, 1]. Higher = more valuable = selected first.
    pub weight: f64,
    /// If true, always include (bypasses knapsack gating).
    #[serde(default)]
    pub required: bool,
}

impl SkillWeight {
    /// Create a new skill weight entry.
    pub fn new(skill_id: &str, weight: f64, required: bool) -> Self {
        Self {
            skill_id: SkillId::from(skill_id),
            weight,
            required,
        }
    }

    /// Create a required skill weight (weight=1.0, required=true).
    pub fn required(skill_id: &str) -> Self {
        Self::new(skill_id, 1.0, true)
    }
}

// ─── Tool Policy ─────────────────────────────────────────────────────────────

/// Tool access policy for a pack — what tools are allowed/denied/required.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackToolPolicy {
    /// Tools explicitly allowed (empty = all allowed).
    #[serde(default)]
    pub allow: Vec<String>,
    /// Tools explicitly denied (takes precedence over allow).
    #[serde(default)]
    pub deny: Vec<String>,
    /// Tools that must be registered for the pack to activate.
    #[serde(default)]
    pub require: Vec<String>,
}

impl PackToolPolicy {
    /// Check if a tool is permitted by this policy.
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        if self.deny.iter().any(|d| d == tool_name) {
            return false;
        }
        if self.allow.is_empty() {
            return true;
        }
        self.allow.iter().any(|a| a == tool_name)
    }
}

// ─── Eligibility Predicates ──────────────────────────────────────────────────

/// Conditions that must hold for a pack to be eligible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackEligibility {
    /// Only activate on these channels (empty = all channels).
    #[serde(default)]
    pub channels: Vec<String>,
    /// Minimum context window tokens required.
    #[serde(default)]
    pub min_context_tokens: Option<usize>,
    /// Required capabilities from the CapabilityId taxonomy.
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    /// OS constraints (empty = all platforms).
    #[serde(default)]
    pub platforms: Vec<String>,
}

// ─── Pack Tier ───────────────────────────────────────────────────────────────

/// Life OS category tiers for organizing skill packs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackTier {
    /// email-triage, meeting-actions, task-planner, doc-writer, knowledge-base
    Productivity,
    /// coder, code-reviewer, debugger, architect, test-engineer, devops, security-auditor
    Engineering,
    /// analyst, sales-assistant, customer-support, recruiter, social-media, personal-finance
    Business,
    /// legal-assistant, medical-advisor, translator, researcher
    Professional,
    /// health-tracker, travel-planner, home-automation, tutor, creative-writer
    Life,
    /// orchestrator, advisory-council
    Meta,
}

impl PackTier {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Productivity => "productivity",
            Self::Engineering => "engineering",
            Self::Business => "business",
            Self::Professional => "professional",
            Self::Life => "life",
            Self::Meta => "meta",
        }
    }
}

impl std::fmt::Display for PackTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Skill Pack Definition ───────────────────────────────────────────────────

/// A Skill Pack — the composable unit of agent capability distribution.
///
/// Unlike monolithic agent definitions (one static prompt), a Skill Pack declares
/// a persona + a weighted set of skills that are dynamically selected per-turn
/// by the knapsack selector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillPack {
    /// Unique pack identifier.
    pub id: PackId,
    /// Human-readable display name.
    pub display_name: String,
    /// Short description for listings.
    pub description: String,
    /// Semver version string.
    pub version: String,
    /// Life OS tier categorization.
    pub tier: PackTier,

    // ── Persona ──────────────────────────────────────────────
    /// Base persona prompt (≤ 500 tokens). Identity, tone, domain framing.
    pub persona_prompt: String,
    /// Estimated token cost of the persona prompt.
    #[serde(default)]
    pub persona_tokens: usize,

    // ── Skills ───────────────────────────────────────────────
    /// Ordered list of skill IDs with priority weights for the selector.
    pub skills: Vec<SkillWeight>,

    // ── Pipeline ─────────────────────────────────────────────
    /// Optional default DAG pipeline template ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_template: Option<String>,

    // ── Tool Policy ──────────────────────────────────────────
    /// Tool access control for this pack.
    #[serde(default)]
    pub tool_policy: PackToolPolicy,

    // ── Provider Preferences ─────────────────────────────────
    /// Fallback chain of provider short names (e.g., ["claude", "gpt", "gemini"]).
    #[serde(default)]
    pub fallback_providers: Vec<String>,

    // ── Eligibility ──────────────────────────────────────────
    /// Conditions for pack activation.
    #[serde(default)]
    pub eligibility: PackEligibility,

    // ── Trait Composition ────────────────────────────────────
    /// Trait IDs to compose into the persona (see trait_system).
    #[serde(default)]
    pub traits: Vec<String>,

    // ── Metadata ─────────────────────────────────────────────
    /// Tags for search and discovery.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Author or organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// SHA-256 content address (computed at load time).
    #[serde(skip)]
    pub content_address: Option<ContentAddress>,
    /// Trust level from verification.
    #[serde(skip)]
    pub trust_level: Option<TrustLevel>,
    /// Custom metadata.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

impl SkillPack {
    /// Estimated total token cost if all skills were loaded.
    pub fn max_token_cost(&self, skill_tokens: &HashMap<SkillId, usize>) -> usize {
        let skill_total: usize = self
            .skills
            .iter()
            .filter_map(|sw| skill_tokens.get(&sw.skill_id))
            .sum();
        self.persona_tokens + skill_total
    }

    /// Dynamic token cost with only selected skills.
    pub fn dynamic_token_cost(
        &self,
        active_skills: &[SkillId],
        skill_tokens: &HashMap<SkillId, usize>,
    ) -> usize {
        let skill_total: usize = active_skills
            .iter()
            .filter_map(|id| skill_tokens.get(id))
            .sum();
        self.persona_tokens + skill_total
    }

    /// Token savings ratio: `1 − dynamic/static`.
    pub fn token_savings(
        &self,
        active_skills: &[SkillId],
        skill_tokens: &HashMap<SkillId, usize>,
    ) -> f64 {
        let max = self.max_token_cost(skill_tokens) as f64;
        if max == 0.0 {
            return 0.0;
        }
        let dynamic = self.dynamic_token_cost(active_skills, skill_tokens) as f64;
        1.0 - (dynamic / max)
    }

    /// Check eligibility against runtime context.
    pub fn is_eligible(&self, channel: Option<&str>, context_tokens: usize) -> bool {
        if let Some(ch) = channel {
            if !self.eligibility.channels.is_empty()
                && !self.eligibility.channels.iter().any(|c| c == ch)
            {
                return false;
            }
        }
        if let Some(min) = self.eligibility.min_context_tokens {
            if context_tokens < min {
                return false;
            }
        }
        true
    }

    /// Get skill weights as a map for the knapsack selector.
    pub fn skill_weights(&self) -> HashMap<SkillId, f64> {
        self.skills
            .iter()
            .map(|sw| (sw.skill_id.clone(), sw.weight))
            .collect()
    }

    /// Required skills (always included regardless of budget).
    pub fn required_skills(&self) -> Vec<SkillId> {
        self.skills
            .iter()
            .filter(|sw| sw.required)
            .map(|sw| sw.skill_id.clone())
            .collect()
    }
}

// ─── Pack Registry ───────────────────────────────────────────────────────────

/// In-memory registry of loaded Skill Packs.
pub struct PackRegistry {
    packs: HashMap<PackId, SkillPack>,
    by_tier: HashMap<PackTier, Vec<PackId>>,
}

impl PackRegistry {
    pub fn new() -> Self {
        Self {
            packs: HashMap::new(),
            by_tier: HashMap::new(),
        }
    }

    /// Register a skill pack.
    pub fn register(&mut self, pack: SkillPack) {
        let tier = pack.tier;
        let id = pack.id.clone();
        self.packs.insert(id.clone(), pack);
        self.by_tier.entry(tier).or_default().push(id);
    }

    /// Look up a pack by ID.
    pub fn get(&self, id: &PackId) -> Option<&SkillPack> {
        self.packs.get(id)
    }

    /// Look up a pack by string ID (e.g., "engineering/coder").
    pub fn get_by_str(&self, id: &str) -> Option<&SkillPack> {
        self.packs.get(&PackId(id.to_string()))
    }

    /// All packs in a tier.
    pub fn by_tier(&self, tier: PackTier) -> Vec<&SkillPack> {
        self.by_tier
            .get(&tier)
            .map(|ids| ids.iter().filter_map(|id| self.packs.get(id)).collect())
            .unwrap_or_default()
    }

    /// All registered packs as a Vec.
    pub fn all_packs(&self) -> Vec<&SkillPack> {
        self.packs.values().collect()
    }

    /// All registered packs as an iterator.
    pub fn all(&self) -> impl Iterator<Item = &SkillPack> {
        self.packs.values()
    }

    /// Total number of packs.
    pub fn len(&self) -> usize {
        self.packs.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.packs.is_empty()
    }

    /// All pack IDs.
    pub fn pack_ids(&self) -> Vec<&PackId> {
        self.packs.keys().collect()
    }
}

impl Default for PackRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── TOML Parsing ────────────────────────────────────────────────────────────

/// Parse a Skill Pack from TOML content.
pub fn parse_pack_toml(content: &str) -> Result<SkillPack, toml::de::Error> {
    toml::from_str(content)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pack() -> SkillPack {
        SkillPack {
            id: PackId::new("engineering", "coder"),
            display_name: "Coder".into(),
            description: "Full-stack software engineering assistant".into(),
            version: "1.0.0".into(),
            tier: PackTier::Engineering,
            persona_prompt: "You are a senior software engineer.".into(),
            persona_tokens: 12,
            skills: vec![
                SkillWeight {
                    skill_id: SkillId::from("core/code-analysis"),
                    weight: 0.9,
                    required: true,
                },
                SkillWeight {
                    skill_id: SkillId::from("core/web-search"),
                    weight: 0.6,
                    required: false,
                },
                SkillWeight {
                    skill_id: SkillId::from("core/file-operations"),
                    weight: 0.7,
                    required: false,
                },
            ],
            pipeline_template: None,
            tool_policy: PackToolPolicy::default(),
            fallback_providers: vec!["claude".into(), "gpt".into()],
            eligibility: PackEligibility::default(),
            traits: vec!["concise".into(), "code-first".into()],
            tags: vec!["coding".into(), "engineering".into()],
            author: Some("ClawDesk".into()),
            content_address: None,
            trust_level: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn test_pack_id() {
        let id = PackId::new("engineering", "coder");
        assert_eq!(id.tier(), "engineering");
        assert_eq!(id.name(), "coder");
        assert_eq!(id.as_str(), "engineering/coder");
    }

    #[test]
    fn test_token_savings() {
        let pack = test_pack();
        let mut skill_tokens = HashMap::new();
        skill_tokens.insert(SkillId::from("core/code-analysis"), 400);
        skill_tokens.insert(SkillId::from("core/web-search"), 300);
        skill_tokens.insert(SkillId::from("core/file-operations"), 350);

        let active = vec![SkillId::from("core/code-analysis")];
        let savings = pack.token_savings(&active, &skill_tokens);
        // static = 12 + 400 + 300 + 350 = 1062
        // dynamic = 12 + 400 = 412
        // savings = 1 - 412/1062 ≈ 0.612
        assert!(savings > 0.5);
    }

    #[test]
    fn test_tool_policy() {
        let policy = PackToolPolicy {
            allow: vec!["shell_exec".into(), "web_search".into()],
            deny: vec!["dangerous_tool".into()],
            require: vec![],
        };
        assert!(policy.is_allowed("shell_exec"));
        assert!(!policy.is_allowed("dangerous_tool"));
        assert!(!policy.is_allowed("unknown_tool"));
    }

    #[test]
    fn test_eligibility() {
        let pack = SkillPack {
            eligibility: PackEligibility {
                channels: vec!["discord".into()],
                min_context_tokens: Some(4096),
                ..Default::default()
            },
            ..test_pack()
        };
        assert!(pack.is_eligible(Some("discord"), 8192));
        assert!(!pack.is_eligible(Some("slack"), 8192));
        assert!(!pack.is_eligible(Some("discord"), 2048));
    }

    #[test]
    fn test_registry() {
        let mut reg = PackRegistry::new();
        reg.register(test_pack());
        assert_eq!(reg.len(), 1);
        assert!(reg.get(&PackId::new("engineering", "coder")).is_some());
        assert_eq!(reg.by_tier(PackTier::Engineering).len(), 1);
        assert_eq!(reg.by_tier(PackTier::Business).len(), 0);
    }

    #[test]
    fn test_toml_roundtrip() {
        let pack = test_pack();
        let toml_str = toml::to_string_pretty(&pack).unwrap();
        let parsed = parse_pack_toml(&toml_str).unwrap();
        assert_eq!(parsed.id.as_str(), pack.id.as_str());
        assert_eq!(parsed.skills.len(), 3);
        assert_eq!(parsed.tier, PackTier::Engineering);
    }
}
