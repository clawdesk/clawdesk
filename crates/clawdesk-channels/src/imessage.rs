//! iMessage channel adapter via BlueBubbles HTTP API.
//!
//! Connects to a BlueBubbles server instance for sending and receiving
//! iMessage conversations. BlueBubbles runs on a Mac host and bridges
//! iMessage to an HTTP/WebSocket API.
//!
//! ## Architecture
//!
//! ```text
//! IMessageChannel
//! ├── poll_loop()      — polls GET /api/v1/message for new messages
//! ├── normalize()      — BlueBubbles message → NormalizedMessage
//! ├── send()           — OutboundMessage → POST /api/v1/message/text
//! └── send_tapback()   — Tapback reaction → POST /api/v1/message/react
//! ```
//!
//! ## BlueBubbles API
//!
//! The BlueBubbles REST API (https://bluebubbles.app/):
//! - `GET  /api/v1/message`       — list/search messages
//! - `POST /api/v1/message/text`  — send text message
//! - `POST /api/v1/message/react` — send tapback reaction
//! - `GET  /api/v1/chat`          — list chats
//! - `GET  /api/v1/attachment/:guid/download` — download attachment
//!
//! ## Limits
//!
//! iMessage limits are OS-imposed:
//! - No official rate limit (Apple's relay)
//! - SMS fallback may occur (handled by carrier)
//! - Max attachment size depends on carrier/iCloud settings

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Reactions, StreamHandle, Streaming};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, MediaAttachment, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// iMessage channel adapter via BlueBubbles HTTP API.
pub struct IMessageChannel {
    client: Client,
    /// BlueBubbles server URL (e.g., `http://192.168.1.100:1234`).
    server_url: String,
    /// BlueBubbles server password (query param authentication).
    password: String,
    /// Allowed chat GUIDs. Empty = allow all.
    allowed_chats: Vec<String>,
    /// Last processed message timestamp (epoch ms) for polling.
    last_timestamp: AtomicI64,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for the iMessage channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IMessageConfig {
    pub server_url: String,
    pub password: String,
    #[serde(default)]
    pub allowed_chats: Vec<String>,
}

/// Tapback reaction types supported by iMessage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapbackType {
    Love,
    Like,
    Dislike,
    Laugh,
    Emphasis,
    Question,
}

impl TapbackType {
    /// Convert to the BlueBubbles API reaction string.
    fn api_value(&self) -> &'static str {
        match self {
            Self::Love => "love",
            Self::Like => "like",
            Self::Dislike => "dislike",
            Self::Laugh => "laugh",
            Self::Emphasis => "emphasize",
            Self::Question => "question",
        }
    }

    /// Parse an emoji string into a TapbackType.
    fn from_emoji(emoji: &str) -> Option<Self> {
        match emoji {
            "❤️" | "♥️" | "love" => Some(Self::Love),
            "👍" | "like" => Some(Self::Like),
            "👎" | "dislike" => Some(Self::Dislike),
            "😂" | "😆" | "laugh" => Some(Self::Laugh),
            "‼️" | "❗" | "emphasize" => Some(Self::Emphasis),
            "❓" | "?" | "question" => Some(Self::Question),
            _ => None,
        }
    }
}

