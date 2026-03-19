//! Trigger classification — determines why a message should get a reply.

use clawdesk_types::autoreply::{ReplyPriority, TriggerClassification, TriggerType};
use clawdesk_types::channel::ChannelId;
use clawdesk_types::message::NormalizedMessage;
use serde::{Deserialize, Serialize};

/// Configuration for the trigger classifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifierConfig {
    /// Bot's name/handle for mention detection.
    pub bot_names: Vec<String>,
    /// Command prefix (e.g., "/", "!").
    pub command_prefix: String,
    /// Whether DMs always trigger a reply.
    pub auto_reply_dms: bool,
    /// Channels where the bot responds to all messages.
    pub auto_reply_channels: Vec<ChannelId>,
    /// Keyword triggers — if any of these appear in a group message, activate.
    /// Case-insensitive substring matching. Empty = keyword gating disabled.
    pub keyword_triggers: Vec<String>,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            bot_names: vec!["clawdesk".to_string(), "bot".to_string()],
            command_prefix: "/".to_string(),
            auto_reply_dms: true,
            auto_reply_channels: vec![],
            keyword_triggers: vec![],
        }
    }
}

/// ASCII case-insensitive substring search. Zero-allocation alternative to
/// `haystack.to_lowercase().contains(&needle.to_lowercase())`.
///
/// O(h × n) worst case where h = haystack length, n = needle length.
/// In practice, early exits make this faster than allocating two lowered strings.
#[inline]
fn ascii_contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    'outer: for start in 0..=(h.len() - n.len()) {
        for j in 0..n.len() {
            if h[start + j].to_ascii_lowercase() != n[j].to_ascii_lowercase() {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

/// Classifies incoming messages to determine if and how to reply.
pub struct TriggerClassifier {
    config: ClassifierConfig,
}

impl TriggerClassifier {
    pub fn new(config: ClassifierConfig) -> Self {
        Self { config }
    }

    /// Classify a normalized message.
    ///
    /// Returns `None` if the message should not trigger a reply.
    pub fn classify(&self, msg: &NormalizedMessage) -> Option<TriggerClassification> {
        let body = &msg.body;

        // Check for commands first (highest priority).
        // Use ASCII case-insensitive prefix check — zero allocation.
        if body.len() >= self.config.command_prefix.len()
            && body[..self.config.command_prefix.len()].eq_ignore_ascii_case(&self.config.command_prefix)
        {
            let rest = &body[self.config.command_prefix.len()..];
            let command = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();

            return Some(TriggerClassification {
                trigger: TriggerType::Command {
                    command,
                },
                priority: ReplyPriority::High,
                confidence: 1.0,
                should_reply: true,
            });
        }

        // Check for mentions — case-insensitive substring search without
        // allocating a lowercased copy of the entire body.
        for name in &self.config.bot_names {
            if ascii_contains_ignore_case(body, name) {
                return Some(TriggerClassification {
                    trigger: TriggerType::Mention,
                    priority: ReplyPriority::Normal,
                    confidence: 0.95,
                    should_reply: true,
                });
            }
        }

        // Check for keyword triggers — activate in groups when a trigger
        // phrase appears. Uses the same zero-allocation ASCII search.
        if !self.config.keyword_triggers.is_empty() && !self.is_dm(msg) {
            for keyword in &self.config.keyword_triggers {
                if ascii_contains_ignore_case(body, keyword) {
                    return Some(TriggerClassification {
                        trigger: TriggerType::ChannelSpecific {
                            channel: msg.sender.channel.to_string(),
                            trigger: format!("keyword:{}", keyword),
                        },
                        priority: ReplyPriority::Normal,
                        confidence: 0.9,
                        should_reply: true,
                    });
                }
            }
        }

        // Check for direct messages.
        if self.config.auto_reply_dms && self.is_dm(msg) {
            return Some(TriggerClassification {
                trigger: TriggerType::DirectMessage,
                priority: ReplyPriority::Normal,
                confidence: 1.0,
                should_reply: true,
            });
        }

        // Check for auto-reply channels.
        if self
            .config
            .auto_reply_channels
            .contains(&msg.sender.channel)
        {
            return Some(TriggerClassification {
                trigger: TriggerType::ChannelSpecific {
                    channel: msg.sender.channel.to_string(),
                    trigger: "auto-reply-channel".to_string(),
                },
                priority: ReplyPriority::Low,
                confidence: 1.0,
                should_reply: true,
            });
        }

        None
    }

    /// Determine if a message is a direct message.
    fn is_dm(&self, msg: &NormalizedMessage) -> bool {
        matches!(
            msg.origin,
            clawdesk_types::message::MessageOrigin::WebChat { .. }
                | clawdesk_types::message::MessageOrigin::Internal { .. }
        ) || {
            match &msg.origin {
                clawdesk_types::message::MessageOrigin::Discord { guild_id, .. } => {
                    *guild_id == 0 // DMs have guild_id 0.
                }
                clawdesk_types::message::MessageOrigin::Telegram { chat_id, .. } => {
                    *chat_id > 0 // Private chats have positive chat_id.
                }
                _ => false,
            }
        }
    }

    /// Extract the command name and arguments from a message body.
    pub fn parse_command(body: &str, prefix: &str) -> Option<(String, Vec<String>)> {
        let stripped = body.strip_prefix(prefix)?;
        let parts: Vec<&str> = stripped.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }
        let command = parts[0].to_lowercase();
        let args = parts[1..].iter().map(|s| s.to_string()).collect();
        Some((command, args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_types::message::{MessageOrigin, NormalizedMessage, SenderIdentity};

    fn test_msg(body: &str, channel: ChannelId) -> NormalizedMessage {
        NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key: clawdesk_types::session::SessionKey::new(clawdesk_types::ChannelId::Telegram, "test"),
            body: body.to_string(),
            body_for_agent: None,
            sender: SenderIdentity {
                id: "user-1".to_string(),
                display_name: "Test User".to_string(),
                channel,
            },
            media: vec![],
            artifact_refs: vec![],
            reply_context: None,
            origin: MessageOrigin::Internal {
                source: "test".to_string(),
            },
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_command_classification() {
        let classifier = TriggerClassifier::new(ClassifierConfig::default());
        let msg = test_msg("/help", ChannelId::Internal);
        let result = classifier.classify(&msg).unwrap();
        assert!(matches!(result.trigger, TriggerType::Command { ref command } if command == "help"));
        assert_eq!(result.priority, ReplyPriority::High);
    }

    #[test]
    fn test_mention_classification() {
        let classifier = TriggerClassifier::new(ClassifierConfig::default());
        let msg = test_msg("hey clawdesk, how are you?", ChannelId::Discord);
        let result = classifier.classify(&msg).unwrap();
        assert_eq!(result.trigger, TriggerType::Mention);
    }

    #[test]
    fn test_dm_classification() {
        let classifier = TriggerClassifier::new(ClassifierConfig::default());
        let msg = test_msg("hello", ChannelId::Internal);
        let result = classifier.classify(&msg).unwrap();
        assert_eq!(result.trigger, TriggerType::DirectMessage);
    }

    #[test]
    fn test_no_trigger() {
        let classifier = TriggerClassifier::new(ClassifierConfig {
            auto_reply_dms: false,
            ..Default::default()
        });
        let msg = test_msg("random message", ChannelId::Telegram);
        assert!(classifier.classify(&msg).is_none());
    }

    #[test]
    fn test_keyword_trigger_in_group() {
        let classifier = TriggerClassifier::new(ClassifierConfig {
            keyword_triggers: vec!["deploy".to_string(), "rollback".to_string()],
            auto_reply_dms: false,
            ..Default::default()
        });
        // Telegram group message with keyword.
        let msg = test_msg("we need to deploy the latest build", ChannelId::Telegram);
        let result = classifier.classify(&msg);
        assert!(result.is_some());
        let classification = result.unwrap();
        assert!(matches!(
            classification.trigger,
            TriggerType::ChannelSpecific { ref trigger, .. } if trigger.contains("keyword:deploy")
        ));
    }

    #[test]
    fn test_keyword_not_triggered_in_dm() {
        // Keywords should only trigger in group context, not DMs.
        let classifier = TriggerClassifier::new(ClassifierConfig {
            keyword_triggers: vec!["deploy".to_string()],
            auto_reply_dms: false,
            ..Default::default()
        });
        let msg = test_msg("deploy now", ChannelId::Internal);
        // Internal is a DM, keyword should not trigger.
        let result = classifier.classify(&msg);
        assert!(result.is_none());
    }
}
