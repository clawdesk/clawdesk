//! Allowlist management for message routing.

use clawdesk_types::channel::ChannelId;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Allowlist mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowlistMode {
    /// Only allowed senders can interact.
    AllowlistOnly,
    /// Everyone except blocked senders can interact.
    BlocklistOnly,
    /// No restrictions.
    Open,
}

/// An allowlist entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowlistEntry {
    pub sender_id: String,
    pub channel: Option<ChannelId>,
    pub added_by: String,
    pub added_at: chrono::DateTime<chrono::Utc>,
    pub reason: Option<String>,
}

/// Manages per-channel and global allowlists/blocklists.
pub struct AllowlistManager {
    mode: RwLock<AllowlistMode>,
    /// Global allowlist.
    allowlist: DashMap<String, AllowlistEntry>,
    /// Global blocklist.
    blocklist: RwLock<HashSet<String>>,
    /// Per-channel overrides.
    channel_modes: DashMap<ChannelId, AllowlistMode>,
    channel_allowlists: DashMap<ChannelId, HashMap<String, AllowlistEntry>>,
}

impl AllowlistManager {
    pub fn new(mode: AllowlistMode) -> Self {
        Self {
            mode: RwLock::new(mode),
            allowlist: DashMap::new(),
            blocklist: RwLock::new(HashSet::new()),
            channel_modes: DashMap::new(),
            channel_allowlists: DashMap::new(),
        }
    }

    /// Check if a sender is allowed to interact on a channel.
    pub async fn is_allowed(&self, sender_id: &str, channel: &ChannelId) -> bool {
        // Check blocklist first (always applies).
        if self.blocklist.read().await.contains(sender_id) {
            return false;
        }

        // Check channel-specific mode.
        let channel_mode = self.channel_modes.get(channel).map(|e| *e.value());
        let effective_mode = channel_mode.unwrap_or(*self.mode.read().await);

        match effective_mode {
            AllowlistMode::Open => true,
            AllowlistMode::BlocklistOnly => true, // Already checked blocklist above.
            AllowlistMode::AllowlistOnly => {
                // Check channel-specific allowlist first.
                if let Some(channel_list) = self.channel_allowlists.get(channel) {
                    if channel_list.value().contains_key(sender_id) {
                        return true;
                    }
                }
                // Check global allowlist.
                self.allowlist.contains_key(sender_id)
            }
        }
    }

    /// Add a sender to the global allowlist.
    pub async fn allow(&self, entry: AllowlistEntry) {
        let id = entry.sender_id.clone();
        self.allowlist.insert(id, entry);
    }

    /// Add a sender to a channel-specific allowlist.
    pub async fn allow_on_channel(&self, channel: ChannelId, entry: AllowlistEntry) {
        let id = entry.sender_id.clone();
        self.channel_allowlists
            .entry(channel)
            .or_default()
            .insert(id, entry);
    }

    /// Block a sender globally.
    pub async fn block(&self, sender_id: &str) {
        self.blocklist.write().await.insert(sender_id.to_string());
    }

    /// Unblock a sender.
    pub async fn unblock(&self, sender_id: &str) {
        self.blocklist.write().await.remove(sender_id);
    }

    /// Remove a sender from the global allowlist.
    pub async fn remove(&self, sender_id: &str) {
        self.allowlist.remove(sender_id);
    }

    /// Set the global mode.
    pub async fn set_mode(&self, mode: AllowlistMode) {
        *self.mode.write().await = mode;
    }

    /// Set a channel-specific mode.
    pub async fn set_channel_mode(&self, channel: ChannelId, mode: AllowlistMode) {
        self.channel_modes.insert(channel, mode);
    }

    /// Get the effective mode for a channel.
    pub async fn effective_mode(&self, channel: &ChannelId) -> AllowlistMode {
        self.channel_modes
            .get(channel)
            .map(|e| *e.value())
            .unwrap_or(*self.mode.read().await)
    }

    /// List all global allowlist entries.
    pub async fn list_allowed(&self) -> Vec<AllowlistEntry> {
        self.allowlist.iter().map(|e| e.value().clone()).collect()
    }

    /// List blocked senders.
    pub async fn list_blocked(&self) -> Vec<String> {
        self.blocklist.read().await.iter().cloned().collect()
    }
}

impl Default for AllowlistManager {
    fn default() -> Self {
        Self::new(AllowlistMode::Open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_mode() {
        let mgr = AllowlistManager::new(AllowlistMode::Open);
        assert!(mgr.is_allowed("anyone", &ChannelId::Telegram).await);
    }

    #[tokio::test]
    async fn test_blocklist() {
        let mgr = AllowlistManager::new(AllowlistMode::Open);
        mgr.block("spammer").await;
        assert!(!mgr.is_allowed("spammer", &ChannelId::Telegram).await);
        assert!(mgr.is_allowed("normal_user", &ChannelId::Telegram).await);
    }

    #[tokio::test]
    async fn test_allowlist_only() {
        let mgr = AllowlistManager::new(AllowlistMode::AllowlistOnly);
        assert!(!mgr.is_allowed("stranger", &ChannelId::Discord).await);

        mgr.allow(AllowlistEntry {
            sender_id: "alice".to_string(),
            channel: None,
            added_by: "admin".to_string(),
            added_at: chrono::Utc::now(),
            reason: None,
        })
        .await;

        assert!(mgr.is_allowed("alice", &ChannelId::Discord).await);
        assert!(!mgr.is_allowed("bob", &ChannelId::Discord).await);
    }

    #[tokio::test]
    async fn test_channel_specific_allowlist() {
        let mgr = AllowlistManager::new(AllowlistMode::AllowlistOnly);

        mgr.allow_on_channel(
            ChannelId::Slack,
            AllowlistEntry {
                sender_id: "slack_user".to_string(),
                channel: Some(ChannelId::Slack),
                added_by: "admin".to_string(),
                added_at: chrono::Utc::now(),
                reason: None,
            },
        )
        .await;

        assert!(mgr.is_allowed("slack_user", &ChannelId::Slack).await);
        assert!(!mgr.is_allowed("slack_user", &ChannelId::Discord).await);
    }
}
