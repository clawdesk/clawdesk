//! Agent descriptor — matching-plane projection of an AgentCard.
//!
//! ## Design
//!
//! The `AgentCard` is the protocol DTO: it carries all descriptive metadata,
//! authentication, skills, and protocol negotiation fields suitable for
//! wire transport and discovery payloads.
//!
//! The `AgentDescriptor` is an **offline projection** optimized for:
//! - O(1) capability membership tests via precomputed `CapSet`
//! - O(W) score computation via POPCNT
//! - O(1) ETag/fingerprint lookup (precomputed at projection time)
//! - Minimal memory footprint (no serde metadata, no examples)
//!
//! The projection cost is O(k log k) for sorting skills + O(B) for hashing.
//! After projection, all routing operations are O(1) or O(W).

use crate::agent_card::AgentCard;
use crate::capability::{CapSet, CapabilityId};

/// Matching-plane projection of an `AgentCard`.
///
/// Created once after deserialization or card update. All fields are
/// precomputed and immutable — no lazy initialization, no `OnceLock`.
#[derive(Debug, Clone)]
pub struct AgentDescriptor {
    /// Agent ID (copied from card).
    pub id: String,
    /// Agent name (for display in routing decisions).
    pub name: String,
    /// Endpoint URL (for dispatch).
    pub endpoint_url: String,
    /// Precomputed closed capability set (hierarchical closure applied).
    pub capabilities: CapSet,
    /// Sorted skill IDs (for deterministic matching).
    pub skill_ids: Vec<String>,
    /// Maximum concurrent tasks.
    pub max_concurrent: u32,
    /// Precomputed structural fingerprint (SHA-256 truncated to u64).
    pub fingerprint: u64,
    /// Precomputed ETag header value.
    pub etag: String,
    /// Whether streaming is supported.
    pub supports_streaming: bool,
    /// Whether push notifications are supported.
    pub supports_push: bool,
}

impl AgentDescriptor {
    /// Project an `AgentCard` into a routing-optimized `AgentDescriptor`.
    ///
    /// This is the one-time O(k log k) normalization step. After this,
    /// all matching operations are O(1) for bit tests, O(W) for scores.
    pub fn from_card(card: &AgentCard) -> Self {
        // Compute closed capability set.
        let raw: CapSet = card.capabilities().iter().copied().collect();
        let capabilities = raw.close();

        // Sort skill IDs for deterministic matching.
        let mut skill_ids: Vec<String> = card.skills.iter()
            .map(|s| s.id.clone())
            .collect();
        skill_ids.sort_unstable();

        // Precompute fingerprint and ETag.
        let fingerprint = card.structural_fingerprint();
        let etag = format!("W/\"{:016x}\"", fingerprint);

        Self {
            id: card.id.clone(),
            name: card.name.clone(),
            endpoint_url: card.endpoint.url.clone(),
            capabilities,
            skill_ids,
            max_concurrent: card.max_concurrent_tasks.unwrap_or(10),
            fingerprint,
            etag,
            supports_streaming: card.endpoint.supports_streaming,
            supports_push: card.endpoint.supports_push,
        }
    }

    /// O(1) capability membership test.
    #[inline]
    pub fn has_capability(&self, cap: CapabilityId) -> bool {
        self.capabilities.contains(cap)
    }

    /// O(W) capability overlap score against required capabilities.
    pub fn capability_score(&self, required: &[CapabilityId]) -> f64 {
        if required.is_empty() {
            return 1.0;
        }
        let required_set: CapSet = required.iter().copied().collect();
        let required_closed = required_set.close();
        self.capabilities.overlap_score(&required_closed)
    }

    /// Check if this descriptor has a specific skill.
    pub fn has_skill(&self, skill_id: &str) -> bool {
        self.skill_ids.binary_search_by(|s| s.as_str().cmp(skill_id)).is_ok()
    }

    /// Whether the fingerprint matches (for cache validation).
    #[inline]
    pub fn fingerprint_matches(&self, other_fingerprint: u64) -> bool {
        self.fingerprint == other_fingerprint
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_card::{AgentCard, AgentSkill};

    fn test_card() -> AgentCard {
        AgentCard::new("test-agent", "Test Agent", "http://localhost:8080")
            .with_capability(CapabilityId::TextGeneration)
            .with_capability(CapabilityId::WebSearch)
            .with_skill(AgentSkill {
                id: "summarize".into(),
                name: "Summarize".into(),
                description: "Summarize text".into(),
                input_schema: None,
                output_schema: None,
                tags: vec!["nlp".into()],
                examples: vec![],
            })
            .with_skill(AgentSkill {
                id: "search".into(),
                name: "Search".into(),
                description: "Search the web".into(),
                input_schema: None,
                output_schema: None,
                tags: vec!["web".into()],
                examples: vec![],
            })
    }

    #[test]
    fn projection_preserves_identity() {
        let card = test_card();
        let desc = AgentDescriptor::from_card(&card);
        assert_eq!(desc.id, "test-agent");
        assert_eq!(desc.name, "Test Agent");
        assert_eq!(desc.endpoint_url, "http://localhost:8080");
    }

    #[test]
    fn skill_ids_sorted() {
        let card = test_card();
        let desc = AgentDescriptor::from_card(&card);
        assert_eq!(desc.skill_ids, vec!["search", "summarize"]);
    }

    #[test]
    fn capability_membership_o1() {
        let card = test_card();
        let desc = AgentDescriptor::from_card(&card);
        assert!(desc.has_capability(CapabilityId::TextGeneration));
        assert!(desc.has_capability(CapabilityId::WebSearch));
        assert!(!desc.has_capability(CapabilityId::Mathematics));
    }

    #[test]
    fn capability_score_works() {
        let card = test_card();
        let desc = AgentDescriptor::from_card(&card);
        assert_eq!(desc.capability_score(&[CapabilityId::TextGeneration, CapabilityId::WebSearch]), 1.0);
        assert_eq!(desc.capability_score(&[CapabilityId::TextGeneration]), 1.0);
        assert_eq!(desc.capability_score(&[CapabilityId::Mathematics]), 0.0);
    }

    #[test]
    fn skill_lookup_binary_search() {
        let card = test_card();
        let desc = AgentDescriptor::from_card(&card);
        assert!(desc.has_skill("search"));
        assert!(desc.has_skill("summarize"));
        assert!(!desc.has_skill("nonexistent"));
    }

    #[test]
    fn fingerprint_precomputed() {
        let card = test_card();
        let desc = AgentDescriptor::from_card(&card);
        assert_ne!(desc.fingerprint, 0);
        assert!(desc.etag.starts_with("W/\""));
        assert!(desc.fingerprint_matches(card.structural_fingerprint()));
    }

    #[test]
    fn hierarchical_closure_in_descriptor() {
        let card = AgentCard::new("audio", "Audio Agent", "http://localhost")
            .with_capability(CapabilityId::AudioProcessing);
        let desc = AgentDescriptor::from_card(&card);
        // AudioProcessing → MediaProcessing via closure
        assert!(desc.has_capability(CapabilityId::AudioProcessing));
        assert!(desc.has_capability(CapabilityId::MediaProcessing));
    }
}
