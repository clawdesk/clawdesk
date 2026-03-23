//! Degraded mode for the gateway when dependencies are unavailable.
//!
//! When the circuit breaker registry reports unhealthy dependencies,
//! this module determines what functionality remains available and
//! adapts the gateway's behavior accordingly.
//!
//! ## Degradation Tiers
//!
//! ```text
//! Tier 0 — Full: All systems operational
//! Tier 1 — Degraded Memory: Vector DB down → keyword search fallback
//! Tier 2 — Degraded Models: Primary LLM down → fallback provider
//! Tier 3 — Degraded Channels: Some channels unavailable → queue messages
//! Tier 4 — Core Only: Only direct chat remains functional
//! ```
//!
//! ## Always Available
//!
//! Even in maximum degradation, these remain:
//! - Health check endpoint
//! - WebSocket connection (may return degraded status)
//! - Session listing (from cache)
//! - Configuration endpoints

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::{info, warn};

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// Current degradation tier of the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DegradationTier {
    /// All systems operational.
    Full,
    /// Memory/retrieval degraded — using fallback search.
    DegradedMemory,
    /// LLM provider degraded — using fallback model.
    DegradedModels,
    /// Some channels unavailable — messages queued.
    DegradedChannels,
    /// Minimum viable: direct chat only.
    CoreOnly,
}

/// Capabilities that may be degraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    VectorSearch,
    EmbeddingGeneration,
    PrimaryLlm,
    FallbackLlm,
    ChannelSend,
    ChannelReceive,
    ToolExecution,
    SkillDiscovery,
    AuditLogging,
    CronScheduling,
}

/// Snapshot of the gateway's degraded state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DegradedModeStatus {
    /// Current degradation tier.
    pub tier: DegradationTier,
    /// Capabilities that are currently unavailable.
    pub unavailable: HashSet<Capability>,
    /// Capabilities that are degraded (using fallbacks).
    pub degraded: HashSet<Capability>,
    /// Human-readable status message.
    pub message: String,
    /// Whether chat is still functional (should always be true).
    pub chat_available: bool,
}

/// Evaluates the system's degradation state from dependency health.
pub struct DegradedModeEvaluator;

impl DegradedModeEvaluator {
    /// Evaluate degradation tier based on which dependencies are unhealthy.
    ///
    /// `unhealthy` contains the names of dependencies whose circuit breakers
    /// are currently open.
    pub fn evaluate(unhealthy: &[String]) -> DegradedModeStatus {
        let unhealthy_set: HashSet<&str> = unhealthy.iter().map(|s| s.as_str()).collect();
        let mut unavailable = HashSet::new();
        let mut degraded = HashSet::new();

        // Classify by capability impact
        let vector_db_down = unhealthy_set.iter().any(|n| {
            n.contains("sochdb") || n.contains("vector") || n.contains("database")
        });
        let embedding_down = unhealthy_set.iter().any(|n| {
            n.contains("embedding") || n.contains("voyage") || n.contains("openai_embed")
        });
        let primary_llm_down = unhealthy_set.iter().any(|n| {
            n.contains("claude") || n.contains("anthropic") || n.contains("primary_llm")
        });
        let fallback_llm_down = unhealthy_set.iter().any(|n| {
            n.contains("gpt") || n.contains("openai") || n.contains("fallback_llm")
        });
        let channel_down = unhealthy_set.iter().any(|n| {
            n.contains("slack") || n.contains("discord") || n.contains("telegram")
                || n.contains("channel")
        });

        if vector_db_down {
            unavailable.insert(Capability::VectorSearch);
            degraded.insert(Capability::VectorSearch); // falls back to keyword
        }
        if embedding_down {
            unavailable.insert(Capability::EmbeddingGeneration);
        }
        if primary_llm_down {
            if fallback_llm_down {
                unavailable.insert(Capability::PrimaryLlm);
                unavailable.insert(Capability::FallbackLlm);
            } else {
                degraded.insert(Capability::PrimaryLlm);
            }
        }
        if channel_down {
            degraded.insert(Capability::ChannelSend);
        }

        // Determine tier
        let all_llm_down = primary_llm_down && fallback_llm_down;
        let tier = if unhealthy.is_empty() {
            DegradationTier::Full
        } else if all_llm_down {
            DegradationTier::CoreOnly
        } else if primary_llm_down {
            DegradationTier::DegradedModels
        } else if channel_down {
            DegradationTier::DegradedChannels
        } else if vector_db_down || embedding_down {
            DegradationTier::DegradedMemory
        } else {
            DegradationTier::DegradedMemory
        };

        let message = match tier {
            DegradationTier::Full => "All systems operational".to_string(),
            DegradationTier::DegradedMemory => format!(
                "Memory/retrieval degraded: {}. Using keyword search fallback.",
                unhealthy.join(", ")
            ),
            DegradationTier::DegradedModels => format!(
                "Primary LLM unavailable. Using fallback provider. Affected: {}",
                unhealthy.join(", ")
            ),
            DegradationTier::DegradedChannels => format!(
                "Some channels unavailable: {}. Messages are being queued.",
                unhealthy.join(", ")
            ),
            DegradationTier::CoreOnly => format!(
                "Most systems down: {}. Only direct chat available.",
                unhealthy.join(", ")
            ),
        };

        if tier != DegradationTier::Full {
            warn!(
                tier = ?tier,
                unhealthy_count = unhealthy.len(),
                "Gateway operating in degraded mode"
            );
        }

        DegradedModeStatus {
            tier,
            unavailable,
            degraded,
            message,
            // Chat is available as long as at least one LLM is up
            chat_available: !all_llm_down,
        }
    }

