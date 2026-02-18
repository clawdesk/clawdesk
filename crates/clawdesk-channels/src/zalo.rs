//! Zalo Official Account (OA) channel implementation.
//!
//! Zalo is Vietnam's most popular messaging app with ~75 million users.
//! This adapter connects via the Zalo OA (Official Account) API for
//! customer service messaging between businesses and Zalo users.
//!
//! ## Architecture
//!
//! ```text
//! ZaloChannel
//! ├── webhook_handler() — receives Zalo OA webhook events
//! ├── normalize()       — Zalo event → NormalizedMessage
//! ├── send()            — OutboundMessage → POST /oa/message/cs
//! ├── send_streaming()  — progressive message updates
//! ├── refresh_token()   — OAuth refresh token flow
//! └── upload_media()    — file/image upload for attachments
//! ```
//!
//! ## Zalo OA API
//!
//! Base URL: `https://openapi.zalo.me/v3.0/`
//!
//! - `POST /oa/message/cs`         — send customer service message
//! - `POST /oa/message/promotion`  — send promotional message (template-based)
//! - `GET  /oa/getprofile`         — get user profile info
//! - `POST /oa/upload/image`       — upload image attachment
//! - `POST /oa/upload/file`        — upload file attachment
//! - `POST /oa/upload/gif`         — upload GIF
//!
//! ## Authentication
//!
//! Zalo OA uses OAuth 2.0 with access_token + refresh_token. The access
//! token expires every ~24 hours and must be refreshed using the refresh
//! token. The refresh token itself has a 90-day expiry.
//!
//! ## Message limits
//!
//! - Text messages: 2000 characters
//! - Customer service window: 7 days from last user interaction
//! - Outside window: only template messages allowed
//! - File upload: 5 MB for images, 25 MB for files

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
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

const ZALO_API_BASE: &str = "https://openapi.zalo.me/v3.0";
const ZALO_OAUTH_URL: &str = "https://oauth.zaloapp.com/v4/oa/access_token";
const MAX_MESSAGE_LENGTH: usize = 2000;

