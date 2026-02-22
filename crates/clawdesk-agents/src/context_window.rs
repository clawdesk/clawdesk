//! Context window discovery cache with multi-source resolution.
//!
//! Multi-source context window resolution system:
//! 1. **Discovery Cache**: When models are scanned, context windows are cached
//!    per model ID. Duplicate discoveries keep the **minimum** (conservative safety).
//! 2. **Config Override**: User-specified `contextWindow` overrides discovered values.
//! 3. **Agent-Level Cap**: Global agent context token cap applies below model limit.
//! 4. **Source Tracking**: Every resolved context window carries its `source` for debugging.
//! 5. **Guard Evaluation**: Windows below 32K warn, below 16K block.
//!
//! ## Math/Algo
//!
//! The min-dedup strategy is a *pessimistic estimator*: if model `m` has true
//! context window `C` and discoveries report `{c_1, ..., c_k}` where `c_i ∈ [C-ε, C+ε]`,
//! then `min(c_i) ≤ C` with probability `1 - (1 - P(c_i < C))^k → 1` as `k → ∞`.
//! Guarantees no overflow at the cost of slight under-utilization.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

/// Source of a resolved context window value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextWindowSource {
    /// Discovered from model metadata (e.g., OpenRouter model list).
    Model,
    /// User-configured override in `models.providers.<provider>.models[].contextWindow`.
    ModelsConfig,
    /// Agent-level cap from `agents.defaults.contextTokens`.
    AgentContextTokens,
    /// Default fallback value.
    Default,
}

impl std::fmt::Display for ContextWindowSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model => write!(f, "model"),
            Self::ModelsConfig => write!(f, "modelsConfig"),
            Self::AgentContextTokens => write!(f, "agentContextTokens"),
            Self::Default => write!(f, "default"),
        }
    }
}

/// A resolved context window with source tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedContextWindow {
    /// Effective context window size in tokens.
    pub tokens: usize,
    /// Which source determined this value.
    pub source: ContextWindowSource,
    /// All discovered values (for debugging).
    pub discoveries: Vec<usize>,
}

/// Result of guard evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextWindowGuard {
    /// Context window is adequate.
    Ok,
    /// Context window is small but usable — emit warning.
    Warning {
        tokens: usize,
        message: String,
    },
    /// Context window is too small — block execution.
    Blocked {
        tokens: usize,
        message: String,
    },
}

/// Cache for context window discoveries.
///
/// Stores per-model context windows, keeping the **minimum** for duplicate
/// discoveries (pessimistic/safe strategy).
pub struct ContextWindowCache {
    /// model_id → (min_discovered_window, all_discoveries)
    cache: HashMap<String, (usize, Vec<usize>)>,
    /// model_id → user-configured override
    config_overrides: HashMap<String, usize>,
    /// Global agent-level cap (None = no cap).
    agent_cap: Option<usize>,
    /// Default fallback window size.
    default_window: usize,
    /// Minimum acceptable window (blocking threshold).
    min_blocking_threshold: usize,
    /// Warning threshold.
    warning_threshold: usize,
}

