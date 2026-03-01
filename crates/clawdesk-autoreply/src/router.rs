//! Message router — allowlist checking, agent selection, send policy enforcement.

use clawdesk_types::autoreply::{TriggerClassification, TriggerType};
use clawdesk_types::message::NormalizedMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Routing decision for a message.
#[derive(Debug, Clone)]
pub enum RoutingDecision {
    /// Process the message with the specified agent.
    Process {
        agent_id: String,
        classification: TriggerClassification,
    },
    /// Drop the message (sender not allowed, rate limited, etc.).
    Drop { reason: String },
    /// Queue for later processing.
    Queue { reason: String },
}

/// Router configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    /// Default agent to use when no specific routing applies.
    pub default_agent: String,
    /// Per-channel agent overrides.
    pub channel_agents: HashMap<String, String>,
    /// Per-command agent overrides.
    pub command_agents: HashMap<String, String>,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            default_agent: "default".to_string(),
            channel_agents: HashMap::new(),
            command_agents: HashMap::new(),
        }
    }
}

/// Routes messages to the appropriate agent based on classification and config.
pub struct MessageRouter {
    config: RouterConfig,
}

impl MessageRouter {
    pub fn new(config: RouterConfig) -> Self {
        Self { config }
    }

    /// Route a classified message to an agent.
    pub fn route(
        &self,
        msg: &NormalizedMessage,
        classification: TriggerClassification,
    ) -> RoutingDecision {
        // Check command-specific routing.
        if let TriggerType::Command { ref command } = classification.trigger {
            if let Some(agent) = self.config.command_agents.get(command) {
                return RoutingDecision::Process {
                    agent_id: agent.clone(),
                    classification,
                };
            }
        }

        // Check channel-specific routing.
        let channel_key = msg.sender.channel.to_string();
        if let Some(agent) = self.config.channel_agents.get(&channel_key) {
            return RoutingDecision::Process {
                agent_id: agent.clone(),
                classification,
            };
        }

        // Default routing.
        RoutingDecision::Process {
            agent_id: self.config.default_agent.clone(),
            classification,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_types::autoreply::ReplyPriority;
    use clawdesk_types::channel::ChannelId;

    fn test_classification(trigger: TriggerType) -> TriggerClassification {
        TriggerClassification {
            trigger,
            priority: ReplyPriority::Normal,
            confidence: 1.0,
            should_reply: true,
        }
    }

    fn test_msg(channel: ChannelId) -> NormalizedMessage {
        NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key: clawdesk_types::session::SessionKey::new(clawdesk_types::ChannelId::Telegram, "test"),
            body: "test".to_string(),
            body_for_agent: None,
            sender: clawdesk_types::message::SenderIdentity {
                id: "user-1".to_string(),
                display_name: "Test".to_string(),
                channel,
            },
            media: vec![],
            artifact_refs: vec![],
            reply_context: None,
            origin: clawdesk_types::message::MessageOrigin::Internal {
                source: "test".to_string(),
            },
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_default_routing() {
        let router = MessageRouter::new(RouterConfig::default());
        let msg = test_msg(ChannelId::Telegram);
        let class = test_classification(TriggerType::DirectMessage);
        let decision = router.route(&msg, class);
        assert!(
            matches!(decision, RoutingDecision::Process { agent_id, .. } if agent_id == "default")
        );
    }

    #[test]
    fn test_command_routing() {
        let mut config = RouterConfig::default();
        config
            .command_agents
            .insert("admin".to_string(), "admin-agent".to_string());
        let router = MessageRouter::new(config);
        let msg = test_msg(ChannelId::Internal);
        let class = test_classification(TriggerType::Command {
            command: "admin".to_string(),
        });
        let decision = router.route(&msg, class);
        assert!(
            matches!(decision, RoutingDecision::Process { agent_id, .. } if agent_id == "admin-agent")
        );
    }

    #[test]
    fn test_channel_routing() {
        let mut config = RouterConfig::default();
        config
            .channel_agents
            .insert("telegram".to_string(), "telegram-bot".to_string());
        let router = MessageRouter::new(config);
        let msg = test_msg(ChannelId::Telegram);
        let class = test_classification(TriggerType::Mention);
        let decision = router.route(&msg, class);
        assert!(
            matches!(decision, RoutingDecision::Process { agent_id, .. } if agent_id == "telegram-bot")
        );
    }
}
