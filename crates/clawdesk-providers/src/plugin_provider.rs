//! Plugin-driven provider registration.
//!
//! ## Problem
//!
//! Adding a new LLM provider to `auto_register_from_env` requires editing
//! two files (the if-else chain and the module list). This violates the
//! Open-Closed Principle and makes the registry awareness-coupled to every
//! concrete provider type.
//!
//! ## Design
//!
//! Invert the dependency: providers self-register through a manifest-based
//! discovery system. The registry becomes a consumer of provider manifests,
//! not a factory that knows every concrete type.
//!
//! The provider catalog is a dependent product:
//!   `Catalog : Π(p : ProviderId) → ModelSet(p) × AuthMethod(p) × Caps(p)`
//!
//! Each provider contributes a fiber. The registry merges fibers via
//! disjoint union with last-writer-wins conflict resolution.
//!
//! ## Lifecycle
//!
//! 1. Discovery: scan manifests (zero code execution, zero I/O)
//! 2. Auth resolution: check env vars / credential vault
//! 3. Catalog hydration: produce `Arc<dyn Provider>` with resolved auth
//! 4. Registration: insert into `ProviderRegistry`

use crate::capability::ProviderCaps;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Provider Manifest ─────────────────────────────────────

/// Declarative metadata for a provider plugin.
///
/// Parsed at discovery time without loading provider code.
/// Enables the registry to enumerate available providers,
/// their models, and auth requirements without instantiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderManifest {
    /// Unique provider identifier (e.g., "openai", "anthropic", "ollama").
    pub id: String,
    /// Display name for UI rendering.
    pub name: String,
    /// Provider version (semver).
    pub version: String,
    /// Model definitions — the static catalog of known models.
    pub models: Vec<ModelDefinition>,
    /// Auth environment variables that trigger auto-discovery.
    /// If any of these are set, the provider should be registered.
    pub auth_env_vars: Vec<AuthEnvVar>,
    /// Available auth methods for onboarding.
    pub auth_choices: Vec<ProviderAuthChoice>,
    /// Default capabilities for all models unless overridden per-model.
    pub default_capabilities: ProviderCaps,
    /// Whether this provider requires no auth (e.g., local inference).
    #[serde(default)]
    pub no_auth: bool,
    /// Optional base URL override env var name.
    pub base_url_env: Option<String>,
}

/// A single model definition with capabilities and cost metadata.
///
/// The provider declares: "I support these models with these
/// capabilities and these cost characteristics." The capability
/// detector can augment this at runtime, but the manifest is ground truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDefinition {
    /// Wire-format model ID (e.g., "claude-sonnet-4-20250514").
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Context window size in tokens.
    pub context_window: u32,
    /// Maximum output tokens.
    pub max_output_tokens: Option<u32>,
    /// Per-model capability overrides. If `None`, inherits from manifest default.
    pub capabilities: Option<ProviderCaps>,
    /// Cost per 1K input tokens (USD). `None` for local/free models.
    pub input_cost_per_1k: Option<f64>,
    /// Cost per 1K output tokens (USD).
    pub output_cost_per_1k: Option<f64>,
    /// Whether this model supports vision (images in input).
    #[serde(default)]
    pub vision: bool,
    /// Whether this model supports extended thinking / chain-of-thought.
    #[serde(default)]
    pub extended_thinking: bool,
    /// Aliases — alternative IDs that resolve to this model.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Whether this model is hidden from UI selection (still usable via API).
    #[serde(default)]
    pub hidden: bool,
}

/// Environment variable that, when set, enables auto-registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthEnvVar {
    /// Env var name (e.g., "OPENAI_API_KEY").
    pub name: String,
    /// Whether this env var is required (vs optional supplement).
    pub required: bool,
    /// Description for documentation / wizard.
    pub description: String,
}

// ── Provider Auth Choice ──────────────────────────────────

