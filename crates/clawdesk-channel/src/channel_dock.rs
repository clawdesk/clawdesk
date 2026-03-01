//! Channel Dock — lightweight metadata registry decoupled from channel instances.
//!
//! ## Channel Dock Pattern
//!
//! The full `Channel` trait requires a running channel instance (with network
//! connections, auth state, etc.). The `ChannelDock` provides a way to query
//! channel metadata and formatting requirements without instantiating the
//! channel. This is used by the agent runner to inject channel context into
//! prompts without requiring a running channel.
//!
//! ```text
//! ChannelDock (metadata only)     ChannelRegistry (full instances)
//! ┌──────────────────────┐        ┌──────────────────────────┐
//! │ telegram: DockEntry  │        │ telegram: Arc<dyn Channel>│
//! │ discord:  DockEntry  │        │ discord:  Arc<dyn Channel>│
//! │ slack:    DockEntry  │        │ slack:    Arc<dyn Channel>│
//! └──────────────────────┘        └──────────────────────────┘
//!       ↓ (no I/O)                      ↓ (requires connection)
//!   Agent prompt injection           Message delivery
//! ```

use clawdesk_types::channel::ChannelId;
use crate::channel_bridge::ChannelCapabilities;
use crate::reply_formatter::MarkupFormat;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

/// A dock entry for a single channel — capabilities + format metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockEntry {
    /// Channel identifier.
    pub channel_id: ChannelId,
    /// Channel capabilities (threading, streaming, reactions, media, message length).
    pub capabilities: ChannelCapabilities,
    /// Preferred markup format for this channel.
    pub markup_format: MarkupFormat,
    /// Whether this channel is currently active/connected.
    pub is_active: bool,
}

impl DockEntry {
    /// Create a dock entry from a channel ID with sensible defaults.
    pub fn from_channel_id(id: ChannelId) -> Self {
        let capabilities = ChannelCapabilities::from_channel_id(id);
        let markup_format = Self::default_markup_format(id);
        Self {
            channel_id: id,
            capabilities,
            markup_format,
            is_active: false,
        }
    }

    /// Default markup format for a channel.
    fn default_markup_format(id: ChannelId) -> MarkupFormat {
        match id {
            ChannelId::Slack => MarkupFormat::SlackMrkdwn,
            ChannelId::Telegram => MarkupFormat::TelegramMarkdownV2,
            ChannelId::WhatsApp => MarkupFormat::PlainText,
            ChannelId::Email => MarkupFormat::Html,
            ChannelId::Discord | ChannelId::WebChat => MarkupFormat::Markdown,
            _ => MarkupFormat::PlainText,
        }
    }
}

/// The Channel Dock — lightweight metadata registry.
///
/// Thread-safe: can be shared across agent runners via `Arc<ChannelDock>`.
/// No I/O, no network connections — purely in-memory metadata lookup.
pub struct ChannelDock {
    entries: HashMap<ChannelId, DockEntry>,
}

impl ChannelDock {
    /// Create an empty dock.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Create a dock pre-populated with default entries for all known channels.
    pub fn with_all_defaults() -> Self {
        let mut dock = Self::new();
        let all_channels = [
            ChannelId::Telegram,
            ChannelId::Discord,
            ChannelId::Slack,
            ChannelId::WhatsApp,
            ChannelId::WebChat,
            ChannelId::Email,
            ChannelId::Internal,
        ];
        for id in &all_channels {
            dock.register(DockEntry::from_channel_id(*id));
        }
        dock
    }

    /// Register or update a dock entry.
    pub fn register(&mut self, entry: DockEntry) {
        debug!(channel = ?entry.channel_id, "registered channel dock entry");
        self.entries.insert(entry.channel_id, entry);
    }

    /// Look up a channel's dock entry.
    pub fn get(&self, id: ChannelId) -> Option<&DockEntry> {
        self.entries.get(&id)
    }

    /// Mark a channel as active.
    pub fn set_active(&mut self, id: ChannelId, active: bool) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.is_active = active;
        }
    }

    /// List all registered channels.
    pub fn list(&self) -> Vec<&DockEntry> {
        self.entries.values().collect()
    }

    /// List only active channels.
    pub fn active_channels(&self) -> Vec<&DockEntry> {
        self.entries.values().filter(|e| e.is_active).collect()
    }

    /// Number of registered channels.
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// Convert a dock entry into a `ChannelContext` suitable for the agent runner.
    ///
    /// This bridges the channel metadata system to the agent runner's
    /// `ChannelContext` struct, enabling channel-aware prompt injection
    /// without circular crate dependencies.
    pub fn to_runner_context(&self, id: ChannelId) -> Option<RunnerChannelContext> {
        self.entries.get(&id).map(|entry| RunnerChannelContext {
            channel_name: entry.capabilities.channel.clone(),
            supports_threading: entry.capabilities.supports_threading,
            supports_streaming: entry.capabilities.supports_streaming,
            supports_reactions: entry.capabilities.supports_reactions,
            supports_media: entry.capabilities.supports_media,
            max_message_length: entry.capabilities.max_message_length,
            markup_format: format_name(entry.markup_format),
        })
    }
}

impl Default for ChannelDock {
    fn default() -> Self {
        Self::new()
    }
}

/// Lightweight channel context for the runner (mirrors `runner::ChannelContext`
/// without depending on clawdesk-agents to avoid circular deps).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerChannelContext {
    pub channel_name: String,
    pub supports_threading: bool,
    pub supports_streaming: bool,
    pub supports_reactions: bool,
    pub supports_media: bool,
    pub max_message_length: Option<usize>,
    pub markup_format: String,
}

/// Map a `MarkupFormat` to its string name for the runner.
fn format_name(fmt: MarkupFormat) -> String {
    match fmt {
        MarkupFormat::Markdown => "markdown".into(),
        MarkupFormat::SlackMrkdwn => "slack_mrkdwn".into(),
        MarkupFormat::TelegramMarkdownV2 => "telegram_markdown_v2".into(),
        MarkupFormat::WhatsApp => "whatsapp".into(),
        MarkupFormat::PlainText => "plain_text".into(),
        MarkupFormat::Html => "html".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_from_defaults() {
        let dock = ChannelDock::with_all_defaults();
        assert_eq!(dock.count(), 7);

        let slack = dock.get(ChannelId::Slack).unwrap();
        assert!(slack.capabilities.supports_threading);
        assert_eq!(slack.markup_format, MarkupFormat::SlackMrkdwn);

        let telegram = dock.get(ChannelId::Telegram).unwrap();
        assert_eq!(telegram.markup_format, MarkupFormat::TelegramMarkdownV2);
        assert_eq!(telegram.capabilities.max_message_length, Some(4096));
    }

    #[test]
    fn to_runner_context() {
        let dock = ChannelDock::with_all_defaults();
        let ctx = dock.to_runner_context(ChannelId::Slack).unwrap();
        assert_eq!(ctx.channel_name, "slack");
        assert_eq!(ctx.markup_format, "slack_mrkdwn");
        assert!(ctx.supports_threading);
    }

    #[test]
    fn active_tracking() {
        let mut dock = ChannelDock::with_all_defaults();
        assert!(dock.active_channels().is_empty());

        dock.set_active(ChannelId::Slack, true);
        dock.set_active(ChannelId::Telegram, true);
        assert_eq!(dock.active_channels().len(), 2);

        dock.set_active(ChannelId::Slack, false);
        assert_eq!(dock.active_channels().len(), 1);
    }

    #[test]
    fn unknown_channel_returns_none() {
        let dock = ChannelDock::new();
        assert!(dock.get(ChannelId::Slack).is_none());
    }
}
