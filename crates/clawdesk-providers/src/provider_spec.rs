//! # Provider Spec — Type-safe provider resolution parsed at the IPC boundary.
//!
//! Replaces stringly-typed provider dispatch (`match prov_name.as_str()`)
//! with a type-safe enum. Parsed once from frontend strings at the Tauri
//! command boundary; all downstream dispatch uses the typed enum.
//!
//! ## Compiler Guarantees
//!
//! Adding a new provider requires adding a variant here. The `resolve()`
//! method's exhaustive match ensures all providers are handled.
//! No string alias drift between frontend and backend.

use crate::Provider;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ═══════════════════════════════════════════════════════════════════════════
// Provider specification enum
// ═══════════════════════════════════════════════════════════════════════════

/// Type-safe provider specification — parsed once from frontend strings.
///
/// Each variant carries exactly the configuration its provider needs.
/// Invalid states are unrepresentable at the type level.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ProviderSpec {
    Anthropic {
        api_key: String,
        model: String,
    },
    OpenAi {
        api_key: String,
        base_url: Option<String>,
        model: String,
    },
    AzureOpenAi {
        api_key: String,
        endpoint: String,
        model: String,
    },
    Google {
        api_key: String,
        model: String,
    },
    Cohere {
        api_key: String,
        base_url: Option<String>,
        model: String,
    },
    Ollama {
        base_url: Option<String>,
        model: String,
    },
    LocalCompatible {
        api_key: String,
        base_url: String,
        model: String,
    },
    OpenRouter {
        api_key: String,
        model: String,
    },
}

impl ProviderSpec {
    /// Parse a provider spec from frontend string parameters.
    ///
    /// This is the single point where stringly-typed provider names are
    /// converted to the type-safe enum. Called at the Tauri IPC boundary.
    pub fn from_frontend(
        provider_name: &str,
        api_key: Option<String>,
        base_url: Option<String>,
        model: String,
    ) -> Result<Self, ProviderSpecError> {
        let key = api_key.unwrap_or_default();

        match provider_name {
            "Anthropic" | "anthropic" => Ok(Self::Anthropic {
                api_key: key,
                model,
            }),
            "OpenAI" | "openai" => Ok(Self::OpenAi {
                api_key: key,
                base_url,
                model,
            }),
            "Azure OpenAI" | "azure_openai" | "azure" => Ok(Self::AzureOpenAi {
                api_key: key,
                endpoint: base_url.unwrap_or_default(),
                model,
            }),
            "Google" | "google" | "Gemini" | "gemini" => Ok(Self::Google {
                api_key: key,
                model,
            }),
            "Cohere" | "cohere" => Ok(Self::Cohere {
                api_key: key,
                base_url,
                model,
            }),
            "Ollama (Local)" | "ollama" | "Ollama" => Ok(Self::Ollama {
                base_url,
                model,
            }),
            "Local (OpenAI Compatible)" | "local_compatible" => Ok(Self::LocalCompatible {
                api_key: key,
                base_url: base_url.unwrap_or_else(|| "http://localhost:8080/v1".to_string()),
                model,
            }),
            "OpenRouter" | "openrouter" => Ok(Self::OpenRouter {
                api_key: key,
                model,
            }),
            _ => Err(ProviderSpecError::UnknownProvider(provider_name.to_string())),
        }
    }

    /// Resolve a model short name to the full model identifier.
    ///
    /// Single source of truth for model alias resolution.
    pub fn resolve_model_id(model: &str) -> String {
        match model {
            "haiku" => "claude-haiku-4-20250514".to_string(),
            "sonnet" => "claude-sonnet-4-20250514".to_string(),
            "opus" => "claude-opus-4-20250514".to_string(),
            "local" => "llama3.2".to_string(),
            other => other.to_string(),
        }
    }

