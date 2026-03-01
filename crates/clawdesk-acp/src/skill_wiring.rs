//! Skill → A2A wiring — automatic mapping from `SkillRegistry` to `AgentCard` skills.
//!
//! ## Problem
//!
//! ClawDesk's `SkillRegistry` (52+ skills compiled into `.rodata`) defines
//! what the local agent can do. The `AgentCard` (served at `/.well-known/agent.json`)
//! advertises these capabilities to remote agents. But the mapping between
//! these two was manual — adding a new skill required also updating the AgentCard.
//!
//! ## Solution
//!
//! `SkillWiring` automatically syncs the `SkillRegistry` → `AgentCard.skills`:
//! 1. Scans all active skills in the registry.
//! 2. Maps each `Skill.manifest` → `AgentSkill` (id, name, description, parameter schema).
//! 3. Derives `CapabilityId` from skill tags (e.g., tag "web" → `WebSearch`).
//! 4. Patches the `AgentCard` with the derived skills and capabilities.
//!
//! This runs on-demand (not blocking the hot path) as a sync function that
//! any system can call after skill registry changes.

use crate::agent_card::{AgentCard, AgentSkill};
use crate::capability::CapabilityId;
use serde_json::json;
use tracing::info;

/// Maps a skill registry snapshot to A2A protocol types.
///
/// This is a standalone function rather than a trait impl because
/// `clawdesk-acp` depends on `clawdesk-agents` but not on `clawdesk-skills`
/// directly. The caller provides the skill data as a vec of `SkillSnapshot`.
pub fn sync_skills_to_card(card: &mut AgentCard, skills: &[SkillSnapshot]) {
    let mut agent_skills = Vec::with_capacity(skills.len());
    let mut capabilities = std::collections::HashSet::new();

    for skill in skills {
        // Map to AgentSkill
        let agent_skill = AgentSkill {
            id: skill.id.clone(),
            name: skill.display_name.clone(),
            description: skill.description.clone(),
            input_schema: Some(parameters_to_json_schema(&skill.parameters)),
            output_schema: None,
            tags: skill.tags.clone(),
            examples: Vec::new(),
        };
        agent_skills.push(agent_skill);

        // Derive capabilities from tags using CapabilityId::from_tag()
        for tag in &skill.tags {
            if let Some(cap) = CapabilityId::from_tag(tag) {
                capabilities.insert(cap);
            }
        }
    }

    let cap_count = capabilities.len();
    let skill_count = agent_skills.len();

    card.skills = agent_skills;
    // Merge derived capabilities with any existing ones
    for cap in capabilities {
        if !card.capabilities.contains(&cap) {
            card.capabilities.push(cap);
        }
    }
    card.rebuild_capset();

    info!(
        skills = skill_count,
        capabilities = cap_count,
        "synced skill registry to agent card"
    );
}

/// Lightweight snapshot of a skill's manifest for cross-crate transfer.
///
/// This avoids a direct dependency on `clawdesk-skills` from `clawdesk-acp`.
/// The gateway (which depends on both crates) constructs these from the
/// `SkillRegistry` and passes them to `sync_skills_to_card`.
#[derive(Debug, Clone)]
pub struct SkillSnapshot {
    /// Skill ID (e.g., "core/web-search").
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Description.
    pub description: String,
    /// Parameter definitions.
    pub parameters: Vec<SkillParam>,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// Estimated token cost.
    pub estimated_tokens: usize,
}

/// Simplified parameter definition.
#[derive(Debug, Clone)]
pub struct SkillParam {
    pub name: String,
    pub description: String,
    pub param_type: String, // "string", "integer", "boolean", etc.
    pub required: bool,
}

/// Map a skill tag to a `CapabilityId`.
///
/// Returns `None` for tags that don't map to a known capability.
/// Delegates to `CapabilityId::from_tag()` for the canonical mapping.
fn tag_to_capability(tag: &str) -> Option<CapabilityId> {
    CapabilityId::from_tag(tag)
}

/// Convert parameter definitions to a JSON Schema object.
fn parameters_to_json_schema(params: &[SkillParam]) -> serde_json::Value {
    if params.is_empty() {
        return json!({
            "type": "object",
            "properties": {},
        });
    }

    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for param in params {
        properties.insert(
            param.name.clone(),
            json!({
                "type": param.param_type,
                "description": param.description,
            }),
        );
        if param.required {
            required.push(serde_json::Value::String(param.name.clone()));
        }
    }

    let mut schema = json!({
        "type": "object",
        "properties": properties,
    });
    if !required.is_empty() {
        schema["required"] = serde_json::Value::Array(required);
    }
    schema
}

