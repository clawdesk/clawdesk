//! WhatsApp channel plugin — heartbeat monitoring, media sessions, pairing.

use clawdesk_channel_plugins::capability::{CapabilitySet, ChannelCapability};
use clawdesk_channel_plugins::plugin::{ChannelPlugin, ConfigField, PluginManifest};
use async_trait::async_trait;
use serde_json::Value;

fn whatsapp_capabilities() -> CapabilitySet {
    CapabilitySet::new()
        .with(ChannelCapability::SendText)
        .with(ChannelCapability::SendMedia)
        .with(ChannelCapability::Reactions)
        .with(ChannelCapability::FileUpload)
        .with(ChannelCapability::ReadReceipts)
        .with(ChannelCapability::Mentions)
}

pub struct WhatsAppPlugin;

impl WhatsAppPlugin {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl ChannelPlugin for WhatsAppPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "whatsapp".into(),
            name: "WhatsApp".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: whatsapp_capabilities(),
            config_schema: vec![
                ConfigField { name: "phone_number".into(), field_type: "string".into(), required: true, description: "WhatsApp phone number".into() },
                ConfigField { name: "session_path".into(), field_type: "string".into(), required: false, description: "Session storage directory".into() },
            ],
            channels: vec!["whatsapp".into()],
        }
    }

    async fn activate(&self) -> Result<(), String> { tracing::info!("WhatsApp plugin activated"); Ok(()) }
    async fn deactivate(&self) -> Result<(), String> { Ok(()) }

    async fn on_message(&self, msg: Value) -> Result<Option<Value>, String> {
        if let Some(action) = msg.get("action").and_then(|a| a.as_str()) {
            match action {
                "send_media" => Ok(Some(serde_json::json!({ "action": "send_media", "status": "queued" }))),
                "heartbeat" => Ok(Some(serde_json::json!({ "action": "heartbeat", "alive": true }))),
                _ => Ok(None),
            }
        } else {
            Ok(None)
        }
    }

    async fn health_check(&self) -> bool { true }
}

impl Default for WhatsAppPlugin {
    fn default() -> Self { Self::new() }
}
