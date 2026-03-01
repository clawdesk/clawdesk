//! Archetypes — high-level agent templates assembled from Skill Packs + Traits.
//!
//! An archetype is the user-facing concept of "an agent type" — like "Coder",
//! "Legal Assistant", or "Recruiter". Internally, each archetype is resolved
//! into a concrete `SkillPack` + composed `TraitSet` that feeds the existing
//! `PromptAssembler` and `SkillOrchestrator` pipelines.
//!
//! ## Combinatorial Power
//!
//! Given |persona|=p, |method|=m, |domain|=d, |output|=o trait categories:
//!   Valid archetypes ≤ p × m × d × o  (minus conflicts)
//!   For p=5, m=4, d=10, o=3: up to 600 archetypes from 22 trait definitions.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::pack::{PackId, PackRegistry, SkillPack};

/// An archetype — a named agent template that references a Skill Pack
/// and optionally overrides traits, pipeline, or provider preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Archetype {
    /// Unique archetype name (e.g., "coder", "legal-assistant").
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Short description.
    pub description: String,
    /// The base Skill Pack to use.
    pub pack_id: PackId,
    /// Additional traits to compose on top of the pack's traits.
    #[serde(default)]
    pub extra_traits: Vec<String>,
    /// Override the pack's fallback providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_override: Option<Vec<String>>,
    /// Override the pack's pipeline template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_override: Option<String>,
    /// Custom metadata (icon, color, etc.).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

/// Resolved archetype — all traits composed, pack loaded, ready for execution.
#[derive(Debug, Clone)]
pub struct ResolvedArchetype {
    /// Original archetype definition.
    pub archetype: Archetype,
    /// The resolved skill pack.
    pub pack: SkillPack,
    /// All trait IDs (pack traits + extra traits), deduplicated.
    pub composed_traits: Vec<String>,
    /// The final persona prompt (after trait composition).
    pub final_persona: String,
    /// Estimated persona token cost.
    pub persona_tokens: usize,
}

/// Resolve an archetype against a pack registry.
///
/// Returns `None` if the referenced pack is not found.
pub fn resolve_archetype(
    archetype: &Archetype,
    registry: &PackRegistry,
) -> Option<ResolvedArchetype> {
    let pack = registry.get(&archetype.pack_id)?.clone();

    // Merge traits: pack base + archetype extras, deduplicated
    let mut traits: Vec<String> = pack.traits.clone();
    for t in &archetype.extra_traits {
        if !traits.contains(t) {
            traits.push(t.clone());
        }
    }

    // Apply provider override if specified
    let mut pack = pack;
    if let Some(ref providers) = archetype.provider_override {
        pack.fallback_providers = providers.clone();
    }
    if let Some(ref pipeline) = archetype.pipeline_override {
        pack.pipeline_template = Some(pipeline.clone());
    }

    let persona_tokens = pack.persona_tokens;
    let final_persona = pack.persona_prompt.clone();

    Some(ResolvedArchetype {
        archetype: archetype.clone(),
        pack,
        composed_traits: traits,
        final_persona,
        persona_tokens,
    })
}

/// Registry of archetypes. Wraps a `PackRegistry` for pack resolution.
pub struct ArchetypeRegistry {
    archetypes: HashMap<String, Archetype>,
}

impl ArchetypeRegistry {
    pub fn new() -> Self {
        Self {
            archetypes: HashMap::new(),
        }
    }

    /// Register an archetype.
    pub fn register(&mut self, archetype: Archetype) {
        self.archetypes
            .insert(archetype.name.clone(), archetype);
    }

    /// Get an archetype by name.
    pub fn get(&self, name: &str) -> Option<&Archetype> {
        self.archetypes.get(name)
    }

    /// Resolve an archetype against a pack registry.
    pub fn resolve(
        &self,
        name: &str,
        packs: &PackRegistry,
    ) -> Option<ResolvedArchetype> {
        let archetype = self.archetypes.get(name)?;
        resolve_archetype(archetype, packs)
    }

    /// All archetype names.
    pub fn names(&self) -> Vec<&str> {
        self.archetypes.keys().map(|s| s.as_str()).collect()
    }

    /// Total count.
    pub fn len(&self) -> usize {
        self.archetypes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.archetypes.is_empty()
    }
}

impl Default for ArchetypeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{
        PackTier, PackToolPolicy, PackEligibility, SkillWeight,
    };
    use crate::definition::SkillId;

    fn test_pack() -> SkillPack {
        SkillPack {
            id: PackId::new("engineering", "coder"),
            display_name: "Coder".into(),
            description: "Coding assistant".into(),
            version: "1.0.0".into(),
            tier: PackTier::Engineering,
            persona_prompt: "You are a senior engineer.".into(),
            persona_tokens: 10,
            skills: vec![SkillWeight {
                skill_id: SkillId::from("core/code-analysis"),
                weight: 0.9,
                required: true,
            }],
            pipeline_template: None,
            tool_policy: PackToolPolicy::default(),
            fallback_providers: vec!["claude".into()],
            eligibility: PackEligibility::default(),
            traits: vec!["concise".into()],
            tags: vec![],
            author: None,
            content_address: None,
            trust_level: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn test_resolve_archetype() {
        let mut packs = PackRegistry::new();
        packs.register(test_pack());

        let arch = Archetype {
            name: "senior-coder".into(),
            display_name: "Senior Coder".into(),
            description: "Expert-level coding".into(),
            pack_id: PackId::new("engineering", "coder"),
            extra_traits: vec!["first-principles".into()],
            provider_override: Some(vec!["gpt".into(), "claude".into()]),
            pipeline_override: None,
            metadata: HashMap::new(),
        };

        let resolved = resolve_archetype(&arch, &packs).unwrap();
        assert_eq!(resolved.composed_traits, vec!["concise", "first-principles"]);
        assert_eq!(resolved.pack.fallback_providers, vec!["gpt", "claude"]);
    }

    #[test]
    fn test_archetype_registry() {
        let mut reg = ArchetypeRegistry::new();
        reg.register(Archetype {
            name: "coder".into(),
            display_name: "Coder".into(),
            description: "Coding".into(),
            pack_id: PackId::new("engineering", "coder"),
            extra_traits: vec![],
            provider_override: None,
            pipeline_override: None,
            metadata: HashMap::new(),
        });
        assert_eq!(reg.len(), 1);
        assert!(reg.get("coder").is_some());
        assert!(reg.get("unknown").is_none());
    }
}
