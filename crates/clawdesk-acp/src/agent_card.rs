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
    ///
    /// **Do not mutate directly.** Use `add_capability()` / `set_capabilities()`
    /// which automatically rebuild the cached `cap_set`.
    capabilities: Vec<CapabilityId>,
    /// Precomputed capability bitset with hierarchical closure.
    /// Lazily initialised on first access (e.g. after deserialization).
    /// Automatically rebuilt by all mutation methods.
    #[serde(skip)]
    cap_set: OnceLock<CapSet>,
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

    /// Add a capability (builder pattern), automatically rebuilding the capset.
    /// O(1) amortized via set-check before push.
    pub fn with_capability(mut self, cap: CapabilityId) -> Self {
        // Use bit_index as a fast membership check via the closed capset.
        // For the raw list, linear scan is acceptable at current scale (~22 caps).
        if !self.capabilities.contains(&cap) {
            self.capabilities.push(cap);
        }
        self.rebuild_capset();
        self
    }

    /// Add a capability in-place, automatically rebuilding the capset.
    pub fn add_capability(&mut self, cap: CapabilityId) {
        if !self.capabilities.contains(&cap) {
            self.capabilities.push(cap);
        }
        self.rebuild_capset();
    }

    /// Replace all capabilities, automatically rebuilding the capset.
    pub fn set_capabilities(&mut self, caps: Vec<CapabilityId>) {
        self.capabilities = caps;
        self.rebuild_capset();
    }

    /// Read-only access to the capability list.
    pub fn capabilities(&self) -> &[CapabilityId] {
        &self.capabilities
    }

    /// Rebuild the cached capset with hierarchical closure.
    fn rebuild_capset(&mut self) {
        let raw: CapSet = self.capabilities.iter().copied().collect();
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

    /// Content-addressed structural fingerprint — exact, no truncation.
    ///
    /// Hashes: capabilities (via closed CapSet bits) + skills (canonical sort)
    /// + all skill tags (canonical sort) + endpoint URL + max_concurrent_tasks
    /// + protocol_versions (canonical sort).
    ///
    /// Uses SHA-256 truncated to 64 bits for the fingerprint value.
    /// No data is truncated — all skills, tags, and protocol versions
    /// contribute to the hash regardless of count.
    ///
    /// Returns a 64-bit hash (first 8 bytes of SHA-256).
    pub fn structural_fingerprint(&self) -> u64 {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();

        // Capabilities — use the closed CapSet's native bits representation.
        let cap_set = self.closed_capset();
        for word in cap_set.bits() {
            hasher.update(word.to_le_bytes());
        }

        // Skills — sort by ID for canonical order.
        let mut skill_ids: Vec<&str> = self.skills.iter().map(|s| s.id.as_str()).collect();
        skill_ids.sort_unstable();

        for skill_id in &skill_ids {
            hasher.update(skill_id.as_bytes());
            hasher.update(b":");

            // Find the skill and sort its tags.
            if let Some(skill) = self.skills.iter().find(|s| s.id.as_str() == *skill_id) {
                let mut sorted_tags: Vec<&str> = skill.tags.iter().map(|t| t.as_str()).collect();
                sorted_tags.sort_unstable();
                for (i, tag) in sorted_tags.iter().enumerate() {
                    if i > 0 {
                        hasher.update(b",");
                    }
                    hasher.update(tag.as_bytes());
                }
            }
        }

        // Endpoint URL.
        hasher.update(self.endpoint.url.as_bytes());

        // Max concurrent tasks.
        let max_tasks = self.max_concurrent_tasks.unwrap_or(0);
        hasher.update(max_tasks.to_le_bytes());

        // Protocol versions — sorted for canonical order.
        let mut sorted_pvs: Vec<&str> = self.protocol_versions.iter().map(|p| p.as_str()).collect();
        sorted_pvs.sort_unstable();
        for pv in &sorted_pvs {
            hasher.update(pv.as_bytes());
        }

        // Truncate SHA-256 to u64.
        let result = hasher.finalize();
        u64::from_le_bytes(result[..8].try_into().unwrap())
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