impl ContextWindowCache {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            config_overrides: HashMap::new(),
            agent_cap: None,
            default_window: 128_000,
            min_blocking_threshold: 16_000,
            warning_threshold: 32_000,
        }
    }

    /// Set the default fallback window size.
    pub fn with_default(mut self, default: usize) -> Self {
        self.default_window = default;
        self
    }

    /// Set the agent-level context token cap.
    pub fn with_agent_cap(mut self, cap: usize) -> Self {
        self.agent_cap = Some(cap);
        self
    }

    /// Set the blocking threshold (windows below this block execution).
    pub fn with_blocking_threshold(mut self, threshold: usize) -> Self {
        self.min_blocking_threshold = threshold;
        self
    }

    /// Set the warning threshold.
    pub fn with_warning_threshold(mut self, threshold: usize) -> Self {
        self.warning_threshold = threshold;
        self
    }

    /// Register a user-configured context window override.
    pub fn set_config_override(&mut self, model_id: &str, window: usize) {
        self.config_overrides.insert(model_id.to_string(), window);
    }

    /// Record a discovered context window for a model.
    ///
    /// If the model already has discoveries, keeps the **minimum** (conservative).
    pub fn record_discovery(&mut self, model_id: &str, window: usize) {
        let entry = self
            .cache
            .entry(model_id.to_string())
            .or_insert((window, Vec::new()));

        entry.1.push(window);
        entry.0 = entry.0.min(window);

        debug!(
            model = model_id,
            window,
            effective_min = entry.0,
            discoveries = entry.1.len(),
            "context window discovery recorded"
        );
    }

    /// Resolve the effective context window for a model.
    ///
    /// Priority: config_override > discovered_min > agent_cap > default.
    /// The final value is `min(resolved_source, agent_cap)`.
    pub fn resolve(&self, model_id: &str) -> ResolvedContextWindow {
        // 1. Check config override
        if let Some(&override_window) = self.config_overrides.get(model_id) {
            let effective = match self.agent_cap {
                Some(cap) if cap < override_window => cap,
                _ => override_window,
            };
            return ResolvedContextWindow {
                tokens: effective,
                source: if effective == override_window {
                    ContextWindowSource::ModelsConfig
                } else {
                    ContextWindowSource::AgentContextTokens
                },
                discoveries: self
                    .cache
                    .get(model_id)
                    .map(|(_, d)| d.clone())
                    .unwrap_or_default(),
            };
        }

        // 2. Check discovered values
        if let Some((min_discovered, discoveries)) = self.cache.get(model_id) {
            let effective = match self.agent_cap {
                Some(cap) if cap < *min_discovered => cap,
                _ => *min_discovered,
            };
            return ResolvedContextWindow {
                tokens: effective,
                source: if effective == *min_discovered {
                    ContextWindowSource::Model
                } else {
                    ContextWindowSource::AgentContextTokens
                },
                discoveries: discoveries.clone(),
            };
        }

        // 3. Agent cap or default
        let effective = match self.agent_cap {
            Some(cap) if cap < self.default_window => cap,
            _ => self.default_window,
        };

        ResolvedContextWindow {
            tokens: effective,
            source: if self.agent_cap.is_some() && self.agent_cap.unwrap() < self.default_window {
                ContextWindowSource::AgentContextTokens
            } else {
                ContextWindowSource::Default
            },
            discoveries: vec![],
        }
    }

    /// Evaluate the guard for a resolved context window.
    pub fn evaluate_guard(&self, resolved: &ResolvedContextWindow) -> ContextWindowGuard {
        if resolved.tokens < self.min_blocking_threshold {
            ContextWindowGuard::Blocked {
                tokens: resolved.tokens,
                message: format!(
                    "Context window {}K tokens is below minimum {}K. \
                     Source: {}. This will cause immediate overflow.",
                    resolved.tokens / 1000,
                    self.min_blocking_threshold / 1000,
                    resolved.source,
                ),
            }
        } else if resolved.tokens < self.warning_threshold {
            ContextWindowGuard::Warning {
                tokens: resolved.tokens,
                message: format!(
                    "Context window {}K tokens is small (source: {}). \
                     Consider using a model with larger context.",
                    resolved.tokens / 1000,
                    resolved.source,
                ),
            }
        } else {
            ContextWindowGuard::Ok
        }
    }

    /// Resolve and evaluate in one step.
    pub fn resolve_and_evaluate(&self, model_id: &str) -> (ResolvedContextWindow, ContextWindowGuard) {
        let resolved = self.resolve(model_id);
        let guard = self.evaluate_guard(&resolved);
        if let ContextWindowGuard::Warning { ref message, .. } = guard {
            warn!(model = model_id, "{}", message);
        }
        (resolved, guard)
    }

    /// Get all cached model IDs.
    pub fn cached_models(&self) -> Vec<String> {
        self.cache.keys().cloned().collect()
    }

    /// Clear all discoveries (but keep config overrides).
    pub fn clear_discoveries(&mut self) {
        self.cache.clear();
    }
}

impl Default for ContextWindowCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovery_keeps_minimum() {
        let mut cache = ContextWindowCache::new();
        cache.record_discovery("claude-opus-4-20250514", 200_000);
        cache.record_discovery("claude-opus-4-20250514", 180_000);
        cache.record_discovery("claude-opus-4-20250514", 200_000);

        let resolved = cache.resolve("claude-opus-4-20250514");
        assert_eq!(resolved.tokens, 180_000);
        assert_eq!(resolved.source, ContextWindowSource::Model);
        assert_eq!(resolved.discoveries.len(), 3);
    }

    #[test]
    fn test_config_override_takes_precedence() {
        let mut cache = ContextWindowCache::new();
        cache.record_discovery("claude-opus-4-20250514", 200_000);
        cache.set_config_override("claude-opus-4-20250514", 100_000);

        let resolved = cache.resolve("claude-opus-4-20250514");
        assert_eq!(resolved.tokens, 100_000);
        assert_eq!(resolved.source, ContextWindowSource::ModelsConfig);
    }

    #[test]
    fn test_agent_cap_applies() {
        let mut cache = ContextWindowCache::new().with_agent_cap(64_000);
        cache.record_discovery("gpt-4o", 128_000);

        let resolved = cache.resolve("gpt-4o");
        assert_eq!(resolved.tokens, 64_000);
        assert_eq!(resolved.source, ContextWindowSource::AgentContextTokens);
    }

    #[test]
    fn test_default_fallback() {
        let cache = ContextWindowCache::new().with_default(128_000);
        let resolved = cache.resolve("unknown-model");
        assert_eq!(resolved.tokens, 128_000);
        assert_eq!(resolved.source, ContextWindowSource::Default);
    }

    #[test]
    fn test_guard_blocks_small_window() {
        let cache = ContextWindowCache::new();
        let resolved = ResolvedContextWindow {
            tokens: 8_000,
            source: ContextWindowSource::Model,
            discoveries: vec![8_000],
        };
        let guard = cache.evaluate_guard(&resolved);
        assert!(matches!(guard, ContextWindowGuard::Blocked { .. }));
    }

    #[test]
    fn test_guard_warns_medium_window() {
        let cache = ContextWindowCache::new();
        let resolved = ResolvedContextWindow {
            tokens: 24_000,
            source: ContextWindowSource::Model,
            discoveries: vec![24_000],
        };
        let guard = cache.evaluate_guard(&resolved);
        assert!(matches!(guard, ContextWindowGuard::Warning { .. }));
    }

    #[test]
    fn test_guard_ok_large_window() {
        let cache = ContextWindowCache::new();
        let resolved = ResolvedContextWindow {
            tokens: 128_000,
            source: ContextWindowSource::Model,
            discoveries: vec![128_000],
        };
        let guard = cache.evaluate_guard(&resolved);
        assert_eq!(guard, ContextWindowGuard::Ok);
    }
}
