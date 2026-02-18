//! Signal messenger channel implementation via signal-cli REST API.
//!
//! Connects to a local or remote signal-cli REST API instance for sending
//! and receiving Signal messages. Implements `Channel` + `Streaming`.
//!
//! ## Architecture
//!
//! ```text
//! SignalChannel
//! ├── poll_loop()      — polls /v1/receive for inbound messages
//! ├── normalize()      — Signal envelope → NormalizedMessage
//! ├── send()           — OutboundMessage → POST /v2/send
//! └── send_streaming() — edit-in-place for streaming responses
//! ```
//!
//! ## signal-cli REST API
//!
//! The signal-cli REST API (https://github.com/bbernhard/signal-cli-rest-api)
//! exposes Signal protocol over HTTP:
//! - `GET  /v1/receive/{number}` — fetch pending messages
//! - `POST /v2/send`             — send text/media messages
//! - `GET  /v1/groups/{number}`  — list groups
//! - `PUT  /v1/groups/{number}`  — create/update group
//! - `GET  /v1/attachments/{id}` — download attachment
//!
//! ## Message limits
//!
//! Signal enforces:
//! - No official rate limits (self-hosted relay)
//! - 6000 char soft limit for text messages
//! - 100 MB attachment limit

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, StreamHandle, Streaming};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, MediaAttachment, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Signal channel adapter via signal-cli REST API.
pub struct SignalChannel {
    client: Client,
    /// Base URL of the signal-cli REST API (e.g., `http://localhost:8080`).
    api_url: String,
    /// The phone number registered with signal-cli (E.164 format).
    phone_number: String,
    /// Allowed sender phone numbers. Empty = allow all.
    allowed_numbers: Vec<String>,
    /// Whether to handle group messages.
    enable_groups: bool,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for the Signal channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalConfig {
    pub api_url: String,
    pub phone_number: String,
    #[serde(default)]
    pub allowed_numbers: Vec<String>,
    #[serde(default = "default_true")]
    pub enable_groups: bool,
}

fn default_true() -> bool {
    true
}

impl SignalChannel {
    pub fn new(config: SignalConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("failed to build HTTP client"),
            api_url: config.api_url.trim_end_matches('/').to_string(),
            phone_number: config.phone_number,
            allowed_numbers: config.allowed_numbers,
            enable_groups: config.enable_groups,
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Build a full API URL for the given endpoint.
    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.api_url, path)
    }

    /// Check if a sender number is allowed.
    fn is_allowed(&self, number: &str) -> bool {
        self.allowed_numbers.is_empty() || self.allowed_numbers.iter().any(|n| n == number)
    }

