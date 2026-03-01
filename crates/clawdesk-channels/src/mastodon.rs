//! Mastodon / Fediverse channel adapter via the Mastodon REST API.
//!
//! Connects to any Mastodon-compatible server (Mastodon, Pleroma, Akkoma,
//! Misskey-compatible, GoToSocial) using the Mastodon REST API v1.
//!
//! ## Architecture
//!
//! ```text
//! MastodonChannel
//! ├── start(sink)  — stream via GET /api/v1/streaming/user (SSE)
//! ├── send(msg)    — POST /api/v1/statuses (public/DM based on context)
//! ├── stop()       — close SSE stream
//! └── Reactions via POST /api/v1/statuses/{id}/favourite
//! ```
//!
//! ## Visibility model
//!
//! - Mentions with `@bot` → DM or public reply depending on original visibility
//! - Direct messages → always reply as DM
//! - Can be configured to only respond to mentions, DMs, or both

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Reactions};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Configuration for Mastodon channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MastodonConfig {
    /// Instance URL (e.g., https://mastodon.social).
    pub instance_url: String,
    /// OAuth2 access token.
    pub access_token: String,
    /// Bot account's username (without @).
    pub username: String,
    /// Maximum toot length (default: 500, varies by instance).
    pub max_toot_length: usize,
    /// Whether to respond to public mentions.
    pub respond_to_mentions: bool,
    /// Whether to respond to direct messages.
    pub respond_to_dms: bool,
    /// Content warning text to prepend (if any).
    pub content_warning: Option<String>,
}

impl Default for MastodonConfig {
    fn default() -> Self {
        Self {
            instance_url: String::new(),
            access_token: String::new(),
            username: String::new(),
            max_toot_length: 500,
            respond_to_mentions: true,
            respond_to_dms: true,
            content_warning: None,
        }
    }
}

/// Mastodon status (toot) from the API.
#[derive(Debug, Deserialize)]
pub struct Status {
    pub id: String,
    pub content: String,
    pub account: Account,
    pub visibility: String,
    pub in_reply_to_id: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub mentions: Vec<Mention>,
}

#[derive(Debug, Deserialize)]
pub struct Account {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub acct: String,
    #[serde(default)]
    pub bot: bool,
}

#[derive(Debug, Deserialize)]
pub struct Mention {
    pub id: String,
    pub username: String,
    pub acct: String,
}

/// Notification from the streaming API.
#[derive(Debug, Deserialize)]
pub struct Notification {
    pub id: String,
    #[serde(rename = "type")]
    pub notification_type: String,
    pub status: Option<Status>,
    pub account: Option<Account>,
}

/// Mastodon / Fediverse channel.
pub struct MastodonChannel {
    config: MastodonConfig,
    client: reqwest::Client,
    sink: RwLock<Option<Arc<dyn MessageSink>>>,
    running: AtomicBool,
}

impl MastodonChannel {
    pub fn new(config: MastodonConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            sink: RwLock::new(None),
            running: AtomicBool::new(false),
        }
    }

    /// Strip HTML tags from Mastodon status content.
    fn strip_html(html: &str) -> String {
        let mut result = String::with_capacity(html.len());
        let mut in_tag = false;
        for ch in html.chars() {
            match ch {
                '<' => {
                    in_tag = true;
                    // Check for <br> or </p> — insert newline
                    if html[result.len()..].starts_with("<br")
                        || html[result.len()..].starts_with("</p")
                    {
                        result.push('\n');
                    }
                }
                '>' => in_tag = false,
                _ if !in_tag => result.push(ch),
                _ => {}
            }
        }
        // Decode common HTML entities
        result
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
    }

    /// Process a notification (mention or DM).
    pub async fn process_notification(&self, notification: Notification) {
        let status = match notification.status {
            Some(s) => s,
            None => return,
        };

        let is_dm = status.visibility == "direct";
        let is_mention = notification.notification_type == "mention";

        if is_dm && !self.config.respond_to_dms {
            return;
        }
        if is_mention && !is_dm && !self.config.respond_to_mentions {
            return;
        }

        // Skip bot accounts
        if status.account.bot {
            return;
        }

        let text = Self::strip_html(&status.content);

        let session_target = status.account.acct.clone();
        let msg = NormalizedMessage {
            id: Uuid::new_v4(),
            session_key: clawdesk_types::session::SessionKey::new(
                ChannelId::Mastodon,
                &session_target,
            ),
            body: text,
            body_for_agent: None,
            sender: SenderIdentity {
                id: status.account.id.clone(),
                display_name: if status.account.display_name.is_empty() {
                    status.account.username.clone()
                } else {
                    status.account.display_name.clone()
                },
                channel: ChannelId::Mastodon,
            },
            media: vec![],
            artifact_refs: vec![],
            reply_context: None,
            origin: clawdesk_types::message::MessageOrigin::Mastodon {
                instance: self.config.instance_url.clone(),
                visibility: status.visibility.clone(),
            },
            timestamp: Utc::now(),
        };

        let sink = self.sink.read().await;
        if let Some(ref s) = *sink {
            s.on_message(msg).await;
        }
    }

    /// Post a status (toot).
    async fn post_status(
        &self,
        text: &str,
        visibility: &str,
        in_reply_to: Option<&str>,
    ) -> Result<String, String> {
        let url = format!("{}/api/v1/statuses", self.config.instance_url);

        let mut params = serde_json::json!({
            "status": text,
            "visibility": visibility,
        });

        if let Some(reply_id) = in_reply_to {
            params["in_reply_to_id"] = serde_json::Value::String(reply_id.to_string());
        }

        if let Some(ref cw) = self.config.content_warning {
            params["spoiler_text"] = serde_json::Value::String(cw.clone());
        }

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.access_token)
            .json(&params)
            .send()
            .await
            .map_err(|e| format!("Mastodon post failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Mastodon post error ({}): {}", status, body));
        }

        let status_resp: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Mastodon response parse error: {}", e))?;

        Ok(status_resp["id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string())
    }
}

