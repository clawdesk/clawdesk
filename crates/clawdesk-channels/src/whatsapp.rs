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
use tracing::{debug, info, warn};

pub struct WhatsAppChannel {
    phone_number_id: String,
    access_token: String,
    verify_token: String,
    client: Client,
    /// Stored sink for webhook injection.
    sink: tokio::sync::RwLock<Option<Arc<dyn MessageSink>>>,
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
            sink: tokio::sync::RwLock::new(None),
        }
    }

    fn api_url(&self) -> String {
        format!(
            "https://graph.facebook.com/v18.0/{}/messages",
            self.phone_number_id
        )
    }

    /// Verify token for webhook setup (GET request from Meta).
    pub fn verify_token(&self) -> &str {
        &self.verify_token
    }

    /// Inject a webhook payload from the gateway's HTTP handler.
    ///
    /// WhatsApp uses push-based webhooks rather than a pull-based loop.
    /// The gateway receives the POST from Meta and forwards the raw JSON
    /// body here for normalization and dispatch.
    pub async fn inject_webhook(&self, payload: &serde_json::Value) {
        let sink = self.sink.read().await;
        let Some(sink) = sink.as_ref() else {
            warn!("WhatsApp webhook received but no sink configured");
            return;
        };

        // WhatsApp webhook payload structure:
        // { "entry": [{ "changes": [{ "value": { "messages": [...] } }] }] }
        let entries = match payload.get("entry").and_then(|e| e.as_array()) {
            Some(e) => e,
            None => return,
        };

        for entry in entries {
            let changes = match entry.get("changes").and_then(|c| c.as_array()) {
                Some(c) => c,
                None => continue,
            };

            for change in changes {
                let value = match change.get("value") {
                    Some(v) => v,
                    None => continue,
                };

                let messages = match value.get("messages").and_then(|m| m.as_array()) {
                    Some(m) => m,
                    None => continue,
                };

                for msg in messages {
                    let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if msg_type != "text" {
                        debug!(msg_type, "WhatsApp: skipping non-text message");
                        continue;
                    }

                    let from = match msg.get("from").and_then(|f| f.as_str()) {
                        Some(f) => f,
                        None => continue,
                    };

                    let body = match msg
                        .get("text")
                        .and_then(|t| t.get("body"))
                        .and_then(|b| b.as_str())
                    {
                        Some(b) => b,
                        None => continue,
                    };

                    let wamid = msg
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("unknown");

                    let sender_name = value
                        .get("contacts")
                        .and_then(|c| c.as_array())
                        .and_then(|c| c.first())
                        .and_then(|c| c.get("profile"))
                        .and_then(|p| p.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or(from);

                    let normalized = clawdesk_types::message::NormalizedMessage {
                        id: uuid::Uuid::new_v4(),
                        session_key: clawdesk_types::session::SessionKey::new(
                            ChannelId::WhatsApp,
                            from,
                        ),
                        body: body.to_string(),
                        body_for_agent: None,
                        sender: clawdesk_types::message::SenderIdentity {
                            id: from.to_string(),
                            display_name: sender_name.to_string(),
                            channel: ChannelId::WhatsApp,
                        },
                        media: vec![],
                        artifact_refs: vec![],
                        reply_context: None,
                        origin: clawdesk_types::message::MessageOrigin::WhatsApp {
                            phone_number: from.to_string(),
                            message_id: wamid.to_string(),
                        },
                        timestamp: chrono::Utc::now(),
                    };

                    sink.on_message(normalized).await;
                }
            }
        }
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

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        // Store the sink so inject_webhook() can dispatch inbound messages.
        // WhatsApp Cloud API uses push-based webhooks — Meta POSTs to our
        // gateway endpoint, which calls inject_webhook() on this channel.
        *self.sink.write().await = Some(sink);

        // Verify access token by fetching the phone number metadata
        let verify_url = format!(
            "https://graph.facebook.com/v18.0/{}",
            self.phone_number_id
        );
        match self
            .client
            .get(&verify_url)
            .bearer_auth(&self.access_token)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                info!("WhatsApp Cloud API credentials verified");
            }
            Ok(resp) => {
                warn!(
                    status = %resp.status(),
                    "WhatsApp Cloud API token may be invalid"
                );
            }
            Err(e) => {
                warn!(error = %e, "WhatsApp Cloud API verification failed (network)");
            }
        }

        info!(
            "WhatsApp channel started (webhook mode — configure Meta webhook to POST to gateway)"
        );
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
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