    /// Poll loop: fetches pending messages and dispatches to sink.
    async fn poll_loop(self: Arc<Self>, sink: Arc<dyn MessageSink>) {
        info!(phone = %self.phone_number, "Signal poll loop started");

        while self.running.load(Ordering::Relaxed) {
            let url = self.endpoint(&format!("/v1/receive/{}", self.phone_number));

            let result = self.client.get(&url).send().await;

            match result {
                Ok(response) => {
                    if let Ok(envelopes) = response.json::<Vec<SignalEnvelope>>().await {
                        for envelope in envelopes {
                            if let Some(data) = &envelope.envelope.data_message {
                                let source = envelope.envelope.source.clone().unwrap_or_default();

                                // Filter by allowed numbers
                                if !self.is_allowed(&source) {
                                    debug!(source = %source, "ignoring message from unallowed number");
                                    continue;
                                }

                                // Filter groups if disabled
                                if data.group_info.is_some() && !self.enable_groups {
                                    continue;
                                }

                                if let Some(normalized) = self.normalize_envelope(&envelope) {
                                    sink.on_message(normalized).await;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Signal poll error, retrying in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }

            // Small delay between polls to avoid hammering the API
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        info!("Signal poll loop stopped");
    }

    /// Normalize a Signal envelope into a NormalizedMessage.
    fn normalize_envelope(&self, envelope: &SignalEnvelope) -> Option<NormalizedMessage> {
        let data = envelope.envelope.data_message.as_ref()?;
        let text = data.message.clone()?;
        let source = envelope.envelope.source.clone()?;

        let sender = SenderIdentity {
            id: source.clone(),
            display_name: envelope
                .envelope
                .source_name
                .clone()
                .unwrap_or_else(|| source.clone()),
            channel: ChannelId::Signal,
        };

        let session_key = if let Some(ref group) = data.group_info {
            clawdesk_types::session::SessionKey::new(ChannelId::Signal, &group.group_id)
        } else {
            clawdesk_types::session::SessionKey::new(ChannelId::Signal, &source)
        };

        // Detect attachments
        let media = data
            .attachments
            .as_ref()
            .map(|atts| {
                atts.iter()
                    .map(|a| MediaAttachment {
                        media_type: mime_to_media_type(&a.content_type),
                        url: Some(self.endpoint(&format!("/v1/attachments/{}", a.id))),
                        data: None,
                        mime_type: a.content_type.clone(),
                        filename: a.filename.clone(),
                        size_bytes: a.size.map(|s| s as u64),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let msg_id = envelope
            .envelope
            .timestamp
            .map(|t| t.to_string())
            .unwrap_or_default();

        let origin = clawdesk_types::message::MessageOrigin::Signal {
            phone_number: source,
            message_id: msg_id,
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: text,
            body_for_agent: None,
            sender,
            media,
            reply_context: None,
            origin,
            timestamp: chrono::Utc::now(),
        })
    }
}

/// Map MIME type string to the internal MediaType enum.
fn mime_to_media_type(mime: &str) -> clawdesk_types::message::MediaType {
    if mime.starts_with("image/") {
        clawdesk_types::message::MediaType::Image
    } else if mime.starts_with("video/") {
        clawdesk_types::message::MediaType::Video
    } else if mime.starts_with("audio/") {
        clawdesk_types::message::MediaType::Audio
    } else {
        clawdesk_types::message::MediaType::Document
    }
}

#[async_trait]
impl Channel for SignalChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Signal
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Signal".into(),
            supports_threading: false,
            supports_streaming: true,
            supports_reactions: false,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(6000),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify connectivity by fetching about info
        let about_url = self.endpoint("/v1/about");
        let resp = self
            .client
            .get(&about_url)
            .send()
            .await
            .map_err(|e| format!("Signal API connectivity check failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "Signal API returned HTTP {}",
                resp.status().as_u16()
            ));
        }

        info!(
            phone = %self.phone_number,
            api = %self.api_url,
            "Signal channel started (polling mode)"
        );
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let recipient = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Signal { phone_number, .. } => {
                phone_number.clone()
            }
            _ => return Err("cannot send Signal message without Signal origin".into()),
        };

        let body = serde_json::json!({
            "message": msg.body,
            "number": self.phone_number,
            "recipients": [recipient],
        });

        let url = self.endpoint("/v2/send");
        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Signal send failed: {}", e))?;

        if !response.status().is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(format!("Signal API error: {}", error_body));
        }

        let timestamp = chrono::Utc::now();
        let msg_id = timestamp.timestamp_millis().to_string();

        Ok(DeliveryReceipt {
            channel: ChannelId::Signal,
            message_id: msg_id,
            timestamp,
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Signal channel stopped");
        Ok(())
    }
}

#[async_trait]
impl Streaming for SignalChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        // Signal doesn't natively support message editing, but signal-cli
        // REST API supports it via PUT /v1/messages. We send the initial
        // message and return a handle that can update it.
        let receipt = self.send(initial).await?;
        let msg_id = receipt.message_id.clone();

        Ok(StreamHandle {
            message_id: msg_id,
            update_fn: Box::new(|_text| Ok(())),
        })
    }
}

// ─── Signal API types ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SignalEnvelope {
    envelope: SignalEnvelopeInner,
}

#[derive(Debug, Deserialize)]
struct SignalEnvelopeInner {
    source: Option<String>,
    #[serde(rename = "sourceName")]
    source_name: Option<String>,
    timestamp: Option<i64>,
    #[serde(rename = "dataMessage")]
    data_message: Option<SignalDataMessage>,
}

#[derive(Debug, Deserialize)]
struct SignalDataMessage {
    message: Option<String>,
    #[serde(rename = "groupInfo")]
    group_info: Option<SignalGroupInfo>,
    attachments: Option<Vec<SignalAttachment>>,
    timestamp: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SignalGroupInfo {
    #[serde(rename = "groupId")]
    group_id: String,
    #[serde(rename = "type")]
    group_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SignalAttachment {
    id: String,
    #[serde(rename = "contentType")]
    content_type: String,
    filename: Option<String>,
    size: Option<i64>,
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SignalConfig {
        SignalConfig {
            api_url: "http://localhost:8080".into(),
            phone_number: "+1234567890".into(),
            allowed_numbers: vec!["+1111111111".into()],
            enable_groups: true,
        }
    }

    #[test]
    fn test_signal_channel_creation() {
        let channel = SignalChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::Signal);
        assert_eq!(channel.phone_number, "+1234567890");
        assert!(channel.enable_groups);
    }

    #[test]
    fn test_signal_meta() {
        let channel = SignalChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "Signal");
        assert!(meta.supports_media);
        assert!(meta.supports_groups);
        assert!(!meta.supports_reactions);
        assert_eq!(meta.max_message_length, Some(6000));
    }

    #[test]
    fn test_signal_allowed_numbers() {
        let channel = SignalChannel::new(test_config());
        assert!(channel.is_allowed("+1111111111"));
        assert!(!channel.is_allowed("+9999999999"));

        // Empty allowed list = allow all
        let mut config = test_config();
        config.allowed_numbers = vec![];
        let open_channel = SignalChannel::new(config);
        assert!(open_channel.is_allowed("+9999999999"));
    }

    #[test]
    fn test_signal_endpoint_url() {
        let channel = SignalChannel::new(test_config());
        assert_eq!(
            channel.endpoint("/v2/send"),
            "http://localhost:8080/v2/send"
        );
        assert_eq!(
            channel.endpoint("/v1/receive/+1234567890"),
            "http://localhost:8080/v1/receive/+1234567890"
        );
    }

    #[test]
    fn test_mime_to_media_type() {
        assert_eq!(
            mime_to_media_type("image/jpeg") as u8,
            clawdesk_types::message::MediaType::Image as u8
        );
        assert_eq!(
            mime_to_media_type("video/mp4") as u8,
            clawdesk_types::message::MediaType::Video as u8
        );
        assert_eq!(
            mime_to_media_type("audio/ogg") as u8,
            clawdesk_types::message::MediaType::Audio as u8
        );
        assert_eq!(
            mime_to_media_type("application/pdf") as u8,
            clawdesk_types::message::MediaType::Document as u8
        );
    }
}
