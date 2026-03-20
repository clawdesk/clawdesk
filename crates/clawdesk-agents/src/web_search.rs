//! Web search provider system — plugin-based web search.
//!
//! ## Design
//!
//! A pluggable web search provider system where each search provider
//! (Perplexity, Google, Brave, Grok, etc.) declares its credential
//! requirements, creates a tool definition, and handles search execution.
//! The registry discovers available providers through env var probing
//! and presents them as agent tools.
//!
//! The auto-detect chain is an ordered fallback:
//!   `detect : Env → Option<ProviderId>`
//!   `search : ProviderId × Query → Results`
//!   `chain = detect₁ ⊕ detect₂ ⊕ ... ⊕ detectₙ`  (first match wins)

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

// ── Types ─────────────────────────────────────────────────

/// Web search request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchRequest {
    /// Search query.
    pub query: String,
    /// Maximum number of results to return.
    pub max_results: Option<u32>,
    /// Freshness filter (e.g., "day", "week", "month").
    pub freshness: Option<SearchFreshness>,
    /// Search region/locale (e.g., "en-US").
    pub locale: Option<String>,
}

/// Search freshness filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchFreshness {
    Day,
    Week,
    Month,
    Year,
    /// No freshness constraint.
    All,
}

/// Web search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchResult {
    /// Title of the search result.
    pub title: String,
    /// URL of the search result.
    pub url: String,
    /// Text snippet / description.
    pub snippet: String,
    /// Published date (if available).
    pub published_date: Option<String>,
}

/// Web search response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchResponse {
    /// Search results.
    pub results: Vec<WebSearchResult>,
    /// The provider that executed the search.
    pub provider_id: String,
    /// Optional summary (some providers generate a natural language summary).
    pub summary: Option<String>,
}

/// Credential info for a web search provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchCredentialInfo {
    /// Config path for this credential (e.g., "tools.web.search.perplexity.api_key").
    pub config_path: String,
    /// Environment variable to check.
    pub env_var: String,
    /// Human-readable label.
    pub label: String,
}

/// Web search provider errors.
#[derive(Debug, thiserror::Error)]
pub enum WebSearchError {
    #[error("auth error: {0}")]
    Auth(String),
    #[error("rate limited: {0}")]
    RateLimit(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("not configured: {0}")]
    NotConfigured(String),
}

// ── Provider Trait ────────────────────────────────────────

/// Web search provider — the core abstraction.
///
/// Each provider (Perplexity, Brave, Google, Grok, etc.) implements this
/// trait. The registry discovers providers via credential probing and
/// presents the best available one as an agent tool.
#[async_trait]
pub trait WebSearchProvider: Send + Sync {
    /// Provider identifier (e.g., "perplexity", "brave", "google").
    fn id(&self) -> &str;

    /// Display name.
    fn display_name(&self) -> &str;

    /// Credential requirements for this provider.
    fn credential_info(&self) -> WebSearchCredentialInfo;

    /// Auto-detection order (lower = checked first).
    /// Providers with credentials are preferred by the registry.
    fn auto_detect_order(&self) -> u32;

    /// Check if this provider has valid credentials available.
    fn is_configured(&self, env_vars: &HashMap<String, String>) -> bool {
        let info = self.credential_info();
        env_vars.contains_key(&info.env_var)
    }

    /// Execute a web search.
    async fn search(
        &self,
        request: &WebSearchRequest,
        api_key: &str,
    ) -> Result<WebSearchResponse, WebSearchError>;
}

// ── Provider Registry ─────────────────────────────────────

/// Registry for web search providers.
///
/// Handles auto-detection (scan env vars to find which providers are
/// configured) and fallback chains.
pub struct WebSearchRegistry {
    providers: Vec<Arc<dyn WebSearchProvider>>,
}

impl WebSearchRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Register a provider. Providers are sorted by `auto_detect_order`.
    pub fn register(&mut self, provider: Arc<dyn WebSearchProvider>) {
        tracing::info!(
            provider = provider.id(),
            order = provider.auto_detect_order(),
            "registered web search provider"
        );
        self.providers.push(provider);
        self.providers.sort_by_key(|p| p.auto_detect_order());
    }

    /// Find the best available provider (first configured in detection order).
    pub fn auto_detect(&self, env_vars: &HashMap<String, String>) -> Option<&Arc<dyn WebSearchProvider>> {
        self.providers
            .iter()
            .find(|p| p.is_configured(env_vars))
    }

    /// List all registered providers with their configuration status.
    pub fn list_with_status(
        &self,
        env_vars: &HashMap<String, String>,
    ) -> Vec<(&str, bool)> {
        self.providers
            .iter()
            .map(|p| (p.id(), p.is_configured(env_vars)))
            .collect()
    }

    /// Search using the best available provider (auto-detect + fallback).
    pub async fn search_auto(
        &self,
        request: &WebSearchRequest,
        env_vars: &HashMap<String, String>,
    ) -> Result<WebSearchResponse, WebSearchError> {
        for provider in &self.providers {
            let info = provider.credential_info();
            if let Some(api_key) = env_vars.get(&info.env_var) {
                match provider.search(request, api_key).await {
                    Ok(response) => return Ok(response),
                    Err(e) => {
                        tracing::warn!(
                            provider = provider.id(),
                            error = %e,
                            "web search fallback, trying next provider"
                        );
                    }
                }
            }
        }

        Err(WebSearchError::NotConfigured(
            "No web search providers configured. Set PERPLEXITY_API_KEY, BRAVE_API_KEY, \
             or GOOGLE_SEARCH_API_KEY environment variable."
                .into(),
        ))
    }

    /// Get a specific provider by ID.
    pub fn get(&self, id: &str) -> Option<&Arc<dyn WebSearchProvider>> {
        self.providers.iter().find(|p| p.id() == id)
    }
}

impl Default for WebSearchRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_serialization() {
        let f = SearchFreshness::Week;
        let json = serde_json::to_string(&f).unwrap();
        assert_eq!(json, r#""week""#);

        let parsed: SearchFreshness = serde_json::from_str(r#""month""#).unwrap();
        assert_eq!(parsed, SearchFreshness::Month);
    }

    #[test]
    fn empty_registry() {
        let reg = WebSearchRegistry::new();
        let env = HashMap::new();
        assert!(reg.auto_detect(&env).is_none());
        assert!(reg.list_with_status(&env).is_empty());
    }

    #[test]
    fn search_request_defaults() {
        let req = WebSearchRequest {
            query: "rust async trait".into(),
            max_results: Some(5),
            freshness: Some(SearchFreshness::Week),
            locale: None,
        };
        assert_eq!(req.max_results, Some(5));
    }
}
