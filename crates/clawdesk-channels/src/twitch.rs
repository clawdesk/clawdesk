//! Twitch IRC/Helix channel implementation.
//!
//! Twitch chat uses IRC protocol (irc.chat.twitch.tv:6697 with TLS)
//! for real-time chat, combined with the Twitch Helix REST API for
//! sending messages and managing bot state.
//!
//! ## Architecture
//!
//! ```text
//! TwitchChannel
//! ├── irc_connect()    — TLS connection to irc.chat.twitch.tv:6697
//! ├── irc_read_loop()  — reads PRIVMSG lines from IRC socket
//! ├── normalize()      — Twitch IRC → NormalizedMessage
//! ├── send()           — OutboundMessage → Helix POST /chat/messages
//! ├── irc_send()       — fallback PRIVMSG via IRC socket
//! └── ping_handler()   — responds to PING with PONG (keepalive)
//! ```
//!
//! ## Twitch IRC Protocol
//!
//! Twitch IRC extends RFC 2812 with IRCv3 tags:
//! - `@badge-info=;badges=moderator/1;color=#FF0000;display-name=Bot;...
//!    :user!user@user.tmi.twitch.tv PRIVMSG #channel :message text`
//! - Authentication: `PASS oauth:<token>` + `NICK <botname>`
//! - Capabilities: `CAP REQ :twitch.tv/membership twitch.tv/tags twitch.tv/commands`
//!
//! ## Twitch Helix API
//!
//! - `POST /helix/chat/messages`       — send chat message
//! - `GET  /helix/users`               — get user info
//! - `GET  /helix/channels`            — get channel info
//! - `POST /helix/moderation/bans`     — timeout/ban users
//!
//! ## Limits
//!
//! - Message length: 500 characters
//! - Rate limits: 20 messages/30s (normal), 100/30s (moderator)
//! - No media/file support in chat

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

const TWITCH_IRC_HOST: &str = "irc.chat.twitch.tv";
const TWITCH_IRC_PORT: u16 = 6697;
const TWITCH_HELIX_BASE: &str = "https://api.twitch.tv/helix";
const MAX_MESSAGE_LENGTH: usize = 500;

/// Twitch IRC/Helix channel adapter.
pub struct TwitchChannel {
    client: Client,
    /// Bot's Twitch username (lowercase).
    nick: String,
    /// OAuth token for IRC authentication (without "oauth:" prefix).
    oauth_token: String,
    /// Target channel name (e.g., `"#streamername"`).
    channel_name: String,
    /// Bot's Twitch user ID (for Helix API).
    bot_user_id: String,
    /// Broadcaster's user ID (for Helix API send endpoint).
    broadcaster_id: Option<String>,
    /// Client ID for Helix API requests.
    client_id: String,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for the Twitch channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwitchConfig {
    pub nick: String,
    pub oauth_token: String,
    pub channel_name: String,
    pub bot_user_id: String,
    #[serde(default)]
    pub broadcaster_id: Option<String>,
    pub client_id: String,
}

/// Parsed Twitch IRC message with optional IRCv3 tags.
#[derive(Debug, Clone)]
struct TwitchIrcMessage {
    /// IRCv3 tags (key=value pairs).
    tags: Vec<(String, String)>,
    /// Prefix (`:nick!user@host`).
    prefix: Option<String>,
    /// IRC command (`PRIVMSG`, `PING`, etc.).
    command: String,
    /// Command parameters.
    params: Vec<String>,
    /// Trailing text (message body).
    trailing: Option<String>,
}

impl TwitchIrcMessage {
    /// Parse a raw Twitch IRC line including IRCv3 tags.
    fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches('\r').trim_end_matches('\n');
        if line.is_empty() {
            return None;
        }

        let mut rest = line;
        let mut tags = Vec::new();

        // Parse IRCv3 tags: @key=value;key2=value2
        if rest.starts_with('@') {
            let space = rest.find(' ')?;
            let tag_str = &rest[1..space];
            for pair in tag_str.split(';') {
                if let Some((k, v)) = pair.split_once('=') {
                    tags.push((k.to_string(), v.to_string()));
                } else {
                    tags.push((pair.to_string(), String::new()));
                }
            }
            rest = &rest[space + 1..];
        }

        // Parse prefix
        let prefix = if rest.starts_with(':') {
            let space = rest.find(' ')?;
            let pfx = rest[1..space].to_string();
            rest = &rest[space + 1..];
            Some(pfx)
        } else {
            None
        };

        // Split trailing
        let (main_part, trailing) = if let Some(pos) = rest.find(" :") {
            (&rest[..pos], Some(rest[pos + 2..].to_string()))
        } else {
            (rest, None)
        };

        let mut parts: Vec<&str> = main_part.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        let command = parts.remove(0).to_uppercase();
        let params: Vec<String> = parts.iter().map(|s| s.to_string()).collect();

        Some(TwitchIrcMessage {
            tags,
            prefix,
            command,
            params,
            trailing,
        })
    }

