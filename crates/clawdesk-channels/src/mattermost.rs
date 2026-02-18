//! Mattermost channel adapter via WebSocket + REST API.
//!
//! Connects to a Mattermost server using its WebSocket API (v4) for
//! real-time events and the REST API for sending messages.
//! Implements `Channel` + `Threaded` + `Reactions`.
//!
//! ## Architecture
//!
//! ```text
//! MattermostChannel
//! ├── ws_loop()       — WebSocket connection for real-time events
//! ├── normalize()     — Mattermost posted event → NormalizedMessage
//! ├── send()          — OutboundMessage → POST /api/v4/posts
//! ├── send_to_thread()— reply to root post via root_id field
//! └── add_reaction()  — POST /api/v4/reactions
//! ```
//!
//! ## Mattermost API (v4)
//!
//! REST:
//! - `POST /api/v4/posts`           — create a post
//! - `GET  /api/v4/posts/{id}`      — get a post
//! - `PUT  /api/v4/posts/{id}`      — update a post
//! - `POST /api/v4/reactions`       — add a reaction
//! - `GET  /api/v4/users/me`        — verify token
//! - `GET  /api/v4/channels/{id}`   — get channel info
//!
//! WebSocket:
//! - `wss://{server}/api/v4/websocket` — real-time event stream
//! - Events: `posted`, `post_edited`, `post_deleted`, `typing`
//!
//! ## Rate limits
//!
//! Default Mattermost rate limits:
//! - 10 requests/second per user
//! - Configurable per-server

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Reactions, StreamHandle, Streaming, Threaded};
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

/// Mattermost channel adapter.
pub struct MattermostChannel {
    client: Client,
    /// Mattermost server URL (e.g., `https://mattermost.example.com`).
    server_url: String,
    /// Personal access token or bot token.
    token: String,
    /// Team ID to operate in.
    team_id: String,
    /// Default channel ID for sending.
    default_channel_id: Option<String>,
    /// Allowed channel IDs. Empty = allow all.
    allowed_channel_ids: Vec<String>,
    /// Our own user ID (populated during start).
    bot_user_id: std::sync::Mutex<Option<String>>,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for the Mattermost channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MattermostConfig {
    pub server_url: String,
    pub token: String,
    pub team_id: String,
    pub default_channel_id: Option<String>,
    #[serde(default)]
    pub allowed_channel_ids: Vec<String>,
}

impl MattermostChannel {
    pub fn new(config: MattermostConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            server_url: config.server_url.trim_end_matches('/').to_string(),
            token: config.token,
            team_id: config.team_id,
            default_channel_id: config.default_channel_id,
            allowed_channel_ids: config.allowed_channel_ids,
            bot_user_id: std::sync::Mutex::new(None),
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Build a Mattermost REST API URL.
    fn api_url(&self, path: &str) -> String {
        format!("{}/api/v4{}", self.server_url, path)
    }

    /// Build the WebSocket URL for real-time events.
    fn ws_url(&self) -> String {
        let base = self
            .server_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        format!("{}/api/v4/websocket", base)
    }

    /// Check if a channel ID is allowed.
    fn is_allowed_channel(&self, channel_id: &str) -> bool {
        self.allowed_channel_ids.is_empty()
            || self.allowed_channel_ids.iter().any(|c| c == channel_id)
    }

    /// Check if a message is from our own bot user.
    fn is_own_message(&self, user_id: &str) -> bool {
        self.bot_user_id
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|id| id == user_id))
            .unwrap_or(false)
    }

    /// Normalize a Mattermost posted event to NormalizedMessage.
    fn normalize_post(&self, post: &MattermostPost) -> Option<NormalizedMessage> {
        // Ignore bot's own messages
        if self.is_own_message(&post.user_id) {
            return None;
        }

        // Filter by allowed channels
        if !self.is_allowed_channel(&post.channel_id) {
            debug!(channel_id = %post.channel_id, "ignoring post from unallowed channel");
            return None;
        }

        let sender = SenderIdentity {
            id: post.user_id.clone(),
            display_name: post.user_id.clone(), // Resolved via GET /users/{id} in production
            channel: ChannelId::Mattermost,
        };

        let session_key = clawdesk_types::session::SessionKey::new(
            ChannelId::Mattermost,
            &format!("{}:{}", self.server_url, post.channel_id),
        );

        let origin = clawdesk_types::message::MessageOrigin::Mattermost {
            server_url: self.server_url.clone(),
            channel_id: post.channel_id.clone(),
            post_id: post.id.clone(),
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: post.message.clone(),
            body_for_agent: None,
            sender,
            media: vec![],
            reply_context: post.root_id.as_ref().filter(|s| !s.is_empty()).map(|id| {
                clawdesk_types::message::ReplyContext {
                    original_message_id: id.clone(),
                    original_text: None,
                    original_sender: None,
                }
            }),
            origin,
            timestamp: chrono::Utc::now(),
        })
    }

    /// WebSocket event loop for receiving real-time posts.
    async fn ws_loop(self: Arc<Self>, _sink: Arc<dyn MessageSink>) {
        info!(url = %self.ws_url(), "Mattermost WebSocket loop started");

        // In production: connect to WebSocket, authenticate with token,
        // handle `posted` events, reconnect on disconnect.
        //
        // 1. Connect to ws_url()
        // 2. Send auth challenge: {"seq": 1, "action": "authentication_challenge", "data": {"token": "..."}}
        // 3. Read events: {"event": "posted", "data": {"post": "{...json...}"}}
        // 4. Parse post JSON, normalize, dispatch via sink.on_message()
        // 5. Handle seq/seq_reply for request-response
        // 6. Reconnect with exponential backoff on disconnect

        while self.running.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        info!("Mattermost WebSocket loop stopped");
    }
}

