//! Google Chat channel adapter via Webhook and REST API.
//!
//! Supports two modes:
//! 1. **Webhook mode** — outbound only, posts to a Google Chat space webhook URL
//! 2. **API mode** — full bidirectional via Google Chat API with service account
//!
//! ## Architecture
//!
//! ```text
//! GoogleChatChannel
//! ├── poll_loop()     — polls spaces.messages.list for inbound (API mode)
//! ├── normalize()     — Google Chat event → NormalizedMessage
//! ├── send()          — OutboundMessage → POST webhook or spaces.messages.create
//! └── send_to_thread()— thread support via thread.name / threadKey
//! ```
//!
//! ## Google Chat API
//!
//! - `POST /v1/spaces/{space}/messages`      — send a message
//! - `GET  /v1/spaces/{space}/messages`      — list messages
//! - `GET  /v1/spaces/{space}/messages/{id}` — get a single message
//! - `PUT  /v1/spaces/{space}/messages/{id}` — update a message
//! - Webhook: `POST https://chat.googleapis.com/v1/spaces/.../messages?key=...&token=...`
//!
//! ## Limits
//!
//! - Message text: 28 KB (approximately 4000 chars)
//! - Card messages: 32 KB
//! - Webhook: 1 message/second per webhook
//! - API: Based on Google Cloud quotas

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Threaded};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Google Chat channel adapter.
pub struct GoogleChatChannel {
    client: Client,
    /// Space resource name (e.g., `spaces/AAAA...`).
    space_name: String,
    /// Webhook URL for outbound-only mode (optional).
    webhook_url: Option<String>,
    /// Service account access token for API mode (optional).
    access_token: Option<String>,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for the Google Chat channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleChatConfig {
    pub space_name: String,
    pub webhook_url: Option<String>,
    pub access_token: Option<String>,
}

const GOOGLE_CHAT_API_BASE: &str = "https://chat.googleapis.com/v1";

impl GoogleChatChannel {
    pub fn new(config: GoogleChatConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            space_name: config.space_name,
            webhook_url: config.webhook_url,
            access_token: config.access_token,
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Whether this channel is operating in webhook-only mode.
    fn is_webhook_mode(&self) -> bool {
        self.webhook_url.is_some() && self.access_token.is_none()
    }

    /// Build a Google Chat API URL for the given path.
    fn api_url(&self, path: &str) -> String {
        format!("{}{}", GOOGLE_CHAT_API_BASE, path)
    }

    /// Poll for new messages via the Chat API (API mode only).
    async fn poll_loop(self: Arc<Self>, sink: Arc<dyn MessageSink>) {
        info!(space = %self.space_name, "Google Chat poll loop started");

        let token = match &self.access_token {
            Some(t) => t.clone(),
            None => {
                warn!("Google Chat poll loop requires access_token; webhook-only mode active");
                return;
            }
        };

        while self.running.load(Ordering::Relaxed) {
            let url = self.api_url(&format!("/{}/messages", self.space_name));

            let result = self
                .client
                .get(&url)
                .bearer_auth(&token)
                .send()
                .await;

            match result {
                Ok(response) => {
                    if let Ok(body) = response.json::<GoogleChatMessageList>().await {
                        for message in body.messages.unwrap_or_default() {
                            if let Some(normalized) = self.normalize_message(&message) {
                                sink.on_message(normalized).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Google Chat poll error, retrying in 10s");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }

            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        info!("Google Chat poll loop stopped");
    }

    /// Normalize a Google Chat message to NormalizedMessage.
    fn normalize_message(&self, msg: &GoogleChatMessage) -> Option<NormalizedMessage> {
        let text = msg.text.clone()?;

        // Ignore bot messages (sender type != HUMAN)
        let sender_info = msg.sender.as_ref()?;
        if sender_info.sender_type != "HUMAN" {
            return None;
        }

        let sender = SenderIdentity {
            id: sender_info.name.clone(),
            display_name: sender_info.display_name.clone(),
            channel: ChannelId::GoogleChat,
        };

        let session_key = if let Some(ref thread) = msg.thread {
            clawdesk_types::session::SessionKey::new(
                ChannelId::GoogleChat,
                &format!("{}:{}", self.space_name, thread.name),
            )
        } else {
            clawdesk_types::session::SessionKey::new(ChannelId::GoogleChat, &self.space_name)
        };

        let origin = clawdesk_types::message::MessageOrigin::GoogleChat {
            space_name: msg.space.clone().unwrap_or_else(|| self.space_name.clone()),
            message_name: msg.name.clone(),
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: text,
            body_for_agent: None,
            sender,
            media: vec![],
            reply_context: None,
            origin,
            timestamp: chrono::Utc::now(),
        })
    }

    /// Send a message via webhook URL.
    async fn send_webhook(&self, text: &str, thread_key: Option<&str>) -> Result<String, String> {
        let webhook = self
            .webhook_url
            .as_ref()
            .ok_or("webhook URL not configured")?;

        let mut body = serde_json::json!({ "text": text });

        if let Some(tk) = thread_key {
            body["thread"] = serde_json::json!({ "threadKey": tk });
        }

        let url = if thread_key.is_some() {
            format!("{}&messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD", webhook)
        } else {
            webhook.clone()
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Google Chat webhook send failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Google Chat webhook HTTP {}: {}", status, err));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse webhook response: {}", e))?;

        Ok(result
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Send a message via the Chat REST API.
    async fn send_api(
        &self,
        text: &str,
        thread_name: Option<&str>,
    ) -> Result<String, String> {
        let token = self
            .access_token
            .as_ref()
            .ok_or("access_token not configured for API mode")?;

        let url = self.api_url(&format!("/{}/messages", self.space_name));

        let mut body = serde_json::json!({ "text": text });

        if let Some(tn) = thread_name {
            body["thread"] = serde_json::json!({ "name": tn });
        }

        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Google Chat API send failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Google Chat API HTTP {}: {}", status, err));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse API response: {}", e))?;

        Ok(result
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }
}

#[async_trait]
impl Channel for GoogleChatChannel {
    fn id(&self) -> ChannelId {
        ChannelId::GoogleChat
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Google Chat".into(),
            supports_threading: true,
            supports_streaming: false,
            supports_reactions: false,
            supports_media: false,
            supports_groups: true,
            max_message_length: Some(4096),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        if self.is_webhook_mode() {
            info!(
                space = %self.space_name,
                "Google Chat channel started (webhook-only mode)"
            );
        } else {
            // Verify API access
            let token = self
                .access_token
                .as_ref()
                .ok_or("Google Chat API mode requires access_token")?;

            let url = self.api_url(&format!("/{}", self.space_name));
            let resp = self
                .client
                .get(&url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| format!("Google Chat API check failed: {}", e))?;

            if !resp.status().is_success() {
                return Err(format!(
                    "Google Chat API returned HTTP {}",
                    resp.status().as_u16()
                ));
            }

            info!(
                space = %self.space_name,
                "Google Chat channel started (API mode)"
            );
        }

        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let thread_id = msg.thread_id.as_deref();

        let message_name = if self.is_webhook_mode() {
            self.send_webhook(&msg.body, thread_id).await?
        } else {
            self.send_api(&msg.body, thread_id).await?
        };

        Ok(DeliveryReceipt {
            channel: ChannelId::GoogleChat,
            message_id: message_name,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Google Chat channel stopped");
        Ok(())
    }
}

#[async_trait]
impl Threaded for GoogleChatChannel {
    async fn send_to_thread(
        &self,
        thread_id: &str,
        msg: OutboundMessage,
    ) -> Result<DeliveryReceipt, String> {
        let message_name = if self.is_webhook_mode() {
            self.send_webhook(&msg.body, Some(thread_id)).await?
        } else {
            self.send_api(&msg.body, Some(thread_id)).await?
        };

        Ok(DeliveryReceipt {
            channel: ChannelId::GoogleChat,
            message_id: message_name,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn create_thread(
        &self,
        _parent_msg_id: &str,
        title: &str,
    ) -> Result<String, String> {
        // In Google Chat, threads are created implicitly when you reply
        // with a threadKey or thread.name. The first message with a new
        // threadKey creates the thread.
        let thread_key = format!("thread-{}", uuid::Uuid::new_v4());
        debug!(thread_key = %thread_key, title, "creating Google Chat thread");
        Ok(thread_key)
    }
}

// ─── Google Chat API types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GoogleChatMessageList {
    messages: Option<Vec<GoogleChatMessage>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleChatMessage {
    name: String,
    text: Option<String>,
    sender: Option<GoogleChatSender>,
    space: Option<String>,
    thread: Option<GoogleChatThread>,
    #[serde(rename = "createTime")]
    create_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleChatSender {
    name: String,
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(rename = "type")]
    sender_type: String,
}

#[derive(Debug, Deserialize)]
struct GoogleChatThread {
    name: String,
    #[serde(rename = "threadKey")]
    thread_key: Option<String>,
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn webhook_config() -> GoogleChatConfig {
        GoogleChatConfig {
            space_name: "spaces/AAAA1234".into(),
            webhook_url: Some("https://chat.googleapis.com/v1/spaces/AAAA1234/messages?key=test&token=test".into()),
            access_token: None,
        }
    }

    fn api_config() -> GoogleChatConfig {
        GoogleChatConfig {
            space_name: "spaces/AAAA1234".into(),
            webhook_url: None,
            access_token: Some("ya29.test-token".into()),
        }
    }

    #[test]
    fn test_googlechat_creation_webhook() {
        let channel = GoogleChatChannel::new(webhook_config());
        assert_eq!(channel.id(), ChannelId::GoogleChat);
        assert!(channel.is_webhook_mode());
        assert_eq!(channel.space_name, "spaces/AAAA1234");
    }

    #[test]
    fn test_googlechat_creation_api() {
        let channel = GoogleChatChannel::new(api_config());
        assert!(!channel.is_webhook_mode());
        assert!(channel.access_token.is_some());
    }

    #[test]
    fn test_googlechat_meta() {
        let channel = GoogleChatChannel::new(webhook_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "Google Chat");
        assert!(meta.supports_threading);
        assert!(!meta.supports_streaming);
        assert!(meta.supports_groups);
        assert_eq!(meta.max_message_length, Some(4096));
    }

    #[test]
    fn test_googlechat_api_url() {
        let channel = GoogleChatChannel::new(api_config());
        assert_eq!(
            channel.api_url("/spaces/AAAA1234/messages"),
            "https://chat.googleapis.com/v1/spaces/AAAA1234/messages"
        );
    }
}
