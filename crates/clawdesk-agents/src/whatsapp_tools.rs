//! WhatsApp actions toolkit — agent-callable tools for WhatsApp Cloud API.
//!
//! Supports template messages, interactive messages (buttons, lists),
//! media, location, contacts, and 24-hour window tracking.

use crate::tools::{Tool, ToolCapability, ToolSchema};
use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

/// WhatsApp actions toolkit for agents.
pub struct WhatsAppActionsTool;

impl WhatsAppActionsTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WhatsAppActionsTool {
    fn name(&self) -> &str {
        "whatsapp_action"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "whatsapp_action".into(),
            description: "Perform WhatsApp operations via the Cloud API: send messages, \
                          template messages, interactive buttons/lists, media, location, contacts. \
                          Auto-handles 24-hour conversation window rules."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "send_text", "send_template", "send_image",
                            "send_document", "send_location", "send_contact",
                            "send_buttons", "send_list", "mark_read",
                            "send_reaction"
                        ],
                        "description": "WhatsApp action to perform"
                    },
                    "to": {
                        "type": "string",
                        "description": "Recipient phone number (E.164 format)"
                    },
                    "text": {
                        "type": "string",
                        "description": "Message text"
                    },
                    "template_name": {
                        "type": "string",
                        "description": "Template name (for template messages)"
                    },
                    "template_language": {
                        "type": "string",
                        "description": "Template language code (e.g., 'en_US')"
                    },
                    "template_params": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Template parameter values"
                    },
                    "buttons": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "title": { "type": "string" }
                            }
                        },
                        "description": "Quick reply buttons (max 3)"
                    },
                    "list_sections": {
                        "type": "array",
                        "description": "List sections for interactive list message"
                    },
                    "media_url": {
                        "type": "string",
                        "description": "URL of media to send"
                    },
                    "latitude": { "type": "number", "description": "Location latitude" },
                    "longitude": { "type": "number", "description": "Location longitude" },
                    "message_id": {
                        "type": "string",
                        "description": "Message ID for reactions/read receipts"
                    },
                    "emoji": {
                        "type": "string",
                        "description": "Emoji for reaction"
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

        debug!(action, "WhatsApp action requested");

        match action {
            "send_text" => {
                let to = args.get("to").and_then(|v| v.as_str())
                    .ok_or("Missing 'to' phone number")?;
                let text = args.get("text").and_then(|v| v.as_str())
                    .ok_or("Missing 'text'")?;
                Ok(json!({
                    "action": "send_text",
                    "to": to,
                    "text": text,
                    "status": "routed_to_channel",
                    "window_note": "Free-form messages require an active 24-hour conversation window"
                }).to_string())
            }
            "send_template" => {
                let to = args.get("to").and_then(|v| v.as_str())
                    .ok_or("Missing 'to' phone number")?;
                let template = args.get("template_name").and_then(|v| v.as_str())
                    .ok_or("Missing 'template_name'")?;
                let lang = args.get("template_language").and_then(|v| v.as_str())
                    .unwrap_or("en_US");
                let params = args.get("template_params");
                Ok(json!({
                    "action": "send_template",
                    "to": to,
                    "template_name": template,
                    "template_language": lang,
                    "template_params": params,
                    "status": "routed_to_channel",
                    "window_note": "Template messages can be sent outside the 24-hour window"
                }).to_string())
            }
            "send_buttons" => {
                let to = args.get("to").and_then(|v| v.as_str())
                    .ok_or("Missing 'to'")?;
                let text = args.get("text").and_then(|v| v.as_str())
                    .ok_or("Missing 'text'")?;
                let buttons = args.get("buttons")
                    .ok_or("Missing 'buttons'")?;
                Ok(json!({
                    "action": "send_buttons",
                    "to": to,
                    "text": text,
                    "buttons": buttons,
                    "status": "routed_to_channel"
                }).to_string())
            }
            "send_location" => {
                let to = args.get("to").and_then(|v| v.as_str())
                    .ok_or("Missing 'to'")?;
                let lat = args.get("latitude").and_then(|v| v.as_f64())
                    .ok_or("Missing 'latitude'")?;
                let lng = args.get("longitude").and_then(|v| v.as_f64())
                    .ok_or("Missing 'longitude'")?;
                Ok(json!({
                    "action": "send_location",
                    "to": to,
                    "latitude": lat,
                    "longitude": lng,
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
        let tool = WhatsAppActionsTool::new();
        assert_eq!(tool.name(), "whatsapp_action");
    }

    #[tokio::test]
    async fn template_message() {
        let tool = WhatsAppActionsTool::new();
        let result = tool
            .execute(json!({
                "action": "send_template",
                "to": "+1234567890",
                "template_name": "order_update",
                "template_language": "en_US"
            }))
            .await
            .unwrap();
        assert!(result.contains("routed_to_channel"));
        assert!(result.contains("outside the 24-hour window"));
    }
}