    /// Check if a specific capability should use its degraded fallback.
    pub fn should_use_fallback(status: &DegradedModeStatus, cap: Capability) -> bool {
        status.degraded.contains(&cap)
    }

    /// Check if a capability is completely unavailable.
    pub fn is_unavailable(status: &DegradedModeStatus, cap: Capability) -> bool {
        status.unavailable.contains(&cap) && !status.degraded.contains(&cap)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_when_nothing_unhealthy() {
        let status = DegradedModeEvaluator::evaluate(&[]);
        assert_eq!(status.tier, DegradationTier::Full);
        assert!(status.chat_available);
        assert!(status.unavailable.is_empty());
    }

    #[test]
    fn degraded_memory_when_vector_db_down() {
        let status = DegradedModeEvaluator::evaluate(&["sochdb".to_string()]);
        assert_eq!(status.tier, DegradationTier::DegradedMemory);
        assert!(status.chat_available);
        assert!(status.degraded.contains(&Capability::VectorSearch));
    }

    #[test]
    fn degraded_models_when_primary_llm_down() {
        let status = DegradedModeEvaluator::evaluate(&["claude".to_string()]);
        assert_eq!(status.tier, DegradationTier::DegradedModels);
        assert!(status.chat_available);
        assert!(status.degraded.contains(&Capability::PrimaryLlm));
    }

    #[test]
    fn core_only_when_all_llms_down() {
        let status = DegradedModeEvaluator::evaluate(&[
            "claude".to_string(),
            "gpt".to_string(),
        ]);
        assert_eq!(status.tier, DegradationTier::CoreOnly);
        assert!(!status.chat_available);
    }

    #[test]
    fn degraded_channels_when_channel_down() {
        let status = DegradedModeEvaluator::evaluate(&["slack".to_string()]);
        assert_eq!(status.tier, DegradationTier::DegradedChannels);
        assert!(status.chat_available);
        assert!(status.degraded.contains(&Capability::ChannelSend));
    }

    #[test]
    fn fallback_check() {
        let status = DegradedModeEvaluator::evaluate(&["sochdb".to_string()]);
        assert!(DegradedModeEvaluator::should_use_fallback(
            &status,
            Capability::VectorSearch
        ));
        assert!(!DegradedModeEvaluator::should_use_fallback(
            &status,
            Capability::PrimaryLlm
        ));
    }
}
