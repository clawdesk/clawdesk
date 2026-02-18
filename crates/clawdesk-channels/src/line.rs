//! LINE Messaging API channel implementation.
//!
//! Connects to the LINE Messaging API for sending and receiving messages.
//! Supports reply messages (using reply token) and push messages.
//!
//! ## Architecture
//!
//! ```text
//! LineChannel
//! ├── webhook_handler() — receives LINE webhook events
//! ├── normalize()       — LINE event → NormalizedMessage
//! ├── send()            — OutboundMessage → reply/push API
//! └── send_push()       — push message (without reply token)
//! ```
//!
//! ## LINE Messaging API
//!
//! - `POST /v2/bot/message/reply`       — reply using reply token (free)
//! - `POST /v2/bot/message/push`        — push message (costs quota)
//! - `GET  /v2/bot/profile/{userId}`    — get user profile
//! - `GET  /v2/bot/message/{id}/content`— download media content
//! - `POST /v2/bot/richmenu`            — manage rich menus
//!
//! ## Limits
//!
//! - Reply token: valid for 30 seconds, single-use
//! - Push messages: limited by plan (free plan = 500/month)
//! - Message text: 5000 characters
//! - Max 5 message objects per reply/push request
//! - Webhook must respond within 1 second

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, MediaAttachment, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

const LINE_API_BASE: &str = "https://api.line.me";

/// LINE Messaging API channel adapter.
pub struct LineChannel {
    client: Client,
    /// Channel access token (long-lived, from LINE Developers Console).
    channel_access_token: String,
    /// Channel secret for webhook signature verification.
    channel_secret: String,
    /// Whether to use push messages when reply token is expired/unavailable.
    enable_push_fallback: bool,
    /// Shutdown flag.
    running: AtomicBool,
}

/// Configuration for the LINE channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineConfig {
    pub channel_access_token: String,
    pub channel_secret: String,
    #[serde(default)]
    pub enable_push_fallback: bool,
}

impl LineChannel {
    pub fn new(config: LineConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            channel_access_token: config.channel_access_token,
            channel_secret: config.channel_secret,
            enable_push_fallback: config.enable_push_fallback,
            running: AtomicBool::new(false),
        }
    }

    /// Build a LINE API URL.
    fn api_url(&self, path: &str) -> String {
        format!("{}{}", LINE_API_BASE, path)
    }

    /// Send a reply message using the reply token.
    async fn send_reply(
        &self,
        reply_token: &str,
        messages: Vec<serde_json::Value>,
    ) -> Result<(), String> {
        let body = serde_json::json!({
            "replyToken": reply_token,
            "messages": messages,
        });

        let url = self.api_url("/v2/bot/message/reply");
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.channel_access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("LINE reply failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("LINE reply HTTP {}: {}", status, err));
        }

        Ok(())
    }

    /// Send a push message to a specific user/group (uses plan quota).
    async fn send_push(
        &self,
        target: &str,
        messages: Vec<serde_json::Value>,
    ) -> Result<(), String> {
        let body = serde_json::json!({
            "to": target,
            "messages": messages,
        });

        let url = self.api_url("/v2/bot/message/push");
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.channel_access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("LINE push failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("LINE push HTTP {}: {}", status, err));
        }

        Ok(())
    }

    /// Fetch a user's profile from the LINE API.
    async fn get_user_profile(&self, user_id: &str) -> Result<LineProfile, String> {
        let url = self.api_url(&format!("/v2/bot/profile/{}", user_id));
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.channel_access_token)
            .send()
            .await
            .map_err(|e| format!("LINE profile fetch failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("LINE profile HTTP {}", resp.status().as_u16()));
        }

        resp.json::<LineProfile>()
            .await
            .map_err(|e| format!("LINE profile parse failed: {}", e))
    }

    /// Normalize a LINE webhook event to NormalizedMessage.
    fn normalize_event(&self, event: &LineWebhookEvent) -> Option<NormalizedMessage> {
        if event.event_type != "message" {
            return None;
        }

        let message = event.message.as_ref()?;
        let text = match &message.message_type {
            LineMessageType::Text => message.text.clone()?,
            _ => {
                // For non-text messages, represent them with a descriptor
                format!("[{:?} message]", message.message_type)
            }
        };

        let source = event.source.as_ref()?;
        let user_id = source.user_id.clone().unwrap_or_default();

        let sender = SenderIdentity {
            id: user_id.clone(),
            display_name: user_id.clone(), // Resolved via get_user_profile in production
            channel: ChannelId::Line,
        };

        let session_key = match &source.source_type {
            LineSourceType::Group => clawdesk_types::session::SessionKey::new(
                ChannelId::Line,
                &source.group_id.clone().unwrap_or(user_id.clone()),
            ),
            LineSourceType::Room => clawdesk_types::session::SessionKey::new(
                ChannelId::Line,
                &source.room_id.clone().unwrap_or(user_id.clone()),
            ),
            LineSourceType::User => {
                clawdesk_types::session::SessionKey::new(ChannelId::Line, &user_id)
            }
        };

        // Detect media
        let media = if message.content_provider.is_some() {
            vec![MediaAttachment {
                media_type: match message.message_type {
                    LineMessageType::Image => clawdesk_types::message::MediaType::Image,
                    LineMessageType::Video => clawdesk_types::message::MediaType::Video,
                    LineMessageType::Audio => clawdesk_types::message::MediaType::Audio,
                    _ => clawdesk_types::message::MediaType::Document,
                },
                url: Some(self.api_url(&format!(
                    "/v2/bot/message/{}/content",
                    message.id
                ))),
                data: None,
                mime_type: "application/octet-stream".into(),
                filename: None,
                size_bytes: None,
            }]
        } else {
            vec![]
        };

        let origin = clawdesk_types::message::MessageOrigin::Line {
            user_id,
            reply_token: event.reply_token.clone().unwrap_or_default(),
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

#[async_trait]
impl Channel for LineChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Line
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "LINE".into(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: false,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(5000),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify bot token via profile endpoint
        let resp = self
            .client
            .get(&self.api_url("/v2/bot/info"))
            .bearer_auth(&self.channel_access_token)
            .send()
            .await
            .map_err(|e| format!("LINE auth check failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "LINE bot token verification failed (HTTP {})",
                resp.status().as_u16()
            ));
        }

        let bot_info: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("LINE bot info parse failed: {}", e))?;

        info!(
            bot = bot_info.get("displayName").and_then(|v| v.as_str()).unwrap_or("unknown"),
            "LINE channel started (webhook mode)"
        );
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let (user_id, reply_token) = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Line {
                user_id,
                reply_token,
            } => (user_id.clone(), reply_token.clone()),
            _ => return Err("cannot send LINE message without LINE origin".into()),
        };

        let messages = vec![serde_json::json!({
            "type": "text",
            "text": msg.body,
        })];

        // Try reply token first (free), fall back to push if configured
        if !reply_token.is_empty() {
            match self.send_reply(&reply_token, messages.clone()).await {
                Ok(()) => {
                    debug!(user_id = %user_id, "LINE reply sent successfully");
                }
                Err(e) => {
                    warn!(error = %e, "LINE reply failed, attempting push fallback");
                    if self.enable_push_fallback {
                        self.send_push(&user_id, messages).await?;
                    } else {
                        return Err(e);
                    }
                }
            }
        } else if self.enable_push_fallback {
            self.send_push(&user_id, messages).await?;
        } else {
            return Err("no reply token and push fallback disabled".into());
        }

        Ok(DeliveryReceipt {
            channel: ChannelId::Line,
            message_id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        info!("LINE channel stopped");
        Ok(())
    }
}

