//! Plugin registry — named lookup, hot-reload tracking.

use crate::host::{PluginHost, PluginInstance};
use clawdesk_types::error::PluginError;
use clawdesk_types::plugin::{PluginInfo, PluginManifest, PluginSource, PluginState};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Provides categorized plugin lookup by capability.
pub struct PluginRegistry {
    host: Arc<PluginHost>,
    /// Index: tool name → plugin name.
    tool_index: RwLock<HashMap<String, String>>,
    /// Index: command name → plugin name.
    command_index: RwLock<HashMap<String, String>>,
    /// Index: channel name → plugin name.
    channel_index: RwLock<HashMap<String, String>>,
    /// Slot map: slot name → occupying plugin name (mutual exclusion).
    slot_map: RwLock<HashMap<String, String>>,
}

impl PluginRegistry {
    pub fn new(host: Arc<PluginHost>) -> Self {
        Self {
            host,
            tool_index: RwLock::new(HashMap::new()),
            command_index: RwLock::new(HashMap::new()),
            channel_index: RwLock::new(HashMap::new()),
            slot_map: RwLock::new(HashMap::new()),
        }
    }

    /// Rebuild all indexes from current plugin state.
    pub async fn rebuild_indexes(&self) {
        let plugins = self.host.list_plugins().await;
        let mut tools = HashMap::new();
        let mut commands = HashMap::new();
        let mut channels = HashMap::new();
        let mut slots = HashMap::new();

        for info in &plugins {
            if info.state != PluginState::Active {
                continue;
            }
            let name = &info.manifest.name;

            for tool in &info.manifest.capabilities.tools {
                tools.insert(tool.clone(), name.clone());
            }
            for cmd in &info.manifest.capabilities.commands {
                commands.insert(cmd.clone(), name.clone());
            }
            for ch in &info.manifest.capabilities.channels {
                channels.insert(ch.clone(), name.clone());
            }
            if let Some(ref slot) = info.manifest.capabilities.slot {
                slots.insert(slot.clone(), name.clone());
            }
        }

        *self.tool_index.write().await = tools;
        *self.command_index.write().await = commands;
        *self.channel_index.write().await = channels;
        *self.slot_map.write().await = slots;
        info!("Plugin indexes rebuilt");
    }

    /// Find which plugin provides a specific tool.
    pub async fn find_tool_provider(&self, tool_name: &str) -> Option<String> {
        self.tool_index.read().await.get(tool_name).cloned()
    }

    /// Find which plugin provides a specific command.
    pub async fn find_command_provider(&self, command: &str) -> Option<String> {
        self.command_index.read().await.get(command).cloned()
    }

    /// Find which plugin provides a specific channel.
    pub async fn find_channel_provider(&self, channel: &str) -> Option<String> {
        self.channel_index.read().await.get(channel).cloned()
    }

    /// Find which plugin currently occupies a slot.
    pub async fn find_slot_occupant(&self, slot: &str) -> Option<String> {
        self.slot_map.read().await.get(slot).cloned()
    }

    /// Install a plugin with slot-aware CAS (compare-and-swap) semantics.
    ///
    /// If the plugin declares a slot and another plugin already occupies it,
    /// the old occupant is deactivated first (mutual exclusion).
    pub async fn install(
        &self,
        manifest: PluginManifest,
        source: PluginSource,
    ) -> Result<(), PluginError> {
        // CAS slot swap: if this plugin declares a slot, evict the current occupant.
        if let Some(ref slot) = manifest.capabilities.slot {
            let current_occupant = self.slot_map.read().await.get(slot).cloned();
            if let Some(ref occupant) = current_occupant {
                if occupant != &manifest.name {
                    info!(
                        slot = %slot,
                        evicting = %occupant,
                        activating = %manifest.name,
                        "CAS slot swap: evicting current occupant"
                    );
                    if let Err(e) = self.host.deactivate(occupant).await {
                        warn!(plugin = %occupant, error = %e, "Failed to deactivate slot occupant");
                    }
                }
            }
        }

        self.host
            .install_and_activate(manifest, source)
            .await?;
        self.rebuild_indexes().await;
        Ok(())
    }

    /// Uninstall a plugin and rebuild indexes.
    pub async fn uninstall(&self, name: &str) -> Result<(), PluginError> {
        self.host.deactivate(name).await?;
        self.rebuild_indexes().await;
        Ok(())
    }

    /// Get all active plugin infos.
    pub async fn active_plugins(&self) -> Vec<PluginInfo> {
        self.host
            .list_plugins()
            .await
            .into_iter()
            .filter(|p| p.state == PluginState::Active)
            .collect()
    }

    /// Hot-reload a plugin: deactivate → reload → reactivate.
    pub async fn reload(&self, name: &str, manifest: PluginManifest, source: PluginSource) -> Result<(), PluginError> {
        // Deactivate existing.
        if let Err(e) = self.host.deactivate(name).await {
            warn!(plugin = %name, error = %e, "Deactivation failed during reload");
        }

        // Re-install.
        self.host.install_and_activate(manifest, source).await?;
        self.rebuild_indexes().await;
        info!(plugin = %name, "Plugin reloaded");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{PluginFactory, PluginInstance};
    use async_trait::async_trait;
    use clawdesk_types::plugin::PluginCapabilities;

    struct TestFactory;

    #[async_trait]
    impl PluginFactory for TestFactory {
        async fn create(
            &self,
            _manifest: &PluginManifest,
        ) -> Result<Arc<dyn PluginInstance>, PluginError> {
            Ok(Arc::new(TestPlugin))
        }
    }

    struct TestPlugin;

    #[async_trait]
    impl PluginInstance for TestPlugin {
        async fn on_activate(&self) -> Result<(), String> { Ok(()) }
        async fn on_deactivate(&self) -> Result<(), String> { Ok(()) }
        async fn on_message(&self, p: serde_json::Value) -> Result<serde_json::Value, String> { Ok(p) }
    }

    #[tokio::test]
    async fn test_registry_install() {
        let host = Arc::new(PluginHost::new(Arc::new(TestFactory), 10));
        let registry = PluginRegistry::new(host);

        let manifest = PluginManifest {
            name: "my-tool".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            author: "test".to_string(),
            min_sdk_version: "0.1.0".to_string(),
            dependencies: vec![],
            capabilities: PluginCapabilities {
                tools: vec!["weather".to_string()],
                ..Default::default()
            },
        };

        registry.install(manifest, PluginSource::Bundled).await.unwrap();

        let provider = registry.find_tool_provider("weather").await;
        assert_eq!(provider.as_deref(), Some("my-tool"));
    }

    #[tokio::test]
    async fn test_registry_uninstall() {
        let host = Arc::new(PluginHost::new(Arc::new(TestFactory), 10));
        let registry = PluginRegistry::new(host);

        let manifest = PluginManifest {
            name: "p1".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            author: "test".to_string(),
            min_sdk_version: "0.1.0".to_string(),
            dependencies: vec![],
            capabilities: PluginCapabilities::default(),
        };

        registry.install(manifest, PluginSource::Bundled).await.unwrap();
        registry.uninstall("p1").await.unwrap();

        let active = registry.active_plugins().await;
        assert!(active.is_empty());
    }
}
