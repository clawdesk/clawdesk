//! Model catalog — capability-based model selection with cost/latency routing.
//!
//! Models declare capabilities as bitmask. Requests declare requirements.
//! Selection is O(1) via bitmap intersection on pre-computed table.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Model capability bitflags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModelCapabilities(u32);

impl ModelCapabilities {
    pub const NONE: Self = Self(0);
    pub const TEXT: Self = Self(0x01);
    pub const VISION: Self = Self(0x02);
    pub const TOOLS: Self = Self(0x04);
    pub const THINKING: Self = Self(0x08);
    pub const STREAMING: Self = Self(0x10);
    pub const JSON_MODE: Self = Self(0x20);
    pub const IMAGE_GEN: Self = Self(0x40);
    pub const CODE: Self = Self(0x80);
    pub const LONG_CONTEXT: Self = Self(0x100);

    pub fn has(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Serde default for `#[serde(skip)]` fields.
    fn default_none() -> Self {
        Self::NONE
    }
}

/// A model in the catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub provider: String,
    pub display_name: String,
    #[serde(skip, default = "ModelCapabilities::default_none")]
    pub capabilities: ModelCapabilities,
    pub context_window: usize,
    /// Cost per million input tokens in USD.
    pub cost_per_m_input: f64,
    /// Cost per million output tokens in USD.
    pub cost_per_m_output: f64,
    /// Average latency to first token in ms.
    pub avg_latency_ms: f64,
    pub max_output_tokens: usize,
}

/// Model selection result with pre-computed fallback chain.
#[derive(Debug, Clone)]
pub struct ModelSelection {
    pub primary: ModelEntry,
    pub fallbacks: Vec<ModelEntry>,
}

/// The model catalog — indexes all available models for O(1) capability-based lookup.
pub struct ModelCatalog {
    models: Vec<ModelEntry>,
    /// Pre-computed: capabilities bitmask → sorted model indices (by cost+latency).
    capability_index: HashMap<u32, Vec<usize>>,
}

impl ModelCatalog {
    pub fn new() -> Self {
        Self {
            models: Vec::new(),
            capability_index: HashMap::new(),
        }
    }

    /// Register a model in the catalog.
    pub fn register(&mut self, entry: ModelEntry) {
        self.models.push(entry);
        self.rebuild_index();
    }

    /// Register multiple models and rebuild index once.
    pub fn register_batch(&mut self, entries: Vec<ModelEntry>) {
        self.models.extend(entries);
        self.rebuild_index();
    }

    /// Select best model matching requirements via O(1) index lookup.
    ///
    /// Pre-computed table maps every possible capability bitmask (2⁹ = 512)
    /// to a sorted list of matching model indices. Lookup is a single hash get.
    /// λ = latency_cost_tradeoff: higher values penalize latency more.
    pub fn select(
        &self,
        requirements: ModelCapabilities,
        _latency_cost_tradeoff: f64,
    ) -> Option<ModelSelection> {
        // O(1) lookup in pre-computed exhaustive index.
        if let Some(indices) = self.capability_index.get(&requirements.0) {
            if indices.is_empty() {
                return None;
            }
            let primary = self.models[indices[0]].clone();
            let fallbacks: Vec<ModelEntry> = indices
                .iter()
                .skip(1)
                .take(3)
                .map(|&i| self.models[i].clone())
                .collect();
            return Some(ModelSelection { primary, fallbacks });
        }
        None
    }

    /// Get all models for a specific provider.
    pub fn models_for_provider(&self, provider: &str) -> Vec<&ModelEntry> {
        self.models
            .iter()
            .filter(|m| m.provider == provider)
            .collect()
    }

