//! Provider registry — manages and selects LLM providers.

use crate::Provider;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

/// Registry of available LLM providers.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
    /// Explicit default provider key. When set, `default_provider()` returns
    /// this provider deterministically. Falls back to alphabetically first.
    default_key: Option<String>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            default_key: None,
        }
    }

    /// Register a provider.
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        let name = provider.name().to_string();
        info!(%name, models = ?provider.models(), "registering provider");
        self.providers.insert(name, provider);
    }

    /// Set the explicit default provider by name.
    /// Returns `true` if the provider exists, `false` otherwise.
    pub fn set_default(&mut self, name: &str) -> bool {
        if self.providers.contains_key(name) {
            self.default_key = Some(name.to_string());
            true
        } else {
            false
        }
    }

    /// Get a provider by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Provider>> {
        self.providers.get(name)
    }

    /// List all registered provider names.
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.keys().cloned().collect();
        names.sort(); // Deterministic ordering.
        names
    }

    /// Get the default provider.
    ///
    /// Priority: (1) explicit `default_key`, (2) alphabetically first provider.
    /// This is deterministic — `HashMap::values().next()` iteration order is not.
    pub fn default_provider(&self) -> Option<&Arc<dyn Provider>> {
        // 1. Explicit default.
        if let Some(key) = &self.default_key {
            if let Some(p) = self.providers.get(key) {
                return Some(p);
            }
        }
        // 2. Alphabetically first — deterministic fallback.
        self.providers
            .keys()
            .min()
            .and_then(|k| self.providers.get(k))
    }

    /// Iterate over all registered providers.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Arc<dyn Provider>)> {
        self.providers.iter()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Plugin-based provider registration ───────────────────────────────────

/// Register providers from a `ProviderPluginRegistry` by hydrating all
/// plugins with the given catalog context.
///
/// This is the plugin-system equivalent of `auto_register_from_env`:
/// instead of hardcoding env-var → provider mappings, each provider plugin
/// declares its own env vars and handles its own construction.
///
/// Returns the count of providers registered.
pub async fn register_from_plugins(
    registry: &mut ProviderRegistry,
    plugins: &crate::plugin_provider::ProviderPluginRegistry,
    ctx: &crate::plugin_provider::ProviderCatalogContext,
) -> usize {
    let hydrated = plugins.hydrate(ctx).await;
    let count = hydrated.len();
    for (id, provider) in hydrated {
        tracing::info!(provider = id.as_str(), "registering from plugin");
        registry.register(provider);
    }
    count
}

// ─── Auto-registration from environment variables ─────────────────────────

