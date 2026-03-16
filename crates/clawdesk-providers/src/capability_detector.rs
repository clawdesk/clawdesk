//! # Dynamic Model Capability Detection
//!
//! Runtime discovery of model capabilities from provider APIs, with a 3-layer
//! cache (memory → disk → API fetch) to stay current without code changes.
//!
//! When a new model is requested but not found in the static capability matrix,
//! this module fetches capabilities from the provider's API and caches them.
//!
//! Inspired by openclaw's `openrouter-model-capabilities.ts`.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ───────────────────────────────────────────────────────────────────────────
// Types
// ───────────────────────────────────────────────────────────────────────────

/// Dynamically discovered model capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Model identifier as returned by the provider.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Supported input modalities.
    pub input_modalities: Vec<InputModality>,
    /// Whether the model supports reasoning/thinking tokens.
    pub reasoning: bool,
    /// Context window size in tokens.
    pub context_window: u64,
    /// Maximum completion tokens.
    pub max_output_tokens: u64,
    /// Per-million-token costs (USD).
    pub cost: ModelCost,
    /// Provider-level capability flags (serializable snapshot).
    pub caps: u32,
}

/// Input modality a model supports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputModality {
    Text,
    Image,
    Audio,
    Video,
}

/// Cost per million tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

/// Configuration for the capability detector.
#[derive(Debug, Clone)]
pub struct CapabilityDetectorConfig {
    /// Directory for disk cache (e.g., `~/.clawdesk/cache/`).
    pub cache_dir: PathBuf,
    /// HTTP timeout for API fetches.
    pub fetch_timeout: std::time::Duration,
    /// OpenRouter API base URL.
    pub openrouter_base_url: String,
}

