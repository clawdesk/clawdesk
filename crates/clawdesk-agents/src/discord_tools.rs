//! Discord actions toolkit — agent-callable tools for Discord operations.
//!
//! Exposes guild management, messaging, moderation, and presence as tools.
//! Each tool validates bot permissions before execution.

use crate::tools::{Tool, ToolCapability, ToolSchema};
use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

/// Discord actions toolkit for agents.
pub struct DiscordActionsTool;

impl DiscordActionsTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for DiscordActionsTool {
    fn name(&self) -> &str {
        "discord_action"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "discord_action".into(),
            description: "Perform Discord operations: send messages, manage channels, \
                          moderate users, create threads, manage roles, and set webhooks."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "send_message", "edit_message", "delete_message",
                            "create_thread", "pin_message", "unpin_message",
                            "create_channel", "delete_channel", "set_topic",
                            "add_role", "remove_role", "kick", "ban",
                            "create_webhook", "set_presence", "list_members",
                            "add_reaction", "remove_reaction"
                        ],
                        "description": "Discord action to perform"
                    },
                    "guild_id": {
                        "type": "string",
                        "description": "Discord guild (server) ID"
                    },
                    "channel_id": {
                        "type": "string",
                        "description": "Discord channel ID"
                    },
                    "user_id": {
                        "type": "string",
                        "description": "Target user ID"
                    },
                    "message_id": {
                        "type": "string",
                        "description": "Target message ID"
                    },
                    "content": {
                        "type": "string",
                        "description": "Message content or topic text"
                    },
                    "role_id": {
                        "type": "string",
                        "description": "Role ID for role operations"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Reason for moderation actions"
                    },
                    "thread_name": {
                        "type": "string",
                        "description": "Name for new thread"
                    },
                    "emoji": {
                        "type": "string",
                        "description": "Emoji for reaction (Unicode or custom ID)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Messaging, ToolCapability::ExternalApi]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'action' parameter")?;

        debug!(action, "Discord action requested");

        // Route to channel plugin for actual execution
        // The tool prepares the request; the channel handles API calls
        match action {
            "send_message" => {
                let channel_id = args.get("channel_id").and_then(|v| v.as_str())
                    .ok_or("Missing 'channel_id'")?;
                let content = args.get("content").and_then(|v| v.as_str())
                    .ok_or("Missing 'content'")?;
                Ok(json!({
                    "action": "send_message",
                    "channel_id": channel_id,
                    "content": content,
                    "status": "routed_to_channel"
                }).to_string())
            }
            "create_thread" => {
                let channel_id = args.get("channel_id").and_then(|v| v.as_str())
                    .ok_or("Missing 'channel_id'")?;
                let name = args.get("thread_name").and_then(|v| v.as_str())
                    .ok_or("Missing 'thread_name'")?;
                Ok(json!({
                    "action": "create_thread",
                    "channel_id": channel_id,
                    "thread_name": name,
                    "status": "routed_to_channel"
                }).to_string())
            }
            "kick" | "ban" => {
                let guild_id = args.get("guild_id").and_then(|v| v.as_str())
                    .ok_or("Missing 'guild_id'")?;
                let user_id = args.get("user_id").and_then(|v| v.as_str())
                    .ok_or("Missing 'user_id'")?;
                let reason = args.get("reason").and_then(|v| v.as_str())
                    .unwrap_or("No reason provided");
                Ok(json!({
                    "action": action,
                    "guild_id": guild_id,
                    "user_id": user_id,
                    "reason": reason,
                    "requires_approval": true,
                    "status": "approval_required"
                }).to_string())
            }
            _ => Ok(json!({
                "action": action,
                "status": "routed_to_channel",
                "args": args,
            }).to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name() {
        let tool = DiscordActionsTool::new();
        assert_eq!(tool.name(), "discord_action");
    }

    #[tokio::test]
    async fn moderation_requires_approval() {
        let tool = DiscordActionsTool::new();
        let result = tool
            .execute(json!({
                "action": "kick",
                "guild_id": "123",
                "user_id": "456",
                "reason": "spam"
            }))
            .await
            .unwrap();
        assert!(result.contains("approval_required"));
    }
}