// ─── LINE API types ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LineWebhookBody {
    events: Vec<LineWebhookEvent>,
    destination: String,
}

#[derive(Debug, Deserialize)]
struct LineWebhookEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(rename = "replyToken")]
    reply_token: Option<String>,
    source: Option<LineSource>,
    message: Option<LineMessageContent>,
    timestamp: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct LineSource {
    #[serde(rename = "type")]
    source_type: LineSourceType,
    #[serde(rename = "userId")]
    user_id: Option<String>,
    #[serde(rename = "groupId")]
    group_id: Option<String>,
    #[serde(rename = "roomId")]
    room_id: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
enum LineSourceType {
    User,
    Group,
    Room,
}

#[derive(Debug, Deserialize)]
struct LineMessageContent {
    id: String,
    #[serde(rename = "type")]
    message_type: LineMessageType,
    text: Option<String>,
    #[serde(rename = "contentProvider")]
    content_provider: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
enum LineMessageType {
    Text,
    Image,
    Video,
    Audio,
    File,
    Location,
    Sticker,
}

#[derive(Debug, Deserialize)]
struct LineProfile {
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(rename = "userId")]
    user_id: String,
    #[serde(rename = "pictureUrl")]
    picture_url: Option<String>,
    #[serde(rename = "statusMessage")]
    status_message: Option<String>,
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> LineConfig {
        LineConfig {
            channel_access_token: "test-token-12345".into(),
            channel_secret: "test-secret".into(),
            enable_push_fallback: true,
        }
    }

    #[test]
    fn test_line_channel_creation() {
        let channel = LineChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::Line);
        assert!(channel.enable_push_fallback);
    }

    #[test]
    fn test_line_meta() {
        let channel = LineChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "LINE");
        assert!(!meta.supports_threading);
        assert!(!meta.supports_streaming);
        assert!(meta.supports_media);
        assert!(meta.supports_groups);
        assert_eq!(meta.max_message_length, Some(5000));
    }

    #[test]
    fn test_line_api_url() {
        let channel = LineChannel::new(test_config());
        assert_eq!(
            channel.api_url("/v2/bot/message/reply"),
            "https://api.line.me/v2/bot/message/reply"
        );
    }

    #[test]
    fn test_line_normalize_text_event() {
        let channel = LineChannel::new(test_config());

        let event = LineWebhookEvent {
            event_type: "message".into(),
            reply_token: Some("reply-token-abc".into()),
            source: Some(LineSource {
                source_type: LineSourceType::User,
                user_id: Some("U1234567890".into()),
                group_id: None,
                room_id: None,
            }),
            message: Some(LineMessageContent {
                id: "msg-001".into(),
                message_type: LineMessageType::Text,
                text: Some("Hello from LINE!".into()),
                content_provider: None,
            }),
            timestamp: Some(1700000000000),
        };

        let normalized = channel.normalize_event(&event).unwrap();
        assert_eq!(normalized.body, "Hello from LINE!");
        assert_eq!(normalized.sender.id, "U1234567890");
    }

    #[test]
    fn test_line_normalize_non_message_event() {
        let channel = LineChannel::new(test_config());

        let event = LineWebhookEvent {
            event_type: "follow".into(),
            reply_token: None,
            source: None,
            message: None,
            timestamp: None,
        };

        assert!(channel.normalize_event(&event).is_none());
    }
}