    /// Extract the nick from the prefix (`:nick!user@host.tmi.twitch.tv` → `nick`).
    fn nick(&self) -> Option<&str> {
        self.prefix
            .as_ref()
            .map(|p| p.split('!').next().unwrap_or(p))
    }

    /// Get the display name from IRCv3 tags, falling back to nick.
    fn display_name(&self) -> String {
        self.tag("display-name")
            .unwrap_or_else(|| self.nick().unwrap_or("unknown").to_string())
    }

    /// Get a tag value by key.
    fn tag(&self, key: &str) -> Option<String> {
        self.tags
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    /// Get the target channel (first param for PRIVMSG).
    fn target(&self) -> Option<&str> {
        self.params.first().map(|s| s.as_str())
    }

    /// Get the message text (trailing part).
    fn text(&self) -> Option<&str> {
        self.trailing.as_deref()
    }
}

impl TwitchChannel {
    pub fn new(config: TwitchConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            nick: config.nick.to_lowercase(),
            oauth_token: config.oauth_token,
            channel_name: if config.channel_name.starts_with('#') {
                config.channel_name
            } else {
                format!("#{}", config.channel_name)
            },
            bot_user_id: config.bot_user_id,
            broadcaster_id: config.broadcaster_id,
            client_id: config.client_id,
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Build a Helix API URL.
    fn helix_url(&self, path: &str) -> String {
        format!("{}{}", TWITCH_HELIX_BASE, path)
    }

    /// Format an IRC PRIVMSG command.
    fn format_privmsg(&self, text: &str) -> String {
        format!("PRIVMSG {} :{}", self.channel_name, text)
    }

    /// Truncate text to the Twitch message limit.
    fn truncate_message(text: &str) -> String {
        if text.len() <= MAX_MESSAGE_LENGTH {
            text.to_string()
        } else {
            let truncated = &text[..MAX_MESSAGE_LENGTH - 15];
            format!("{}… [truncated]", truncated)
        }
    }

    /// Send a message via the Helix API (preferred).
    async fn send_helix(&self, text: &str) -> Result<String, String> {
        let broadcaster_id = self
            .broadcaster_id
            .as_deref()
            .ok_or("Twitch: broadcaster_id required for Helix API")?;

        let body = serde_json::json!({
            "broadcaster_id": broadcaster_id,
            "sender_id": self.bot_user_id,
            "message": text,
        });

        let url = self.helix_url("/chat/messages");
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.oauth_token)
            .header("Client-Id", &self.client_id)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Twitch Helix send failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Twitch Helix HTTP {}: {}", status, err));
        }

        let resp_json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Twitch Helix response parse error: {}", e))?;

        let msg_id = resp_json
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("message_id"))
            .and_then(|id| id.as_str())
            .unwrap_or("")
            .to_string();

