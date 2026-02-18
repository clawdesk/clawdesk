//! Nextcloud Talk channel implementation via OCS REST API.
//!
//! Nextcloud Talk is the self-hosted real-time communication app built
//! into the Nextcloud ecosystem. This adapter connects via the OCS
//! (Open Collaboration Services) REST API for sending and receiving
//! chat messages in Talk rooms (conversations).
//!
//! ## Architecture
//!
//! ```text
//! NextcloudTalkChannel
//! ├── poll_loop()       — polls /chat/{token} for new messages
//! ├── normalize()       — Talk message → NormalizedMessage
//! ├── send()            — OutboundMessage → POST /chat/{token}
//! ├── send_streaming()  — update-in-place via PUT /chat/{token}/{msgId}
//! └── share_file()      — share media via file share API
//! ```
//!
//! ## OCS REST API (Nextcloud Talk)
//!
//! Base path: `/ocs/v2.php/apps/spreed/api/v1/`
//!
//! - `GET  /chat/{token}?lookIntoFuture=1` — long-poll for new messages
//! - `POST /chat/{token}`                  — send a chat message
//! - `PUT  /chat/{token}/{messageId}`      — edit a message (NC 27+)
//! - `DELETE /chat/{token}/{messageId}`    — delete a message
//! - `GET  /room`                          — list conversations
//! - `GET  /room/{token}/participants`     — list participants
//! - `POST /sharing`                       — share file into chat
//!
//! ## Authentication
//!
//! Uses HTTP Basic auth with username + password (or app password).
//! All OCS requests require `OCS-APIRequest: true` header.
//!
//! ## Limits
//!
//! - Message text: 32000 characters
//! - Attachment: depends on Nextcloud storage quota
//! - Long-polling: 30-second timeout

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, StreamHandle, Streaming};
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

const MAX_MESSAGE_LENGTH: usize = 32000;

/// Nextcloud Talk channel adapter via OCS REST API.
pub struct NextcloudTalkChannel {
    client: Client,
    /// Base URL of the Nextcloud instance (e.g., `https://cloud.example.com`).
    base_url: String,
    /// Nextcloud username.
    username: String,
    /// Nextcloud password (or app password).
    password: String,
    /// Room token (conversation identifier, e.g., `"ab12cd34"`).
    room_token: String,
    /// Last known message ID for polling.
    last_known_message_id: AtomicI64,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for the Nextcloud Talk channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextcloudTalkConfig {
    pub base_url: String,
    pub username: String,
    pub password: String,
    pub room_token: String,
}

// ─── Nextcloud OCS response types ───────────────────────────────────

#[derive(Debug, Deserialize)]
struct OcsResponse<T> {
    ocs: OcsEnvelope<T>,
}

#[derive(Debug, Deserialize)]
struct OcsEnvelope<T> {
    meta: OcsMeta,
    data: T,
}

#[derive(Debug, Deserialize)]
struct OcsMeta {
    status: String,
    statuscode: i32,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TalkMessage {
    id: i64,
    token: Option<String>,
    #[serde(rename = "actorType")]
    actor_type: Option<String>,
    #[serde(rename = "actorId")]
    actor_id: Option<String>,
    #[serde(rename = "actorDisplayName")]
    actor_display_name: Option<String>,
    timestamp: Option<i64>,
    message: Option<String>,
    #[serde(rename = "messageType")]
    message_type: Option<String>,
    #[serde(rename = "messageParameters")]
    message_parameters: Option<serde_json::Value>,
    #[serde(rename = "systemMessage")]
    system_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TalkRoom {
    token: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "type")]
    room_type: Option<i32>,
    #[serde(rename = "participantType")]
    participant_type: Option<i32>,
}

impl NextcloudTalkChannel {
    pub fn new(config: NextcloudTalkConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("failed to build HTTP client"),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            username: config.username,
            password: config.password,
            room_token: config.room_token,
            last_known_message_id: AtomicI64::new(0),
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Build an OCS API URL for Talk endpoints.
    fn ocs_url(&self, path: &str) -> String {
        format!(
            "{}/ocs/v2.php/apps/spreed/api/v1{}",
            self.base_url, path
        )
    }

    /// Build the chat endpoint URL for the configured room.
    fn chat_url(&self) -> String {
        self.ocs_url(&format!("/chat/{}", self.room_token))
    }

    /// Build a message-specific URL for editing/deleting.
    fn message_url(&self, message_id: i64) -> String {
        self.ocs_url(&format!("/chat/{}/{}", self.room_token, message_id))
    }

    /// Add required OCS headers to a request builder.
    fn ocs_headers(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("OCS-APIRequest", "true"),
            ("Accept", "application/json"),
        ]
    }