/// Scan environment variables and register all discovered providers.
///
/// This is the **single source of truth** for env-var → provider mapping.
/// All binaries (CLI, Tauri, gateway) call this instead of duplicating the
/// probe logic. If you add a new provider, add it here **once**.
///
/// Checked env vars (in order):
/// - `ANTHROPIC_API_KEY` → Anthropic (Claude)
/// - `OPENAI_API_KEY` + optional `OPENAI_BASE_URL` → OpenAI
/// - `GOOGLE_API_KEY` → Google Gemini
/// - `AZURE_OPENAI_API_KEY` + `AZURE_OPENAI_ENDPOINT` → Azure OpenAI
/// - `COHERE_API_KEY` → Cohere
/// - `VERTEX_PROJECT_ID` + `VERTEX_LOCATION` → Google Vertex AI
/// - `OPENROUTER_API_KEY` → OpenRouter
/// - Ollama is always registered (local, no API key needed)
///
/// Returns the count of providers registered.
pub fn auto_register_from_env(registry: &mut ProviderRegistry) -> usize {
    let mut count = 0;

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        info!("registering Anthropic provider from ANTHROPIC_API_KEY");
        registry.register(Arc::new(
            crate::anthropic::AnthropicProvider::new(key, None),
        ));
        count += 1;
    }

    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let base_url = std::env::var("OPENAI_BASE_URL").ok();
        info!("registering OpenAI provider from OPENAI_API_KEY");
        registry.register(Arc::new(
            crate::openai::OpenAiProvider::new(key, base_url, None),
        ));
        count += 1;
    }

    if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
        info!("registering Gemini provider from GOOGLE_API_KEY");
        registry.register(Arc::new(
            crate::gemini::GeminiProvider::new(key, None),
        ));
        count += 1;
    }

    if let Ok(key) = std::env::var("AZURE_OPENAI_API_KEY") {
        if let Ok(endpoint) = std::env::var("AZURE_OPENAI_ENDPOINT") {
            let api_version = std::env::var("AZURE_OPENAI_API_VERSION").ok();
            info!("registering Azure OpenAI provider from AZURE_OPENAI_API_KEY");
            registry.register(Arc::new(
                crate::azure::AzureOpenAiProvider::new(key, endpoint, api_version, None),
            ));
            count += 1;
        }
    }

    if let Ok(key) = std::env::var("COHERE_API_KEY") {
        info!("registering Cohere provider from COHERE_API_KEY");
        registry.register(Arc::new(
            crate::cohere::CohereProvider::new(key, None, None),
        ));
        count += 1;
    }

    if let Ok(project) = std::env::var("VERTEX_PROJECT_ID") {
        if let Ok(location) = std::env::var("VERTEX_LOCATION") {
            info!("registering Vertex AI provider from VERTEX_PROJECT_ID");
            registry.register(Arc::new(
                crate::vertex::VertexProvider::new(project, location, None),
            ));
            count += 1;
        }
    }

    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        info!("registering OpenRouter provider from OPENROUTER_API_KEY");
        registry.register(Arc::new(
            crate::openrouter::OpenRouterProvider::new(key),
        ));
        count += 1;
    }

    // Ollama — always available if running locally (no API key needed)
    {
        let base_url = std::env::var("OLLAMA_HOST").ok();
        info!(base_url = ?base_url, "registering Ollama provider (local)");
        registry.register(Arc::new(
            crate::ollama::OllamaProvider::new(base_url, None),
        ));
        count += 1;
    }

    // Local — built-in llama.cpp inference (no external tools needed)
    {
        info!("registering Local provider (llama.cpp)");
        registry.register(Arc::new(
            crate::local::LocalProvider::new(None),
        ));
        count += 1;
    }

    info!(count, "auto_register_from_env complete");
    count
}

/// Load provider overrides from a `channel_provider.json` file.
///
/// This supports the pattern where users configure a provider via the UI
/// and it's persisted as JSON. The file format:
/// ```json
/// { "provider": "Azure OpenAI", "api_key": "...", "base_url": "...", "model": "..." }
/// ```
///
/// Providers loaded from this file are registered **in addition to** env-var
/// providers. If the same provider is registered twice, the last one wins.
pub fn register_from_config_file(
    registry: &mut ProviderRegistry,
    path: &std::path::Path,
) -> usize {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(path = ?path, error = %e, "channel_provider.json not found or unreadable");
            return 0;
        }
    };

    let cp: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(path = ?path, error = %e, "channel_provider.json parse error");
            return 0;
        }
    };

    let provider_name = cp.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    let api_key = cp.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
    let base_url = cp.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
    let model = cp.get("model").and_then(|v| v.as_str());

    if api_key.is_empty() || base_url.is_empty() {
        tracing::debug!("channel_provider.json: api_key or base_url empty, skipping");
        return 0;
    }

    let mut count = 0;

    if provider_name.contains("Azure") {
        info!("registering Azure OpenAI from channel_provider.json");
        registry.register(Arc::new(
            crate::azure::AzureOpenAiProvider::new(
                api_key.to_string(),
                base_url.to_string(),
                None,
                model.map(|m| m.to_string()),
            ),
        ));
        count += 1;
    } else if provider_name.contains("OpenAI") {
        info!("registering OpenAI from channel_provider.json");
        registry.register(Arc::new(
            crate::openai::OpenAiProvider::new(
                api_key.to_string(),
                Some(base_url.to_string()),
                model.map(|m| m.to_string()),
            ),
        ));
        count += 1;
    } else if !provider_name.is_empty() {
        tracing::warn!(provider = provider_name, "unknown provider in channel_provider.json — skipping");
    }

    count
}
