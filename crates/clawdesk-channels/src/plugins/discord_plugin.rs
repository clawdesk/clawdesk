//! Discord channel plugin — registers Discord-specific capabilities and actions.

use clawdesk_channel_plugins::capability::discord_capabilities;
use clawdesk_channel_plugins::plugin::{ChannelPlugin, ConfigField, PluginManifest};
use async_trait::async_trait;
use serde_json::Value;

pub struct DiscordPlugin;

impl DiscordPlugin {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ChannelPlugin for DiscordPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "discord".into(),
            name: "Discord".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: discord_capabilities(),
            config_schema: vec![
                ConfigField { name: "bot_token".into(), field_type: "string".into(), required: true, description: "Discord bot token".into() },
                ConfigField { name: "application_id".into(), field_type: "string".into(), required: true, description: "Discord application ID".into() },
                ConfigField { name: "guild_ids".into(), field_type: "string_array".into(), required: false, description: "Allowed guild IDs (empty = all)".into() },
            ],
            channels: vec!["discord".into()],
        }
    }

    async fn activate(&self) -> Result<(), String> {
        tracing::info!("Discord plugin activated");
        Ok(())
    }

    async fn deactivate(&self) -> Result<(), String> {
        tracing::info!("Discord plugin deactivated");
        Ok(())
    }

    async fn on_message(&self, msg: Value) -> Result<Option<Value>, String> {
        // Guild admin action dispatch
        if let Some(action) = msg.get("action").and_then(|a| a.as_str()) {
            match action {
                "assign_role" => {
                    let user_id = msg.get("user_id").and_then(|v| v.as_str()).unwrap_or("");
                    let role_id = msg.get("role_id").and_then(|v| v.as_str()).unwrap_or("");
                    Ok(Some(serde_json::json!({ "action": "assign_role", "user_id": user_id, "role_id": role_id, "status": "queued" })))
                }
                "create_thread" => {
                    let name = msg.get("name").and_then(|v| v.as_str()).unwrap_or("Thread");
                    Ok(Some(serde_json::json!({ "action": "create_thread", "name": name, "status": "queued" })))
                }
                _ => Ok(None),
            }
        } else {
            Ok(None)
        }
    }

    async fn health_check(&self) -> bool {
        true // Delegate to Discord gateway health in production
    }
}

impl Default for DiscordPlugin {
    fn default() -> Self { Self::new() }
}