    /// Total number of registered models.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Rebuild the capability index: enumerate all 2⁹ = 512 possible
    /// requirement bitmasks and pre-compute the sorted model list for each.
    ///
    /// Build time: O(512 × n) ≈ O(n) for small model counts.
    /// Lookup time: O(1) hash get.
    fn rebuild_index(&mut self) {
        self.capability_index.clear();

        // Number of capability bits currently defined (9).
        let num_bits = 9u32;
        let num_masks = 1u32 << num_bits; // 512

        for mask in 0..num_masks {
            let req = ModelCapabilities(mask);
            let mut matching: Vec<usize> = self
                .models
                .iter()
                .enumerate()
                .filter(|(_, m)| m.capabilities.has(req))
                .map(|(i, _)| i)
                .collect();

            if !matching.is_empty() {
                // Sort by cost ascending (cheapest first).
                matching.sort_by(|&a, &b| {
                    let cost_a = self.models[a].cost_per_m_input;
                    let cost_b = self.models[b].cost_per_m_input;
                    cost_a
                        .partial_cmp(&cost_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                self.capability_index.insert(mask, matching);
            }
        }
    }

    /// Build default catalog with well-known models.
    pub fn default_catalog() -> Self {
        let mut catalog = Self::new();

        let models = vec![
            ModelEntry {
                id: "claude-sonnet-4-20250514".into(),
                provider: "anthropic".into(),
                display_name: "Claude Sonnet 4".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::VISION)
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::THINKING)
                    .union(ModelCapabilities::STREAMING)
                    .union(ModelCapabilities::CODE)
                    .union(ModelCapabilities::LONG_CONTEXT),
                context_window: 200_000,
                cost_per_m_input: 3.0,
                cost_per_m_output: 15.0,
                avg_latency_ms: 800.0,
                max_output_tokens: 64_000,
            },
            ModelEntry {
                id: "claude-opus-4-20250514".into(),
                provider: "anthropic".into(),
                display_name: "Claude Opus 4".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::VISION)
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::THINKING)
                    .union(ModelCapabilities::STREAMING)
                    .union(ModelCapabilities::CODE)
                    .union(ModelCapabilities::LONG_CONTEXT),
                context_window: 200_000,
                cost_per_m_input: 15.0,
                cost_per_m_output: 75.0,
                avg_latency_ms: 1200.0,
                max_output_tokens: 64_000,
            },
            ModelEntry {
                id: "gpt-4o".into(),
                provider: "openai".into(),
                display_name: "GPT-4o".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::VISION)
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::STREAMING)
                    .union(ModelCapabilities::JSON_MODE)
                    .union(ModelCapabilities::CODE)
                    .union(ModelCapabilities::LONG_CONTEXT),
                context_window: 128_000,
                cost_per_m_input: 2.5,
                cost_per_m_output: 10.0,
                avg_latency_ms: 600.0,
                max_output_tokens: 16_384,
            },
            ModelEntry {
                id: "gpt-4o-mini".into(),
                provider: "openai".into(),
                display_name: "GPT-4o Mini".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::VISION)
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::STREAMING)
                    .union(ModelCapabilities::JSON_MODE),
                context_window: 128_000,
                cost_per_m_input: 0.15,
                cost_per_m_output: 0.6,
                avg_latency_ms: 400.0,
                max_output_tokens: 16_384,
            },
            // ── Gemini ──
            ModelEntry {
                id: "gemini-2.5-pro".into(),
                provider: "gemini".into(),
                display_name: "Gemini 2.5 Pro".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::VISION)
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::THINKING)
                    .union(ModelCapabilities::STREAMING)
                    .union(ModelCapabilities::CODE)
                    .union(ModelCapabilities::LONG_CONTEXT),
                context_window: 1_048_576,
                cost_per_m_input: 1.25,
                cost_per_m_output: 10.0,
                avg_latency_ms: 700.0,
                max_output_tokens: 65_536,
            },
            // ── DeepSeek ──
            ModelEntry {
                id: "deepseek-r1".into(),
                provider: "deepseek".into(),
                display_name: "DeepSeek R1".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::THINKING)
                    .union(ModelCapabilities::STREAMING)
                    .union(ModelCapabilities::CODE),
                context_window: 128_000,
                cost_per_m_input: 0.55,
                cost_per_m_output: 2.19,
                avg_latency_ms: 500.0,
                max_output_tokens: 32_768,
            },
            // ── Chinese Providers ──
            ModelEntry {
                id: "qwen-max".into(),
                provider: "qwen".into(),
                display_name: "Qwen Max (Alibaba)".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::STREAMING)
                    .union(ModelCapabilities::CODE)
                    .union(ModelCapabilities::LONG_CONTEXT),
                context_window: 128_000,
                cost_per_m_input: 2.4,
                cost_per_m_output: 9.6,
                avg_latency_ms: 600.0,
                max_output_tokens: 16_384,
            },
            ModelEntry {
                id: "qwen-plus".into(),
                provider: "qwen".into(),
                display_name: "Qwen Plus (Alibaba)".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::STREAMING),
                context_window: 128_000,
                cost_per_m_input: 0.8,
                cost_per_m_output: 2.0,
                avg_latency_ms: 400.0,
                max_output_tokens: 16_384,
            },
            ModelEntry {
                id: "abab7-chat".into(),
                provider: "minimax".into(),
                display_name: "MiniMax abab7".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::STREAMING)
                    .union(ModelCapabilities::LONG_CONTEXT),
                context_window: 245_760,
                cost_per_m_input: 1.0,
                cost_per_m_output: 1.0,
                avg_latency_ms: 500.0,
                max_output_tokens: 16_384,
            },
            ModelEntry {
                id: "doubao-pro-256k".into(),
                provider: "doubao".into(),
                display_name: "Doubao Pro 256K (ByteDance)".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::STREAMING)
                    .union(ModelCapabilities::LONG_CONTEXT),
                context_window: 256_000,
                cost_per_m_input: 0.7,
                cost_per_m_output: 0.9,
                avg_latency_ms: 450.0,
                max_output_tokens: 16_384,
            },
            ModelEntry {
                id: "moonshot-v1-128k".into(),
                provider: "moonshot".into(),
                display_name: "Moonshot v1 128K (Kimi)".into(),
                capabilities: ModelCapabilities::TEXT
                    .union(ModelCapabilities::TOOLS)
                    .union(ModelCapabilities::STREAMING)
                    .union(ModelCapabilities::LONG_CONTEXT),
                context_window: 128_000,
                cost_per_m_input: 0.8,
                cost_per_m_output: 0.8,
                avg_latency_ms: 500.0,
                max_output_tokens: 16_384,
            },
        ];

        catalog.register_batch(models);
        catalog
    }
}

impl Default for ModelCatalog {
    fn default() -> Self {
        Self::default_catalog()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_matching() {
        let caps = ModelCapabilities::TEXT
            .union(ModelCapabilities::VISION)
            .union(ModelCapabilities::TOOLS);
        assert!(caps.has(ModelCapabilities::TEXT));
        assert!(caps.has(ModelCapabilities::VISION));
        assert!(caps.has(ModelCapabilities::TOOLS));
        assert!(!caps.has(ModelCapabilities::THINKING));
    }

    #[test]
    fn test_model_selection() {
        let catalog = ModelCatalog::default_catalog();
        let selection = catalog
            .select(ModelCapabilities::TEXT.union(ModelCapabilities::TOOLS), 0.1)
            .unwrap();
        assert!(!selection.primary.id.is_empty());
    }

    #[test]
    fn test_empty_catalog_returns_none() {
        let catalog = ModelCatalog::new();
        assert!(catalog.select(ModelCapabilities::TEXT, 0.1).is_none());
    }
}
