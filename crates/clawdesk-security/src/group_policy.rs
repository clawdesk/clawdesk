//! Group-aware message policy engine.
//!
//! Provides per-group (per-channel-within-channel) access control with @mention gating.
//! Policy evaluation is O(1) amortized via two-level HashMap lookup.
//!
//! ## Fail-Closed Design
//! Groups without an explicit policy entry default to `{allowed: false, require_mention: true}`.
//! This prevents the bot from responding in unconfigured channels.

use clawdesk_types::channel::ChannelId;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Policy for a single group (channel/room/workspace-channel).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupPolicyEntry {
    /// Whether the bot is allowed to respond in this group at all.
    pub allowed: bool,
    /// If true, the bot only responds when @mentioned.
    pub require_mention: bool,
    /// If set, only these sender IDs can invoke the bot in this group.
    /// If None, any sender is allowed (subject to the global allowlist).
    pub allowed_senders: Option<HashSet<String>>,
    /// The bot's mention handle for this channel (e.g., "@clawdesk", "<@U123>").
    pub mention_handle: Option<String>,
}

impl Default for GroupPolicyEntry {
    /// Fail-closed default: not allowed, require mention.
    fn default() -> Self {
        Self {
            allowed: false,
            require_mention: true,
            allowed_senders: None,
            mention_handle: None,
        }
    }
}

/// Result of evaluating a group policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Message should be processed.
    Allow,
    /// Message rejected: group is not configured or not allowed.
    DenyGroupNotAllowed,
    /// Message rejected: @mention required but not present.
    DenyMentionRequired,
    /// Message rejected: sender not in the group's allowed list.
    DenySenderNotAllowed,
}

/// Group-aware message policy engine.
///
/// Two-level map: `ChannelId → (group_id → GroupPolicyEntry)`.
/// The policy check runs *before* agent invocation, rejecting messages
/// from unconfigured groups at the gateway layer.
pub struct GroupPolicyManager {
    /// Per-channel group policies.
    policies: RwLock<HashMap<ChannelId, HashMap<String, GroupPolicyEntry>>>,
    /// Default mention handles per channel type.
    default_handles: RwLock<HashMap<ChannelId, String>>,
}

impl GroupPolicyManager {
    pub fn new() -> Self {
        Self {
            policies: RwLock::new(HashMap::new()),
            default_handles: RwLock::new(HashMap::new()),
        }
    }

    /// Set the default mention handle for a channel type.
    pub async fn set_default_handle(&self, channel: ChannelId, handle: String) {
        self.default_handles.write().await.insert(channel, handle);
    }

    /// Configure policy for a specific group within a channel.
    pub async fn set_group_policy(
        &self,
        channel: ChannelId,
        group_id: impl Into<String>,
        policy: GroupPolicyEntry,
    ) {
        let group_id = group_id.into();
        info!(%group_id, allowed = policy.allowed, mention = policy.require_mention, "group policy set");
        self.policies
            .write()
            .await
            .entry(channel)
            .or_default()
            .insert(group_id, policy);
    }

    /// Remove policy for a group (reverts to fail-closed default).
    pub async fn remove_group_policy(&self, channel: &ChannelId, group_id: &str) {
        if let Some(groups) = self.policies.write().await.get_mut(channel) {
            groups.remove(group_id);
            info!(%group_id, "group policy removed");
        }
    }

    /// Evaluate whether a message should be processed.
    ///
    /// # Arguments
    /// - `channel`: The channel type (Slack, Discord, etc.)
    /// - `group_id`: The specific group/channel/room ID within the channel
    /// - `sender_id`: The sender's user ID
    /// - `message_body`: The raw message body (checked for @mention)
    pub async fn evaluate(
        &self,
        channel: &ChannelId,
        group_id: &str,
        sender_id: &str,
        message_body: &str,
    ) -> PolicyDecision {
        let policies = self.policies.read().await;

        // Look up group policy. If none exists, fail-closed.
        let policy = match policies.get(channel).and_then(|g| g.get(group_id)) {
            Some(p) => p.clone(),
            None => {
                debug!(%group_id, "no group policy found, denying (fail-closed)");
                return PolicyDecision::DenyGroupNotAllowed;
            }
        };

        // Check if group is allowed at all.
        if !policy.allowed {
            return PolicyDecision::DenyGroupNotAllowed;
        }

        // Check sender restriction.
        if let Some(ref allowed) = policy.allowed_senders {
            if !allowed.contains(sender_id) {
                debug!(%sender_id, %group_id, "sender not in group allow list");
                return PolicyDecision::DenySenderNotAllowed;
            }
        }

        // Check @mention requirement.
        if policy.require_mention {
            let handles = self.default_handles.read().await;
            let mention = policy
                .mention_handle
                .as_deref()
                .or_else(|| handles.get(channel).map(|s| s.as_str()));

            if let Some(handle) = mention {
                if !self.message_contains_mention(message_body, handle) {
                    debug!(%group_id, %handle, "mention required but not found");
                    return PolicyDecision::DenyMentionRequired;
                }
            } else {
                // No handle configured but mention required — deny.
                warn!(%group_id, "mention required but no handle configured");
                return PolicyDecision::DenyMentionRequired;
            }
        }

        PolicyDecision::Allow
    }

    /// Check if message body contains an @mention.
    /// Handles both plain-text mentions (`@clawdesk`) and platform-specific
    /// formats (`<@U123456>` for Slack, `<@!123456>` for Discord).
    fn message_contains_mention(&self, body: &str, handle: &str) -> bool {
        // Case-insensitive prefix scan: O(M) where M = handle length.
        let lower_body = body.to_lowercase();
        let lower_handle = handle.to_lowercase();
        lower_body.contains(&lower_handle)
    }

