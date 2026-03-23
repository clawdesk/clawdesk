//! Temporal decay scoring — configurable decay profiles for memory recency.
//!
//! ## Decay Functions
//!
//! **Exponential** (default for chat):
//! ```text
//! w(t) = 2^(-t / h)        half-life semantics
//! ```
//!
//! **Logarithmic** (default for documents):
//! ```text
//! w(t) = 1 / (1 + α × ln(1 + t/τ))
//! ```
//! Slow decay — good for documentation and reference material.
//!
//! **Power law** (Ebbinghaus forgetting curve):
//! ```text
//! w(t) = (1 + t/τ)^(-β)
//! ```
//! Matches human memory decay patterns.
//!
//! **None** (for pinned memories):
//! ```text
//! w(t) = 1.0
//! ```
//!
//! Applied as post-multiplication on search scores:
//! ```text
//! s_final = s_similarity × w(t_now − t_memory)
//! ```
//!
//! Total cost: O(1) per memory, O(k) for k candidates.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Configuration for temporal decay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalDecayConfig {
    /// Half-life in days. After this many days, a memory's score is halved.
    pub half_life_days: f64,
    /// Minimum decay factor (floor). Prevents ancient memories from
    /// scoring exactly zero.
    pub min_decay: f64,
    /// Whether to apply decay at all. Set to false to disable.
    pub enabled: bool,
}

impl Default for TemporalDecayConfig {
    fn default() -> Self {
        Self {
            half_life_days: 180.0,
            min_decay: 0.15,
            enabled: true,
        }
    }
}

/// Decay function profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DecayProfile {
    /// Exponential: w(t) = 2^(-t/h). Good for fast-fading memories.
    Exponential {
        /// Half-life in days.
        half_life_days: f64,
    },
    /// Logarithmic: w(t) = 1/(1 + α×ln(1 + t/τ)). Slow decay for reference material.
    Logarithmic {
        /// Scaling factor (higher = faster decay).
        alpha: f64,
        /// Time constant in days.
        tau_days: f64,
    },
    /// Power law (Ebbinghaus): w(t) = (1 + t/τ)^(-β).
    PowerLaw {
        /// Exponent (higher = faster decay).
        beta: f64,
        /// Time constant in days.
        tau_days: f64,
    },
    /// No decay — memory retains full weight forever.
    None,
}

impl Default for DecayProfile {
    fn default() -> Self {
        Self::Exponential {
            half_life_days: 180.0,
        }
    }
}

/// Memory type classification for per-type decay profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryType {
    /// Chat messages — fast decay (default: exponential, h=7 days).
    Chat,
    /// Documents/reference — slow decay (default: logarithmic).
    Document,
    /// Code/technical — moderate decay (default: power-law).
    Code,
    /// Pinned/important — no decay.
    Pinned,
    /// Episodic memories — moderate exponential.
    Episodic,
    /// Custom type (uses default profile).
    Custom,
}

/// Per-memory-type decay profile configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedDecayConfig {
    /// Whether typed decay is enabled. If false, uses the global config.
    pub enabled: bool,
    /// Minimum decay factor (floor) for all profiles.
    pub min_decay: f64,
    /// Profile for chat memories (default half-life 7 days).
    pub chat: DecayProfile,
    /// Profile for document memories (default logarithmic).
    pub document: DecayProfile,
    /// Profile for code memories (default power-law).
    pub code: DecayProfile,
    /// Profile for pinned memories (always None).
    pub pinned: DecayProfile,
    /// Profile for episodic memories.
    pub episodic: DecayProfile,
    /// Default profile for custom/unclassified memory types.
    pub default: DecayProfile,
}

impl Default for TypedDecayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_decay: 0.10,
            chat: DecayProfile::Exponential {
                half_life_days: 7.0,
            },
            document: DecayProfile::Logarithmic {
                alpha: 0.3,
                tau_days: 30.0,
            },
            code: DecayProfile::PowerLaw {
                beta: 0.5,
                tau_days: 14.0,
            },
            pinned: DecayProfile::None,
            episodic: DecayProfile::Exponential {
                half_life_days: 90.0,
            },
            default: DecayProfile::Exponential {
                half_life_days: 180.0,
            },
        }
    }
}