impl IMessageChannel {
    pub fn new(config: IMessageConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            server_url: config.server_url.trim_end_matches('/').to_string(),
            password: config.password,
            allowed_chats: config.allowed_chats,
            last_timestamp: AtomicI64::new(0),
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Build a full API URL with password authentication.
    fn api_url(&self, path: &str) -> String {
        let separator = if path.contains('?') { '&' } else { '?' };
        format!(
            "{}{}{}password={}",
            self.server_url, path, separator, self.password
        )
    }

    /// Check if a chat GUID is allowed.
    fn is_allowed_chat(&self, chat_guid: &str) -> bool {
        self.allowed_chats.is_empty() || self.allowed_chats.iter().any(|c| c == chat_guid)
    }

    /// Poll loop: fetches new messages and dispatches to sink.
    async fn poll_loop(self: Arc<Self>, sink: Arc<dyn MessageSink>) {
        info!("iMessage poll loop started");

        while self.running.load(Ordering::Relaxed) {
            let after = self.last_timestamp.load(Ordering::Relaxed);
            let url = self.api_url(&format!(
                "/api/v1/message?after={}&sort=ASC&limit=50",
                after
            ));

            let result = self.client.get(&url).send().await;

            match result {
                Ok(response) => {
                    if let Ok(body) = response.json::<BlueBubblesListResponse>().await {
                        for message in body.data {
                            // Update last seen timestamp
                            if message.date_created > after {
                                self.last_timestamp
                                    .store(message.date_created, Ordering::Relaxed);
                            }

                            // Skip messages we sent ourselves
                            if message.is_from_me {
                                continue;
                            }

                            // Filter by allowed chats
                            let chat_guid = message
                                .chats
                                .first()
                                .map(|c| c.guid.as_str())
                                .unwrap_or("");

                            if !self.is_allowed_chat(chat_guid) {
                                debug!(chat = %chat_guid, "ignoring message from unallowed chat");
                                continue;
                            }

                            if let Some(normalized) = self.normalize_message(&message) {
                                sink.on_message(normalized).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "iMessage poll error, retrying in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        info!("iMessage poll loop stopped");
    }

    /// Normalize a BlueBubbles message to NormalizedMessage.
    fn normalize_message(&self, msg: &BlueBubblesMessage) -> Option<NormalizedMessage> {
        let text = msg.text.clone()?;
        let handle = msg.handle.as_ref()?;

        let sender = SenderIdentity {
            id: handle.address.clone(),
            display_name: handle
                .contact
                .as_ref()
                .and_then(|c| c.display_name.clone())
                .unwrap_or_else(|| handle.address.clone()),
            channel: ChannelId::IMessage,
        };

        let chat_guid = msg
            .chats
            .first()
            .map(|c| c.guid.clone())
            .unwrap_or_default();

        let session_key =
            clawdesk_types::session::SessionKey::new(ChannelId::IMessage, &chat_guid);

        // Detect media attachments
        let media = msg
            .attachments
            .iter()
            .map(|a| MediaAttachment {
                media_type: mime_to_media_type(&a.mime_type),
                url: Some(self.api_url(&format!(
                    "/api/v1/attachment/{}/download",
                    a.guid
                ))),
                data: None,
                mime_type: a.mime_type.clone(),
                filename: a.filename.clone(),
                size_bytes: a.total_bytes.map(|b| b as u64),
            })
            .collect();

        let origin = clawdesk_types::message::MessageOrigin::IMessage {
            apple_id: handle.address.clone(),
            message_id: msg.guid.clone(),
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

/// Map MIME type string to MediaType.
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
impl Channel for IMessageChannel {
    fn id(&self) -> ChannelId {
        ChannelId::IMessage
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "iMessage".into(),
            supports_threading: false,
            supports_streaming: true,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: None, // No hard limit on iMessage text
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify server connectivity
        let ping_url = self.api_url("/api/v1/server/info");
        let resp = self
            .client
            .get(&ping_url)
            .send()
            .await
            .map_err(|e| format!("BlueBubbles connectivity check failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "BlueBubbles server returned HTTP {}",
                resp.status().as_u16()
            ));
        }

        info!(
            server = %self.server_url,
            "iMessage channel started (BlueBubbles polling mode)"
        );
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let chat_guid = match &msg.origin {
            clawdesk_types::message::MessageOrigin::IMessage { apple_id, .. } => {
                // BlueBubbles uses chat GUIDs; for DMs it's typically
                // `iMessage;-;<apple_id>` or `SMS;-;<phone>`
                format!("iMessage;-;{}", apple_id)
            }
            _ => return Err("cannot send iMessage without iMessage origin".into()),
        };

        let body = serde_json::json!({
            "chatGuid": chat_guid,
            "message": msg.body,
            "method": "private-api",
        });

        let url = self.api_url("/api/v1/message/text");
        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("iMessage send failed: {}", e))?;

        if !response.status().is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(format!("BlueBubbles API error: {}", error_body));
        }

        let result: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("failed to parse send response: {}", e))?;

        let message_guid = result
            .get("data")
            .and_then(|d| d.get("guid"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Ok(DeliveryReceipt {
            channel: ChannelId::IMessage,
            message_id: message_guid,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("iMessage channel stopped");
        Ok(())
    }
}

#[async_trait]
impl Streaming for IMessageChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        let receipt = self.send(initial).await?;
        let msg_id = receipt.message_id.clone();

        // iMessage doesn't support message editing. For streaming, we send
        // successive messages. The handle tracks the conversation context.
        Ok(StreamHandle {
            message_id: msg_id,
            update_fn: Box::new(|_text| Ok(())),
        })
    }
}

#[async_trait]
impl Reactions for IMessageChannel {
    async fn add_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        let tapback = TapbackType::from_emoji(emoji)
            .ok_or_else(|| format!("unsupported iMessage tapback: {}", emoji))?;

        let body = serde_json::json!({
            "chatGuid": msg_id,
            "reaction": tapback.api_value(),
            "partIndex": 0,
        });

        let url = self.api_url("/api/v1/message/react");
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("iMessage tapback failed: {}", e))?;

        if !resp.status().is_success() {
            let error_body = resp.text().await.unwrap_or_default();
            return Err(format!("BlueBubbles reaction error: {}", error_body));
        }

        debug!(msg_id, emoji, "added iMessage tapback");
        Ok(())
    }

    async fn remove_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        // iMessage tapbacks are toggles; sending the same reaction again removes it
        debug!(msg_id, emoji, "removing iMessage tapback (toggle)");
        self.add_reaction(msg_id, emoji).await
    }
}

// ─── BlueBubbles API types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct BlueBubblesListResponse {
    status: i32,
    data: Vec<BlueBubblesMessage>,
}

#[derive(Debug, Deserialize)]
struct BlueBubblesMessage {
    guid: String,
    text: Option<String>,
    #[serde(rename = "isFromMe")]
    is_from_me: bool,
    #[serde(rename = "dateCreated")]
    date_created: i64,
    handle: Option<BlueBubblesHandle>,
    chats: Vec<BlueBubblesChat>,
    #[serde(default)]
    attachments: Vec<BlueBubblesAttachment>,
}

#[derive(Debug, Deserialize)]
struct BlueBubblesHandle {
    address: String,
    contact: Option<BlueBubblesContact>,
}

#[derive(Debug, Deserialize)]
struct BlueBubblesContact {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlueBubblesChat {
    guid: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlueBubblesAttachment {
    guid: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
    filename: Option<String>,
    #[serde(rename = "totalBytes")]
    total_bytes: Option<i64>,
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> IMessageConfig {
        IMessageConfig {
            server_url: "http://192.168.1.100:1234".into(),
            password: "test-password".into(),
            allowed_chats: vec!["iMessage;-;+1234567890".into()],
        }
    }

    #[test]
    fn test_imessage_channel_creation() {
        let channel = IMessageChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::IMessage);
        assert_eq!(channel.server_url, "http://192.168.1.100:1234");
        assert_eq!(channel.password, "test-password");
    }

    #[test]
    fn test_imessage_meta() {
        let channel = IMessageChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "iMessage");
        assert!(meta.supports_reactions);
        assert!(meta.supports_media);
        assert!(meta.supports_groups);
        assert!(meta.max_message_length.is_none());
    }

    #[test]
    fn test_imessage_api_url() {
        let channel = IMessageChannel::new(test_config());
        assert_eq!(
            channel.api_url("/api/v1/message"),
            "http://192.168.1.100:1234/api/v1/message?password=test-password"
        );
        assert_eq!(
            channel.api_url("/api/v1/message?after=123"),
            "http://192.168.1.100:1234/api/v1/message?after=123&password=test-password"
        );
    }

    #[test]
    fn test_imessage_allowed_chats() {
        let channel = IMessageChannel::new(test_config());
        assert!(channel.is_allowed_chat("iMessage;-;+1234567890"));
        assert!(!channel.is_allowed_chat("iMessage;-;+9999999999"));

        // Empty = allow all
        let mut config = test_config();
        config.allowed_chats = vec![];
        let open = IMessageChannel::new(config);
        assert!(open.is_allowed_chat("anything"));
    }

    #[test]
    fn test_tapback_type_from_emoji() {
        assert_eq!(TapbackType::from_emoji("❤️"), Some(TapbackType::Love));
        assert_eq!(TapbackType::from_emoji("👍"), Some(TapbackType::Like));
        assert_eq!(TapbackType::from_emoji("👎"), Some(TapbackType::Dislike));
        assert_eq!(TapbackType::from_emoji("😂"), Some(TapbackType::Laugh));
        assert_eq!(TapbackType::from_emoji("‼️"), Some(TapbackType::Emphasis));
        assert_eq!(TapbackType::from_emoji("❓"), Some(TapbackType::Question));
        assert_eq!(TapbackType::from_emoji("🤷"), None);
    }

    #[test]
    fn test_tapback_api_value() {
        assert_eq!(TapbackType::Love.api_value(), "love");
        assert_eq!(TapbackType::Like.api_value(), "like");
        assert_eq!(TapbackType::Emphasis.api_value(), "emphasize");
    }
}
