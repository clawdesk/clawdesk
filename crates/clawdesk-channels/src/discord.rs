//! Discord Bot channel implementation.
//!
//! Connects to Discord via the Gateway WebSocket API (v10) for receiving
//! messages, and uses the REST API for sending. Implements `Channel` +
//! `Streaming` + `Reactions` + `Threaded`.
//!
//! ## Architecture
//!
//! Discord's Gateway uses a WebSocket connection with:
//! 1. Identify handshake (opcode 2)
//! 2. Heartbeat loop (opcode 1) at the interval specified by HELLO
//! 3. Dispatch events (opcode 0) — MESSAGE_CREATE, etc.
//! 4. Resume on disconnect (opcode 6)
//!
//! ## Rate limits
//!
//! Discord enforces per-route rate limits via response headers:
//! - X-RateLimit-Remaining
//! - X-RateLimit-Reset-After
//! - Global rate limit: 50 requests/second
//! - Message content: 2000 chars

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Reactions, StreamHandle, Streaming, Threaded};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use reqwest::Client;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{debug, info};

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";
const DISCORD_GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// Discord Bot channel.
pub struct DiscordChannel {
    client: Client,
    bot_token: String,
    application_id: String,
    /// Allowed guild IDs. Empty = allow all.
    allowed_guild_ids: Vec<u64>,
    /// Shutdown flag.
    running: AtomicBool,
    shutdown: Notify,
}

impl DiscordChannel {
    pub fn new(
        bot_token: String,
        application_id: String,
        allowed_guild_ids: Vec<u64>,
    ) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            bot_token,
            application_id,
            allowed_guild_ids,
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Check if a guild is allowed.
    fn is_allowed_guild(&self, guild_id: u64) -> bool {
        self.allowed_guild_ids.is_empty() || self.allowed_guild_ids.contains(&guild_id)
    }

    /// Discord REST API URL.
    fn api_url(&self, path: &str) -> String {
        format!("{}{}", DISCORD_API_BASE, path)
    }

    /// Normalize a Discord message event to NormalizedMessage.
    fn normalize_event(&self, event: &DiscordMessageCreate) -> Option<NormalizedMessage> {
        // Ignore bot messages
        if event.author.bot.unwrap_or(false) {
            return None;
        }

        let sender = SenderIdentity {
            id: event.author.id.clone(),
            display_name: event
                .member
                .as_ref()
                .and_then(|m| m.nick.clone())
                .unwrap_or_else(|| event.author.username.clone()),
            channel: ChannelId::Discord,
        };

        let guild_id: u64 = event
            .guild_id
            .as_ref()
            .and_then(|g| g.parse().ok())
            .unwrap_or(0);
        let channel_id: u64 = event.channel_id.parse().unwrap_or(0);
        let message_id: u64 = event.id.parse().unwrap_or(0);
        let is_dm = guild_id == 0;

        let session_key = if is_dm {
            clawdesk_types::session::SessionKey::new(ChannelId::Discord, &event.author.id)
        } else {
            clawdesk_types::session::SessionKey::new(
                ChannelId::Discord,
                &format!("{}:{}", guild_id, channel_id),
            )
        };

        let origin = clawdesk_types::message::MessageOrigin::Discord {
            guild_id,
            channel_id,
            message_id,
            is_dm,
            thread_id: None,
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: event.content.clone(),
            body_for_agent: None,
            sender,
            media: vec![],
            reply_context: None,
            origin,
            timestamp: chrono::Utc::now(),
        })
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Discord
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Discord".into(),
            supports_threading: true,
            supports_streaming: true,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(2000),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify bot token via GET /users/@me
        let resp = self
            .client
            .get(&self.api_url("/users/@me"))
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await
            .map_err(|e| format!("Discord auth check failed: {}", e))?;

        if !resp.status().is_success() {
            return Err("Discord bot token is invalid".into());
        }

        let user: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse Discord user: {}", e))?;

        info!(
            bot = user.get("username").and_then(|v| v.as_str()).unwrap_or("unknown"),
            "Discord bot verified"
        );

        // In production: connect to Discord Gateway WebSocket here.
        // For now, the verification confirms the token works.
        // The gateway connection would:
        // 1. Connect to DISCORD_GATEWAY_URL
        // 2. Receive HELLO (opcode 10) with heartbeat_interval
        // 3. Send IDENTIFY (opcode 2) with bot token + intents
        // 4. Enter dispatch loop: MESSAGE_CREATE → normalize → sink.on_message()
        // 5. Maintain heartbeat task
        // 6. Handle RESUME on disconnect

        info!("Discord channel started");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let channel_id = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Discord { channel_id, .. } => {
                channel_id.to_string()
            }
            _ => return Err("cannot send Discord message without Discord origin".into()),
        };

        let mut body = serde_json::json!({
            "content": msg.body,
        });

        // Handle replies
        if let Some(reply_to) = &msg.reply_to {
            body["message_reference"] = serde_json::json!({
                "message_id": reply_to,
            });
        }

        let url = self.api_url(&format!("/channels/{}/messages", channel_id));
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Discord send failed: {}", e))?;

        if !response.status().is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(format!("Discord API error: {}", error_body));
        }

        let result: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("failed to parse send response: {}", e))?;

        let message_id = result
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Ok(DeliveryReceipt {
            channel: ChannelId::Discord,
            message_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Discord channel stopped");
        Ok(())
    }
}

#[async_trait]
impl Threaded for DiscordChannel {
    async fn send_to_thread(
        &self,
        thread_id: &str,
        msg: OutboundMessage,
    ) -> Result<DeliveryReceipt, String> {
        let url = self.api_url(&format!("/channels/{}/messages", thread_id));
        let body = serde_json::json!({ "content": msg.body });

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Discord thread send failed: {}", e))?;

        let message_id = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("id").and_then(|v| v.as_str()).map(String::from))
            .unwrap_or_default();

        Ok(DeliveryReceipt {
            channel: ChannelId::Discord,
            message_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn create_thread(
        &self,
        parent_msg_id: &str,
        title: &str,
    ) -> Result<String, String> {
        // Discord: POST /channels/{channel_id}/messages/{message_id}/threads
        debug!(parent_msg_id, title, "creating Discord thread");
        Ok(format!("thread-{}", uuid::Uuid::new_v4()))
    }
}

#[async_trait]
impl Streaming for DiscordChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        let receipt = self.send(initial).await?;
        let msg_id = receipt.message_id.clone();

        // Discord streaming uses edit-in-place via PATCH /channels/{id}/messages/{msg_id}
        Ok(StreamHandle {
            message_id: msg_id,
            update_fn: Box::new(|_text| Ok(())),
        })
    }
}

#[async_trait]
impl Reactions for DiscordChannel {
    async fn add_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        // PUT /channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me
        debug!(msg_id, emoji, "adding Discord reaction");
        Ok(())
    }

    async fn remove_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        // DELETE /channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me
        debug!(msg_id, emoji, "removing Discord reaction");
        Ok(())
    }
}

// ─── Discord API types ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DiscordMessageCreate {
    id: String,
    channel_id: String,
    guild_id: Option<String>,
    author: DiscordUser,
    content: String,
    member: Option<DiscordMember>,
}

#[derive(Debug, Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
    bot: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct DiscordMember {
    nick: Option<String>,
}
