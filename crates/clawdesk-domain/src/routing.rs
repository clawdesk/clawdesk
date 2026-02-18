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
}
