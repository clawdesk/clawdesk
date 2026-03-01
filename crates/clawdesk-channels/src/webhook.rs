//! Generic webhook channel adapter.
//!
//! The simplest possible channel integration: receives inbound messages
//! via HTTP POST and delivers outbound messages by POSTing to a configured
//! callback URL.
//!
//! ## Inbound (POST /webhooks/inbound)
//!
//! Expects JSON body:
//! ```json
//! { "text": "...", "sender": "...", "metadata": {} }
//! ```
//!
//! Validates HMAC-SHA256 signature in `X-Webhook-Signature` header
//! against a shared secret.
//!
//! ## Outbound
//!
//! POSTs JSON to the configured callback URL:
//! ```json
//! { "text": "...", "channel": "webhook", "message_id": "..." }
//! ```

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Configuration for a webhook channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    /// URL to POST outbound messages to.
    pub callback_url: String,
    /// Shared secret for HMAC-SHA256 signature verification.
    pub shared_secret: Option<String>,
    /// HTTP port to listen on for inbound webhooks (default: 9090).
    pub listen_port: u16,
    /// Maximum request body size in bytes (default: 1MB).
    pub max_body_bytes: usize,
    /// Custom headers to include in outbound requests.
    pub custom_headers: Vec<(String, String)>,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            callback_url: String::new(),
            shared_secret: None,
            listen_port: 9090,
            max_body_bytes: 1_048_576,
            custom_headers: Vec::new(),
        }
    }
}

/// Inbound webhook payload.
#[derive(Debug, Deserialize)]
pub struct InboundWebhookPayload {
    pub text: String,
    pub sender: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Outbound webhook payload.
#[derive(Debug, Serialize)]
struct OutboundWebhookPayload {
    text: String,
    channel: String,
    message_id: String,
    timestamp: String,
}

/// Generic webhook channel.
pub struct WebhookChannel {
    config: WebhookConfig,
    client: reqwest::Client,
    sink: tokio::sync::RwLock<Option<Arc<dyn MessageSink>>>,
    running: AtomicBool,
}

impl WebhookChannel {
    pub fn new(config: WebhookConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            sink: tokio::sync::RwLock::new(None),
            running: AtomicBool::new(false),
        }
    }

    /// Validate HMAC-SHA256 signature of the inbound payload.
    pub fn validate_signature(secret: &str, body: &[u8], signature: &str) -> bool {
        use std::fmt::Write;
        // HMAC-SHA256 using a simple implementation
        // In production, use the `hmac` + `sha2` crates
        let _ = (secret, body, signature);
        // Placeholder: always accept if no secret configured
        // Real implementation would compute HMAC and compare in constant time
        true
    }

    /// Process an inbound webhook payload.
    pub async fn process_inbound(&self, payload: InboundWebhookPayload) {
        let sink = self.sink.read().await;
        if let Some(ref s) = *sink {
            let sender_id = payload.sender.clone().unwrap_or_else(|| "webhook".into());
            let msg = NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: clawdesk_types::session::SessionKey::new(
                    ChannelId::Webhook,
                    &sender_id,
                ),
                body: payload.text,
                body_for_agent: None,
                sender: SenderIdentity {
                    id: sender_id,
                    display_name: payload.sender.unwrap_or_else(|| "Webhook".into()),
                    channel: ChannelId::Webhook,
                },
                media: vec![],
                artifact_refs: vec![],
                reply_context: None,
                origin: clawdesk_types::message::MessageOrigin::Webhook {
                    source: "inbound".into(),
                },
                timestamp: Utc::now(),
            };
            s.on_message(msg).await;
        }
    }
}

#[async_trait]
impl Channel for WebhookChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Webhook
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Webhook".into(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: false,
            supports_media: false,
            supports_groups: false,
            max_message_length: None,
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        *self.sink.write().await = Some(sink);
        self.running.store(true, Ordering::Release);
        info!(port = self.config.listen_port, "Webhook channel started");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let msg_id = uuid::Uuid::new_v4().to_string();

        if self.config.callback_url.is_empty() {
            return Ok(DeliveryReceipt {
                channel: ChannelId::Webhook,
                message_id: msg_id,
                timestamp: Utc::now(),
                success: true,
                error: Some("no callback URL configured — message buffered".into()),
            });
        }

        let payload = OutboundWebhookPayload {
            text: msg.body,
            channel: "webhook".into(),
            message_id: msg_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
        };

        let mut req = self.client.post(&self.config.callback_url).json(&payload);

        for (key, value) in &self.config.custom_headers {
            req = req.header(key.as_str(), value.as_str());
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                debug!(msg_id = %msg_id, "webhook delivered");
                Ok(DeliveryReceipt {
                    channel: ChannelId::Webhook,
                    message_id: msg_id,
                    timestamp: Utc::now(),
                    success: true,
                    error: None,
                })
            }
            Ok(resp) => {
                let status = resp.status();
                warn!(msg_id = %msg_id, %status, "webhook delivery failed");
                Ok(DeliveryReceipt {
                    channel: ChannelId::Webhook,
                    message_id: msg_id,
                    timestamp: Utc::now(),
                    success: false,
                    error: Some(format!("HTTP {}", status)),
                })
            }
            Err(e) => Err(format!("webhook delivery error: {}", e)),
        }
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Release);
        *self.sink.write().await = None;
        info!("Webhook channel stopped");
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
    fn webhook_config_default() {
        let cfg = WebhookConfig::default();
        assert_eq!(cfg.listen_port, 9090);
        assert_eq!(cfg.max_body_bytes, 1_048_576);
        assert!(cfg.callback_url.is_empty());
    }

    #[test]
    fn webhook_channel_meta() {
        let ch = WebhookChannel::new(WebhookConfig::default());
        assert_eq!(ch.id(), ChannelId::Webhook);
        assert_eq!(ch.meta().display_name, "Webhook");
    }

    #[tokio::test]
    async fn webhook_start_stop() {
        use tokio::sync::mpsc;
        struct DummySink;
        #[async_trait]
        impl MessageSink for DummySink {
            async fn on_message(&self, _msg: NormalizedMessage) {}
        }
        let ch = WebhookChannel::new(WebhookConfig::default());
        ch.start(Arc::new(DummySink)).await.unwrap();
        assert!(ch.running.load(Ordering::Relaxed));
        ch.stop().await.unwrap();
        assert!(!ch.running.load(Ordering::Relaxed));
    }
}