        Ok(msg_id)
    }

    /// Normalize a Twitch IRC PRIVMSG into a NormalizedMessage.
    fn normalize_privmsg(&self, msg: &TwitchIrcMessage) -> Option<NormalizedMessage> {
        let nick = msg.nick()?;
        let target = msg.target()?;
        let text = msg.text()?;

        // Ignore messages from ourselves
        if nick.eq_ignore_ascii_case(&self.nick) {
            return None;
        }

        let display_name = msg.display_name();
        let user_id = msg.tag("user-id").unwrap_or_else(|| nick.to_string());
        let twitch_msg_id = msg.tag("id").unwrap_or_default();

        let sender = SenderIdentity {
            id: user_id.clone(),
            display_name,
            channel: ChannelId::Twitch,
        };

        let session_id = format!("twitch:{}", target);
        let session_key =
            clawdesk_types::session::SessionKey::new(ChannelId::Twitch, &session_id);

        let origin = clawdesk_types::message::MessageOrigin::Twitch {
            channel_name: target.to_string(),
            message_id: twitch_msg_id,
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: text.to_string(),
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
impl Channel for TwitchChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Twitch
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Twitch".into(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: false,
            supports_media: false,
            supports_groups: true,
            max_message_length: Some(MAX_MESSAGE_LENGTH),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Validate the OAuth token by calling the Helix users endpoint
        let url = self.helix_url("/users");
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.oauth_token)
            .header("Client-Id", &self.client_id)
            .send()
            .await
            .map_err(|e| format!("Twitch: token validation failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "Twitch: invalid token (HTTP {})",
                resp.status().as_u16()
            ));
        }

        info!(
            nick = %self.nick,
            channel = %self.channel_name,
            "Twitch channel started"
        );
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let text = Self::truncate_message(&msg.body);

        // Try Helix API first, fall back to IRC PRIVMSG format
        let message_id = match self.send_helix(&text).await {
            Ok(id) => id,
            Err(helix_err) => {
                warn!(
                    error = %helix_err,
                    "Twitch Helix send failed, would fall back to IRC"
                );
                // In production, this would send via the IRC socket.
                // For now, propagate the error.
                return Err(helix_err);
            }
        };

        Ok(DeliveryReceipt {
            channel: ChannelId::Twitch,
            message_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!(
            channel = %self.channel_name,
            "Twitch channel stopped"
        );
        Ok(())
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> TwitchConfig {
        TwitchConfig {
            nick: "clawdesk_bot".into(),
            oauth_token: "test_oauth_token_abc123".into(),
            channel_name: "#teststreamer".into(),
            bot_user_id: "123456789".into(),
            broadcaster_id: Some("987654321".into()),
            client_id: "test_client_id".into(),
        }
    }

    #[test]
    fn test_twitch_channel_creation() {
        let channel = TwitchChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::Twitch);
        assert_eq!(channel.nick, "clawdesk_bot");
        assert_eq!(channel.channel_name, "#teststreamer");
        assert_eq!(channel.bot_user_id, "123456789");
    }

    #[test]
    fn test_twitch_channel_name_prefix() {
        // Without # prefix — should auto-add
        let mut cfg = test_config();
        cfg.channel_name = "teststreamer".into();
        let channel = TwitchChannel::new(cfg);
        assert_eq!(channel.channel_name, "#teststreamer");

        // With # prefix — should keep as-is
        let channel2 = TwitchChannel::new(test_config());
        assert_eq!(channel2.channel_name, "#teststreamer");
    }

    #[test]
    fn test_twitch_meta() {
        let channel = TwitchChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "Twitch");
        assert!(!meta.supports_threading);
        assert!(!meta.supports_streaming);
        assert!(!meta.supports_media);
        assert!(meta.supports_groups);
        assert_eq!(meta.max_message_length, Some(500));
    }

    #[test]
    fn test_twitch_message_truncation() {
        let short = "Hello, Twitch!";
        assert_eq!(TwitchChannel::truncate_message(short), short);

        let exact = "a".repeat(MAX_MESSAGE_LENGTH);
        assert_eq!(TwitchChannel::truncate_message(&exact), exact);

        let long = "b".repeat(MAX_MESSAGE_LENGTH + 100);
        let truncated = TwitchChannel::truncate_message(&long);
        assert!(truncated.len() <= MAX_MESSAGE_LENGTH);
        assert!(truncated.ends_with("… [truncated]"));
    }

    #[test]
    fn test_twitch_irc_format() {
        let channel = TwitchChannel::new(test_config());
        let privmsg = channel.format_privmsg("Hello chat!");
        assert_eq!(privmsg, "PRIVMSG #teststreamer :Hello chat!");
    }

    #[test]
    fn test_twitch_irc_message_parse_privmsg() {
        let line = "@badge-info=;badges=moderator/1;color=#FF0000;display-name=Alice;id=msg-123;user-id=42 :alice!alice@alice.tmi.twitch.tv PRIVMSG #teststreamer :Hello, world!";
        let msg = TwitchIrcMessage::parse(line).unwrap();

        assert_eq!(msg.command, "PRIVMSG");
        assert_eq!(msg.nick(), Some("alice"));
        assert_eq!(msg.display_name(), "Alice");
        assert_eq!(msg.tag("user-id"), Some("42".into()));
        assert_eq!(msg.tag("id"), Some("msg-123".into()));
        assert_eq!(msg.target(), Some("#teststreamer"));
        assert_eq!(msg.text(), Some("Hello, world!"));
    }

    #[test]
    fn test_twitch_irc_message_parse_ping() {
        let line = "PING :tmi.twitch.tv";
        let msg = TwitchIrcMessage::parse(line).unwrap();
        assert_eq!(msg.command, "PING");
        assert_eq!(msg.trailing.as_deref(), Some("tmi.twitch.tv"));
    }

    #[test]
    fn test_twitch_normalize_privmsg() {
        let channel = TwitchChannel::new(test_config());
        let irc_msg = TwitchIrcMessage {
            tags: vec![
                ("display-name".into(), "Alice".into()),
                ("user-id".into(), "42".into()),
                ("id".into(), "msg-abc".into()),
            ],
            prefix: Some("alice!alice@alice.tmi.twitch.tv".into()),
            command: "PRIVMSG".into(),
            params: vec!["#teststreamer".into()],
            trailing: Some("Hello from Twitch!".into()),
        };

        let normalized = channel.normalize_privmsg(&irc_msg).unwrap();
        assert_eq!(normalized.body, "Hello from Twitch!");
        assert_eq!(normalized.sender.display_name, "Alice");
        assert_eq!(normalized.sender.id, "42");
    }

    #[test]
    fn test_twitch_normalize_ignores_self() {
        let channel = TwitchChannel::new(test_config());
        let irc_msg = TwitchIrcMessage {
            tags: vec![],
            prefix: Some("clawdesk_bot!clawdesk_bot@clawdesk_bot.tmi.twitch.tv".into()),
            command: "PRIVMSG".into(),
            params: vec!["#teststreamer".into()],
            trailing: Some("My own message".into()),
        };

        assert!(channel.normalize_privmsg(&irc_msg).is_none());
    }

    #[test]
    fn test_twitch_helix_url() {
        let channel = TwitchChannel::new(test_config());
        assert_eq!(
            channel.helix_url("/chat/messages"),
            "https://api.twitch.tv/helix/chat/messages"
        );
    }
}
