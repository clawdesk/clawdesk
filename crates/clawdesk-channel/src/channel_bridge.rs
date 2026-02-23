//! Channel ID bijection — lossless mapping between ClawDesk's `ChannelId`
//! enum and string-based channel identifiers.
//!
//! ## Channel Abstraction Layer (P3)
//!
//! ClawDesk uses a compiler-enforced `ChannelId` enum (24 variants).
//! The gateway uses plain string identifiers (`"telegram"`, `"discord"`, etc.).
//! This module provides a type-safe bijection between the two representations
//! with a registry for custom/unknown channels.
//!
//! ## Bijection properties
//!
//! - `from_string(to_string(x)) == x` ∀ known x
//! - `to_string(from_string(s)) == s` ∀ canonical strings
//! - Unknown strings map to `ChannelMapping::Unknown` with the original preserved

use clawdesk_types::channel::ChannelId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Result of mapping a string to a channel ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelMapping {
    /// Successfully mapped to a known ClawDesk ChannelId variant.
    Known(ChannelId),
    /// Unknown channel string — not in ClawDesk's enum.
    /// Preserved for round-trip fidelity when bridging to the gateway.
    Unknown(String),
}

/// Bidirectional channel mapper between ClawDesk enum and gateway strings.
pub struct ChannelBridge {
    /// Custom aliases: map non-canonical strings to ChannelId.
    aliases: HashMap<String, ChannelId>,
}

impl ChannelBridge {
    /// Create a new bridge with no custom aliases.
    pub fn new() -> Self {
        Self {
            aliases: HashMap::new(),
        }
    }

    /// Register a custom alias (e.g., "tg" → Telegram).
    pub fn register_alias(&mut self, alias: impl Into<String>, id: ChannelId) {
        self.aliases.insert(alias.into(), id);
    }

    /// Map a ClawDesk `ChannelId` to a gateway string identifier.
    ///
    /// This is the canonical string representation used by the gateway.
    pub fn to_string(id: ChannelId) -> &'static str {
        match id {
            ChannelId::Telegram => "telegram",
            ChannelId::Discord => "discord",
            ChannelId::Slack => "slack",
            ChannelId::WhatsApp => "whatsapp",
            ChannelId::WebChat => "webchat",
            ChannelId::Email => "email",
            ChannelId::IMessage => "imessage",
            ChannelId::Irc => "irc",
            ChannelId::Internal => "internal",
        }
    }

    /// Map a gateway string identifier to a ClawDesk `ChannelId`.
    ///
    /// Checks canonical names first, then custom aliases.
    /// Returns `ChannelMapping::Unknown` for unrecognized strings.
    pub fn from_string(&self, s: &str) -> ChannelMapping {
        let lower = s.to_lowercase();
        let known = match lower.as_str() {
            "telegram" => Some(ChannelId::Telegram),
            "discord" => Some(ChannelId::Discord),
            "slack" => Some(ChannelId::Slack),
            "whatsapp" => Some(ChannelId::WhatsApp),
            "webchat" => Some(ChannelId::WebChat),
            "email" => Some(ChannelId::Email),
            "imessage" => Some(ChannelId::IMessage),
            "irc" => Some(ChannelId::Irc),
            "internal" => Some(ChannelId::Internal),
            _ => None,
        };

        if let Some(id) = known {
            return ChannelMapping::Known(id);
        }

        // Check custom aliases
        if let Some(id) = self.aliases.get(&lower) {
            return ChannelMapping::Known(*id);
        }

        ChannelMapping::Unknown(s.to_string())
    }

    /// Check if a string maps to a known channel.
    pub fn is_known(&self, s: &str) -> bool {
        matches!(self.from_string(s), ChannelMapping::Known(_))
    }

    /// List all canonical channel strings.
    pub fn all_canonical() -> Vec<&'static str> {
        vec![
            "telegram", "discord", "slack", "whatsapp",
            "webchat", "email", "internal",
        ]
    }
}