/// Derive a capability summary string for logging.
pub fn capability_summary(caps: &[CapabilityId]) -> String {
    caps.iter()
        .map(|c| c.name())
        .collect::<Vec<_>>()
        .join(", ")
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn test_card() -> AgentCard {
        AgentCard::new("test", "Test Agent", "http://localhost:18789")
    }

    fn sample_skills() -> Vec<SkillSnapshot> {
        vec![
            SkillSnapshot {
                id: "core/web-search".into(),
                display_name: "Web Search".into(),
                description: "Search the web for information".into(),
                parameters: vec![SkillParam {
                    name: "query".into(),
                    description: "Search query".into(),
                    param_type: "string".into(),
                    required: true,
                }],
                tags: vec!["web".into(), "search".into()],
                estimated_tokens: 100,
            },
            SkillSnapshot {
                id: "core/code-review".into(),
                display_name: "Code Review".into(),
                description: "Review code for issues".into(),
                parameters: vec![
                    SkillParam {
                        name: "file".into(),
                        description: "File to review".into(),
                        param_type: "string".into(),
                        required: true,
                    },
                    SkillParam {
                        name: "language".into(),
                        description: "Programming language".into(),
                        param_type: "string".into(),
                        required: false,
                    },
                ],
                tags: vec!["code".into(), "review".into()],
                estimated_tokens: 200,
            },
            SkillSnapshot {
                id: "core/math".into(),
                display_name: "Mathematics".into(),
                description: "Perform calculations".into(),
                parameters: vec![],
                tags: vec!["math".into()],
                estimated_tokens: 50,
            },
        ]
    }

    #[test]
    fn sync_populates_agent_skills() {
        let mut card = test_card();
        sync_skills_to_card(&mut card, &sample_skills());

        assert_eq!(card.skills.len(), 3);
        assert_eq!(card.skills[0].id, "core/web-search");
        assert_eq!(card.skills[1].id, "core/code-review");
        assert_eq!(card.skills[2].id, "core/math");
    }

    #[test]
    fn sync_derives_capabilities_from_tags() {
        let mut card = test_card();
        sync_skills_to_card(&mut card, &sample_skills());

        // Should have WebSearch, CodeExecution, Mathematics
        assert!(card.capabilities.contains(&CapabilityId::WebSearch));
        assert!(card.capabilities.contains(&CapabilityId::CodeExecution));
        assert!(card.capabilities.contains(&CapabilityId::Mathematics));
    }

    #[test]
    fn sync_preserves_existing_capabilities() {
        let mut card = test_card();
        card.capabilities.push(CapabilityId::Messaging);
        sync_skills_to_card(&mut card, &sample_skills());

        assert!(card.capabilities.contains(&CapabilityId::Messaging));
        assert!(card.capabilities.contains(&CapabilityId::WebSearch));
    }

    #[test]
    fn parameters_to_schema() {
        let params = vec![
            SkillParam {
                name: "query".into(),
                description: "Search query".into(),
                param_type: "string".into(),
                required: true,
            },
            SkillParam {
                name: "limit".into(),
                description: "Max results".into(),
                param_type: "integer".into(),
                required: false,
            },
        ];

        let schema = parameters_to_json_schema(&params);
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["properties"]["limit"].is_object());
        assert_eq!(schema["required"][0], "query");
    }

    #[test]
    fn empty_params_produces_empty_schema() {
        let schema = parameters_to_json_schema(&[]);
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn tag_mapping_coverage() {
        assert_eq!(tag_to_capability("web"), Some(CapabilityId::WebSearch));
        assert_eq!(tag_to_capability("code"), Some(CapabilityId::CodeExecution));
        assert_eq!(tag_to_capability("math"), Some(CapabilityId::Mathematics));
        assert_eq!(tag_to_capability("audio"), Some(CapabilityId::AudioProcessing));
        assert_eq!(tag_to_capability("unknown_tag"), None);
    }

    #[test]
    fn no_duplicate_capabilities() {
        let mut card = test_card();
        // Two skills with the same tag
        let skills = vec![
            SkillSnapshot {
                id: "a".into(),
                display_name: "A".into(),
                description: "".into(),
                parameters: vec![],
                tags: vec!["web".into()],
                estimated_tokens: 0,
            },
            SkillSnapshot {
                id: "b".into(),
                display_name: "B".into(),
                description: "".into(),
                parameters: vec![],
                tags: vec!["web".into()],
                estimated_tokens: 0,
            },
        ];
        sync_skills_to_card(&mut card, &skills);

        let web_count = card
            .capabilities
            .iter()
            .filter(|c| **c == CapabilityId::WebSearch)
            .count();
        assert_eq!(web_count, 1);
    }
}