impl TypedDecayConfig {
    /// Get the profile for a given memory type.
    pub fn profile_for(&self, memory_type: MemoryType) -> &DecayProfile {
        match memory_type {
            MemoryType::Chat => &self.chat,
            MemoryType::Document => &self.document,
            MemoryType::Code => &self.code,
            MemoryType::Pinned => &self.pinned,
            MemoryType::Episodic => &self.episodic,
            MemoryType::Custom => &self.default,
        }
    }
}

/// Compute the decay factor using a specific profile, O(1).
pub fn decay_factor_profile(age_days: f64, profile: &DecayProfile, min_decay: f64) -> f64 {
    if age_days <= 0.0 {
        return 1.0;
    }
    let raw = match profile {
        DecayProfile::Exponential { half_life_days } => {
            2.0_f64.powf(-age_days / half_life_days)
        }
        DecayProfile::Logarithmic { alpha, tau_days } => {
            1.0 / (1.0 + alpha * (1.0 + age_days / tau_days).ln())
        }
        DecayProfile::PowerLaw { beta, tau_days } => {
            (1.0 + age_days / tau_days).powf(-beta)
        }
        DecayProfile::None => 1.0,
    };
    raw.max(min_decay)
}

/// Compute the typed decay factor from a timestamp and memory type.
pub fn typed_decay_factor(
    timestamp: &DateTime<Utc>,
    now: &DateTime<Utc>,
    memory_type: MemoryType,
    config: &TypedDecayConfig,
) -> f64 {
    if !config.enabled {
        return 1.0;
    }
    let age = now.signed_duration_since(timestamp);
    let age_days = age.num_seconds() as f64 / 86400.0;
    let profile = config.profile_for(memory_type);
    decay_factor_profile(age_days, profile, config.min_decay)
}

/// Apply typed temporal decay to scored results.
///
/// Memory type is extracted from metadata field `"memory_type"`.
/// If absent, defaults to `MemoryType::Custom`.
pub fn apply_typed_temporal_decay(
    results: &mut Vec<(String, f32, serde_json::Value)>,
    config: &TypedDecayConfig,
) {
    if !config.enabled {
        return;
    }
    let now = Utc::now();

    for (_id, score, metadata) in results.iter_mut() {
        let ts = extract_timestamp(metadata);
        let mem_type = extract_memory_type(metadata);

        if let Some(ts) = ts {
            let factor = typed_decay_factor(&ts, &now, mem_type, config) as f32;
            *score *= factor;
        }
    }

    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
}

/// Extract memory type from metadata (field `"memory_type"`).
pub fn extract_memory_type(metadata: &serde_json::Value) -> MemoryType {
    metadata
        .get("memory_type")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "chat" => MemoryType::Chat,
            "document" | "doc" => MemoryType::Document,
            "code" => MemoryType::Code,
            "pinned" => MemoryType::Pinned,
            "episodic" => MemoryType::Episodic,
            _ => MemoryType::Custom,
        })
        .unwrap_or(MemoryType::Custom)
}

/// Compute the exponential decay factor for a given age.
///
/// Returns a value in `[min_decay, 1.0]`.
pub fn decay_factor(age_days: f64, config: &TemporalDecayConfig) -> f64 {
    if !config.enabled || age_days <= 0.0 {
        return 1.0;
    }
    let raw = 2.0_f64.powf(-age_days / config.half_life_days);
    raw.max(config.min_decay)
}

/// Compute the decay factor from a timestamp.
pub fn decay_factor_from_timestamp(
    timestamp: &DateTime<Utc>,
    now: &DateTime<Utc>,
    config: &TemporalDecayConfig,
) -> f64 {
    let age = now.signed_duration_since(timestamp);
    let age_days = age.num_seconds() as f64 / 86400.0;
    decay_factor(age_days, config)
}

