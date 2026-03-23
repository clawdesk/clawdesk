//! Telegram actions toolkit — agent-callable tools for Telegram Bot API.
//!
//! Exposes messaging, polls, inline keyboards, and admin operations.
//! Rate-limit aware: 30 msg/s private, 20 msg/min group.

use crate::tools::{Tool, ToolCapability, ToolSchema};
use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

/// Telegram actions toolkit for agents.
pub struct TelegramActionsTool;

impl TelegramActionsTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for TelegramActionsTool {
    fn name(&self) -> &str {
        "telegram_action"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "telegram_action".into(),
            description: "Perform Telegram operations: send/edit/delete messages, create polls, \
                          manage inline keyboards, pin messages, forward messages, set chat descriptions."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "send_message", "edit_message", "delete_message",
                            "forward_message", "pin_message", "unpin_message",
                            "create_poll", "send_photo", "send_document",
                            "set_chat_description", "send_inline_keyboard",
                            "answer_callback", "send_location", "send_contact"
                        ],
                        "description": "Telegram action to perform"
                    },
                    "chat_id": {
                        "type": "string",
                        "description": "Target chat ID"
                    },
                    "message_id": {
                        "type": "integer",
                        "description": "Message ID for edit/delete/pin operations"
                    },
                    "text": {
                        "type": "string",
                        "description": "Message text content"
                    },
                    "parse_mode": {
                        "type": "string",
                        "enum": ["Markdown", "MarkdownV2", "HTML"],
                        "description": "Text formatting mode"
                    },
                    "question": {
                        "type": "string",
                        "description": "Poll question"
                    },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Poll options (2-10 items)"
                    },
                    "keyboard": {
                        "type": "array",
                        "description": "Inline keyboard rows, each row is array of {text, callback_data}"
                    },
                    "callback_query_id": {
                        "type": "string",
                        "description": "Callback query ID for answering"
                    },
                    "from_chat_id": {
                        "type": "string",
                        "description": "Source chat ID for forwarding"
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

        debug!(action, "Telegram action requested");

        match action {
            "send_message" => {
                let chat_id = args.get("chat_id").and_then(|v| v.as_str())
                    .ok_or("Missing 'chat_id'")?;
                let text = args.get("text").and_then(|v| v.as_str())
                    .ok_or("Missing 'text'")?;
                let parse_mode = args.get("parse_mode").and_then(|v| v.as_str());
                Ok(json!({
                    "action": "send_message",
                    "chat_id": chat_id,
                    "text": text,
                    "parse_mode": parse_mode,
                    "status": "routed_to_channel"
                }).to_string())
            }
            "create_poll" => {
                let chat_id = args.get("chat_id").and_then(|v| v.as_str())
                    .ok_or("Missing 'chat_id'")?;
                let question = args.get("question").and_then(|v| v.as_str())
                    .ok_or("Missing 'question'")?;
                let options = args.get("options")
                    .ok_or("Missing 'options' array")?;
                Ok(json!({
                    "action": "create_poll",
                    "chat_id": chat_id,
                    "question": question,
                    "options": options,
                    "status": "routed_to_channel"
                }).to_string())
            }
            "send_inline_keyboard" => {
                let chat_id = args.get("chat_id").and_then(|v| v.as_str())
                    .ok_or("Missing 'chat_id'")?;
                let text = args.get("text").and_then(|v| v.as_str())
                    .ok_or("Missing 'text'")?;
                let keyboard = args.get("keyboard")
                    .ok_or("Missing 'keyboard'")?;
                Ok(json!({
                    "action": "send_inline_keyboard",
                    "chat_id": chat_id,
                    "text": text,
                    "keyboard": keyboard,
                    "status": "routed_to_channel"
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
        let tool = TelegramActionsTool::new();
        assert_eq!(tool.name(), "telegram_action");
    }

    #[tokio::test]
    async fn send_message_action() {
        let tool = TelegramActionsTool::new();
        let result = tool
            .execute(json!({
                "action": "send_message",
                "chat_id": "12345",
                "text": "Hello from agent!"
            }))
            .await
            .unwrap();
        assert!(result.contains("routed_to_channel"));
    }
}
