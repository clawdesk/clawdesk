//! Agent Card — capability advertisement for A2A discovery.
//!
//! An Agent Card is a JSON document served at `/.well-known/agent.json`
//! that describes an agent's capabilities, endpoint, authentication
//! requirements, and supported skills.
//!
//! The card follows the principle of **self-describing services**:
//! an agent can discover another agent's capabilities without prior
//! configuration, enabling zero-config agent-to-agent communication.

use std::sync::OnceLock;
use serde::{Deserialize, Serialize};
use crate::capability::{CapabilityId, CapSet};

/// The Agent Card — a self-describing capability advertisement.
///
/// Served at `GET /.well-known/agent.json` by every A2A-capable agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    /// Unique agent identifier (UUID or namespaced string).
    pub id: String,
    /// Human-readable agent name.
    pub name: String,
    /// Description of the agent's purpose.
    pub description: String,
    /// Semantic version of the agent.
    pub version: String,
    /// Base URL for A2A protocol endpoints.
    pub endpoint: AgentEndpoint,
    /// Authentication requirements.
    pub auth: AgentAuth,
    /// Capabilities this agent offers (typed capability IDs).
    pub capabilities: Vec<CapabilityId>,
    /// Precomputed capability bitset with hierarchical closure.
    /// Lazily initialised on first access (e.g. after deserialization).
    /// Call `rebuild_capset()` after directly mutating `capabilities`.
    #[serde(skip)]
    pub cap_set: OnceLock<CapSet>,
    /// Skills this agent can perform (more granular than capabilities).
    pub skills: Vec<AgentSkill>,
    /// Supported protocol versions.
    pub protocol_versions: Vec<String>,
    /// Maximum concurrent tasks this agent can handle.
    pub max_concurrent_tasks: Option<u32>,
    /// Agent metadata (arbitrary key-value pairs).
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Agent endpoint configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEndpoint {
    /// Base URL (e.g., "https://agent.example.com").
    pub url: String,
    /// Whether the agent supports streaming responses (SSE).
    pub supports_streaming: bool,
    /// Whether the agent supports push notifications via webhook.
    pub supports_push: bool,
    /// Push notification URL (if supports_push is true).
    pub push_url: Option<String>,
}

/// Authentication requirements for communicating with this agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuth {
    /// No authentication required.
    None,
    /// Bearer token authentication.
    Bearer,
    /// API key in header.
    ApiKey { header_name: String },
    /// OAuth 2.0.
    OAuth2 {
        token_url: String,
        scopes: Vec<String>,
    },
    /// Mutual TLS.
    Mtls,
}

/// A specific skill the agent can perform — more granular than capabilities.
///
/// Skills map to specific actions the agent can take, potentially with
/// tool use. This is the unit of task delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    /// Unique skill identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of what this skill does.
    pub description: String,
    /// Input parameter schema (JSON Schema).
    pub input_schema: Option<serde_json::Value>,
    /// Output schema (JSON Schema).
    pub output_schema: Option<serde_json::Value>,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// Sample inputs for testing.
    pub examples: Vec<SkillExample>,
}

/// Example input/output for a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillExample {
    pub input: String,
    pub output: String,
}

