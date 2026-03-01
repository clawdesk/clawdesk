//! Agent Card — capability advertisement for A2A discovery.
//!
//! An Agent Card is a JSON document served at `/.well-known/agent.json`
//! that describes an agent's capabilities, endpoint, authentication
//! requirements, and supported skills.
//!
//! The card follows the principle of **self-describing services**:
//! an agent can discover another agent's capabilities without prior
//! configuration, enabling zero-config agent-to-agent communication.

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
    /// Call `rebuild_capset()` after directly mutating `capabilities`.
    #[serde(skip)]
    pub cap_set: CapSet,
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
            cap_set: CapSet::empty(),
            skills: vec![],
            protocol_versions: vec!["1.0".into()],
            max_concurrent_tasks: Some(10),
            metadata: serde_json::Value::Null,
        }
    }

    /// Add a capability, rebuilding the closed capset.
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
        self.cap_set = raw.close();
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
    pub fn has_capability(&self, cap: CapabilityId) -> bool {
        let set = if self.cap_set.is_empty() && !self.capabilities.is_empty() {
            // Lazy rebuild: capset not yet computed (e.g., after deserialization).
            let raw: CapSet = self.capabilities.iter().copied().collect();
            raw.close()
        } else {
            self.cap_set
        };
        set.contains(cap)
    }

    /// Content-addressed structural fingerprint.
    ///
    /// Hashes: capabilities (sorted) + skills (IDs + tags) + endpoint URL +
    /// max_concurrent_tasks + protocol_versions. Excludes description, metadata,
    /// and version string — those are "cosmetic" changes that don't affect routing.
    ///
    /// Returns a 64-bit FNV-1a hash as hex ETag (e.g., `"W/\"a3f7c20b1e4d9851\""`).
    /// Use with `DiscoveryCache::put_if_changed()` to skip re-indexing when only
    /// metadata changed.
    pub fn structural_fingerprint(&self) -> u64 {
        let mut h = 0xcbf29ce484222325u64; // FNV-1a offset basis

        #[inline(always)]
        fn fnv_bytes(h: &mut u64, bytes: &[u8]) {
            for &b in bytes {
                *h ^= b as u64;
                *h = h.wrapping_mul(0x100000001b3);
            }
        }

        // Capabilities (sorted for determinism).
        let mut caps: Vec<u8> = self.capabilities.iter().map(|c| *c as u8).collect();
        caps.sort_unstable();
        fnv_bytes(&mut h, &caps);

        // Skill IDs + tags (sorted).
        let mut skill_keys: Vec<String> = self.skills.iter().map(|s| {
            let mut key = s.id.clone();
            let mut tags = s.tags.clone();
            tags.sort_unstable();
            key.push(':');
            key.push_str(&tags.join(","));
            key
        }).collect();
        skill_keys.sort_unstable();
        for sk in &skill_keys {
            fnv_bytes(&mut h, sk.as_bytes());
        }

        // Endpoint URL.
        fnv_bytes(&mut h, self.endpoint.url.as_bytes());

        // Max concurrent tasks.
        let max_tasks = self.max_concurrent_tasks.unwrap_or(0);
        fnv_bytes(&mut h, &max_tasks.to_le_bytes());

        // Protocol versions (sorted).
        let mut pvs = self.protocol_versions.clone();
        pvs.sort_unstable();
        for pv in &pvs {
            fnv_bytes(&mut h, pv.as_bytes());
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

        let offered = if self.cap_set.is_empty() && !self.capabilities.is_empty() {
            let raw: CapSet = self.capabilities.iter().copied().collect();
            raw.close()
        } else {
            self.cap_set
        };

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
