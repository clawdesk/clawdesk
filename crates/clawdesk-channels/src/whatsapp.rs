//! WhatsApp channel adapter via Cloud API.
//!
//! Uses the WhatsApp Business Cloud API (Graph API v18.0+).
//! Requires: phone_number_id, access_token, verify_token (for webhook).
//!
//! ## Architecture
//! - Inbound: Webhook receives messages from WhatsApp
//! - Outbound: POST to Graph API to send messages
//! - Media: Download via URL, upload via multipart

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{DeliveryReceipt, OutboundMessage};
use chrono::Utc;
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

pub struct WhatsAppChannel {
    phone_number_id: String,
    access_token: String,
    verify_token: String,
    client: Client,
}

impl WhatsAppChannel {
    pub fn new(phone_number_id: String, access_token: String, verify_token: String) -> Self {
        Self {
            phone_number_id,
            access_token,
            verify_token,
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
        }
    }

    fn api_url(&self) -> String {
        format!(
            "https://graph.facebook.com/v18.0/{}/messages",
            self.phone_number_id
        )
    }
}

#[async_trait]
impl Channel for WhatsAppChannel {
    fn id(&self) -> ChannelId {
        ChannelId::WhatsApp
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "WhatsApp".to_string(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(4096),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        // WhatsApp uses webhooks — start webhook server or register
        info!("WhatsApp channel started (webhook mode)");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let target = match &msg.origin {
            clawdesk_types::message::MessageOrigin::WhatsApp { phone_number, .. } => {
                phone_number.clone()
            }
            _ => return Err("cannot send WhatsApp message without WhatsApp origin".into()),
        };

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "to": target,
            "type": "text",
            "text": { "body": msg.body }
        });

        let resp = self
            .client
            .post(&self.api_url())
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("WhatsApp send failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("WhatsApp HTTP {}: {}", status, text));
        }

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("WhatsApp response parse: {}", e))?;

        let msg_id = data
            .pointer("/messages/0/id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        debug!(%msg_id, "sent WhatsApp message");
        Ok(DeliveryReceipt {
            channel: ChannelId::WhatsApp,
            message_id: msg_id,
            timestamp: Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        info!("WhatsApp channel stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whatsapp_meta() {
        let ch = WhatsAppChannel::new(
            "123".to_string(),
            "token".to_string(),
            "verify".to_string(),
        );
        assert_eq!(ch.meta().display_name, "WhatsApp");
        assert_eq!(ch.id(), ChannelId::WhatsApp);
    }
}

