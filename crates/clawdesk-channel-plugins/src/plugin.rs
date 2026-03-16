//! Channel plugin trait and registry with dynamic self-registration.

use crate::capability::CapabilitySet;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Plugin manifest — declares capabilities and config schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub capabilities: CapabilitySet,
    pub config_schema: Vec<ConfigField>,
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    pub name: String,
    pub field_type: String,
    pub required: bool,
    pub description: String,
}

/// Trait for channel plugins — self-contained units with lifecycle hooks.
#[async_trait]
pub trait ChannelPlugin: Send + Sync {
    fn manifest(&self) -> PluginManifest;
    async fn activate(&self) -> Result<(), String>;
    async fn deactivate(&self) -> Result<(), String>;
    async fn on_message(&self, msg: serde_json::Value) -> Result<Option<serde_json::Value>, String>;
    async fn health_check(&self) -> bool;
}

/// Dynamic plugin registry with self-registration.
pub struct PluginRegistry {
    plugins: HashMap<String, Arc<dyn ChannelPlugin>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self { plugins: HashMap::new() }
    }

    pub fn register(&mut self, plugin: Arc<dyn ChannelPlugin>) {
        let manifest = plugin.manifest();
        tracing::info!(id = %manifest.id, name = %manifest.name, "registering channel plugin");
        self.plugins.insert(manifest.id.clone(), plugin);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ChannelPlugin>> {
        self.plugins.get(id).cloned()
    }

    pub fn list(&self) -> Vec<PluginManifest> {
        self.plugins.values().map(|p| p.manifest()).collect()
    }

    /// Get combined capabilities across all active plugins.
    pub fn combined_capabilities(&self) -> CapabilitySet {
        self.plugins.values()
            .map(|p| p.manifest().capabilities)
            .fold(CapabilitySet::new(), |acc, caps| acc.union(caps))
    }

    pub fn count(&self) -> usize {
        self.plugins.len()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::ChannelCapability;

    struct MockPlugin;

    #[async_trait]
    impl ChannelPlugin for MockPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test".into(), name: "Test Plugin".into(), version: "0.1".into(),
                capabilities: CapabilitySet::new().with(ChannelCapability::SendText),
                config_schema: vec![], channels: vec!["test".into()],
            }
        }
        async fn activate(&self) -> Result<(), String> { Ok(()) }
        async fn deactivate(&self) -> Result<(), String> { Ok(()) }
        async fn on_message(&self, _msg: serde_json::Value) -> Result<Option<serde_json::Value>, String> { Ok(None) }
        async fn health_check(&self) -> bool { true }
    }

    #[test]
    fn register_and_list() {
        let mut reg = PluginRegistry::new();
        reg.register(Arc::new(MockPlugin));
        assert_eq!(reg.count(), 1);
        assert!(reg.get("test").is_some());
    }
}