#[async_trait]
impl Channel for MattermostChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Mattermost
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Mattermost".into(),
            supports_threading: true,
            supports_streaming: true,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(16383),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify token via /users/me
        let resp = self
            .client
            .get(&self.api_url("/users/me"))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| format!("Mattermost auth check failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "Mattermost token invalid (HTTP {})",
                resp.status().as_u16()
            ));
        }

        let user: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Mattermost user parse failed: {}", e))?;

        let user_id = user
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let username = user
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Store our user ID to filter own messages
        if let Ok(mut guard) = self.bot_user_id.lock() {
            *guard = Some(user_id);
        }

        info!(
            username = %username,
            server = %self.server_url,
            team = %self.team_id,
            "Mattermost bot verified"
        );

        // In production: spawn WebSocket loop here
        info!("Mattermost channel started");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let channel_id = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Mattermost { channel_id, .. } => {
                channel_id.clone()
            }
            _ => {
                // Fall back to default channel
                self.default_channel_id
                    .clone()
                    .ok_or("cannot determine Mattermost channel for message")?
            }
        };

        let mut body = serde_json::json!({
            "channel_id": channel_id,
            "message": msg.body,
        });

        // Thread reply via root_id
        if let Some(ref thread_id) = msg.thread_id {
            body["root_id"] = serde_json::json!(thread_id);
        }

        let url = self.api_url("/posts");
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Mattermost send failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let err = response.text().await.unwrap_or_default();
            return Err(format!("Mattermost API HTTP {}: {}", status, err));
        }

        let result: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Mattermost response parse failed: {}", e))?;

        let post_id = result
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Ok(DeliveryReceipt {
            channel: ChannelId::Mattermost,
            message_id: post_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Mattermost channel stopped");
        Ok(())
    }
}

#[async_trait]
impl Threaded for MattermostChannel {
    async fn send_to_thread(
        &self,
        thread_id: &str,
        msg: OutboundMessage,
    ) -> Result<DeliveryReceipt, String> {
        let channel_id = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Mattermost { channel_id, .. } => {
                channel_id.clone()
            }
            _ => self
                .default_channel_id
                .clone()
                .ok_or("cannot determine channel for thread reply")?,
        };

        let body = serde_json::json!({
            "channel_id": channel_id,
            "message": msg.body,
            "root_id": thread_id,
        });

        let response = self
            .client
            .post(&self.api_url("/posts"))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Mattermost thread send failed: {}", e))?;

        let post_id = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("id").and_then(|v| v.as_str()).map(String::from))
            .unwrap_or_default();

        Ok(DeliveryReceipt {
            channel: ChannelId::Mattermost,
            message_id: post_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn create_thread(
        &self,
        parent_msg_id: &str,
        _title: &str,
    ) -> Result<String, String> {
        // Mattermost threads are created by replying with root_id.
        // The parent message ID becomes the thread's root_id.
        Ok(parent_msg_id.to_string())
    }
}

#[async_trait]
impl Streaming for MattermostChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        let receipt = self.send(initial).await?;
        let msg_id = receipt.message_id.clone();

        // Mattermost supports message editing via PUT /api/v4/posts/{id}
        Ok(StreamHandle {
            message_id: msg_id,
            update_fn: Box::new(|_text| Ok(())),
        })
    }
}

#[async_trait]
impl Reactions for MattermostChannel {
    async fn add_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        let user_id = self
            .bot_user_id
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .ok_or("bot user ID not available")?;

        let body = serde_json::json!({
            "user_id": user_id,
            "post_id": msg_id,
            "emoji_name": emoji.trim_matches(':'),
        });