impl Default for ChannelBridge {
    fn default() -> Self {
        Self::new()
    }
}

/// Capability metadata for cross-system channel feature negotiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelCapabilities {
    pub channel: String,
    pub supports_threading: bool,
    pub supports_streaming: bool,
    pub supports_reactions: bool,
    pub supports_media: bool,
    pub max_message_length: Option<usize>,
}

impl ChannelCapabilities {
    /// Create capabilities from a `ChannelId` with sensible defaults.
    pub fn from_channel_id(id: ChannelId) -> Self {
        let name = ChannelBridge::to_string(id).to_string();
        match id {
            ChannelId::Slack | ChannelId::Discord => Self {
                channel: name,
                supports_threading: true,
                supports_streaming: true,
                supports_reactions: true,
                supports_media: true,
                max_message_length: Some(4000),
            },
            ChannelId::Telegram => Self {
                channel: name,
                supports_threading: true,
                supports_streaming: false,
                supports_reactions: true,
                supports_media: true,
                max_message_length: Some(4096),
            },
            ChannelId::WebChat => Self {
                channel: name,
                supports_threading: false,
                supports_streaming: true,
                supports_reactions: false,
                supports_media: true,
                max_message_length: None,
            },
            ChannelId::Email => Self {
                channel: name,
                supports_threading: true,
                supports_streaming: false,
                supports_reactions: false,
                supports_media: true,
                max_message_length: None,
            },
            _ => Self {
                channel: name,
                supports_threading: false,
                supports_streaming: false,
                supports_reactions: false,
                supports_media: false,
                max_message_length: None,
            },
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_known_channels() {
        let bridge = ChannelBridge::new();
        let all_ids = [
            ChannelId::Telegram,
            ChannelId::Discord,
            ChannelId::Slack,
            ChannelId::WhatsApp,
            ChannelId::WebChat,
            ChannelId::Email,
            ChannelId::Internal,
        ];

        for id in &all_ids {
            let s = ChannelBridge::to_string(*id);
            let mapping = bridge.from_string(s);
            assert_eq!(
                mapping,
                ChannelMapping::Known(*id),
                "roundtrip failed for {:?} → {:?}",
                id,
                s
            );
        }
    }

    #[test]
    fn unknown_channel_preserved() {
        let bridge = ChannelBridge::new();
        let mapping = bridge.from_string("my_custom_channel");
        assert_eq!(
            mapping,
            ChannelMapping::Unknown("my_custom_channel".to_string())
        );
    }

    #[test]
    fn alias_mapping() {
        let mut bridge = ChannelBridge::new();
        bridge.register_alias("tg", ChannelId::Telegram);
        bridge.register_alias("dc", ChannelId::Discord);

        assert_eq!(
            bridge.from_string("tg"),
            ChannelMapping::Known(ChannelId::Telegram)
        );
        assert_eq!(
            bridge.from_string("dc"),
            ChannelMapping::Known(ChannelId::Discord)
        );
    }

    #[test]
    fn case_insensitive_lookup() {
        let bridge = ChannelBridge::new();
        assert_eq!(
            bridge.from_string("TELEGRAM"),
            ChannelMapping::Known(ChannelId::Telegram)
        );
        assert_eq!(
            bridge.from_string("Discord"),
            ChannelMapping::Known(ChannelId::Discord)
        );
    }

    #[test]
    fn capabilities_from_channel_id() {
        let slack = ChannelCapabilities::from_channel_id(ChannelId::Slack);
        assert!(slack.supports_threading);
        assert!(slack.supports_streaming);
        assert!(slack.supports_reactions);

        let email = ChannelCapabilities::from_channel_id(ChannelId::Email);
        assert!(email.supports_threading);
        assert!(!email.supports_streaming);
        assert!(!email.supports_reactions);
    }
}