    /// List all configured groups for a channel.
    pub async fn list_groups(&self, channel: &ChannelId) -> Vec<(String, GroupPolicyEntry)> {
        self.policies
            .read()
            .await
            .get(channel)
            .map(|g| g.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    /// Bulk-set policies for a channel (replaces all existing).
    pub async fn set_channel_policies(
        &self,
        channel: ChannelId,
        groups: HashMap<String, GroupPolicyEntry>,
    ) {
        info!(channel = ?channel, count = groups.len(), "bulk group policies set");
        self.policies.write().await.insert(channel, groups);
    }
}

impl Default for GroupPolicyManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fail_closed_by_default() {
        let mgr = GroupPolicyManager::new();
        let result = mgr
            .evaluate(&ChannelId::Slack, "C12345", "U001", "hello")
            .await;
        assert_eq!(result, PolicyDecision::DenyGroupNotAllowed);
    }

    #[tokio::test]
    async fn allowed_group_no_mention() {
        let mgr = GroupPolicyManager::new();
        mgr.set_group_policy(
            ChannelId::Slack,
            "C12345",
            GroupPolicyEntry {
                allowed: true,
                require_mention: false,
                allowed_senders: None,
                mention_handle: None,
            },
        )
        .await;

        let result = mgr
            .evaluate(&ChannelId::Slack, "C12345", "U001", "hello")
            .await;
        assert_eq!(result, PolicyDecision::Allow);
    }

    #[tokio::test]
    async fn mention_required_and_present() {
        let mgr = GroupPolicyManager::new();
        mgr.set_group_policy(
            ChannelId::Slack,
            "C12345",
            GroupPolicyEntry {
                allowed: true,
                require_mention: true,
                allowed_senders: None,
                mention_handle: Some("<@U_BOT>".to_string()),
            },
        )
        .await;

        let deny = mgr
            .evaluate(&ChannelId::Slack, "C12345", "U001", "hey there")
            .await;
        assert_eq!(deny, PolicyDecision::DenyMentionRequired);

        let allow = mgr
            .evaluate(&ChannelId::Slack, "C12345", "U001", "hey <@U_BOT> help me")
            .await;
        assert_eq!(allow, PolicyDecision::Allow);
    }

    #[tokio::test]
    async fn sender_restriction() {
        let mgr = GroupPolicyManager::new();
        let mut allowed = HashSet::new();
        allowed.insert("U001".to_string());
        allowed.insert("U002".to_string());

        mgr.set_group_policy(
            ChannelId::Discord,
            "general",
            GroupPolicyEntry {
                allowed: true,
                require_mention: false,
                allowed_senders: Some(allowed),
                mention_handle: None,
            },
        )
        .await;

        assert_eq!(
            mgr.evaluate(&ChannelId::Discord, "general", "U001", "hi")
                .await,
            PolicyDecision::Allow
        );
        assert_eq!(
            mgr.evaluate(&ChannelId::Discord, "general", "U999", "hi")
                .await,
            PolicyDecision::DenySenderNotAllowed
        );
    }

    #[tokio::test]
    async fn default_handle_used_when_no_per_group_handle() {
        let mgr = GroupPolicyManager::new();
        mgr.set_default_handle(ChannelId::Slack, "<@UBOT>".to_string())
            .await;
        mgr.set_group_policy(
            ChannelId::Slack,
            "C999",
            GroupPolicyEntry {
                allowed: true,
                require_mention: true,
                allowed_senders: None,
                mention_handle: None, // uses default
            },
        )
        .await;

        let deny = mgr
            .evaluate(&ChannelId::Slack, "C999", "U001", "hello world")
            .await;
        assert_eq!(deny, PolicyDecision::DenyMentionRequired);

        let allow = mgr
            .evaluate(&ChannelId::Slack, "C999", "U001", "hey <@UBOT> do this")
            .await;
        assert_eq!(allow, PolicyDecision::Allow);
    }

    #[tokio::test]
    async fn remove_group_reverts_to_deny() {
        let mgr = GroupPolicyManager::new();
        mgr.set_group_policy(
            ChannelId::Telegram,
            "grp1",
            GroupPolicyEntry {
                allowed: true,
                require_mention: false,
                ..Default::default()
            },
        )
        .await;

        assert_eq!(
            mgr.evaluate(&ChannelId::Telegram, "grp1", "u1", "hi")
                .await,
            PolicyDecision::Allow
        );

        mgr.remove_group_policy(&ChannelId::Telegram, "grp1")
            .await;

        assert_eq!(
            mgr.evaluate(&ChannelId::Telegram, "grp1", "u1", "hi")
                .await,
            PolicyDecision::DenyGroupNotAllowed
        );
    }

    #[tokio::test]
    async fn list_groups_returns_configured() {
        let mgr = GroupPolicyManager::new();
        mgr.set_group_policy(
            ChannelId::Slack,
            "C1",
            GroupPolicyEntry {
                allowed: true,
                ..Default::default()
            },
        )
        .await;
        mgr.set_group_policy(
            ChannelId::Slack,
            "C2",
            GroupPolicyEntry {
                allowed: false,
                ..Default::default()
            },
        )
        .await;

        let groups = mgr.list_groups(&ChannelId::Slack).await;
        assert_eq!(groups.len(), 2);
    }
}
