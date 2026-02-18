//! Provider capability algebra — O(1) bitwise capability negotiation.
//!
//! Each provider declares its capabilities as a `ProviderCaps` bitset.
//! The router performs capability intersection via `AND` + `CMP` in O(1)
//! to determine whether a provider satisfies all request requirements.
//!
//! ## Capability matrix
//!
//! ```text
//! Bit  Capability         Anthropic  OpenAI  Gemini  Ollama  Bedrock
//!  0   TextCompletion        ✓         ✓       ✓       ✓       ✓
//!  1   Streaming             ✓         ✓       ✓       ✓       ✓
//!  2   ToolUse               ✓         ✓       ✓       ✗       ✓
//!  3   Vision                ✓         ✓       ✓       ✗       △
//!  4   Embeddings            ✗         ✓       ✓       ✓       ✓
//!  5   JsonMode              ✗         ✓       ✓       ✓       ✗
//!  6   SystemPrompt          ✓         ✓       ✓       ✓       ✓
//!  7   ExtendedThinking      ✓         ✓       ✗       ✗       ✗
//!  8   StructuredOutput      ✗         ✓       ✓       ✗       ✗
//!  9   Caching               ✓         ✗       ✗       ✗       ✗
//! 10   BatchAPI              ✗         ✓       ✗       ✗       ✗
//! 11   CodeExecution         ✗         ✓       ✓       ✗       ✗
//! 12   ImageGeneration       ✗         ✓       ✓       ✗       ✗
//! ```
//!
//! Matching: `provider_caps & request_caps == request_caps` — single AND + CMP.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A bitset of provider capabilities. Supports up to 32 capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderCaps(u32);

impl ProviderCaps {
    // --- Capability bits ---
    pub const TEXT_COMPLETION: Self = Self(1 << 0);
    pub const STREAMING: Self = Self(1 << 1);
    pub const TOOL_USE: Self = Self(1 << 2);
    pub const VISION: Self = Self(1 << 3);
    pub const EMBEDDINGS: Self = Self(1 << 4);
    pub const JSON_MODE: Self = Self(1 << 5);
    pub const SYSTEM_PROMPT: Self = Self(1 << 6);
    pub const EXTENDED_THINKING: Self = Self(1 << 7);
    pub const STRUCTURED_OUTPUT: Self = Self(1 << 8);
    pub const CACHING: Self = Self(1 << 9);
    pub const BATCH_API: Self = Self(1 << 10);
    pub const CODE_EXECUTION: Self = Self(1 << 11);
    pub const IMAGE_GENERATION: Self = Self(1 << 12);

    /// Empty capabilities.
    pub const NONE: Self = Self(0);

    /// All capabilities.
    pub const ALL: Self = Self(0x1FFF);

    /// Create from raw bits.
    #[inline]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Get raw bits.
    #[inline]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Union of two capability sets.
    #[inline]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Intersection of two capability sets.
    #[inline]
    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Check if all capabilities in `required` are present. O(1).
    #[inline]
    pub const fn satisfies(self, required: Self) -> bool {
        (self.0 & required.0) == required.0
    }

    /// Count the number of capabilities. Uses POPCNT.
    #[inline]
    pub const fn count(self) -> u32 {
        self.0.count_ones()
    }

    /// Check if a single capability is present.
    #[inline]
    pub const fn has(self, cap: Self) -> bool {
        (self.0 & cap.0) != 0
    }

