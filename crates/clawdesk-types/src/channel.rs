//! Channel identifiers and metadata.
//!
//! Each messaging platform is identified by a `ChannelId` enum variant.
//! The compiler enforces exhaustive matching whenever a new channel is added.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique identifier for each messaging channel.
///
/// Adding a new variant here forces handling in every `match` statement
/// across the codebase — the compiler finds every callsite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelId {
    Telegram,
    Discord,
    Slack,
    WhatsApp,
    Signal,
    IMessage,
    WebChat,
    Matrix,
    Line,
    GoogleChat,
    MsTeams,
    Nostr,
    Irc,
    Mattermost,
    Email,
    Feishu,
    Twitch,
    NextcloudTalk,
    Zalo,
    /// Zalo personal account via zca CLI (vs official account `Zalo`).
    ZaloUser,
    Tlon,
    /// iMessage via BlueBubbles REST API (full-featured alternative to `IMessage`).
    BlueBubbles,
    /// CLI / gateway internal message
    Internal,
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Telegram => write!(f, "telegram"),
            Self::Discord => write!(f, "discord"),
            Self::Slack => write!(f, "slack"),
            Self::WhatsApp => write!(f, "whatsapp"),
            Self::Signal => write!(f, "signal"),
            Self::IMessage => write!(f, "imessage"),
            Self::WebChat => write!(f, "webchat"),
            Self::Matrix => write!(f, "matrix"),
            Self::Line => write!(f, "line"),
            Self::GoogleChat => write!(f, "googlechat"),
            Self::MsTeams => write!(f, "msteams"),
            Self::Nostr => write!(f, "nostr"),
            Self::Irc => write!(f, "irc"),
            Self::Mattermost => write!(f, "mattermost"),
            Self::Email => write!(f, "email"),
            Self::Feishu => write!(f, "feishu"),
            Self::Twitch => write!(f, "twitch"),
            Self::NextcloudTalk => write!(f, "nextcloud_talk"),
            Self::Zalo => write!(f, "zalo"),
            Self::ZaloUser => write!(f, "zalouser"),
            Self::Tlon => write!(f, "tlon"),
            Self::BlueBubbles => write!(f, "bluebubbles"),
            Self::Internal => write!(f, "internal"),
        }
    }
}

/// Metadata about a channel's capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMeta {
    /// Human-readable display name
    pub display_name: String,
    /// Whether the channel supports threading
    pub supports_threading: bool,
    /// Whether the channel supports streaming (partial message updates)
    pub supports_streaming: bool,
    /// Whether the channel supports reactions
    pub supports_reactions: bool,
    /// Whether the channel supports media attachments
    pub supports_media: bool,
    /// Whether the channel supports group conversations
    pub supports_groups: bool,
    /// Maximum message length (None = unlimited)
    pub max_message_length: Option<usize>,
}

impl ChannelMeta {
    /// Create minimal metadata for a basic text-only channel.
    pub fn basic(display_name: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: false,
            supports_media: false,
            supports_groups: false,
            max_message_length: None,
        }
    }
}
