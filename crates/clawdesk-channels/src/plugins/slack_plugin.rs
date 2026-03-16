//! Slack channel plugin — threads, slash commands, Block Kit.

use clawdesk_channel_plugins::capability::slack_capabilities;
use clawdesk_channel_plugins::plugin::{ChannelPlugin, ConfigField, PluginManifest};
use async_trait::async_trait;
use serde_json::Value;

pub struct SlackPlugin;

impl SlackPlugin {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl ChannelPlugin for SlackPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "slack".into(),
            name: "Slack".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: slack_capabilities(),
            config_schema: vec![
                ConfigField { name: "bot_token".into(), field_type: "string".into(), required: true, description: "Slack bot OAuth token (xoxb-...)".into() },
                ConfigField { name: "app_token".into(), field_type: "string".into(), required: false, description: "App-level token for Socket Mode (xapp-...)".into() },
                ConfigField { name: "signing_secret".into(), field_type: "string".into(), required: true, description: "Slack signing secret for request verification".into() },
            ],
            channels: vec!["slack".into()],
        }
    }

    async fn activate(&self) -> Result<(), String> { tracing::info!("Slack plugin activated"); Ok(()) }
    async fn deactivate(&self) -> Result<(), String> { Ok(()) }

    async fn on_message(&self, msg: Value) -> Result<Option<Value>, String> {
        if let Some(action) = msg.get("action").and_then(|a| a.as_str()) {
            match action {
                "post_blocks" => Ok(Some(serde_json::json!({ "action": "post_blocks", "status": "queued" }))),
                "open_modal" => Ok(Some(serde_json::json!({ "action": "open_modal", "status": "queued" }))),
                _ => Ok(None),
            }
        } else {
            Ok(None)
        }
    }

    async fn health_check(&self) -> bool { true }
}

impl Default for SlackPlugin {
    fn default() -> Self { Self::new() }
}
