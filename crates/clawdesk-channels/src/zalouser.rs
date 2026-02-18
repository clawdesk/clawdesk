//! ZaloUser channel — personal Zalo account messaging via `zca` CLI bridge.
//!
//! Unlike the `ZaloChannel` (Official Account API), this adapter connects
//! through a personal Zalo account using the `zca` CLI tool for direct
//! and group messaging. This matches OpenClaw's `zalouser` extension.
//!
//! ## Architecture
//!
//! ```text
//! ZaloUserChannel
//! ├── poll_loop()     — runs `zca listen` in a subprocess, reads NDJSON events
//! ├── normalize()     — zca event → NormalizedMessage
//! ├── send()          — OutboundMessage → `zca send` subprocess
//! ├── send_media()    — `zca send --file <path>` subprocess
//! └── list_contacts() — `zca contacts` for address book
//! ```
//!
//! ## `zca` CLI
//!
//! The [zca](https://github.com/nickenilsson/zca) CLI is a Node.js tool
//! that bridges personal Zalo accounts via the internal Zalo web protocol.
//!
//! - `zca listen [--profile <name>]`     — stream events as NDJSON
//! - `zca send <user-id> <text> [--profile <name>]` — send text message
//! - `zca send <user-id> --file <path> [--profile <name>]` — send file
//! - `zca login [--profile <name>]`      — interactive QR login
//! - `zca contacts [--profile <name>]`   — list contacts
//!
//! ## Event format (NDJSON from `zca listen`)
//!
//! ```json
//! {"type":"message","from":"1234567","text":"hello","msgId":"abc123","ts":1700000000}
//! {"type":"group_message","from":"1234567","groupId":"g567","text":"hi","msgId":"def456","ts":1700000001}
//! {"type":"reaction","from":"1234567","msgId":"abc123","emoji":"❤️","ts":1700000002}
//! ```
//!
//! ## Capabilities
//!
//! Direct messages, group messages, media attachments, reactions (receive-only).
//! 2000 character message limit (Zalo platform restriction).

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, StreamHandle, Streaming};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

/// Maximum message length on Zalo (platform restriction).
const MAX_MESSAGE_LENGTH: usize = 2000;

// ─── Configuration ──────────────────────────────────────────────────

/// Configuration for the ZaloUser channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZaloUserConfig {
    /// Profile name for `zca` CLI multi-account support (default: "default").
    #[serde(default = "default_profile")]
    pub profile_name: String,

    /// Path to the `zca` CLI binary (default: "zca", resolved from PATH).
    #[serde(default = "default_zca_bin")]
    pub zca_binary: String,

    /// Whether to handle group messages (default: true).
    #[serde(default = "default_true")]
    pub enable_groups: bool,

    /// Allowed user IDs for direct messages. Empty = allow all.
    #[serde(default)]
    pub allowed_users: Vec<String>,

    /// Allowed group IDs. Empty = allow all groups.
    #[serde(default)]
    pub allowed_groups: Vec<String>,
}

fn default_profile() -> String {
    "default".into()
}

fn default_zca_bin() -> String {
    "zca".into()
}

fn default_true() -> bool {
    true
}

// ─── Channel struct ─────────────────────────────────────────────────

/// ZaloUser channel — personal Zalo account via `zca` CLI.
pub struct ZaloUserChannel {
    config: ZaloUserConfig,
    running: AtomicBool,
    shutdown: Notify,
}