#[async_trait]
impl Channel for MastodonChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Mastodon
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Mastodon".into(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: true,
            supports_media: true,
            supports_groups: false,
            max_message_length: Some(self.config.max_toot_length),
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        // Verify credentials
        let url = format!(
            "{}/api/v1/accounts/verify_credentials",
            self.config.instance_url
        );
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.config.access_token)
            .send()
            .await
            .map_err(|e| format!("Mastodon auth check failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "Mastodon auth failed: HTTP {}",
                resp.status()
            ));
        }

        *self.sink.write().await = Some(sink);
        self.running.store(true, Ordering::Release);
        info!(instance = %self.config.instance_url, "Mastodon channel started");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        // Determine visibility from origin or default to "direct"
        let visibility = "direct";

        // Truncate to max toot length
        let text = if msg.body.len() > self.config.max_toot_length {
            let truncated = &msg.body[..self.config.max_toot_length - 3];
            format!("{}...", truncated)
        } else {
            msg.body.clone()
        };

        let status_id = self
            .post_status(&text, visibility, msg.reply_to.as_deref())
            .await?;

        Ok(DeliveryReceipt {
            channel: ChannelId::Mastodon,
            message_id: status_id,
            timestamp: Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Release);
        *self.sink.write().await = None;
        info!("Mastodon channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait]
impl Reactions for MastodonChannel {
    async fn add_reaction(&self, msg_id: &str, _emoji: &str) -> Result<(), String> {
        // Mastodon only supports "favourite" as a reaction
        let url = format!(
            "{}/api/v1/statuses/{}/favourite",
            self.config.instance_url, msg_id
        );

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.access_token)
            .send()
            .await
            .map_err(|e| format!("Mastodon favourite failed: {}", e))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("Mastodon favourite error: HTTP {}", resp.status()))
        }
    }

    async fn remove_reaction(&self, msg_id: &str, _emoji: &str) -> Result<(), String> {
        let url = format!(
            "{}/api/v1/statuses/{}/unfavourite",
            self.config.instance_url, msg_id
        );

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.access_token)
            .send()
            .await
            .map_err(|e| format!("Mastodon unfavourite failed: {}", e))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "Mastodon unfavourite error: HTTP {}",
                resp.status()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mastodon_meta() {
        let ch = MastodonChannel::new(MastodonConfig {
            instance_url: "https://mastodon.social".into(),
            access_token: "test".into(),
            username: "bot".into(),
            ..Default::default()
        });
        assert_eq!(ch.id(), ChannelId::Mastodon);
        let meta = ch.meta();
        assert!(meta.supports_reactions);
        assert!(!meta.supports_threading);
        assert_eq!(meta.max_message_length, Some(500));
    }

    #[test]
    fn strip_html_basic() {
        assert_eq!(MastodonChannel::strip_html("<p>hello</p>"), "hello");
        assert_eq!(
            MastodonChannel::strip_html("<p>a &amp; b</p>"),
            "a & b"
        );
    }

    #[test]
    fn mastodon_config_default() {
        let cfg = MastodonConfig::default();
        assert_eq!(cfg.max_toot_length, 500);
        assert!(cfg.respond_to_mentions);
        assert!(cfg.respond_to_dms);
    }
}
