//! Telegram channel plugin — inline keyboards, native polls, forum topics.

use clawdesk_channel_plugins::capability::telegram_capabilities;
use clawdesk_channel_plugins::plugin::{ChannelPlugin, ConfigField, PluginManifest};
use async_trait::async_trait;
use serde_json::Value;

pub struct TelegramPlugin;

impl TelegramPlugin {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl ChannelPlugin for TelegramPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "telegram".into(),
            name: "Telegram".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: telegram_capabilities(),
            config_schema: vec![
                ConfigField { name: "bot_token".into(), field_type: "string".into(), required: true, description: "Telegram bot token from @BotFather".into() },
                ConfigField { name: "allowed_chat_ids".into(), field_type: "string_array".into(), required: false, description: "Allowed chat/group IDs".into() },
                ConfigField { name: "webhook_url".into(), field_type: "string".into(), required: false, description: "Webhook URL (if not using long-poll)".into() },
            ],
            channels: vec!["telegram".into()],
        }
    }

    async fn activate(&self) -> Result<(), String> {
        tracing::info!("Telegram plugin activated");
        Ok(())
    }

    async fn deactivate(&self) -> Result<(), String> { Ok(()) }

    async fn on_message(&self, msg: Value) -> Result<Option<Value>, String> {
        if let Some(action) = msg.get("action").and_then(|a| a.as_str()) {
            match action {
                "send_poll" => {
                    let question = msg.get("question").and_then(|v| v.as_str()).unwrap_or("");
                    Ok(Some(serde_json::json!({ "action": "send_poll", "question": question, "status": "queued" })))
                }
                "inline_keyboard" => {
                    Ok(Some(serde_json::json!({ "action": "inline_keyboard", "status": "queued" })))
                }
                _ => Ok(None),
            }
        } else {
            Ok(None)
        }
    }

    async fn health_check(&self) -> bool { true }
}

impl Default for TelegramPlugin {
    fn default() -> Self { Self::new() }
}
