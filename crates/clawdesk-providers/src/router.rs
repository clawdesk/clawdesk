//! Router provider — dispatches requests to providers based on model prefix hints.
//!
//! Allows model strings to include a provider hint prefix (e.g., `anthropic:claude-sonnet-4`),
//! which the router uses to dispatch to the correct provider. Falls back to a
//! default provider when no hint matches.
//!
//! ## Model string format
//!
//! `{hint}:{model}` — the hint is stripped before forwarding.
//! Examples: `anthropic:claude-sonnet-4`, `openai:gpt-4o`, `ollama:llama3.2`
//!
//! If no colon is present, the full string is used as the model name and
//! dispatched to the default provider.
//!
//! Uses a route table mapping hints to (provider_index, mapped_model) tuples
//! with a default provider fallback.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;

use crate::{Provider, ProviderRequest, ProviderResponse, StreamChunk};

// ---------------------------------------------------------------------------
// Route table
// ---------------------------------------------------------------------------

/// A route entry mapping a hint to a provider and model.
#[derive(Debug, Clone)]
struct Route {
    /// Index into the providers Vec.
    provider_index: usize,
    /// Model name to use at the target provider (may differ from the hint model).
    model: String,
}

// ---------------------------------------------------------------------------
// Router provider
// ---------------------------------------------------------------------------

/// Dispatches requests to different providers based on model prefix hints.
///
/// ## Example
///
/// ```rust,ignore
/// use clawdesk_providers::router::RouterProvider;
///
/// let router = RouterProvider::builder()
///     .add_provider("anthropic", anthropic_provider)
///     .add_provider("openai", openai_provider)
///     .add_route("reasoning", "openai", "o1")    // "reasoning:*" → openai o1
///     .default_provider("anthropic")
///     .default_model("claude-sonnet-4-20250514")
///     .build();
/// ```
pub struct RouterProvider {
    /// Named providers.
    providers: Vec<(String, Arc<dyn Provider>)>,
    /// Hint → route mapping.
    routes: HashMap<String, Route>,
    /// Index of the default provider in `providers`.
    default_index: usize,
    /// Default model when no hint is present.
    default_model: String,
}

/// Builder for `RouterProvider`.
pub struct RouterBuilder {
    providers: Vec<(String, Arc<dyn Provider>)>,
    routes: HashMap<String, (String, String)>, // hint → (provider_name, model)
    default_provider: Option<String>,
    default_model: String,
}

impl RouterBuilder {
    fn new() -> Self {
        Self {
            providers: Vec::new(),
            routes: HashMap::new(),
            default_provider: None,
            default_model: String::new(),
        }
    }

    /// Register a named provider.
    pub fn add_provider(
        mut self,
        name: impl Into<String>,
        provider: Arc<dyn Provider>,
    ) -> Self {
        self.providers.push((name.into(), provider));
        self
    }

    /// Add a routing rule: `hint:*` dispatches to `provider_name` with `model`.
    pub fn add_route(
        mut self,
        hint: impl Into<String>,
        provider_name: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        self.routes.insert(hint.into(), (provider_name.into(), model.into()));
        self
    }

    /// Set the default provider (used when no hint matches).
    pub fn default_provider(mut self, name: impl Into<String>) -> Self {
        self.default_provider = Some(name.into());
        self
    }

    /// Set the default model (used when no hint is present).
    pub fn default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Build the router provider.
    pub fn build(self) -> RouterProvider {
        // Build provider index map
        let name_to_idx: HashMap<&str, usize> = self
            .providers
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (name.as_str(), i))
            .collect();

        // Resolve routes
        let routes: HashMap<String, Route> = self
            .routes
            .into_iter()
            .filter_map(|(hint, (provider_name, model))| {
                let idx = *name_to_idx.get(provider_name.as_str())?;
                Some((hint, Route { provider_index: idx, model }))
            })
            .collect();

        // Resolve default
        let default_index = self
            .default_provider
            .as_deref()
            .and_then(|name| name_to_idx.get(name).copied())
            .unwrap_or(0);

        RouterProvider {
            providers: self.providers,
            routes,
            default_index,
            default_model: self.default_model,
        }
    }
}

impl RouterProvider {
    /// Create a new router builder.
    pub fn builder() -> RouterBuilder {
        RouterBuilder::new()
    }

    /// Parse a model string into (hint, model).
    /// `"anthropic:claude-sonnet-4"` → `(Some("anthropic"), "claude-sonnet-4")`
    /// `"gpt-4o"` → `(None, "gpt-4o")`
    fn parse_model_hint(model: &str) -> (Option<&str>, &str) {
        if let Some(colon) = model.find(':') {
            let hint = &model[..colon];
            let model_name = &model[colon + 1..];
            if model_name.is_empty() {
                (None, model)
            } else {
                (Some(hint), model_name)
            }
        } else {
            (None, model)
        }
    }