impl Default for CapabilityDetectorConfig {
    fn default() -> Self {
        let cache_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("clawdesk")
            .join("cache");
        Self {
            cache_dir,
            fetch_timeout: std::time::Duration::from_secs(10),
            openrouter_base_url: "https://openrouter.ai/api/v1".to_string(),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Detector
// ───────────────────────────────────────────────────────────────────────────

/// 3-layer model capability detector.
///
/// Resolution order:
/// 1. **In-memory** — `DashMap` for O(1) concurrent lookups.
/// 2. **Disk cache** — JSON file persisted between process restarts.
/// 3. **API fetch** — Query provider API and populate layers 1+2.
pub struct CapabilityDetector {
    /// Layer 1: in-memory cache.
    memory: DashMap<String, ModelCapabilities>,
    /// Layer 2: disk cache path.
    disk_cache_path: PathBuf,
    /// Config.
    config: CapabilityDetectorConfig,
    /// Mutex to prevent concurrent API fetches for the same refresh cycle.
    fetch_lock: Mutex<()>,
    /// HTTP client.
    client: Arc<reqwest::Client>,
}

impl CapabilityDetector {
    /// Create a new detector with the given configuration.
    pub fn new(config: CapabilityDetectorConfig) -> Self {
        let disk_cache_path = config.cache_dir.join("model-capabilities.json");
        Self {
            memory: DashMap::new(),
            disk_cache_path,
            config,
            fetch_lock: Mutex::new(()),
            client: Arc::new(
                reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .unwrap_or_default(),
            ),
        }
    }

    /// Look up capabilities for a model. Checks memory → disk → API.
    pub async fn lookup(&self, model_id: &str) -> Option<ModelCapabilities> {
        // Layer 1: memory.
        if let Some(caps) = self.memory.get(model_id) {
            return Some(caps.clone());
        }

        // Layer 2: disk cache.
        if let Some(caps) = self.load_from_disk(model_id).await {
            self.memory.insert(model_id.to_string(), caps.clone());
            return Some(caps);
        }

        // Layer 3: API fetch — don't block other lookups.
        debug!(model = model_id, "model not in cache — triggering API fetch");
        self.fetch_and_cache().await;

        // Retry memory after fetch.
        self.memory.get(model_id).map(|r| r.clone())
    }

    /// Synchronous memory-only lookup (never blocks on I/O).
    pub fn lookup_sync(&self, model_id: &str) -> Option<ModelCapabilities> {
        self.memory.get(model_id).map(|r| r.clone())
    }

    /// Pre-warm the memory cache from disk on startup.
    pub async fn warm_from_disk(&self) {
        if let Ok(data) = tokio::fs::read_to_string(&self.disk_cache_path).await {
            match serde_json::from_str::<Vec<ModelCapabilities>>(&data) {
                Ok(models) => {
                    let count = models.len();
                    for m in models {
                        self.memory.insert(m.id.clone(), m);
                    }
                    info!(count, "warmed model capability cache from disk");
                }
                Err(e) => {
                    warn!(error = %e, "failed to parse model capability cache");
                }
            }
        }
    }

    /// Load a single model from the disk cache.
    async fn load_from_disk(&self, model_id: &str) -> Option<ModelCapabilities> {
        let data = tokio::fs::read_to_string(&self.disk_cache_path).await.ok()?;
        let models: Vec<ModelCapabilities> = serde_json::from_str(&data).ok()?;
        models.into_iter().find(|m| m.id == model_id)
    }

    /// Fetch capabilities from provider APIs and update both caches.
    async fn fetch_and_cache(&self) {
        // Prevent concurrent fetches.
        let _guard = match self.fetch_lock.try_lock() {
            Ok(g) => g,
            Err(_) => {
                debug!("capability fetch already in progress — skipping");
                return;
            }
        };

        // Fetch from OpenRouter (aggregates many providers).
        match self.fetch_openrouter().await {
            Ok(models) => {
                let count = models.len();
                // Update memory cache.
                for m in &models {
                    self.memory.insert(m.id.clone(), m.clone());
                }
                // Persist to disk.
                if let Err(e) = self.persist_to_disk(&models).await {
                    warn!(error = %e, "failed to persist model capabilities to disk");
                }
                info!(count, "refreshed model capabilities from OpenRouter API");
            }
            Err(e) => {
                warn!(error = %e, "failed to fetch model capabilities");
            }
        }
    }

    /// Fetch model list from OpenRouter API.
    async fn fetch_openrouter(&self) -> Result<Vec<ModelCapabilities>, CapabilityFetchError> {
        let url = format!("{}/models", self.config.openrouter_base_url);
        let resp = self
            .client
            .get(&url)
            .timeout(self.config.fetch_timeout)
            .send()
            .await
            .map_err(|e| CapabilityFetchError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(CapabilityFetchError::Http(format!(
                "status {}",
                resp.status()
            )));
        }

        let body: OpenRouterModelsResponse = resp
            .json()
            .await
            .map_err(|e| CapabilityFetchError::Parse(e.to_string()))?;

        let models = body
            .data
            .into_iter()
            .map(|m| {
                let reasoning = m
                    .supported_parameters
                    .as_ref()
                    .and_then(|params| {
                        params
                            .iter()
                            .find(|p| p == &"reasoning" || p == &"thinking")
                    })
                    .is_some();

                let input_modalities = m
                    .architecture
                    .as_ref()
                    .and_then(|a| a.input_modalities.as_ref())
                    .map(|mods| {
                        mods.iter()
                            .filter_map(|s| match s.as_str() {
                                "text" => Some(InputModality::Text),
                                "image" => Some(InputModality::Image),
                                "audio" => Some(InputModality::Audio),
                                "video" => Some(InputModality::Video),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_else(|| vec![InputModality::Text]);

                let pricing = m.pricing.unwrap_or_default();

                ModelCapabilities {
                    id: m.id.clone(),
                    name: m.name.unwrap_or_else(|| m.id.clone()),
                    input_modalities,
                    reasoning,
                    context_window: m.context_length.unwrap_or(4096),
                    max_output_tokens: m
                        .top_provider
                        .and_then(|tp| tp.max_completion_tokens)
                        .unwrap_or(4096),
                    cost: ModelCost {
                        input: parse_cost(&pricing.prompt),
                        output: parse_cost(&pricing.completion),
                        cache_read: pricing.cache_read.as_deref().map(parse_cost_val),
                        cache_write: pricing.cache_write.as_deref().map(parse_cost_val),
                    },
                    caps: 0, // Derived separately by the negotiator.
                }
            })
            .collect();

        Ok(models)
    }

    /// Persist models to the disk cache.
    async fn persist_to_disk(
        &self,
        models: &[ModelCapabilities],
    ) -> Result<(), CapabilityFetchError> {
        // Ensure cache directory exists.
        if let Some(parent) = self.disk_cache_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CapabilityFetchError::Io(e.to_string()))?;
        }

        let json = serde_json::to_string(models)
            .map_err(|e| CapabilityFetchError::Parse(e.to_string()))?;

        tokio::fs::write(&self.disk_cache_path, json)
            .await
            .map_err(|e| CapabilityFetchError::Io(e.to_string()))?;

        Ok(())
    }

    /// Number of models currently cached in memory.
    pub fn cached_count(&self) -> usize {
        self.memory.len()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// OpenRouter API response types
// ───────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Deserialize)]
struct OpenRouterModel {
    id: String,
    name: Option<String>,
    context_length: Option<u64>,
    supported_parameters: Option<Vec<String>>,
    architecture: Option<OpenRouterArchitecture>,
    pricing: Option<OpenRouterPricing>,
    top_provider: Option<OpenRouterTopProvider>,
}

#[derive(Deserialize)]
struct OpenRouterArchitecture {
    input_modalities: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
struct OpenRouterPricing {
    prompt: Option<String>,
    completion: Option<String>,
    cache_read: Option<String>,
    cache_write: Option<String>,
}

#[derive(Deserialize)]
struct OpenRouterTopProvider {
    max_completion_tokens: Option<u64>,
}

fn parse_cost(s: &Option<String>) -> f64 {
    s.as_deref().map(parse_cost_val).unwrap_or(0.0)
}

fn parse_cost_val(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(0.0)
}

// ───────────────────────────────────────────────────────────────────────────
// Errors
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum CapabilityFetchError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("I/O error: {0}")]
    Io(String),
}

// ───────────────────────────────────────────────────────────────────────────
// Model ID normalization
// ───────────────────────────────────────────────────────────────────────────

/// Normalize a model ID to handle common aliases and versioning quirks.
///
/// This is dependency-free and handles known mappings for fast startup.
pub fn normalize_model_id(model: &str) -> &str {
    match model {
        // Gemini aliases
        "gemini-3-pro" => "gemini-3-pro-preview",
        "gemini-3-flash" => "gemini-3-flash-preview",
        "gemini-3.1-flash" => "gemini-3-flash-preview",
        "gemini-2.0-flash" => "gemini-2.0-flash",
        "gemini-pro" => "gemini-1.5-pro",
        "gemini-flash" => "gemini-1.5-flash",
        // GPT aliases
        "gpt4" => "gpt-4",
        "gpt4o" => "gpt-4o",
        "gpt4o-mini" => "gpt-4o-mini",
        // Claude aliases
        "claude-3-opus" => "claude-3-opus-20240229",
        "claude-3-sonnet" => "claude-3-sonnet-20240229",
        "claude-3-haiku" => "claude-3-haiku-20240307",
        "claude-sonnet" => "claude-sonnet-4-20250514",
        "claude-opus" => "claude-opus-4-20250514",
        // Pass through
        other => other,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_gemini_aliases() {
        assert_eq!(normalize_model_id("gemini-3-pro"), "gemini-3-pro-preview");
        assert_eq!(
            normalize_model_id("gemini-3.1-flash"),
            "gemini-3-flash-preview"
        );
    }

    #[test]
    fn normalize_gpt_aliases() {
        assert_eq!(normalize_model_id("gpt4o"), "gpt-4o");
    }

    #[test]
    fn normalize_claude_aliases() {
        assert_eq!(
            normalize_model_id("claude-sonnet"),
            "claude-sonnet-4-20250514"
        );
    }

    #[test]
    fn normalize_passthrough() {
        assert_eq!(
            normalize_model_id("some-custom-model"),
            "some-custom-model"
        );
    }

    #[test]
    fn model_cost_defaults() {
        let cost = ModelCost::default();
        assert_eq!(cost.input, 0.0);
        assert_eq!(cost.output, 0.0);
    }

    #[tokio::test]
    async fn detector_memory_returns_inserted() {
        let config = CapabilityDetectorConfig::default();
        let detector = CapabilityDetector::new(config);

        let caps = ModelCapabilities {
            id: "test/model".to_string(),
            name: "Test Model".to_string(),
            input_modalities: vec![InputModality::Text],
            reasoning: false,
            context_window: 8192,
            max_output_tokens: 4096,
            cost: ModelCost::default(),
            caps: 0,
        };
        detector.memory.insert("test/model".to_string(), caps);

        let result = detector.lookup("test/model").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().context_window, 8192);
    }

    #[test]
    fn sync_lookup_returns_none_when_empty() {
        let config = CapabilityDetectorConfig::default();
        let detector = CapabilityDetector::new(config);
        assert!(detector.lookup_sync("nonexistent").is_none());
    }
}