    /// Infer the provider name from a model identifier.
    ///
    /// Used when no explicit provider override is given and the negotiator
    /// doesn't have a match. Single source of truth for model→provider mapping.
    pub fn infer_provider_name(model: &str) -> &'static str {
        match model {
            "haiku" | "sonnet" | "opus" => "anthropic",
            m if m.starts_with("claude") => "anthropic",
            m if m.starts_with("gpt-") || m.starts_with("o1") || m.starts_with("o3") => "openai",
            m if m.starts_with("gemini") => "gemini",
            "local" => "ollama",
            m if m.starts_with("llama")
                || m.starts_with("deepseek")
                || m.starts_with("mistral")
                || m.starts_with("codellama")
                || m.starts_with("phi") =>
            {
                "ollama"
            }
            _ => "anthropic", // Safe default
        }
    }

    /// Instantiate the concrete `Provider` from this spec.
    ///
    /// This is the exhaustive match that replaces all string-matching dispatch
    /// tables in commands.rs, message_pipeline.rs, and state.rs.
    pub fn resolve(&self) -> Arc<dyn Provider> {
        match self {
            Self::Anthropic { api_key, model } => {
                Arc::new(crate::anthropic::AnthropicProvider::new(
                    api_key.clone(),
                    Some(model.clone()),
                ))
            }
            Self::OpenAi {
                api_key,
                base_url,
                model,
            } => Arc::new(crate::openai::OpenAiProvider::new(
                api_key.clone(),
                base_url.clone(),
                Some(model.clone()),
            )),
            Self::AzureOpenAi {
                api_key,
                endpoint,
                model,
            } => Arc::new(crate::azure::AzureOpenAiProvider::new(
                api_key.clone(),
                endpoint.clone(),
                None,
                Some(model.clone()),
            )),
            Self::Google { api_key, model } => {
                Arc::new(crate::gemini::GeminiProvider::new(
                    api_key.clone(),
                    Some(model.clone()),
                ))
            }
            Self::Cohere {
                api_key,
                base_url,
                model,
            } => Arc::new(crate::cohere::CohereProvider::new(
                api_key.clone(),
                base_url.clone(),
                Some(model.clone()),
            )),
            Self::Ollama { base_url, model } => {
                Arc::new(crate::ollama::OllamaProvider::new(
                    base_url.clone(),
                    Some(model.clone()),
                ))
            }
            Self::LocalCompatible {
                api_key,
                base_url,
                model,
            } => {
                let config = crate::compatible::CompatibleConfig::new(
                    "local_compatible",
                    base_url,
                    api_key.clone(),
                )
                .with_default_model(model.clone());
                Arc::new(crate::compatible::OpenAiCompatibleProvider::new(config))
            }
            Self::OpenRouter { api_key, model } => {
                let mut provider = crate::openrouter::OpenRouterProvider::new(api_key.clone());
                Arc::new(provider)
            }
        }
    }

    /// Get the model ID from this spec.
    pub fn model(&self) -> &str {
        match self {
            Self::Anthropic { model, .. }
            | Self::OpenAi { model, .. }
            | Self::AzureOpenAi { model, .. }
            | Self::Google { model, .. }
            | Self::Cohere { model, .. }
            | Self::Ollama { model, .. }
            | Self::LocalCompatible { model, .. }
            | Self::OpenRouter { model, .. } => model,
        }
    }

    /// Get the provider name as a string (for logging/display).
    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::Anthropic { .. } => "anthropic",
            Self::OpenAi { .. } => "openai",
            Self::AzureOpenAi { .. } => "azure_openai",
            Self::Google { .. } => "google",
            Self::Cohere { .. } => "cohere",
            Self::Ollama { .. } => "ollama",
            Self::LocalCompatible { .. } => "local_compatible",
            Self::OpenRouter { .. } => "openrouter",
        }
    }
}

/// Error from provider spec parsing.
#[derive(Debug, Clone)]
pub enum ProviderSpecError {
    UnknownProvider(String),
    MissingField(String),
}

impl std::fmt::Display for ProviderSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider(name) => write!(f, "unknown provider: {}", name),
            Self::MissingField(field) => write!(f, "missing required field: {}", field),
        }
    }
}

impl std::error::Error for ProviderSpecError {}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_frontend_anthropic() {
        let spec = ProviderSpec::from_frontend(
            "Anthropic",
            Some("sk-key".into()),
            None,
            "claude-sonnet-4-20250514".into(),
        )
        .unwrap();

        assert_eq!(spec.provider_name(), "anthropic");
        assert_eq!(spec.model(), "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_from_frontend_ollama_aliases() {
        for alias in &["Ollama (Local)", "ollama", "Ollama"] {
            let spec = ProviderSpec::from_frontend(alias, None, None, "llama3.2".into()).unwrap();
            assert_eq!(spec.provider_name(), "ollama");
        }
    }

    #[test]
    fn test_from_frontend_unknown_provider() {
        let result = ProviderSpec::from_frontend("FutureProvider", None, None, "model".into());
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_model_id() {
        assert_eq!(ProviderSpec::resolve_model_id("haiku"), "claude-haiku-4-20250514");
        assert_eq!(ProviderSpec::resolve_model_id("sonnet"), "claude-sonnet-4-20250514");
        assert_eq!(ProviderSpec::resolve_model_id("local"), "llama3.2");
        assert_eq!(ProviderSpec::resolve_model_id("gpt-4o"), "gpt-4o");
    }

    #[test]
    fn test_infer_provider_name() {
        assert_eq!(ProviderSpec::infer_provider_name("haiku"), "anthropic");
        assert_eq!(ProviderSpec::infer_provider_name("claude-sonnet-4-20250514"), "anthropic");
        assert_eq!(ProviderSpec::infer_provider_name("gpt-4o"), "openai");
        assert_eq!(ProviderSpec::infer_provider_name("gemini-pro"), "gemini");
        assert_eq!(ProviderSpec::infer_provider_name("llama3.2"), "ollama");
        assert_eq!(ProviderSpec::infer_provider_name("deepseek-coder"), "ollama");
    }

    #[test]
    fn test_provider_name_exhaustive() {
        // Verify all variants have a name
        let specs = vec![
            ProviderSpec::Anthropic { api_key: "".into(), model: "m".into() },
            ProviderSpec::OpenAi { api_key: "".into(), base_url: None, model: "m".into() },
            ProviderSpec::AzureOpenAi { api_key: "".into(), endpoint: "".into(), model: "m".into() },
            ProviderSpec::Google { api_key: "".into(), model: "m".into() },
            ProviderSpec::Cohere { api_key: "".into(), base_url: None, model: "m".into() },
            ProviderSpec::Ollama { base_url: None, model: "m".into() },
            ProviderSpec::LocalCompatible { api_key: "".into(), base_url: "".into(), model: "m".into() },
            ProviderSpec::OpenRouter { api_key: "".into(), model: "m".into() },
        ];

        for spec in &specs {
            assert!(!spec.provider_name().is_empty());
            assert!(!spec.model().is_empty());
        }
    }
}