/// Declarative auth choice metadata.
///
/// Each provider can support multiple auth methods (API key, OAuth, device
/// code, portal login). The wizard discovers these from manifests and
/// renders a selection UI without provider-specific code paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAuthChoice {
    /// Auth method identifier (e.g., "api-key", "oauth", "device-code").
    pub method: AuthMethod,
    /// Stable choice ID for persistence (e.g., "openai-api-key").
    pub choice_id: String,
    /// Display label (e.g., "API Key", "Sign in with Google").
    pub label: String,
    /// Hint text for the auth input (e.g., "Get your API key at https://...").
    pub hint: Option<String>,
    /// Grouping for selection UI (e.g., "OpenAI", "Azure").
    pub group_id: Option<String>,
    /// Group display label.
    pub group_label: Option<String>,
    /// CLI flag for non-interactive auth (e.g., "--openai-api-key").
    pub cli_flag: Option<String>,
    /// Environment variable to auto-fill from.
    pub env_var: Option<String>,
}

/// Auth method enumeration.
///
/// Finite algebraic type — every auth method the system understands.
/// New methods require a variant here (exhaustive matching catches gaps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMethod {
    /// Standard API key (most common).
    ApiKey,
    /// OAuth2 authorization code + PKCE.
    OAuth,
    /// Device code flow (e.g., GitHub Copilot).
    DeviceCode,
    /// Bearer token (pre-generated).
    Token,
    /// No auth required (e.g., local Ollama).
    None,
    /// Custom auth flow (provider handles it).
    Custom,
}

// ── Provider Plugin Trait ─────────────────────────────────

/// Trait for providers that participate in plugin-based discovery.
///
/// This is the bridge between the manifest (static metadata) and the
/// concrete `Provider` implementation. The registry calls `catalog()`
/// during hydration to produce the runtime provider instance.
///
/// ## Separation of concerns
///
/// - `manifest()` — pure data, no I/O, called during discovery
/// - `catalog()` — may do I/O (check env vars, validate auth), called during hydration
/// - The returned `Provider` — handles LLM requests at runtime
///
/// This three-phase lifecycle (discover → hydrate → run) ensures that
/// provider loading is lazy and auth failures are non-fatal.
#[async_trait::async_trait]
pub trait ProviderPlugin: Send + Sync {
    /// Return the provider manifest (zero I/O).
    fn manifest(&self) -> &ProviderManifest;

    /// Hydrate: resolve auth and produce the runtime provider.
    ///
    /// Returns `None` if auth is not available (env vars missing, no
    /// credential in vault). The registry skips such providers silently.
    ///
    /// The `ctx` provides access to the credential vault and config.
    async fn catalog(&self, ctx: &ProviderCatalogContext) -> Option<Arc<dyn crate::Provider>>;

    /// Optional: dynamically discover models at runtime.
    ///
    /// Called when the user requests a model not in the static manifest.
    /// Returns `None` if the provider doesn't support dynamic discovery.
    async fn resolve_dynamic_model(
        &self,
        _model_id: &str,
        _ctx: &ProviderCatalogContext,
    ) -> Option<ModelDefinition> {
        None
    }
}

/// Context passed to `ProviderPlugin::catalog()` during hydration.
///
/// Provides read-only access to config and credentials without
/// exposing internal state management.
pub struct ProviderCatalogContext {
    /// Resolved environment variables (pre-scanned).
    pub env_vars: std::collections::HashMap<String, String>,
    /// User config values for this provider (from SochDB or TOML).
    pub config: serde_json::Value,
    /// Path to the credential vault file.
    pub vault_path: Option<std::path::PathBuf>,
}

impl ProviderCatalogContext {
    /// Get an env var by name.
    pub fn env(&self, name: &str) -> Option<&str> {
        self.env_vars.get(name).map(|s| s.as_str())
    }

    /// Get a config value by JSON pointer.
    pub fn config_get(&self, pointer: &str) -> Option<&serde_json::Value> {
        self.config.pointer(pointer)
    }

    /// Check if required env vars are present for a set of `AuthEnvVar`s.
    pub fn has_required_env(&self, vars: &[AuthEnvVar]) -> bool {
        vars.iter()
            .filter(|v| v.required)
            .all(|v| self.env_vars.contains_key(&v.name))
    }
}

