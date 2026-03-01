//! Session types — key, state, lifecycle, and summaries.

use crate::channel::ChannelId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique key identifying a conversation session.
///
/// Format: `{channel}:{identifier}` — e.g., `telegram:12345` or `discord:guild:channel`.
///
/// Internally stores channel + identifier as separate fields to avoid
/// `format!()` allocation on construction. `Display`, `Serialize`, and
/// `Hash`/`Eq` compose the canonical string lazily without allocating
/// during equality checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionKey {
    channel: ChannelId,
    identifier: CompactId,
}

/// Stack-allocated identifier for most session IDs (≤ 63 bytes).
/// Falls back to heap `String` for longer IDs.
#[derive(Debug, Clone)]
enum CompactId {
    /// Inline storage: [len, bytes[0..63]]
    Inline { len: u8, buf: [u8; 63] },
    /// Heap fallback for identifiers > 63 bytes.
    Heap(String),
}

impl CompactId {
    fn new(s: &str) -> Self {
        if s.len() <= 63 {
            let mut buf = [0u8; 63];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            Self::Inline {
                len: s.len() as u8,
                buf,
            }
        } else {
            Self::Heap(s.to_string())
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Self::Inline { len, buf } => {
                // Invariant: new() only stores bytes from validated &str,
                // so this will never fail.  Safe alternative to from_utf8_unchecked.
                std::str::from_utf8(&buf[..*len as usize])
                    .expect("CompactId::Inline contains invalid UTF-8 (bug in CompactId::new)")
            }
            Self::Heap(s) => s.as_str(),
        }
    }
}

impl Serialize for CompactId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CompactId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::new(&s))
    }
}

impl PartialEq for SessionKey {
    fn eq(&self, other: &Self) -> bool {
        self.channel == other.channel && self.identifier.as_str() == other.identifier.as_str()
    }
}

impl Eq for SessionKey {}

impl std::hash::Hash for SessionKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.channel.hash(state);
        self.identifier.as_str().hash(state);
    }
}

impl SessionKey {
    pub fn new(channel: ChannelId, identifier: &str) -> Self {
        Self {
            channel,
            identifier: CompactId::new(identifier),
        }
    }

    /// The canonical `channel:identifier` string. Allocates on each call.
    /// Prefer `Display` formatting, `channel()`, or `identifier()` where possible.
    pub fn as_str(&self) -> String {
        format!("{}:{}", self.channel, self.identifier.as_str())
    }

    /// The canonical string as an owned value (same as as_str, explicit name).
    pub fn to_canonical(&self) -> String {
        self.as_str()
    }

    /// Access the channel component without allocation.
    pub fn channel(&self) -> ChannelId {
        self.channel
    }

    /// Access the identifier component without allocation.
    pub fn identifier(&self) -> &str {
        self.identifier.as_str()
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.channel, self.identifier.as_str())
    }
}

impl From<String> for SessionKey {
    fn from(s: String) -> Self {
        // Parse "channel:identifier" format
        if let Some(colon) = s.find(':') {
            let channel_str = &s[..colon];
            let identifier = &s[colon + 1..];
            // Try to parse the channel
            let channel = match channel_str {
                "telegram" => ChannelId::Telegram,
                "discord" => ChannelId::Discord,
                "slack" => ChannelId::Slack,
                "whatsapp" => ChannelId::WhatsApp,
                "webchat" | "web" => ChannelId::WebChat,
                "email" => ChannelId::Email,
                "imessage" => ChannelId::IMessage,
                "irc" => ChannelId::Irc,
                "internal" | "cli" => ChannelId::Internal,
                _ => {
                    // Unknown channel — store whole string as identifier under WebChat
                    return Self {
                        channel: ChannelId::WebChat,
                        identifier: CompactId::new(&s),
                    };
                }
            };
            Self::new(channel, identifier)
        } else {
            // No colon — use as identifier under WebChat
            Self {
                channel: ChannelId::WebChat,
                identifier: CompactId::new(&s),
            }
        }
    }
}

/// Agent message role in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
    ToolResult,
}

/// A single message in a conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub role: Role,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub model: Option<String>,
    pub token_count: Option<usize>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
}

/// States of a session's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Session is active and processing messages.
    Active,
    /// Session is idle (no recent activity).
    Idle,
    /// Session is paused by user.
    Paused,
    /// Session has been archived.
    Archived,
}

impl Default for SessionState {
    fn default() -> Self {
        Self::Active
    }
}

/// A conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub key: SessionKey,
    pub state: SessionState,
    pub channel: ChannelId,
    pub system_prompt: String,
    pub model: Option<String>,
    pub history_limit: usize,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_count: u64,
    pub metadata: serde_json::Value,
}

impl Session {
    pub fn new(key: SessionKey, channel: ChannelId) -> Self {
        let now = Utc::now();
        Self {
            key,
            state: SessionState::Active,
            channel,
            system_prompt: String::new(),
            model: None,
            history_limit: 50,
            created_at: now,
            last_activity: now,
            message_count: 0,
            metadata: serde_json::json!({}),
        }
    }
}

/// Lightweight session summary for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub key: SessionKey,
    pub channel: ChannelId,
    pub state: SessionState,
    pub last_activity: DateTime<Utc>,
    pub message_count: u64,
    pub model: Option<String>,
}

/// Filter for listing sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionFilter {
    pub channel: Option<ChannelId>,
    pub state: Option<SessionState>,
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
}

/// Session configuration defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub default_system_prompt: String,
    pub default_model: Option<String>,
    pub default_history_limit: usize,
    pub idle_timeout_seconds: u64,
    pub max_sessions: Option<usize>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            default_system_prompt: "You are a helpful assistant.".to_string(),
            default_model: None,
            default_history_limit: 50,
            idle_timeout_seconds: 3600,
            max_sessions: None,
        }
    }
}