impl ZaloUserChannel {
    pub fn new(config: ZaloUserConfig) -> Self {
        Self {
            config,
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Check if a user ID is allowed for direct messages.
    fn is_allowed_user(&self, user_id: &str) -> bool {
        self.config.allowed_users.is_empty()
            || self.config.allowed_users.iter().any(|u| u == user_id)
    }

    /// Check if a group ID is allowed.
    fn is_allowed_group(&self, group_id: &str) -> bool {
        self.config.allowed_groups.is_empty()
            || self.config.allowed_groups.iter().any(|g| g == group_id)
    }

    /// Build `zca` command with profile flag.
    fn zca_command(&self) -> Command {
        let mut cmd = Command::new(&self.config.zca_binary);
        cmd.arg("--profile").arg(&self.config.profile_name);
        cmd
    }

    /// Listener loop: spawns `zca listen` and reads NDJSON events.
    async fn listen_loop(self: Arc<Self>, sink: Arc<dyn MessageSink>) {
        info!(
            profile = %self.config.profile_name,
            "ZaloUser listen loop started"
        );

        while self.running.load(Ordering::Relaxed) {
            match self.spawn_listener().await {
                Ok(mut child) => {
                    let stdout = match child.stdout.take() {
                        Some(s) => s,
                        None => {
                            error!("zca listen: no stdout");
                            break;
                        }
                    };

                    let reader = BufReader::new(stdout);
                    let mut lines = reader.lines();

                    loop {
                        if !self.running.load(Ordering::Relaxed) {
                            let _ = child.kill().await;
                            break;
                        }

                        tokio::select! {
                            line = lines.next_line() => {
                                match line {
                                    Ok(Some(json_line)) => {
                                        if let Some(msg) = self.parse_event(&json_line) {
                                            sink.on_message(msg).await;
                                        }
                                    }
                                    Ok(None) => {
                                        warn!("zca listen exited, restarting...");
                                        break;
                                    }
                                    Err(e) => {
                                        warn!(error = %e, "zca listen read error");
                                        break;
                                    }
                                }
                            }
                            _ = self.shutdown.notified() => {
                                let _ = child.kill().await;
                                return;
                            }
                        }
                    }

                    // Small backoff before restarting
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
                Err(e) => {
                    error!(error = %e, "failed to spawn zca listen");
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
            }
        }

        info!("ZaloUser listen loop stopped");
    }

    /// Spawn `zca listen` subprocess.
    async fn spawn_listener(&self) -> Result<tokio::process::Child, String> {
        let mut cmd = self.zca_command();
        cmd.arg("listen")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        cmd.spawn()
            .map_err(|e| format!("failed to spawn zca listen: {}", e))
    }

    /// Parse a single NDJSON event line from `zca listen`.
    fn parse_event(&self, json_line: &str) -> Option<NormalizedMessage> {
        let event: ZcaEvent = serde_json::from_str(json_line)
            .map_err(|e| {
                debug!(line = %json_line, error = %e, "skipping unparseable zca event");
                e
            })
            .ok()?;

        match event.event_type.as_str() {
            "message" => self.normalize_direct_message(&event),
            "group_message" if self.config.enable_groups => {
                self.normalize_group_message(&event)
            }
            _ => {
                debug!(event_type = %event.event_type, "ignoring zca event type");
                None
            }
        }
    }

    /// Normalize a direct message event.
    fn normalize_direct_message(&self, event: &ZcaEvent) -> Option<NormalizedMessage> {
        let from = event.from.as_deref()?;
        if !self.is_allowed_user(from) {
            return None;
        }

        let text = event.text.as_deref()?;
        let msg_id = event.msg_id.as_deref().unwrap_or("unknown");

        let sender = SenderIdentity {
            id: from.to_string(),
            display_name: event
                .sender_name
                .clone()
                .unwrap_or_else(|| from.to_string()),
            channel: ChannelId::ZaloUser,
        };

        let session_key =
            clawdesk_types::session::SessionKey::new(ChannelId::ZaloUser, from);

        let origin = clawdesk_types::message::MessageOrigin::ZaloUser {
            user_id: from.to_string(),
            message_id: msg_id.to_string(),
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

    /// Normalize a group message event.
    fn normalize_group_message(&self, event: &ZcaEvent) -> Option<NormalizedMessage> {
        let from = event.from.as_deref()?;
        let group_id = event.group_id.as_deref()?;

        if !self.is_allowed_group(group_id) {
            return None;
        }

        let text = event.text.as_deref()?;
        let msg_id = event.msg_id.as_deref().unwrap_or("unknown");

        let session_key = clawdesk_types::session::SessionKey::new(
            ChannelId::ZaloUser,
            &format!("group:{}", group_id),
        );

        let sender = SenderIdentity {
            id: from.to_string(),
            display_name: event
                .sender_name
                .clone()
                .unwrap_or_else(|| from.to_string()),
            channel: ChannelId::ZaloUser,
        };

        let origin = clawdesk_types::message::MessageOrigin::ZaloUser {
            user_id: from.to_string(),
            message_id: msg_id.to_string(),
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

    /// Send a text message via `zca send`.
    async fn send_text(
        &self,
        target_id: &str,
        text: &str,
    ) -> Result<String, String> {
        // Enforce message limit
        let text = if text.len() > MAX_MESSAGE_LENGTH {
            &text[..MAX_MESSAGE_LENGTH]
        } else {
            text
        };

        let output = self
            .zca_command()
            .arg("send")
            .arg(target_id)
            .arg(text)
            .output()
            .await
            .map_err(|e| format!("zca send failed: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("zca send error: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        debug!(target = %target_id, "zca send success: {}", stdout.trim());

        // Extract message ID from stdout if available
        let msg_id = stdout
            .trim()
            .lines()
            .last()
            .unwrap_or("sent")
            .to_string();

        Ok(msg_id)
    }

    /// Send a file via `zca send --file`.
    pub async fn send_file(
        &self,
        target_id: &str,
        file_path: &str,
    ) -> Result<String, String> {
        let output = self
            .zca_command()
            .arg("send")
            .arg(target_id)
            .arg("--file")
            .arg(file_path)
            .output()
            .await
            .map_err(|e| format!("zca send file failed: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("zca send file error: {}", stderr));
        }

        Ok("file_sent".to_string())
    }

    /// List contacts via `zca contacts`.
    pub async fn list_contacts(&self) -> Result<Vec<ZcaContact>, String> {
        let output = self
            .zca_command()
            .arg("contacts")
            .output()
            .await
            .map_err(|e| format!("zca contacts failed: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("zca contacts error: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let contacts: Vec<ZcaContact> = serde_json::from_str(&stdout)
            .map_err(|e| format!("parse contacts: {}", e))?;

        Ok(contacts)
    }

    /// Check if `zca` CLI is available.
    pub async fn probe(&self) -> Result<(), String> {
        let output = self
            .zca_command()
            .arg("--version")
            .output()
            .await
            .map_err(|e| format!("zca not found: {}", e))?;

        if !output.status.success() {
            return Err("zca --version failed".into());
        }

        let version = String::from_utf8_lossy(&output.stdout);
        info!(version = %version.trim(), "zca CLI available");
        Ok(())
    }
}

// ─── Channel trait impl ─────────────────────────────────────────────

#[async_trait]
impl Channel for ZaloUserChannel {
    fn id(&self) -> ChannelId {
        ChannelId::ZaloUser
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Zalo (Personal)".into(),
            supports_threading: false,
            supports_streaming: true,
            supports_reactions: false, // Reactions are receive-only
            supports_media: true,
            supports_groups: self.config.enable_groups,
            max_message_length: Some(MAX_MESSAGE_LENGTH),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        // Verify zca is available
        self.probe().await?;

        self.running.store(true, Ordering::Relaxed);
        info!(
            profile = %self.config.profile_name,
            groups = self.config.enable_groups,
            "ZaloUser channel started"
        );
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let target_id = match &msg.origin {
            clawdesk_types::message::MessageOrigin::ZaloUser { user_id, .. } => {
                user_id.clone()
            }
            _ => {
                return Err("ZaloUser send requires ZaloUser origin".into());
            }
        };

        // Send media first
        for attachment in &msg.media {
            if let Some(url) = &attachment.url {
                self.send_file(&target_id, url).await?;
            }
        }

        // Send text
        if !msg.body.is_empty() {
            let msg_id = self.send_text(&target_id, &msg.body).await?;
            return Ok(DeliveryReceipt {
                channel: ChannelId::ZaloUser,
                message_id: msg_id,
                timestamp: chrono::Utc::now(),
                success: true,
                error: None,
            });
        }

        Ok(DeliveryReceipt {
            channel: ChannelId::ZaloUser,
            message_id: String::new(),
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("ZaloUser channel stopped");
        Ok(())
    }
}

#[async_trait]
impl Streaming for ZaloUserChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        // Zalo doesn't support in-place edit streaming.
        // Send full message and return handle.
        let receipt = self.send(initial).await?;
        Ok(StreamHandle {
            message_id: receipt.message_id,
            update_fn: Box::new(|_text| Ok(())),
        })
    }
}

// ─── zca event types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ZcaEvent {
    #[serde(rename = "type")]
    event_type: String,
    from: Option<String>,
    text: Option<String>,
    #[serde(rename = "msgId")]
    msg_id: Option<String>,
    #[serde(rename = "groupId")]
    group_id: Option<String>,
    #[serde(rename = "senderName")]
    sender_name: Option<String>,
    #[allow(dead_code)]
    ts: Option<i64>,
}

/// Contact returned by `zca contacts`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ZcaContact {
    pub id: String,
    pub name: Option<String>,
    pub phone: Option<String>,
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ZaloUserConfig {
        ZaloUserConfig {
            profile_name: "test-profile".into(),
            zca_binary: "zca".into(),
            enable_groups: true,
            allowed_users: vec!["user123".into()],
            allowed_groups: vec![],
        }
    }

    #[test]
    fn test_channel_creation() {
        let ch = ZaloUserChannel::new(test_config());
        assert_eq!(ch.id(), ChannelId::ZaloUser);
    }

    #[test]
    fn test_meta_capabilities() {
        let ch = ZaloUserChannel::new(test_config());
        let meta = ch.meta();
        assert_eq!(meta.display_name, "Zalo (Personal)");
        assert!(!meta.supports_reactions);
        assert!(meta.supports_media);
        assert!(meta.supports_groups);
        assert!(meta.supports_streaming);
        assert!(!meta.supports_threading);
        assert_eq!(meta.max_message_length, Some(2000));
    }

    #[test]
    fn test_allowed_user_filter() {
        let ch = ZaloUserChannel::new(test_config());
        assert!(ch.is_allowed_user("user123"));
        assert!(!ch.is_allowed_user("user999"));

        // Empty = allow all
        let mut config = test_config();
        config.allowed_users = vec![];
        let open = ZaloUserChannel::new(config);
        assert!(open.is_allowed_user("anyone"));
    }

    #[test]
    fn test_allowed_group_filter() {
        let ch = ZaloUserChannel::new(test_config());
        // Empty = allow all
        assert!(ch.is_allowed_group("any_group"));

        let mut config = test_config();
        config.allowed_groups = vec!["allowed_group".into()];
        let restricted = ZaloUserChannel::new(config);
        assert!(restricted.is_allowed_group("allowed_group"));
        assert!(!restricted.is_allowed_group("other_group"));
    }

    #[test]
    fn test_groups_disabled_meta() {
        let mut config = test_config();
        config.enable_groups = false;
        let ch = ZaloUserChannel::new(config);
        assert!(!ch.meta().supports_groups);
    }

    #[test]
    fn test_parse_direct_message() {
        let ch = ZaloUserChannel::new(test_config());
        let json = r#"{"type":"message","from":"user123","text":"hello","msgId":"m001","ts":1700000000}"#;
        let msg = ch.parse_event(json);
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert_eq!(msg.body, "hello");
        assert_eq!(msg.sender.id, "user123");
    }

    #[test]
    fn test_parse_direct_message_filtered() {
        let ch = ZaloUserChannel::new(test_config());
        let json = r#"{"type":"message","from":"unknown_user","text":"hello","msgId":"m002","ts":1700000001}"#;
        assert!(ch.parse_event(json).is_none());
    }

    #[test]
    fn test_parse_group_message() {
        let ch = ZaloUserChannel::new(test_config());
        let json = r#"{"type":"group_message","from":"user123","groupId":"g001","text":"group hello","msgId":"m003","ts":1700000002}"#;
        let msg = ch.parse_event(json);
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert_eq!(msg.body, "group hello");
        assert!(msg.session_key.to_string().contains("group:"));
    }

    #[test]
    fn test_parse_group_disabled() {
        let mut config = test_config();
        config.enable_groups = false;
        let ch = ZaloUserChannel::new(config);
        let json = r#"{"type":"group_message","from":"user123","groupId":"g001","text":"hi","msgId":"m004","ts":1700000003}"#;
        assert!(ch.parse_event(json).is_none());
    }

    #[test]
    fn test_parse_unknown_event() {
        let ch = ZaloUserChannel::new(test_config());
        let json = r#"{"type":"reaction","from":"user123","msgId":"m001","emoji":"❤️","ts":1700000004}"#;
        assert!(ch.parse_event(json).is_none());
    }

    #[test]
    fn test_parse_malformed_event() {
        let ch = ZaloUserChannel::new(test_config());
        assert!(ch.parse_event("not json").is_none());
    }

    #[test]
    fn test_config_defaults() {
        let config: ZaloUserConfig =
            serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(config.profile_name, "default");
        assert_eq!(config.zca_binary, "zca");
        assert!(config.enable_groups);
        assert!(config.allowed_users.is_empty());
        assert!(config.allowed_groups.is_empty());
    }
}
