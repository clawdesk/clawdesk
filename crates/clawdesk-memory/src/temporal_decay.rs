//! Temporal decay scoring — exponential half-life decay for memory recency.
//!
//! ## Algorithm
//!
//! ```text
//! decay(Δt) = 2^(-Δt / h)
//! ```
//!
//! Where:
//! - Δt = age of memory in days
//! - h = half-life in days (default 30)
//!
//! Applied as post-multiplication on hybrid search scores:
//! ```text
//! score_final = score_hybrid × decay(Δt)
//! ```
//!
//! ## Properties
//!
//! - decay(0) = 1.0 (just created — full weight)
//! - decay(h) = 0.5 (one half-life — half weight)
//! - decay(2h) = 0.25 (two half-lives — quarter weight)
//! - Order-preserving within a time window

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
}