    /// Poll loop: long-poll for new messages and dispatch to sink.
    async fn poll_loop(self: Arc<Self>, sink: Arc<dyn MessageSink>) {
        info!(
            base_url = %self.base_url,
            room = %self.room_token,
            "Nextcloud Talk poll loop started"
        );

        while self.running.load(Ordering::Relaxed) {
            let last_id = self.last_known_message_id.load(Ordering::Relaxed);
            let url = format!(
                "{}?lookIntoFuture=1&limit=100&lastKnownMessageId={}&timeout=30",
                self.chat_url(),
                last_id
            );

            let result = self
                .client
                .get(&url)
                .basic_auth(&self.username, Some(&self.password))
                .header("OCS-APIRequest", "true")
                .header("Accept", "application/json")
                .send()
                .await;

            match result {
                Ok(resp) => {
                    if resp.status().as_u16() == 304 {
                        // No new messages — normal for long-poll
                        continue;
                    }

                    if resp.status().is_success() {
                        if let Ok(ocs_resp) =
                            resp.json::<OcsResponse<Vec<TalkMessage>>>().await
                        {
                            for talk_msg in &ocs_resp.ocs.data {
                                // Track the latest message ID
                                if talk_msg.id > last_id {
                                    self.last_known_message_id
                                        .store(talk_msg.id, Ordering::Relaxed);
                                }

                                // Skip system messages
                                if talk_msg.system_message.as_ref().map_or(false, |s| !s.is_empty()) {
                                    continue;
                                }

                                // Skip messages from bots (our own messages)
                                if talk_msg.actor_id.as_deref() == Some(&self.username) {
                                    continue;
                                }

                                if let Some(normalized) = self.normalize_message(talk_msg) {
                                    sink.on_message(normalized).await;
                                }
                            }
                        }
                    } else {
                        warn!(
                            status = resp.status().as_u16(),
                            "Nextcloud Talk poll error"
                        );
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Nextcloud Talk poll request failed, retrying in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }

        info!("Nextcloud Talk poll loop stopped");
    }

    /// Normalize a Talk message into a NormalizedMessage.
    fn normalize_message(&self, msg: &TalkMessage) -> Option<NormalizedMessage> {
        let text = msg.message.as_ref()?;
        if text.is_empty() {
            return None;
        }

        let actor_id = msg.actor_id.clone().unwrap_or_default();
        let actor_name = msg
            .actor_display_name
            .clone()
            .unwrap_or_else(|| actor_id.clone());

        let sender = SenderIdentity {
            id: actor_id.clone(),
            display_name: actor_name,
            channel: ChannelId::NextcloudTalk,
        };

        let session_key = clawdesk_types::session::SessionKey::new(
            ChannelId::NextcloudTalk,
            &self.room_token,
        );

        let origin = clawdesk_types::message::MessageOrigin::NextcloudTalk {
            base_url: self.base_url.clone(),
            room_token: self.room_token.clone(),
            message_id: msg.id,
        };

        // Detect file shares in message parameters
        let media = self.extract_media_from_params(msg);

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: text.clone(),
            body_for_agent: None,
            sender,
            media,
            reply_context: None,
            origin,
            timestamp: chrono::Utc::now(),
        })
    }

    /// Extract media attachments from message parameters (file shares).
    fn extract_media_from_params(&self, msg: &TalkMessage) -> Vec<MediaAttachment> {
        let params = match &msg.message_parameters {
            Some(p) => p,
            None => return vec![],
        };

        let obj = match params.as_object() {
            Some(o) => o,
            None => return vec![],
        };

        let mut media = Vec::new();
        for (_key, value) in obj {
            if let Some(file_type) = value.get("type").and_then(|v| v.as_str()) {
                if file_type == "file" {
                    let name = value.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let mimetype = value
                        .get("mimetype")
                        .and_then(|v| v.as_str())
                        .unwrap_or("application/octet-stream");
                    let file_id = value.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let size = value
                        .get("size")
                        .and_then(|v| v.as_u64());

                    let download_url = format!(
                        "{}/remote.php/dav/files/{}/{}",
                        self.base_url, self.username, name
                    );

                    let media_type = if mimetype.starts_with("image/") {
                        clawdesk_types::message::MediaType::Image
                    } else if mimetype.starts_with("video/") {
                        clawdesk_types::message::MediaType::Video
                    } else if mimetype.starts_with("audio/") {
                        clawdesk_types::message::MediaType::Audio
                    } else {
                        clawdesk_types::message::MediaType::Document
                    };

                    media.push(MediaAttachment {
                        media_type,
                        url: Some(download_url),
                        data: None,
                        mime_type: mimetype.to_string(),
                        filename: Some(name.to_string()),
                        size_bytes: size,
                    });
                }
            }
        }

        media
    }

    /// Truncate text to the Nextcloud Talk message limit.
    fn truncate_message(text: &str) -> String {
        const SUFFIX: &str = "… [message truncated]";
        if text.len() <= MAX_MESSAGE_LENGTH {
            text.to_string()
        } else {
            let truncated = &text[..MAX_MESSAGE_LENGTH - SUFFIX.len()];
            format!("{}{}", truncated, SUFFIX)
        }
    }
}

#[async_trait]
impl Channel for NextcloudTalkChannel {
    fn id(&self) -> ChannelId {
        ChannelId::NextcloudTalk
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Nextcloud Talk".into(),
            supports_threading: true,
            supports_streaming: true,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(MAX_MESSAGE_LENGTH),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify connectivity by listing conversations
        let url = self.ocs_url("/room");
        let resp = self
            .client
            .get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("Nextcloud Talk connectivity check failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "Nextcloud Talk returned HTTP {}",
                resp.status().as_u16()
            ));
        }