// ── Provider Plugin Registry ──────────────────────────────

/// Registry of provider plugins — discovered from manifests, hydrated on demand.
///
/// This complements (not replaces) the existing `ProviderRegistry`.
/// Provider plugins register here; hydrated providers flow into `ProviderRegistry`.
pub struct ProviderPluginRegistry {
    plugins: Vec<Box<dyn ProviderPlugin>>,
}

impl ProviderPluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Register a provider plugin.
    pub fn register(&mut self, plugin: Box<dyn ProviderPlugin>) {
        tracing::info!(
            provider = plugin.manifest().id.as_str(),
            models = plugin.manifest().models.len(),
            "registered provider plugin"
        );
        self.plugins.push(plugin);
    }

    /// List all registered provider manifests (zero I/O).
    pub fn manifests(&self) -> Vec<&ProviderManifest> {
        self.plugins.iter().map(|p| p.manifest()).collect()
    }

    /// Hydrate all providers: resolve auth, produce runtime instances.
    ///
    /// Returns `(provider_id, Arc<dyn Provider>)` pairs for providers
    /// that have valid auth. Providers without auth are silently skipped.
    pub async fn hydrate(
        &self,
        ctx: &ProviderCatalogContext,
    ) -> Vec<(String, Arc<dyn crate::Provider>)> {
        let mut result = Vec::new();

        for plugin in &self.plugins {
            let id = plugin.manifest().id.clone();
            if let Some(provider) = plugin.catalog(ctx).await {
                tracing::info!(provider = id.as_str(), "provider plugin hydrated");
                result.push((id, provider));
            } else {
                tracing::debug!(provider = id.as_str(), "provider plugin skipped (no auth)");
            }
        }

        result
    }

    /// Find a provider plugin by ID.
    pub fn find(&self, id: &str) -> Option<&dyn ProviderPlugin> {
        self.plugins
            .iter()
            .find(|p| p.manifest().id == id)
            .map(|p| p.as_ref())
    }

    /// Get all model definitions across all registered providers.
    /// Useful for capability detection and model resolution.
    pub fn all_model_definitions(&self) -> Vec<(&str, &ModelDefinition)> {
        self.plugins
            .iter()
            .flat_map(|p| {
                let pid = p.manifest().id.as_str();
                p.manifest().models.iter().map(move |m| (pid, m))
            })
            .collect()
    }

    /// Collect all auth choices across all providers.
    /// Used by the wizard to render provider selection UI.
    pub fn all_auth_choices(&self) -> Vec<(&str, &ProviderAuthChoice)> {
        self.plugins
            .iter()
            .flat_map(|p| {
                let pid = p.manifest().id.as_str();
                p.manifest().auth_choices.iter().map(move |c| (pid, c))
            })
            .collect()
    }
}

impl Default for ProviderPluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_definition_aliases() {
        let m = ModelDefinition {
            id: "claude-sonnet-4-20250514".into(),
            display_name: "Claude Sonnet 4".into(),
            context_window: 200_000,
            max_output_tokens: Some(8192),
            capabilities: None,
            input_cost_per_1k: Some(0.003),
            output_cost_per_1k: Some(0.015),
            vision: true,
            extended_thinking: true,
            aliases: vec!["claude-sonnet-4".into(), "sonnet-4".into()],
            hidden: false,
        };
        assert_eq!(m.aliases.len(), 2);
        assert!(m.vision);
    }

    #[test]
    fn auth_env_var_check() {
        let mut env = std::collections::HashMap::new();
        env.insert("OPENAI_API_KEY".into(), "sk-test".into());

        let ctx = ProviderCatalogContext {
            env_vars: env,
            config: serde_json::Value::Null,
            vault_path: None,
        };

        let vars = vec![AuthEnvVar {
            name: "OPENAI_API_KEY".into(),
            required: true,
            description: "OpenAI API key".into(),
        }];

        assert!(ctx.has_required_env(&vars));
    }

    #[test]
    fn registry_manifests_empty() {
        let reg = ProviderPluginRegistry::new();
        assert!(reg.manifests().is_empty());
    }
}