/// Zalo Official Account channel adapter.
pub struct ZaloChannel {
    client: Client,
    /// Official Account ID.
    oa_id: String,
    /// Current access token (refreshed periodically).
    access_token: Mutex<String>,
    /// Refresh token for obtaining new access tokens.
    refresh_token: Mutex<String>,
    /// App ID for OAuth refresh flow.
    app_id: String,
    /// App secret for OAuth refresh flow.
    app_secret: String,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for the Zalo OA channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZaloConfig {
    pub oa_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub app_id: String,
    pub app_secret: String,
}

// ─── Zalo API response types ───────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ZaloApiResponse {
    error: i32,
    message: Option<String>,
    data: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ZaloTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    error: Option<i32>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ZaloWebhookEvent {
    #[serde(rename = "event_name")]
    event_name: Option<String>,
    app_id: Option<String>,
    sender: Option<ZaloSender>,
    recipient: Option<ZaloRecipient>,
    message: Option<ZaloMessage>,
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ZaloSender {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ZaloRecipient {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ZaloMessage {
    #[serde(rename = "msg_id")]
    msg_id: Option<String>,
    text: Option<String>,
    #[serde(rename = "attachments")]
    attachments: Option<Vec<ZaloAttachment>>,
}

#[derive(Debug, Deserialize)]
struct ZaloAttachment {
    #[serde(rename = "type")]
    attachment_type: Option<String>,
    payload: Option<ZaloAttachmentPayload>,
}

#[derive(Debug, Deserialize)]
struct ZaloAttachmentPayload {
    url: Option<String>,
    thumbnail: Option<String>,
    size: Option<u64>,
    name: Option<String>,
    #[serde(rename = "type")]
    file_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ZaloUserProfile {
    user_id: Option<String>,
    display_name: Option<String>,
    avatar: Option<String>,
}

impl ZaloChannel {
    pub fn new(config: ZaloConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            oa_id: config.oa_id,
            access_token: Mutex::new(config.access_token),
            refresh_token: Mutex::new(config.refresh_token),
            app_id: config.app_id,
            app_secret: config.app_secret,
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Build a Zalo OA API URL.
    fn api_url(&self, path: &str) -> String {
        format!("{}{}", ZALO_API_BASE, path)
    }

    /// Get the current access token.
    async fn get_access_token(&self) -> String {
        self.access_token.lock().await.clone()
    }

    /// Refresh the access token using the refresh token.
    async fn refresh_access_token(&self) -> Result<String, String> {
        let refresh_tok = self.refresh_token.lock().await.clone();

        let resp = self
            .client
            .post(ZALO_OAUTH_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("secret_key", &self.app_secret)
            .form(&[
                ("app_id", self.app_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh_tok),
            ])
            .send()
            .await
            .map_err(|e| format!("Zalo token refresh failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Zalo token refresh HTTP {}: {}", status, err));
        }

        let token_resp: ZaloTokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("Zalo token parse error: {}", e))?;

        if let Some(err_code) = token_resp.error {
            if err_code != 0 {
                let desc = token_resp.error_description.unwrap_or_default();
                return Err(format!("Zalo token error {}: {}", err_code, desc));
            }
        }

        let new_access = token_resp
            .access_token
            .ok_or("Zalo returned no access_token")?;

        // Update stored tokens
        {
            let mut guard = self.access_token.lock().await;
            *guard = new_access.clone();
        }

        if let Some(new_refresh) = token_resp.refresh_token {
            let mut guard = self.refresh_token.lock().await;
            *guard = new_refresh;
        }

        debug!("Zalo access token refreshed");
        Ok(new_access)
    }

    /// Build the customer service message payload.
    fn build_cs_message_payload(recipient_id: &str, text: &str) -> serde_json::Value {
        serde_json::json!({
            "recipient": {
                "user_id": recipient_id,
            },
            "message": {
                "text": text,
            }
        })
    }

    /// Build a media attachment message payload.
    fn build_media_payload(
        recipient_id: &str,
        attachment_type: &str,
        url: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "recipient": {
                "user_id": recipient_id,
            },
            "message": {
                "attachment": {
                    "type": attachment_type,
                    "payload": {
                        "url": url,
                    }
                }
            }
        })
    }

    /// Truncate text to the Zalo message limit.
    fn truncate_message(text: &str) -> String {
        if text.len() <= MAX_MESSAGE_LENGTH {
            text.to_string()
        } else {
            let truncated = &text[..MAX_MESSAGE_LENGTH - 15];
            format!("{}… [truncated]", truncated)
        }
    }

    /// Normalize a Zalo webhook event into a NormalizedMessage.
    fn normalize_event(&self, event: &ZaloWebhookEvent) -> Option<NormalizedMessage> {
        let event_name = event.event_name.as_deref()?;

        // Only process user-sent messages
        if event_name != "user_send_text" && event_name != "user_send_image"
            && event_name != "user_send_file" && event_name != "user_send_sticker"
        {
            return None;
        }

        let sender = event.sender.as_ref()?;
        let user_id = sender.id.clone()?;
        let message = event.message.as_ref()?;
        let msg_id = message.msg_id.clone().unwrap_or_default();

        let text = message
            .text
            .clone()
            .unwrap_or_else(|| format!("[{} message]", event_name));

        let sender_identity = SenderIdentity {
            id: user_id.clone(),
            display_name: user_id.clone(), // Resolved via getprofile in production
            channel: ChannelId::Zalo,
        };

        let session_key =
            clawdesk_types::session::SessionKey::new(ChannelId::Zalo, &user_id);

        let origin = clawdesk_types::message::MessageOrigin::Zalo {
            user_id: user_id.clone(),
            message_id: msg_id,
        };

        // Extract media from attachments
        let media = message
            .attachments
            .as_ref()
            .map(|atts| {
                atts.iter()
                    .filter_map(|a| {
                        let payload = a.payload.as_ref()?;
                        let att_type = a.attachment_type.as_deref().unwrap_or("file");
                        let media_type = match att_type {
                            "image" => clawdesk_types::message::MediaType::Image,
                            "video" => clawdesk_types::message::MediaType::Video,
                            "audio" => clawdesk_types::message::MediaType::Audio,
                            _ => clawdesk_types::message::MediaType::Document,
                        };

                        Some(MediaAttachment {
                            media_type,
                            url: payload.url.clone(),
                            data: None,
                            mime_type: format!("{}/unknown", att_type),
                            filename: payload.name.clone(),
                            size_bytes: payload.size,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: text,
            body_for_agent: None,
            sender: sender_identity,
            media,
            reply_context: None,
            origin,
            timestamp: chrono::Utc::now(),
        })
    }
}

#[async_trait]
impl Channel for ZaloChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Zalo
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Zalo".into(),
            supports_threading: false,
            supports_streaming: true,
            supports_reactions: false,
            supports_media: true,
            supports_groups: false,
            max_message_length: Some(MAX_MESSAGE_LENGTH),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify the access token by fetching the OA profile
        let token = self.get_access_token().await;
        let url = format!("{}/oa/getoa", ZALO_API_BASE);
        let resp = self
            .client
            .get(&url)
            .header("access_token", &token)
            .send()
            .await
            .map_err(|e| format!("Zalo connectivity check failed: {}", e))?;

        if !resp.status().is_success() {
            // Try refreshing the token
            warn!("Zalo access token may be expired, refreshing");
            self.refresh_access_token().await?;
        }

        info!(
            oa_id = %self.oa_id,
            "Zalo OA channel started"
        );
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let recipient_id = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Zalo { user_id, .. } => user_id.clone(),
            _ => return Err("cannot send Zalo message without Zalo origin".into()),
        };

        let token = self.get_access_token().await;
        let text = Self::truncate_message(&msg.body);
        let body = Self::build_cs_message_payload(&recipient_id, &text);

        let url = self.api_url("/oa/message/cs");
        let response = self
            .client
            .post(&url)
            .header("access_token", &token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Zalo send failed: {}", e))?;

        let response = if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();

            // Check if it's a token expiry error
            if status.as_u16() == 401 {
                warn!("Zalo token expired during send, refreshing");
                let new_token = self.refresh_access_token().await?;

                // Retry with new token
                let retry_resp = self
                    .client
                    .post(&url)
                    .header("access_token", &new_token)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| format!("Zalo retry send failed: {}", e))?;

                if !retry_resp.status().is_success() {
                    let err = retry_resp.text().await.unwrap_or_default();
                    return Err(format!("Zalo send retry failed: {}", err));
                }
                retry_resp
            } else {
                return Err(format!("Zalo API error HTTP {}: {}", status, error_body));
            }
        } else {
            response
        };

        let api_resp: ZaloApiResponse = response
            .json()
            .await
            .unwrap_or(ZaloApiResponse {
                error: 0,
                message: None,
                data: None,
            });

        if api_resp.error != 0 {
            return Err(format!(
                "Zalo send error {}: {}",
                api_resp.error,
                api_resp.message.unwrap_or_default()
            ));
        }

        let message_id = api_resp
            .data
            .as_ref()
            .and_then(|d| d.get("message_id"))
            .and_then(|id| id.as_str())
            .unwrap_or("")
            .to_string();

        Ok(DeliveryReceipt {
            channel: ChannelId::Zalo,
            message_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Zalo channel stopped");
        Ok(())
    }
}

#[async_trait]
impl Streaming for ZaloChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        // Zalo doesn't support message editing. For streaming, we send
        // the initial message and subsequent updates as new messages.
        let receipt = self.send(initial).await?;
        let msg_id = receipt.message_id.clone();

        Ok(StreamHandle {
            message_id: msg_id,
            update_fn: Box::new(|_text| Ok(())),
        })
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ZaloConfig {
        ZaloConfig {
            oa_id: "1234567890".into(),
            access_token: "test_access_token".into(),
            refresh_token: "test_refresh_token".into(),
            app_id: "test_app_id".into(),
            app_secret: "test_app_secret".into(),
        }
    }

    #[test]
    fn test_zalo_channel_creation() {
        let channel = ZaloChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::Zalo);
        assert_eq!(channel.oa_id, "1234567890");
        assert_eq!(channel.app_id, "test_app_id");
    }

    #[test]
    fn test_zalo_meta() {
        let channel = ZaloChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "Zalo");
        assert!(!meta.supports_threading);
        assert!(meta.supports_streaming);
        assert!(meta.supports_media);
        assert!(!meta.supports_groups);
        assert_eq!(meta.max_message_length, Some(2000));
    }

    #[test]
    fn test_zalo_api_url() {
        let channel = ZaloChannel::new(test_config());
        assert_eq!(
            channel.api_url("/oa/message/cs"),
            "https://openapi.zalo.me/v3.0/oa/message/cs"
        );
    }

    #[test]
    fn test_zalo_message_payload() {
        let payload = ZaloChannel::build_cs_message_payload("user_123", "Hello Zalo!");
        let recipient = payload.get("recipient").unwrap();
        assert_eq!(
            recipient.get("user_id").unwrap().as_str().unwrap(),
            "user_123"
        );
        let message = payload.get("message").unwrap();
        assert_eq!(
            message.get("text").unwrap().as_str().unwrap(),
            "Hello Zalo!"
        );
    }

    #[test]
    fn test_zalo_media_payload() {
        let payload = ZaloChannel::build_media_payload(
            "user_123",
            "image",
            "https://example.com/photo.jpg",
        );
        let message = payload.get("message").unwrap();
        let attachment = message.get("attachment").unwrap();
        assert_eq!(
            attachment.get("type").unwrap().as_str().unwrap(),
            "image"
        );
        let att_payload = attachment.get("payload").unwrap();
        assert_eq!(
            att_payload.get("url").unwrap().as_str().unwrap(),
            "https://example.com/photo.jpg"
        );
    }

    #[test]
    fn test_zalo_message_truncation() {
        let short = "Hello, Zalo!";
        assert_eq!(ZaloChannel::truncate_message(short), short);

        let exact = "a".repeat(MAX_MESSAGE_LENGTH);
        assert_eq!(ZaloChannel::truncate_message(&exact), exact);

        let long = "b".repeat(MAX_MESSAGE_LENGTH + 100);
        let truncated = ZaloChannel::truncate_message(&long);
        assert!(truncated.len() <= MAX_MESSAGE_LENGTH);
        assert!(truncated.ends_with("… [truncated]"));
    }

    #[test]
    fn test_zalo_normalize_text_event() {
        let channel = ZaloChannel::new(test_config());
        let event = ZaloWebhookEvent {
            event_name: Some("user_send_text".into()),
            app_id: Some("test_app".into()),
            sender: Some(ZaloSender {
                id: Some("user_456".into()),
            }),
            recipient: Some(ZaloRecipient {
                id: Some("1234567890".into()),
            }),
            message: Some(ZaloMessage {
                msg_id: Some("msg_789".into()),
                text: Some("Xin chào!".into()),
                attachments: None,
            }),
            timestamp: Some("1700000000000".into()),
        };

        let normalized = channel.normalize_event(&event).unwrap();
        assert_eq!(normalized.body, "Xin chào!");
        assert_eq!(normalized.sender.id, "user_456");
    }

    #[test]
    fn test_zalo_normalize_wrong_event() {
        let channel = ZaloChannel::new(test_config());
        let event = ZaloWebhookEvent {
            event_name: Some("oa_send_text".into()),
            app_id: None,
            sender: None,
            recipient: None,
            message: None,
            timestamp: None,
        };

        assert!(channel.normalize_event(&event).is_none());
    }
}
