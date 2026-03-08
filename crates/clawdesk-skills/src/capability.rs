//! Skill Capability Contract — structured declarations of what a skill can do.
//!
//! Enables the HEFT scheduler and TaskDispatcher to make informed routing
//! decisions based on actual capability matching rather than string-matching
//! skill descriptions.
//!
//! ## Design
//!
//! Each `SkillCapability` declares:
//! - What action the skill performs (a well-known verb like `send_email`)
//! - Input/output schemas (JSON Schema)
//! - Cost and latency estimates (for HEFT scheduling)
//! - Trust and sandboxing requirements (for security gating)
//!
//! The `CapabilityIndex` provides O(1) lookup from action → skills,
//! answering the dispatcher's core question: "can any installed skill handle this task?"

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Trust level required to execute a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// No trust required — pure computation, no side effects.
    Untrusted = 0,
    /// Basic trust — read-only access to local data.
    ReadOnly = 1,
    /// Elevated — can write to local storage, call local APIs.
    Local = 2,
    /// Network — can make outbound network requests.
    Network = 3,
    /// Full — can execute arbitrary commands, manage processes.
    Full = 4,
}

impl Default for TrustLevel {
    fn default() -> Self {
        Self::Untrusted
    }
}

/// Structured declaration of a skill's capability.
///
/// Used by the HEFT scheduler to assign tasks to the best agent/skill
/// and by the `TaskDispatcher` to answer "can anything handle this?"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCapability {
    /// What this skill can do (e.g., "send_email", "parse_diff", "transcribe_audio").
    /// Should be a well-known verb from the capability taxonomy.
    pub action: String,

    /// Human-readable description of this capability.
    pub description: String,

    /// Input schema (JSON Schema) — what the skill expects.
    pub input_schema: serde_json::Value,

    /// Output schema (JSON Schema) — what the skill produces.
    pub output_schema: serde_json::Value,

    /// Estimated cost in tokens per invocation.
    pub estimated_tokens: u64,

    /// Estimated wall-clock latency in milliseconds.
    pub estimated_duration_ms: u64,

    /// Required trust level for this capability.
    pub required_trust: TrustLevel,

    /// Whether this skill can run inside the sandbox.
    pub sandboxable: bool,

    /// Optional tags for semantic grouping (e.g., "communication", "code", "data").
    #[serde(default)]
    pub tags: Vec<String>,
}

impl SkillCapability {
    /// Create a new capability with minimal required fields.
    pub fn new(action: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            description: description.into(),
            input_schema: serde_json::json!({}),
            output_schema: serde_json::json!({}),
            estimated_tokens: 0,
            estimated_duration_ms: 1000,
            required_trust: TrustLevel::default(),
            sandboxable: true,
            tags: Vec::new(),
        }
    }

    pub fn with_tokens(mut self, tokens: u64) -> Self {
        self.estimated_tokens = tokens;
        self
    }

    pub fn with_duration_ms(mut self, ms: u64) -> Self {
        self.estimated_duration_ms = ms;
        self
    }

    pub fn with_trust(mut self, trust: TrustLevel) -> Self {
        self.required_trust = trust;
        self
    }

    pub fn with_sandboxable(mut self, sandboxable: bool) -> Self {
        self.sandboxable = sandboxable;
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn with_input_schema(mut self, schema: serde_json::Value) -> Self {
        self.input_schema = schema;
        self
    }

    pub fn with_output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = schema;
        self
    }

    /// Weight for HEFT scheduling: inversely proportional to estimated duration.
    pub fn heft_weight(&self) -> f64 {
        self.estimated_duration_ms as f64
    }
}

/// Inverted index from action name → list of skill IDs that provide it.
///
/// O(1) lookup for the dispatcher's "can anything handle this?" query.
pub struct CapabilityIndex {
    /// action → Vec<(skill_id, capability)>
    index: HashMap<String, Vec<CapabilityEntry>>,
}

/// An entry in the capability index.
#[derive(Debug, Clone)]
pub struct CapabilityEntry {
    /// The skill that provides this capability.
    pub skill_id: String,
    /// The full capability declaration.
    pub capability: SkillCapability,
}

impl CapabilityIndex {
    pub fn new() -> Self {
        Self {
            index: HashMap::new(),
        }
    }

    /// Register a skill's capabilities in the index.
    pub fn register(&mut self, skill_id: &str, capabilities: &[SkillCapability]) {
        for cap in capabilities {
            self.index
                .entry(cap.action.clone())
                .or_default()
                .push(CapabilityEntry {
                    skill_id: skill_id.to_string(),
                    capability: cap.clone(),
                });
        }
    }

    /// Remove all capabilities for a skill.
    pub fn unregister(&mut self, skill_id: &str) {
        for entries in self.index.values_mut() {
            entries.retain(|e| e.skill_id != skill_id);
        }
        self.index.retain(|_, v| !v.is_empty());
    }

    /// Find all skills that can handle a given action.
    pub fn find_by_action(&self, action: &str) -> &[CapabilityEntry] {
        self.index.get(action).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Find the best skill for an action (lowest estimated duration, highest trust satisfied).
    pub fn best_for_action(
        &self,
        action: &str,
        available_trust: TrustLevel,
    ) -> Option<&CapabilityEntry> {
        self.index.get(action).and_then(|entries| {
            entries
                .iter()
                .filter(|e| e.capability.required_trust <= available_trust)
                .min_by(|a, b| {
                    a.capability
                        .estimated_duration_ms
                        .cmp(&b.capability.estimated_duration_ms)
                })
        })
    }

    /// Check whether any registered skill can handle the given action.
    pub fn can_handle(&self, action: &str) -> bool {
        self.index
            .get(action)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Return all known action names.
    pub fn actions(&self) -> Vec<&str> {
        self.index.keys().map(|s| s.as_str()).collect()
    }

    /// Total number of registered capability entries.
    pub fn len(&self) -> usize {
        self.index.values().map(|v| v.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

impl Default for CapabilityIndex {
    fn default() -> Self {
        Self::new()
    }
}
