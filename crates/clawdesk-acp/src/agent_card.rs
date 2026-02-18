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
    /// Capabilities this agent offers.
    pub capabilities: Vec<AgentCapability>,
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

/// A high-level capability category.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapability {
    /// Can process text messages and generate responses.
    TextGeneration,
    /// Can execute code in a sandbox.
    CodeExecution,
    /// Can search the web.
    WebSearch,
    /// Can read and process files.
    FileProcessing,
    /// Can generate or analyze images.
    ImageProcessing,
    /// Can process audio (transcription, TTS).
    AudioProcessing,
    /// Can interact with external APIs.
    ApiIntegration,
    /// Can manage and query databases.
    DataManagement,
    /// Can perform mathematical computations.
    Mathematics,
    /// Can manage scheduling and reminders.
    Scheduling,
    /// Can send messages to external channels.
    Messaging,
    /// Custom capability with a string identifier.
    Custom(String),
}

impl AgentCapability {
    /// Map a capability to a bit position in a u16 bitset.
    /// `Custom` variants get bit 15 (catch-all) — two Custom capabilities
    /// are considered equal for bitset purposes but distinguished by string
    /// comparison in the fallback path.
    #[inline]
    fn bit_index(&self) -> u16 {
        match self {
            Self::TextGeneration => 0,
            Self::CodeExecution => 1,
            Self::WebSearch => 2,
            Self::FileProcessing => 3,
            Self::ImageProcessing => 4,
            Self::AudioProcessing => 5,
            Self::ApiIntegration => 6,
            Self::DataManagement => 7,
            Self::Mathematics => 8,
            Self::Scheduling => 9,
            Self::Messaging => 10,
            Self::Custom(_) => 15, // catch-all bit
        }
    }

    /// Convert to a single-bit mask.
    #[inline]
    fn to_bit(&self) -> u16 {
        1u16 << self.bit_index()
    }
}

/// Convert a slice of capabilities into a u16 bitset.
/// O(n) where n = number of capabilities.
fn capabilities_to_bitset(caps: &[AgentCapability]) -> u16 {
    let mut bits: u16 = 0;
    for cap in caps {
        bits |= cap.to_bit();
    }
    bits
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
            skills: vec![],
            protocol_versions: vec!["1.0".into()],
            max_concurrent_tasks: Some(10),
            metadata: serde_json::Value::Null,
        }
    }

    /// Add a capability.
    pub fn with_capability(mut self, cap: AgentCapability) -> Self {
        self.capabilities.push(cap);
        self
    }

    /// Add a skill.
    pub fn with_skill(mut self, skill: AgentSkill) -> Self {
        self.skills.push(skill);
        self
    }

    /// Check if this agent has a specific capability.
    ///
    /// Uses bitset for O(1) check on known capabilities; falls back to
    /// linear scan only for `Custom` variants.
    pub fn has_capability(&self, cap: &AgentCapability) -> bool {
        if !matches!(cap, AgentCapability::Custom(_)) {
            let bits = capabilities_to_bitset(&self.capabilities);
            return bits & cap.to_bit() != 0;
        }
        self.capabilities.contains(cap)
    }

    /// Compute capability overlap score with a set of required capabilities.
    ///
    /// Returns a value in [0.0, 1.0]:
    /// - 1.0 = all required capabilities are present
    /// - 0.0 = no overlap
    ///
    /// Score = |required ∩ offered| / |required|
    ///
    /// Uses u16 bitset + POPCNT for O(1) intersection of known capability
    /// variants. Falls back to linear scan for `Custom` variants only.
    pub fn capability_score(&self, required: &[AgentCapability]) -> f64 {
        if required.is_empty() {
            return 1.0;
        }

        let offered_bits = capabilities_to_bitset(&self.capabilities);
        let required_bits = capabilities_to_bitset(required);

        // Fast path: count intersection via bitwise AND + popcount
        let intersection_bits = offered_bits & required_bits;
        let mut fast_matched = intersection_bits.count_ones() as usize;
        let mut total_required = required.len();

        // If Custom bit is set in the intersection, we can't trust the
        // count — different Custom strings map to the same bit. Do a
        // precise count for Custom entries only.
        let has_custom_required = required.iter().any(|r| matches!(r, AgentCapability::Custom(_)));
        if has_custom_required {
            // Subtract the fast-path Custom bit contribution and recount precisely.
            let custom_in_intersection = (intersection_bits >> 15) & 1;
            fast_matched -= custom_in_intersection as usize;

            let custom_matched = required
                .iter()
                .filter(|r| matches!(r, AgentCapability::Custom(_)))
                .filter(|r| self.capabilities.contains(r))
                .count();
            fast_matched += custom_matched;
        }

        fast_matched as f64 / total_required as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_score_full_match() {
        let card = AgentCard::new("test", "Test Agent", "http://localhost:8080")
            .with_capability(AgentCapability::TextGeneration)
            .with_capability(AgentCapability::WebSearch);

        let required = vec![AgentCapability::TextGeneration, AgentCapability::WebSearch];
        assert_eq!(card.capability_score(&required), 1.0);
    }

    #[test]
    fn capability_score_partial_match() {
        let card = AgentCard::new("test", "Test Agent", "http://localhost:8080")
            .with_capability(AgentCapability::TextGeneration);

        let required = vec![AgentCapability::TextGeneration, AgentCapability::WebSearch];
        assert_eq!(card.capability_score(&required), 0.5);
    }

    #[test]
    fn capability_score_no_match() {
        let card = AgentCard::new("test", "Test Agent", "http://localhost:8080")
            .with_capability(AgentCapability::Mathematics);

        let required = vec![AgentCapability::TextGeneration];
        assert_eq!(card.capability_score(&required), 0.0);
    }
}