/// Extract a timestamp from serde_json metadata.
///
/// Tries the `"timestamp"` field as an RFC3339 string.
pub fn extract_timestamp(metadata: &serde_json::Value) -> Option<DateTime<Utc>> {
    metadata
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Apply temporal decay to a set of scored results in-place.
///
/// Each result's score is multiplied by `decay_factor(age)`.
/// Results are re-sorted by decayed score in descending order.
pub fn apply_temporal_decay(
    results: &mut Vec<(String, f32, serde_json::Value)>,
    config: &TemporalDecayConfig,
) {
    if !config.enabled {
        return;
    }
    let now = Utc::now();

    for (_id, score, metadata) in results.iter_mut() {
        if let Some(ts) = extract_timestamp(metadata) {
            let factor = decay_factor_from_timestamp(&ts, &now, config) as f32;
            *score *= factor;
        }
        // If no timestamp, leave score unchanged (benefit of the doubt)
    }

    // Re-sort by decayed score
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_zero_age() {
        let config = TemporalDecayConfig::default();
        assert!((decay_factor(0.0, &config) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn decay_one_half_life() {
        let config = TemporalDecayConfig::default(); // 180 days
        let factor = decay_factor(180.0, &config);
        assert!((factor - 0.5).abs() < 1e-6);
    }

    #[test]
    fn decay_two_half_lives() {
        let config = TemporalDecayConfig::default();
        let factor = decay_factor(360.0, &config);
        assert!((factor - 0.25).abs() < 1e-6);
    }

    #[test]
    fn decay_respects_floor() {
        let config = TemporalDecayConfig {
            half_life_days: 1.0,
            min_decay: 0.05,
            enabled: true,
        };
        // After 100 half-lives, raw decay would be ~0, but floor is 0.05
        let factor = decay_factor(100.0, &config);
        assert!((factor - 0.05).abs() < 1e-6);
    }

    #[test]
    fn decay_disabled() {
        let config = TemporalDecayConfig {
            enabled: false,
            ..Default::default()
        };
        let factor = decay_factor(365.0, &config);
        assert!((factor - 1.0).abs() < 1e-6);
    }

    #[test]
    fn extract_timestamp_valid() {
        let meta = serde_json::json!({
            "timestamp": "2025-01-15T10:30:00Z"
        });
        let ts = extract_timestamp(&meta);
        assert!(ts.is_some());
    }

    #[test]
    fn extract_timestamp_missing() {
        let meta = serde_json::json!({});
        let ts = extract_timestamp(&meta);
        assert!(ts.is_none());
    }

    // ── New profile tests ──────────────────────────────────────

    #[test]
    fn logarithmic_decay_slow() {
        let profile = DecayProfile::Logarithmic {
            alpha: 0.3,
            tau_days: 30.0,
        };
        // 30 days: w = 1/(1 + 0.3*ln(2)) ≈ 0.828
        let factor = decay_factor_profile(30.0, &profile, 0.1);
        assert!(factor > 0.8 && factor < 0.9);
        // 365 days should still be > 0.5 (slow decay)
        let factor_year = decay_factor_profile(365.0, &profile, 0.1);
        assert!(factor_year > 0.5);
    }

    #[test]
    fn power_law_ebbinghaus() {
        let profile = DecayProfile::PowerLaw {
            beta: 0.5,
            tau_days: 14.0,
        };
        // At t=0: w=1.0
        assert!((decay_factor_profile(0.0, &profile, 0.1) - 1.0).abs() < 1e-6);
        // At t=14: w = (1 + 1)^(-0.5) = sqrt(0.5) ≈ 0.707
        let f14 = decay_factor_profile(14.0, &profile, 0.1);
        assert!((f14 - 0.7071).abs() < 0.01);
    }

    #[test]
    fn none_profile_no_decay() {
        let profile = DecayProfile::None;
        let factor = decay_factor_profile(10000.0, &profile, 0.1);
        assert!((factor - 1.0).abs() < 1e-6);
    }

    #[test]
    fn typed_config_chat_is_fast() {
        let config = TypedDecayConfig::default(); // chat: h=7 days
        let chat_profile = config.profile_for(MemoryType::Chat);
        let doc_profile = config.profile_for(MemoryType::Document);

        // After 7 days, chat should be ~0.5, document should be > 0.8
        let chat_7 = decay_factor_profile(7.0, chat_profile, config.min_decay);
        let doc_7 = decay_factor_profile(7.0, doc_profile, config.min_decay);
        assert!(chat_7 < doc_7);
        assert!((chat_7 - 0.5).abs() < 0.01);
    }

    #[test]
    fn pinned_never_decays() {
        let config = TypedDecayConfig::default();
        let profile = config.profile_for(MemoryType::Pinned);
        let factor = decay_factor_profile(3650.0, profile, config.min_decay);
        assert!((factor - 1.0).abs() < 1e-6);
    }

    #[test]
    fn extract_memory_type_from_metadata() {
        assert_eq!(
            extract_memory_type(&serde_json::json!({"memory_type": "chat"})),
            MemoryType::Chat
        );
        assert_eq!(
            extract_memory_type(&serde_json::json!({"memory_type": "pinned"})),
            MemoryType::Pinned
        );
        assert_eq!(
            extract_memory_type(&serde_json::json!({})),
            MemoryType::Custom
        );
    }
}
