//! Message routing logic.
//!
//! Determines which agent/session handles an inbound message
//! based on channel, sender, and configuration.

use clawdesk_types::{
    channel::ChannelId,
    message::NormalizedMessage,
    session::SessionKey,
};

/// Routing decision for an inbound message.
#[derive(Debug, Clone)]
pub enum RoutingDecision {
    /// Route to an existing or new session.
    Session { key: SessionKey },
    /// Drop the message (filtered by allowlist, rate limit, etc.).
    Drop { reason: String },
    /// Queue the message for later processing.
    Queue { key: SessionKey, reason: String },
}

/// Configuration for the message router.
#[derive(Debug, Clone, Default)]
pub struct RouterConfig {
    /// Channels that are allowed (empty = all allowed).
    pub allowed_channels: Vec<ChannelId>,
    /// Specific sender IDs that are allowed (empty = all allowed).
    pub allowed_senders: Vec<String>,
    /// Whether to allow messages from unknown senders.
    pub allow_unknown_senders: bool,
    /// Maximum concurrent sessions per channel.
    pub max_sessions_per_channel: Option<usize>,
}

/// Route an inbound message to a session.
pub fn route_message(
    msg: &NormalizedMessage,
    config: &RouterConfig,
) -> RoutingDecision {
    let channel = msg.origin.channel_id();

    // Check channel allowlist
    if !config.allowed_channels.is_empty() && !config.allowed_channels.contains(&channel) {
        return RoutingDecision::Drop {
            reason: format!("channel {} not in allowlist", channel),
        };
    }

    // Check sender allowlist
    if !config.allowed_senders.is_empty()
        && !config.allowed_senders.contains(&msg.sender.id)
        && !config.allow_unknown_senders
    {
        return RoutingDecision::Drop {
            reason: format!("sender {} not in allowlist", msg.sender.id),
        };
    }

    RoutingDecision::Session {
        key: msg.session_key.clone(),
    }
}

// ── Channel binding resolver ──────────────────────────

/// A channel binding entry describing which agent handles messages
/// for a specific channel/account/group/thread combination.
#[derive(Debug, Clone)]
pub struct ChannelBindingEntry {
    /// Agent ID that owns this binding.
    pub agent_id: String,
    /// Channel type (e.g., "telegram", "slack", "discord").
    pub channel: String,
    /// Account/workspace identifier.
    pub account: String,
    /// Optional group filter (e.g., Telegram group chat ID).
    pub group: Option<String>,
    /// Optional thread filter.
    pub thread: Option<String>,
}

impl ChannelBindingEntry {
    /// Compute binding specificity — more specific bindings win.
    /// Score: 1 point each for channel, account (always present),
    /// +1 for group, +1 for thread.
    pub fn specificity(&self) -> usize {
        let mut score = 2; // channel + account always present
        if self.group.is_some() {
            score += 1;
        }
        if self.thread.is_some() {
            score += 1;
        }
        score
    }

    /// Check whether this binding matches an inbound message's channel
    /// and session key.
    pub fn matches(&self, channel: ChannelId, session_identifier: &str) -> bool {
        // Channel name must match (case-insensitive)
        let channel_str = format!("{}", channel);
        if !self.channel.eq_ignore_ascii_case(&channel_str) {
            return false;
        }

        // Account must be a substring of the session identifier
        // (e.g., account "mybot" matches session "mybot:123")
        if !session_identifier.contains(&self.account) {
            return false;
        }

        // Group filter: if set, must appear in the session identifier
        if let Some(ref group) = self.group {
            if !session_identifier.contains(group.as_str()) {
                return false;
            }
        }

        // Thread filter: if set, must appear in the session identifier
        if let Some(ref thread) = self.thread {
            if !session_identifier.contains(thread.as_str()) {
                return false;
            }
        }

        true
    }
}

/// Resolve which agent should handle a message based on channel bindings.
///
/// Evaluates all bindings against the message's channel and session key,
/// returning the agent_id of the most specific match. If no binding
/// matches, returns `None` (the caller should fall back to default routing).
pub fn resolve_binding(
    bindings: &[ChannelBindingEntry],
    channel: ChannelId,
    session_identifier: &str,
) -> Option<String> {
    let mut best: Option<(&ChannelBindingEntry, usize)> = None;

    for binding in bindings {
        if binding.matches(channel, session_identifier) {
            let spec = binding.specificity();
            if best.as_ref().map_or(true, |(_, best_spec)| spec > *best_spec) {
                best = Some((binding, spec));
            }
        }
    }

    best.map(|(b, _)| b.agent_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_types::message::{MessageOrigin, SenderIdentity};
    use chrono::Utc;
    use uuid::Uuid;

    fn make_msg(channel: ChannelId, sender_id: &str) -> NormalizedMessage {
        NormalizedMessage {
            id: Uuid::new_v4(),
            session_key: SessionKey::new(channel, sender_id),
            body: "test".to_string(),
            body_for_agent: None,
            sender: SenderIdentity {
                id: sender_id.to_string(),
                display_name: "Test".to_string(),
                channel,
            },
            media: vec![],
            artifact_refs: vec![],
            reply_context: None,
            origin: MessageOrigin::Internal {
                source: "test".to_string(),
            },
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_default_config_routes_all() {
        let msg = make_msg(ChannelId::Telegram, "user1");
        let config = RouterConfig::default();
        let decision = route_message(&msg, &config);
        assert!(matches!(decision, RoutingDecision::Session { .. }));
    }

    #[test]
    fn test_channel_allowlist_blocks() {
        let msg = make_msg(ChannelId::Telegram, "user1");
        let config = RouterConfig {
            allowed_channels: vec![ChannelId::Discord],
            ..Default::default()
        };
        let decision = route_message(&msg, &config);
        assert!(matches!(decision, RoutingDecision::Drop { .. }));
    }

    #[test]
    fn test_binding_resolver_exact_match() {
        let bindings = vec![
            ChannelBindingEntry {
                agent_id: "agent-a".into(),
                channel: "telegram".into(),
                account: "mybot".into(),
                group: None,
                thread: None,
            },
            ChannelBindingEntry {
                agent_id: "agent-b".into(),
                channel: "discord".into(),
                account: "server1".into(),
                group: Some("general".into()),
                thread: None,
            },
        ];
        let result = resolve_binding(&bindings, ChannelId::Telegram, "mybot:123");
        assert_eq!(result, Some("agent-a".to_string()));
    }

    #[test]
    fn test_binding_resolver_most_specific_wins() {
        let bindings = vec![
            ChannelBindingEntry {
                agent_id: "generic".into(),
                channel: "telegram".into(),
                account: "bot".into(),
                group: None,
                thread: None,
            },
            ChannelBindingEntry {
                agent_id: "specific".into(),
                channel: "telegram".into(),
                account: "bot".into(),
                group: Some("vip".into()),
                thread: None,
            },
        ];
        let result = resolve_binding(&bindings, ChannelId::Telegram, "bot:vip:456");
        assert_eq!(result, Some("specific".to_string()));
    }

    #[test]
    fn test_binding_resolver_no_match() {
        let bindings = vec![
            ChannelBindingEntry {
                agent_id: "agent-a".into(),
                channel: "slack".into(),
                account: "workspace1".into(),
                group: None,
                thread: None,
            },
        ];
        let result = resolve_binding(&bindings, ChannelId::Telegram, "mybot:123");
        assert_eq!(result, None);
    }
}