    /// Resolve which provider and model to use.
    fn resolve(&self, model: &str) -> (usize, String) {
        let (hint, model_name) = Self::parse_model_hint(model);

        if let Some(hint) = hint {
            if let Some(route) = self.routes.get(hint) {
                return (route.provider_index, route.model.clone());
            }

            // Try hint as a provider name
            for (i, (name, _)) in self.providers.iter().enumerate() {
                if name == hint {
                    return (i, model_name.to_string());
                }
            }
        }

        // Fallback to default
        let effective_model = if model.is_empty() {
            self.default_model.clone()
        } else {
            model.to_string()
        };

        (self.default_index, effective_model)
    }
}

#[async_trait]
impl Provider for RouterProvider {
    fn name(&self) -> &str {
        "router"
    }

    fn models(&self) -> Vec<String> {
        let mut models = Vec::new();
        for (name, provider) in &self.providers {
            for model in provider.models() {
                models.push(format!("{name}:{model}"));
            }
        }
        models
    }

    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let (idx, model) = self.resolve(&request.model);
        let (name, provider) = &self.providers[idx];

        debug!(
            hint_model = %request.model,
            resolved_provider = %name,
            resolved_model = %model,
            "routing request"
        );

        // Skip the O(N) clone when the resolved model matches the
        // request model — a common case when there's no aliasing.
        if request.model == model {
            provider.complete(request).await
        } else {
            let mut routed_request = request.clone();
            routed_request.model = model;
            provider.complete(&routed_request).await
        }
    }

    async fn stream(
        &self,
        request: &ProviderRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), ProviderError> {
        let (idx, model) = self.resolve(&request.model);
        let (name, provider) = &self.providers[idx];

        debug!(
            hint_model = %request.model,
            resolved_provider = %name,
            resolved_model = %model,
            "routing stream"
        );

        // same optimization — avoid O(N) clone when model matches.
        if request.model == model {
            provider.stream(request, chunk_tx).await
        } else {
            let mut routed_request = request.clone();
            routed_request.model = model;
            provider.stream(&routed_request, chunk_tx).await
        }
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        // Check all providers
        let mut any_healthy = false;
        for (name, provider) in &self.providers {
            match provider.health_check().await {
                Ok(()) => {
                    debug!(provider = %name, "health check passed");
                    any_healthy = true;
                }
                Err(err) => {
                    tracing::warn!(provider = %name, error = %err, "health check failed");
                }
            }
        }

        if any_healthy {
            Ok(())
        } else {
            Err(ProviderError::server_error("router", 503))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_model_hint() {
        let (hint, model) = RouterProvider::parse_model_hint("anthropic:claude-sonnet-4");
        assert_eq!(hint, Some("anthropic"));
        assert_eq!(model, "claude-sonnet-4");

        let (hint, model) = RouterProvider::parse_model_hint("gpt-4o");
        assert!(hint.is_none());
        assert_eq!(model, "gpt-4o");

        let (hint, model) = RouterProvider::parse_model_hint("openai:");
        assert!(hint.is_none());
        assert_eq!(model, "openai:");
    }

    #[test]
    fn test_resolve_with_route() {
        // Create a minimal router with routes
        let routes = {
            let mut m = HashMap::new();
            m.insert(
                "reasoning".to_string(),
                Route {
                    provider_index: 1,
                    model: "o1".to_string(),
                },
            );
            m
        };

        let router = RouterProvider {
            providers: vec![
                ("anthropic".into(), Arc::new(DummyProvider("anthropic"))),
                ("openai".into(), Arc::new(DummyProvider("openai"))),
            ],
            routes,
            default_index: 0,
            default_model: "claude-sonnet-4".into(),
        };

        // "reasoning:anything" → openai with model "o1"
        let (idx, model) = router.resolve("reasoning:please");
        assert_eq!(idx, 1);
        assert_eq!(model, "o1");

        // "anthropic:claude-sonnet-4" → provider name lookup
        let (idx, model) = router.resolve("anthropic:claude-sonnet-4");
        assert_eq!(idx, 0);
        assert_eq!(model, "claude-sonnet-4");

        // No hint → default
        let (idx, model) = router.resolve("gpt-4o");
        assert_eq!(idx, 0);
        assert_eq!(model, "gpt-4o");

        // Empty model → default model
        let (idx, model) = router.resolve("");
        assert_eq!(idx, 0);
        assert_eq!(model, "claude-sonnet-4");
    }

    // Minimal Provider impl for tests
    struct DummyProvider(&'static str);

    #[async_trait]
    impl Provider for DummyProvider {
        fn name(&self) -> &str {
            self.0
        }
        fn models(&self) -> Vec<String> {
            vec!["model".into()]
        }
        async fn complete(
            &self,
            _request: &ProviderRequest,
        ) -> Result<ProviderResponse, ProviderError> {
            Ok(ProviderResponse {
                content: "ok".into(),
                model: "model".into(),
                provider: self.0.into(),
                usage: crate::TokenUsage::default(),
                tool_calls: Vec::new(),
                finish_reason: crate::FinishReason::Stop,
                latency: std::time::Duration::ZERO,
            })
        }
        async fn health_check(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }
}