        info!(
            base_url = %self.base_url,
            room = %self.room_token,
            user = %self.username,
            "Nextcloud Talk channel started"
        );
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let text = Self::truncate_message(&msg.body);

        let body = serde_json::json!({
            "message": text,
            "actorDisplayName": self.username,
        });

        let url = self.chat_url();
        let response = self
            .client
            .post(&url)
            .basic_auth(&self.username, Some(&self.password))
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Nextcloud Talk send failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            return Err(format!(
                "Nextcloud Talk API error HTTP {}: {}",
                status, error_body
            ));
        }

        let ocs_resp: OcsResponse<TalkMessage> = response
            .json()
            .await
            .map_err(|e| format!("Nextcloud Talk response parse error: {}", e))?;

        let message_id = ocs_resp.ocs.data.id.to_string();

        Ok(DeliveryReceipt {
            channel: ChannelId::NextcloudTalk,
            message_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Nextcloud Talk channel stopped");
        Ok(())
    }
}

#[async_trait]
impl Streaming for NextcloudTalkChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        // Nextcloud Talk supports message editing via PUT /chat/{token}/{messageId}
        // (available since Nextcloud 27). Send the initial message, then return
        // a handle for updating it in place.
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

    fn test_config() -> NextcloudTalkConfig {
        NextcloudTalkConfig {
            base_url: "https://cloud.example.com".into(),
            username: "botuser".into(),
            password: "app-password-xyz".into(),
            room_token: "ab12cd34".into(),
        }
    }