impl AgentCard {
    /// Create a minimal agent card.
    pub fn new(id: impl Into<String>, name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            version: "0.1.0".into(),
            endpoint: AgentEndpoint {
                url: url.into(),
                supports_streaming: false,
                supports_push: false,
                push_url: None,
            },
            auth: AgentAuth::None,
            capabilities: vec![],
            cap_set: OnceLock::new(),
            skills: vec![],
            protocol_versions: vec!["1.0".into()],
            max_concurrent_tasks: Some(10),
            metadata: serde_json::Value::Null,
        }
    }

    /// Add a capability, invalidating the cached capset.
    pub fn with_capability(mut self, cap: CapabilityId) -> Self {
        if !self.capabilities.contains(&cap) {
            self.capabilities.push(cap);
        }
        self.rebuild_capset();
        self
    }

    /// Rebuild the cached capset with hierarchical closure.
    /// Call after directly mutating `capabilities`.
    pub fn rebuild_capset(&mut self) {
        let raw: CapSet = self.capabilities.iter().copied().collect();
        // Replace the OnceCell — take the old one and create a new pre-filled one.
        self.cap_set = OnceLock::new();
        let _ = self.cap_set.set(raw.close());
    }

    /// Get the closed capability set, computing it lazily if needed.
    fn closed_capset(&self) -> &CapSet {
        self.cap_set.get_or_init(|| {
            let raw: CapSet = self.capabilities.iter().copied().collect();
            raw.close()
        })
    }

    /// Add a skill.
    pub fn with_skill(mut self, skill: AgentSkill) -> Self {
        self.skills.push(skill);
        self
    }

    /// Check if this agent has a specific capability.
    ///
    /// Uses the closed CapSet for O(1) check, including hierarchical
    /// implication (e.g., `AudioProcessing` implies `MediaProcessing`).
    /// The capset is lazily computed on first access and cached via OnceCell,
    /// so repeated calls are O(1) even after deserialization.
    pub fn has_capability(&self, cap: CapabilityId) -> bool {
        self.closed_capset().contains(cap)
    }

    /// Content-addressed structural fingerprint — true zero-heap-allocation.
    ///
    /// Hashes: capabilities (via closed CapSet bits) + skills (sorted indices) +
    /// endpoint URL + max_concurrent_tasks + protocol_versions. Excludes
    /// description, metadata, and version string — those are "cosmetic" changes
    /// that don't affect routing.
    ///
    /// **Zero heap allocations**: Uses fixed-size stack arrays `[usize; 64]`
    /// for index sorting. Capabilities use the CapSet's native `[u64; N]`
    /// representation (consistent with the CapSet width, currently 64 bits).
    ///
    /// Total: O(k log k) comparisons, 0 heap allocations, O(Σ|bytes|) hash ops.
    ///
    /// Returns a 64-bit FNV-1a hash.
    pub fn structural_fingerprint(&self) -> u64 {
        /// Maximum skills/tags/protocol versions before this function would
        /// need a heap fallback. 64 is generous — most agents have < 10 skills.
        const MAX_INDICES: usize = 64;

        let mut h = 0xcbf29ce484222325u64; // FNV-1a offset basis

        #[inline(always)]
        fn fnv_bytes(h: &mut u64, bytes: &[u8]) {
            for &b in bytes {
                *h ^= b as u64;
                *h = h.wrapping_mul(0x100000001b3);
            }
        }

        // Capabilities — use the closed CapSet's native bits representation.
        // This is consistent with the CapSet width (currently N=1 → u64, but
        // automatically scales if CapSet is expanded to N=2 → u128, etc.).
        let cap_set = self.closed_capset();
        // Hash each word of the CapSet backing store
        for word in cap_set.bits() {
            fnv_bytes(&mut h, &word.to_le_bytes());
        }

        // Skill IDs + tags — index-sort on stack, no heap allocation.
        let skill_count = self.skills.len();
        if skill_count > 0 {
            assert!(skill_count <= MAX_INDICES, "skill count {} exceeds stack limit {}", skill_count, MAX_INDICES);
            let mut indices = [0usize; MAX_INDICES];
            for i in 0..skill_count {
                indices[i] = i;
            }
            indices[..skill_count].sort_unstable_by(|&a, &b| self.skills[a].id.cmp(&self.skills[b].id));

            for &idx in &indices[..skill_count] {
                let skill = &self.skills[idx];
                fnv_bytes(&mut h, skill.id.as_bytes());
                fnv_bytes(&mut h, b":");

                // Sort tag indices on stack.
                let tag_count = skill.tags.len();
                if tag_count > 0 {
                    assert!(tag_count <= MAX_INDICES, "tag count {} exceeds stack limit {}", tag_count, MAX_INDICES);
                    let mut tag_indices = [0usize; MAX_INDICES];
                    for i in 0..tag_count {
                        tag_indices[i] = i;
                    }
                    tag_indices[..tag_count].sort_unstable_by(|&a, &b| skill.tags[a].cmp(&skill.tags[b]));
                    for (i, &ti) in tag_indices[..tag_count].iter().enumerate() {
                        if i > 0 {
                            fnv_bytes(&mut h, b",");
                        }
                        fnv_bytes(&mut h, skill.tags[ti].as_bytes());
                    }
                }
            }
        }

        // Endpoint URL — direct hash, no allocation.
        fnv_bytes(&mut h, self.endpoint.url.as_bytes());

        // Max concurrent tasks.
        let max_tasks = self.max_concurrent_tasks.unwrap_or(0);
        fnv_bytes(&mut h, &max_tasks.to_le_bytes());

        // Protocol versions — index-sort on stack.
        let pv_count = self.protocol_versions.len();
        if pv_count > 0 {
            assert!(pv_count <= MAX_INDICES, "protocol version count {} exceeds stack limit {}", pv_count, MAX_INDICES);
            let mut pv_indices = [0usize; MAX_INDICES];
            for i in 0..pv_count {
                pv_indices[i] = i;
            }
            pv_indices[..pv_count].sort_unstable_by(|&a, &b| {
                self.protocol_versions[a].cmp(&self.protocol_versions[b])
            });
            for &pi in &pv_indices[..pv_count] {
                fnv_bytes(&mut h, self.protocol_versions[pi].as_bytes());
            }
        }

        h
    }

    /// Generate a weak ETag from the structural fingerprint.
    /// Format: `W/"<16-hex-digits>"` per RFC 7232.
    pub fn structural_etag(&self) -> String {
        format!("W/\"{:016x}\"", self.structural_fingerprint())
    }

    /// Compute capability overlap score with a set of required capabilities.
    ///
    /// Returns a value in `[0.0, 1.0]`:
    /// - 1.0 = all required capabilities are present (after hierarchical closure)
    /// - 0.0 = no overlap
    ///
    /// Score = `|close(offered) ∩ close(required)| / |close(required)|`
    ///
    /// Uses POPCNT on 64-bit words for O(1) computation.
    /// Hierarchical implication closure ensures `AudioProcessing → MediaProcessing`.
    pub fn capability_score(&self, required: &[CapabilityId]) -> f64 {
        if required.is_empty() {
            return 1.0;
        }

        let offered = self.closed_capset();

        let required_set: CapSet = required.iter().copied().collect();
        let required_closed = required_set.close();

        offered.overlap_score(&required_closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_score_full_match() {
        let card = AgentCard::new("test", "Test Agent", "http://localhost:8080")
            .with_capability(CapabilityId::TextGeneration)
            .with_capability(CapabilityId::WebSearch);

        let required = vec![CapabilityId::TextGeneration, CapabilityId::WebSearch];
        assert_eq!(card.capability_score(&required), 1.0);
    }

    #[test]
    fn capability_score_partial_match() {
        let card = AgentCard::new("test", "Test Agent", "http://localhost:8080")
            .with_capability(CapabilityId::TextGeneration);

        let required = vec![CapabilityId::TextGeneration, CapabilityId::WebSearch];
        assert_eq!(card.capability_score(&required), 0.5);
    }

    #[test]
    fn capability_score_no_match() {
        let card = AgentCard::new("test", "Test Agent", "http://localhost:8080")
            .with_capability(CapabilityId::Mathematics);

        let required = vec![CapabilityId::TextGeneration];
        assert_eq!(card.capability_score(&required), 0.0);
    }

    #[test]
    fn hierarchical_match_audio_implies_media() {
        // An agent advertising AudioProcessing should match MediaProcessing
        // requirement via hierarchical closure.
        let card = AgentCard::new("audio", "Audio Agent", "http://localhost:8080")
            .with_capability(CapabilityId::AudioProcessing);

        let required = vec![CapabilityId::MediaProcessing];
        // After closure: AudioProcessing implies MediaProcessing → match
        assert_eq!(card.capability_score(&required), 1.0);
    }

    #[test]
    fn hierarchical_match_voice_implies_audio_and_media() {
        let card = AgentCard::new("voice", "Voice Agent", "http://localhost:8080")
            .with_capability(CapabilityId::VoiceUnderstanding);

        // VoiceUnderstanding → AudioProcessing → MediaProcessing
        assert!(card.has_capability(CapabilityId::AudioProcessing));
        assert!(card.has_capability(CapabilityId::MediaProcessing));
    }
}
