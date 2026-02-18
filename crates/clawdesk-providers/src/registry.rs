//! Provider registry — manages and selects LLM providers.

use crate::Provider;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

/// Registry of available LLM providers.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    /// Register a provider.
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        let name = provider.name().to_string();
        info!(%name, models = ?provider.models(), "registering provider");
        self.providers.insert(name, provider);
    }

    /// Get a provider by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Provider>> {
        self.providers.get(name)
    }

    /// List all registered provider names.
    pub fn list(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// Get the first available provider (for default fallback).
    pub fn default_provider(&self) -> Option<&Arc<dyn Provider>> {
        self.providers.values().next()
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