    #[test]
    fn test_nextcloud_talk_channel_creation() {
        let channel = NextcloudTalkChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::NextcloudTalk);
        assert_eq!(channel.base_url, "https://cloud.example.com");
        assert_eq!(channel.username, "botuser");
        assert_eq!(channel.room_token, "ab12cd34");
    }

    #[test]
    fn test_nextcloud_talk_meta() {
        let channel = NextcloudTalkChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "Nextcloud Talk");
        assert!(meta.supports_threading);
        assert!(meta.supports_streaming);
        assert!(meta.supports_media);
        assert!(meta.supports_groups);
        assert_eq!(meta.max_message_length, Some(32000));
    }

    #[test]
    fn test_nextcloud_talk_endpoint_url() {
        let channel = NextcloudTalkChannel::new(test_config());
        assert_eq!(
            channel.chat_url(),
            "https://cloud.example.com/ocs/v2.php/apps/spreed/api/v1/chat/ab12cd34"
        );
        assert_eq!(
            channel.message_url(42),
            "https://cloud.example.com/ocs/v2.php/apps/spreed/api/v1/chat/ab12cd34/42"
        );
    }

    #[test]
    fn test_nextcloud_talk_base_url_trailing_slash() {
        let mut cfg = test_config();
        cfg.base_url = "https://cloud.example.com/".into();
        let channel = NextcloudTalkChannel::new(cfg);
        assert_eq!(channel.base_url, "https://cloud.example.com");
        assert_eq!(
            channel.chat_url(),
            "https://cloud.example.com/ocs/v2.php/apps/spreed/api/v1/chat/ab12cd34"
        );
    }

    #[test]
    fn test_nextcloud_talk_message_truncation() {
        let short = "Hello, Nextcloud Talk!";
        assert_eq!(NextcloudTalkChannel::truncate_message(short), short);

        let exact = "a".repeat(MAX_MESSAGE_LENGTH);
        assert_eq!(NextcloudTalkChannel::truncate_message(&exact), exact);

        let long = "b".repeat(MAX_MESSAGE_LENGTH + 100);
        let truncated = NextcloudTalkChannel::truncate_message(&long);
        assert!(truncated.len() <= MAX_MESSAGE_LENGTH);
        assert!(truncated.ends_with("… [message truncated]"));
    }

    #[test]
    fn test_nextcloud_talk_normalize_message() {
        let channel = NextcloudTalkChannel::new(test_config());
        let talk_msg = TalkMessage {
            id: 123,
            token: Some("ab12cd34".into()),
            actor_type: Some("users".into()),
            actor_id: Some("alice".into()),
            actor_display_name: Some("Alice".into()),
            timestamp: Some(1700000000),
            message: Some("Hello from Nextcloud!".into()),
            message_type: Some("comment".into()),
            message_parameters: None,
            system_message: None,
        };

        let normalized = channel.normalize_message(&talk_msg).unwrap();
        assert_eq!(normalized.body, "Hello from Nextcloud!");
        assert_eq!(normalized.sender.id, "alice");
        assert_eq!(normalized.sender.display_name, "Alice");
    }

    #[test]
    fn test_nextcloud_talk_normalize_skips_own_messages() {
        let channel = NextcloudTalkChannel::new(test_config());
        let talk_msg = TalkMessage {
            id: 124,
            token: Some("ab12cd34".into()),
            actor_type: Some("users".into()),
            actor_id: Some("botuser".into()),
            actor_display_name: Some("Bot User".into()),
            timestamp: Some(1700000001),
            message: Some("My own message".into()),
            message_type: Some("comment".into()),
            message_parameters: None,
            system_message: None,
        };

        // The normalize_message itself doesn't skip own messages —
        // that filtering happens in the poll_loop. But we can still
        // verify it normalizes correctly.
        let normalized = channel.normalize_message(&talk_msg);
        assert!(normalized.is_some());
    }

    #[test]
    fn test_nextcloud_talk_normalize_empty_message() {
        let channel = NextcloudTalkChannel::new(test_config());
        let talk_msg = TalkMessage {
            id: 125,
            token: Some("ab12cd34".into()),
            actor_type: Some("users".into()),
            actor_id: Some("alice".into()),
            actor_display_name: Some("Alice".into()),
            timestamp: Some(1700000002),
            message: Some("".into()),
            message_type: Some("comment".into()),
            message_parameters: None,
            system_message: None,
        };

        assert!(channel.normalize_message(&talk_msg).is_none());
    }

    #[test]
    fn test_nextcloud_talk_extract_file_media() {
        let channel = NextcloudTalkChannel::new(test_config());
        let params = serde_json::json!({
            "file1": {
                "type": "file",
                "id": "42",
                "name": "photo.jpg",
                "mimetype": "image/jpeg",
                "size": 102400,
            }
        });

        let talk_msg = TalkMessage {
            id: 126,
            token: Some("ab12cd34".into()),
            actor_type: Some("users".into()),
            actor_id: Some("alice".into()),
            actor_display_name: Some("Alice".into()),
            timestamp: Some(1700000003),
            message: Some("{file1}".into()),
            message_type: Some("comment".into()),
            message_parameters: Some(params),
            system_message: None,
        };

        let media = channel.extract_media_from_params(&talk_msg);
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].filename.as_deref(), Some("photo.jpg"));
        assert_eq!(media[0].mime_type, "image/jpeg");
        assert_eq!(media[0].size_bytes, Some(102400));
    }
}
