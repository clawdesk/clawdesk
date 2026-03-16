//! Channel capability bitvector — O(1) cross-channel compatibility checks.

use serde::{Deserialize, Serialize};

/// Individual channel capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u32)]
pub enum ChannelCapability {
    SendText        = 1 << 0,
    SendMedia       = 1 << 1,
    Reactions       = 1 << 2,
    Polls           = 1 << 3,
    InlineKeyboards = 1 << 4,
    GuildAdmin      = 1 << 5,
    VoiceCalls      = 1 << 6,
    Threads         = 1 << 7,
    Streaming       = 1 << 8,
    Editing         = 1 << 9,
    Deletion        = 1 << 10,
    FileUpload      = 1 << 11,
    ReadReceipts    = 1 << 12,
    Mentions        = 1 << 13,
    SlashCommands   = 1 << 14,
    Webhooks        = 1 << 15,
    RichFormatting  = 1 << 16,
    Stickers        = 1 << 17,
    ForumTopics     = 1 << 18,
    ScheduledSend   = 1 << 19,
}

/// Bitvector capability set for a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CapabilitySet(pub u32);

impl CapabilitySet {
    pub fn new() -> Self { Self(0) }

    pub fn with(mut self, cap: ChannelCapability) -> Self {
        self.0 |= cap as u32;
        self
    }

    pub fn has(self, cap: ChannelCapability) -> bool {
        self.0 & (cap as u32) != 0
    }

    /// O(1) intersection — capabilities available across all channels.
    pub fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// O(1) union.
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Check if an operation is valid across all provided capability sets.
    pub fn valid_across(cap: ChannelCapability, sets: &[CapabilitySet]) -> bool {
        sets.iter().all(|s| s.has(cap))
    }

    /// Count of enabled capabilities.
    pub fn count(self) -> u32 {
        self.0.count_ones()
    }
}

impl std::fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CapabilitySet(0b{:020b}, {} caps)", self.0, self.count())
    }
}

/// Pre-built capability sets for known channel types.
pub fn discord_capabilities() -> CapabilitySet {
    CapabilitySet::new()
        .with(ChannelCapability::SendText)
        .with(ChannelCapability::SendMedia)
        .with(ChannelCapability::Reactions)
        .with(ChannelCapability::InlineKeyboards)
        .with(ChannelCapability::GuildAdmin)
        .with(ChannelCapability::Threads)
        .with(ChannelCapability::Streaming)
        .with(ChannelCapability::Editing)
        .with(ChannelCapability::Deletion)
        .with(ChannelCapability::FileUpload)
        .with(ChannelCapability::Mentions)
        .with(ChannelCapability::SlashCommands)
        .with(ChannelCapability::Webhooks)
        .with(ChannelCapability::RichFormatting)
        .with(ChannelCapability::Stickers)
        .with(ChannelCapability::ForumTopics)
        .with(ChannelCapability::VoiceCalls)
}

pub fn telegram_capabilities() -> CapabilitySet {
    CapabilitySet::new()
        .with(ChannelCapability::SendText)
        .with(ChannelCapability::SendMedia)
        .with(ChannelCapability::Reactions)
        .with(ChannelCapability::Polls)
        .with(ChannelCapability::InlineKeyboards)
        .with(ChannelCapability::Editing)
        .with(ChannelCapability::Deletion)
        .with(ChannelCapability::FileUpload)
        .with(ChannelCapability::Mentions)
        .with(ChannelCapability::Stickers)
        .with(ChannelCapability::ForumTopics)
        .with(ChannelCapability::RichFormatting)
}

pub fn slack_capabilities() -> CapabilitySet {
    CapabilitySet::new()
        .with(ChannelCapability::SendText)
        .with(ChannelCapability::SendMedia)
        .with(ChannelCapability::Reactions)
        .with(ChannelCapability::Threads)
        .with(ChannelCapability::Streaming)
        .with(ChannelCapability::Editing)
        .with(ChannelCapability::Deletion)
        .with(ChannelCapability::FileUpload)
        .with(ChannelCapability::Mentions)
        .with(ChannelCapability::SlashCommands)
        .with(ChannelCapability::RichFormatting)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_set_operations() {
        let discord = discord_capabilities();
        let telegram = telegram_capabilities();
        let common = discord.intersect(telegram);
        assert!(common.has(ChannelCapability::SendText));
        assert!(common.has(ChannelCapability::Reactions));
        assert!(!common.has(ChannelCapability::GuildAdmin)); // Discord only
        assert!(!common.has(ChannelCapability::Polls)); // Telegram only
    }

    #[test]
    fn valid_across_channels() {
        let sets = vec![discord_capabilities(), telegram_capabilities(), slack_capabilities()];
        assert!(CapabilitySet::valid_across(ChannelCapability::SendText, &sets));
        assert!(!CapabilitySet::valid_across(ChannelCapability::Polls, &sets));
    }

    #[test]
    fn capability_count() {
        let discord = discord_capabilities();
        assert!(discord.count() > 10);
    }
}