    /// Check if empty.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for ProviderCaps {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for ProviderCaps {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl fmt::Display for ProviderCaps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let labels = [
            (Self::TEXT_COMPLETION, "text"),
            (Self::STREAMING, "streaming"),
            (Self::TOOL_USE, "tools"),
            (Self::VISION, "vision"),
            (Self::EMBEDDINGS, "embeddings"),
            (Self::JSON_MODE, "json_mode"),
            (Self::SYSTEM_PROMPT, "system_prompt"),
            (Self::EXTENDED_THINKING, "thinking"),
            (Self::STRUCTURED_OUTPUT, "structured"),
            (Self::CACHING, "caching"),
            (Self::BATCH_API, "batch"),
            (Self::CODE_EXECUTION, "code_exec"),
            (Self::IMAGE_GENERATION, "image_gen"),
        ];

        let active: Vec<&str> = labels
            .iter()
            .filter(|(cap, _)| self.has(*cap))
            .map(|(_, label)| *label)
            .collect();

        write!(f, "{{{}}}", active.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Pre-computed provider capability profiles
// ---------------------------------------------------------------------------

/// Anthropic Claude capabilities.
pub const ANTHROPIC_CAPS: ProviderCaps = ProviderCaps::from_bits(
    ProviderCaps::TEXT_COMPLETION.bits()
        | ProviderCaps::STREAMING.bits()
        | ProviderCaps::TOOL_USE.bits()
        | ProviderCaps::VISION.bits()
        | ProviderCaps::SYSTEM_PROMPT.bits()
        | ProviderCaps::EXTENDED_THINKING.bits()
        | ProviderCaps::CACHING.bits(),
);

/// OpenAI capabilities.
pub const OPENAI_CAPS: ProviderCaps = ProviderCaps::from_bits(
    ProviderCaps::TEXT_COMPLETION.bits()
        | ProviderCaps::STREAMING.bits()
        | ProviderCaps::TOOL_USE.bits()
        | ProviderCaps::VISION.bits()
        | ProviderCaps::EMBEDDINGS.bits()
        | ProviderCaps::JSON_MODE.bits()
        | ProviderCaps::SYSTEM_PROMPT.bits()
        | ProviderCaps::EXTENDED_THINKING.bits()
        | ProviderCaps::STRUCTURED_OUTPUT.bits()
        | ProviderCaps::BATCH_API.bits()
        | ProviderCaps::CODE_EXECUTION.bits()
        | ProviderCaps::IMAGE_GENERATION.bits(),
);

/// Google Gemini capabilities.
pub const GEMINI_CAPS: ProviderCaps = ProviderCaps::from_bits(
    ProviderCaps::TEXT_COMPLETION.bits()
        | ProviderCaps::STREAMING.bits()
        | ProviderCaps::TOOL_USE.bits()
        | ProviderCaps::VISION.bits()
        | ProviderCaps::EMBEDDINGS.bits()
        | ProviderCaps::JSON_MODE.bits()
        | ProviderCaps::SYSTEM_PROMPT.bits()
        | ProviderCaps::STRUCTURED_OUTPUT.bits()
        | ProviderCaps::CODE_EXECUTION.bits()
        | ProviderCaps::IMAGE_GENERATION.bits(),
);

/// Ollama capabilities (local models).
pub const OLLAMA_CAPS: ProviderCaps = ProviderCaps::from_bits(
    ProviderCaps::TEXT_COMPLETION.bits()
        | ProviderCaps::STREAMING.bits()
        | ProviderCaps::EMBEDDINGS.bits()
        | ProviderCaps::JSON_MODE.bits()
        | ProviderCaps::SYSTEM_PROMPT.bits(),
);

/// AWS Bedrock capabilities (meta-provider).
pub const BEDROCK_CAPS: ProviderCaps = ProviderCaps::from_bits(
    ProviderCaps::TEXT_COMPLETION.bits()
        | ProviderCaps::STREAMING.bits()
        | ProviderCaps::TOOL_USE.bits()
        | ProviderCaps::VISION.bits()
        | ProviderCaps::EMBEDDINGS.bits()
        | ProviderCaps::SYSTEM_PROMPT.bits(),
);

// ---------------------------------------------------------------------------
// Model cost / latency for routing decisions
// ---------------------------------------------------------------------------

/// Provider routing weight — used by the negotiator for cost-optimal selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderWeight {
    /// Provider name (e.g., "anthropic", "openai", "ollama").
    pub provider: String,
    /// Model name within the provider.
    pub model: String,
    /// Cost per million input tokens (USD).
    pub cost_per_m_input: f64,
    /// Cost per million output tokens (USD).
    pub cost_per_m_output: f64,
    /// Median latency estimate (milliseconds).
    pub latency_p50_ms: u32,
    /// Provider capabilities.
    pub caps: ProviderCaps,
    /// Quality tier (higher = better quality).
    pub quality_tier: u8,
}

impl ProviderWeight {
    /// Composite routing score: lower is better.
    /// `score = cost_weight × cost + latency_weight × latency_norm`
    pub fn routing_score(&self, cost_weight: f64, latency_weight: f64) -> f64 {
        let cost_norm = (self.cost_per_m_input + self.cost_per_m_output) / 2.0;
        let latency_norm = self.latency_p50_ms as f64 / 1000.0;
        cost_weight * cost_norm + latency_weight * latency_norm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_satisfies() {
        let provider = ANTHROPIC_CAPS;
        let required = ProviderCaps::TEXT_COMPLETION | ProviderCaps::STREAMING;
        assert!(provider.satisfies(required));
    }

    #[test]
    fn capability_does_not_satisfy() {
        let provider = OLLAMA_CAPS;
        let required = ProviderCaps::TOOL_USE;
        assert!(!provider.satisfies(required));
    }

    #[test]
    fn capability_union() {
        let a = ProviderCaps::TEXT_COMPLETION;
        let b = ProviderCaps::STREAMING;
        let c = a | b;
        assert!(c.has(ProviderCaps::TEXT_COMPLETION));
        assert!(c.has(ProviderCaps::STREAMING));
        assert!(!c.has(ProviderCaps::VISION));
    }

    #[test]
    fn capability_count() {
        assert_eq!(ANTHROPIC_CAPS.count(), 7);
        assert_eq!(OLLAMA_CAPS.count(), 5);
        assert_eq!(ProviderCaps::NONE.count(), 0);
    }

    #[test]
    fn capability_display() {
        let caps = ProviderCaps::TEXT_COMPLETION | ProviderCaps::STREAMING;
        let s = format!("{}", caps);
        assert!(s.contains("text"));
        assert!(s.contains("streaming"));
    }

    #[test]
    fn routing_score_prefers_cheaper() {
        let cheap = ProviderWeight {
            provider: "ollama".into(),
            model: "llama3".into(),
            cost_per_m_input: 0.0,
            cost_per_m_output: 0.0,
            latency_p50_ms: 50,
            caps: OLLAMA_CAPS,
            quality_tier: 2,
        };
        let expensive = ProviderWeight {
            provider: "openai".into(),
            model: "gpt-4o".into(),
            cost_per_m_input: 5.0,
            cost_per_m_output: 15.0,
            latency_p50_ms: 200,
            caps: OPENAI_CAPS,
            quality_tier: 5,
        };
        assert!(cheap.routing_score(1.0, 0.0) < expensive.routing_score(1.0, 0.0));
    }
}