        let resp = self
            .client
            .post(&self.api_url("/reactions"))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Mattermost reaction failed: {}", e))?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Mattermost reaction error: {}", err));
        }

        debug!(msg_id, emoji, "added Mattermost reaction");
        Ok(())
    }

    async fn remove_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        let user_id = self
            .bot_user_id
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .ok_or("bot user ID not available")?;

        let emoji_name = emoji.trim_matches(':');
        let url = self.api_url(&format!(
            "/users/{}/posts/{}/reactions/{}",
            user_id, msg_id, emoji_name
        ));

        let resp = self
            .client
            .delete(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| format!("Mattermost reaction remove failed: {}", e))?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Mattermost reaction remove error: {}", err));
        }

        debug!(msg_id, emoji, "removed Mattermost reaction");
        Ok(())
    }
}

// ─── Mattermost API types ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct MattermostPost {
    id: String,
    #[serde(rename = "channel_id")]
    channel_id: String,
    #[serde(rename = "user_id")]
    user_id: String,
    message: String,
    #[serde(rename = "root_id")]
    root_id: Option<String>,
    #[serde(rename = "create_at")]
    create_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct MattermostWsEvent {
    event: String,
    data: Option<serde_json::Value>,
    seq: Option<i64>,
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> MattermostConfig {
        MattermostConfig {
            server_url: "https://mattermost.example.com".into(),
            token: "test-token-abc123".into(),
            team_id: "team-id-001".into(),
            default_channel_id: Some("channel-001".into()),
            allowed_channel_ids: vec!["channel-001".into(), "channel-002".into()],
        }
    }

    #[test]
    fn test_mattermost_creation() {
        let channel = MattermostChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::Mattermost);
        assert_eq!(channel.server_url, "https://mattermost.example.com");
        assert_eq!(channel.team_id, "team-id-001");
    }

    #[test]
    fn test_mattermost_meta() {
        let channel = MattermostChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "Mattermost");
        assert!(meta.supports_threading);
        assert!(meta.supports_streaming);
        assert!(meta.supports_reactions);
        assert!(meta.supports_media);
        assert_eq!(meta.max_message_length, Some(16383));
    }

    #[test]
    fn test_mattermost_api_url() {
        let channel = MattermostChannel::new(test_config());
        assert_eq!(
            channel.api_url("/posts"),
            "https://mattermost.example.com/api/v4/posts"
        );
        assert_eq!(
            channel.api_url("/users/me"),
            "https://mattermost.example.com/api/v4/users/me"
        );
    }

    #[test]
    fn test_mattermost_ws_url() {
        let channel = MattermostChannel::new(test_config());
        assert_eq!(
            channel.ws_url(),
            "wss://mattermost.example.com/api/v4/websocket"
        );

        let mut config = test_config();
        config.server_url = "http://localhost:8065".into();
        let local = MattermostChannel::new(config);
        assert_eq!(
            local.ws_url(),
            "ws://localhost:8065/api/v4/websocket"
        );
    }

    #[test]
    fn test_mattermost_allowed_channels() {
        let channel = MattermostChannel::new(test_config());
        assert!(channel.is_allowed_channel("channel-001"));
        assert!(channel.is_allowed_channel("channel-002"));
        assert!(!channel.is_allowed_channel("channel-999"));

        let mut config = test_config();
        config.allowed_channel_ids = vec![];
        let open = MattermostChannel::new(config);
        assert!(open.is_allowed_channel("anything"));
    }

    #[test]
    fn test_mattermost_normalize_post() {
        let channel = MattermostChannel::new(test_config());

        let post = MattermostPost {
            id: "post-001".into(),
            channel_id: "channel-001".into(),
            user_id: "user-alice".into(),
            message: "Hello from Mattermost!".into(),
            root_id: None,
            create_at: Some(1700000000000),
        };

        let normalized = channel.normalize_post(&post).unwrap();
        assert_eq!(normalized.body, "Hello from Mattermost!");
        assert_eq!(normalized.sender.id, "user-alice");
        assert!(normalized.reply_context.is_none());
    }

    #[test]
    fn test_mattermost_normalize_threaded_post() {
        let channel = MattermostChannel::new(test_config());

        let post = MattermostPost {
            id: "post-002".into(),
            channel_id: "channel-001".into(),
            user_id: "user-bob".into(),
            message: "Reply in thread".into(),
            root_id: Some("post-001".into()),
            create_at: Some(1700000001000),
        };

        let normalized = channel.normalize_post(&post).unwrap();
        assert!(normalized.reply_context.is_some());
        assert_eq!(
            normalized.reply_context.unwrap().original_message_id,
            "post-001"
        );
    }
}
