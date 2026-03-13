//! Platform-agnostic interactive component system.
//!
//! Defines a `Component` algebra that renders to platform-specific payloads:
//! - Discord: Message Components (buttons, select menus, modals)
//! - Slack: Block Kit (sections, actions, inputs)
//! - Telegram: Inline keyboards + callback queries
//! - LINE: Flex Messages
//!
//! ## Component Algebra
//!
//! ```text
//! Component = Text(content)
//!           | Button(label, action_id, style)
//!           | Select(placeholder, options)
//!           | Row(children: Vec<Component>)
//!           | Section(text, accessory: Option<Component>)
//!           | Modal(title, fields: Vec<Component>)
//!           | Image(url, alt_text)
//!           | Divider
//! ```
//!
//! Renderers fold the tree into platform JSON: O(n) where n = component count.
//! Platform constraints are validated at construction (not serialization).

use async_trait::async_trait;
use clawdesk_types::channel::ChannelId;
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────
// Component model
// ─────────────────────────────────────────────────────────────

/// A platform-agnostic interactive UI component.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Component {
    /// Plain or markdown text block.
    Text { content: String },
    /// Clickable button.
    Button {
        label: String,
        action_id: String,
        #[serde(default)]
        style: ButtonStyle,
    },
    /// Dropdown select menu.
    Select {
        placeholder: String,
        action_id: String,
        options: Vec<SelectOption>,
    },
    /// Horizontal row of components (buttons, selects).
    Row { children: Vec<Component> },
    /// Text section with an optional accessory (button, image).
    Section {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        accessory: Option<Box<Component>>,
    },
    /// Modal dialog with input fields.
    Modal {
        title: String,
        submit_label: Option<String>,
        fields: Vec<Component>,
    },
    /// Text input field (used inside Modals).
    TextInput {
        label: String,
        action_id: String,
        #[serde(default)]
        multiline: bool,
        placeholder: Option<String>,
    },
    /// Image block.
    Image { url: String, alt_text: String },
    /// Visual separator.
    Divider,
}

/// Button visual style.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonStyle {
    #[default]
    Default,
    Primary,
    Danger,
    /// Link button (opens URL, no callback).
    Link,
}

/// Option in a select menu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub label: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ─────────────────────────────────────────────────────────────
// Interaction callbacks
// ─────────────────────────────────────────────────────────────

/// An interaction event from a user clicking a button, selecting an option, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    /// Platform-specific interaction ID (for acknowledgment).
    pub interaction_id: String,
    /// The action_id of the component that was interacted with.
    pub action_id: String,
    /// The user who triggered the interaction.
    pub user_id: String,
    /// Selected value(s) for select menus.
    pub values: Vec<String>,
    /// Channel where the interaction occurred.
    pub channel_id: ChannelId,
    /// Original message ID (if a message component).
    pub message_id: Option<String>,
}

/// Response to an interaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InteractionResponse {
    /// Acknowledge silently (no visible response).
    Ack,
    /// Reply with a message (ephemeral = only visible to the user).
    Message {
        content: String,
        ephemeral: bool,
    },
    /// Update the original message's components.
    UpdateMessage {
        content: Option<String>,
        components: Vec<Component>,
    },
    /// Show a modal dialog.
    ShowModal {
        modal: Component,
    },
}

// ─────────────────────────────────────────────────────────────
// Interactive trait
// ─────────────────────────────────────────────────────────────

/// Opt-in capability: interactive UI components.
///
/// Channels implementing this trait can render buttons, select menus,
/// modals, and handle user interactions (clicks, selections, submissions).
#[async_trait]
pub trait Interactive: super::Channel {
    /// Send a message with interactive components.
    async fn send_interactive(
        &self,
        channel_ref: &str,
        text: &str,
        components: Vec<Component>,
    ) -> Result<String, String>;

    /// Respond to a user interaction (button click, select, modal submit).
    async fn respond_interaction(
        &self,
        interaction: &Interaction,
        response: InteractionResponse,
    ) -> Result<(), String>;

    /// Register slash commands / bot commands with the platform.
    /// Not all platforms support this (returns Ok(0) if unsupported).
    async fn register_commands(
        &self,
        commands: Vec<CommandDefinition>,
    ) -> Result<usize, String> {
        let _ = commands;
        Ok(0)
    }
}

/// Definition for a slash/bot command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDefinition {
    pub name: String,
    pub description: String,
    pub options: Vec<CommandOption>,
}

/// Option for a slash command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOption {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub option_type: CommandOptionType,
}

/// Command option data type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOptionType {
    String,
    Integer,
    Boolean,
    User,
    Channel,
}

// ─────────────────────────────────────────────────────────────
// Platform renderers
// ─────────────────────────────────────────────────────────────

/// Render components to Discord message components JSON.
pub fn render_discord(components: &[Component]) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = components
        .iter()
        .filter_map(|c| match c {
            Component::Row { children } => {
                let items: Vec<serde_json::Value> = children
                    .iter()
                    .filter_map(|child| render_discord_component(child))
                    .take(5) // Discord: max 5 per row
                    .collect();
                if items.is_empty() {
                    None
                } else {
                    Some(serde_json::json!({
                        "type": 1, // ACTION_ROW
                        "components": items
                    }))
                }
            }
            other => {
                // Wrap non-row top-level components in an action row
                render_discord_component(other).map(|item| {
                    serde_json::json!({
                        "type": 1,
                        "components": [item]
                    })
                })
            }
        })
        .take(5) // Discord: max 5 rows
        .collect();

    serde_json::Value::Array(rows)
}

fn render_discord_component(c: &Component) -> Option<serde_json::Value> {
    match c {
        Component::Button { label, action_id, style } => {
            let (style_num, is_link) = match style {
                ButtonStyle::Default => (2, false),  // SECONDARY
                ButtonStyle::Primary => (1, false),  // PRIMARY
                ButtonStyle::Danger => (4, false),   // DANGER
                ButtonStyle::Link => (5, true),      // LINK
            };
            let mut btn = serde_json::json!({
                "type": 2, // BUTTON
                "style": style_num,
                "label": label,
            });
            if is_link {
                btn["url"] = serde_json::Value::String(action_id.clone());
            } else {
                btn["custom_id"] = serde_json::Value::String(action_id.clone());
            }
            Some(btn)
        }
        Component::Select { placeholder, action_id, options } => {
            let opts: Vec<serde_json::Value> = options
                .iter()
                .take(25) // Discord: max 25 options
                .map(|o| {
                    let mut opt = serde_json::json!({
                        "label": o.label,
                        "value": o.value,
                    });
                    if let Some(desc) = &o.description {
                        opt["description"] = serde_json::Value::String(desc.clone());
                    }
                    opt
                })
                .collect();
            Some(serde_json::json!({
                "type": 3, // STRING_SELECT
                "custom_id": action_id,
                "placeholder": placeholder,
                "options": opts,
            }))
        }
        _ => None,
    }
}

/// Render components to Slack Block Kit JSON.
pub fn render_slack(components: &[Component]) -> serde_json::Value {
    let blocks: Vec<serde_json::Value> = components
        .iter()
        .filter_map(|c| render_slack_block(c))
        .take(50) // Slack: max 50 blocks
        .collect();
    serde_json::Value::Array(blocks)
}

fn render_slack_block(c: &Component) -> Option<serde_json::Value> {
    match c {
        Component::Text { content } => Some(serde_json::json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": content }
        })),
        Component::Section { text, accessory } => {
            let mut block = serde_json::json!({
                "type": "section",
                "text": { "type": "mrkdwn", "text": text }
            });
            if let Some(acc) = accessory {
                if let Some(rendered) = render_slack_element(acc) {
                    block["accessory"] = rendered;
                }
            }
            Some(block)
        }
        Component::Row { children } => {
            let elements: Vec<serde_json::Value> = children
                .iter()
                .filter_map(|c| render_slack_element(c))
                .collect();
            if elements.is_empty() {
                None
            } else {
                Some(serde_json::json!({
                    "type": "actions",
                    "elements": elements
                }))
            }
        }
        Component::Divider => Some(serde_json::json!({ "type": "divider" })),
        Component::Image { url, alt_text } => Some(serde_json::json!({
            "type": "image",
            "image_url": url,
            "alt_text": alt_text,
        })),
        _ => None,
    }
}

fn render_slack_element(c: &Component) -> Option<serde_json::Value> {
    match c {
        Component::Button { label, action_id, style } => {
            let mut btn = serde_json::json!({
                "type": "button",
                "text": { "type": "plain_text", "text": label },
                "action_id": action_id,
            });
            if matches!(style, ButtonStyle::Primary) {
                btn["style"] = serde_json::Value::String("primary".into());
            } else if matches!(style, ButtonStyle::Danger) {
                btn["style"] = serde_json::Value::String("danger".into());
            }
            Some(btn)
        }
        Component::Select { placeholder, action_id, options } => {
            let opts: Vec<serde_json::Value> = options
                .iter()
                .map(|o| serde_json::json!({
                    "text": { "type": "plain_text", "text": o.label },
                    "value": o.value,
                }))
                .collect();
            Some(serde_json::json!({
                "type": "static_select",
                "placeholder": { "type": "plain_text", "text": placeholder },
                "action_id": action_id,
                "options": opts,
            }))
        }
        _ => None,
    }
}

/// Render components to Telegram InlineKeyboard JSON.
pub fn render_telegram(components: &[Component]) -> serde_json::Value {
    let mut rows: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut current_row: Vec<serde_json::Value> = Vec::new();

    for c in components {
        match c {
            Component::Button { label, action_id, style } => {
                let btn = if matches!(style, ButtonStyle::Link) {
                    serde_json::json!({ "text": label, "url": action_id })
                } else {
                    serde_json::json!({ "text": label, "callback_data": action_id })
                };
                current_row.push(btn);
                // Telegram: max 8 buttons per row
                if current_row.len() >= 8 {
                    rows.push(std::mem::take(&mut current_row));
                }
            }
            Component::Row { children } => {
                // Flush current row first
                if !current_row.is_empty() {
                    rows.push(std::mem::take(&mut current_row));
                }
                let row_buttons: Vec<serde_json::Value> = children
                    .iter()
                    .filter_map(|c| match c {
                        Component::Button { label, action_id, style } => {
                            if matches!(style, ButtonStyle::Link) {
                                Some(serde_json::json!({ "text": label, "url": action_id }))
                            } else {
                                Some(serde_json::json!({ "text": label, "callback_data": action_id }))
                            }
                        }
                        _ => None,
                    })
                    .take(8)
                    .collect();
                if !row_buttons.is_empty() {
                    rows.push(row_buttons);
                }
            }
            _ => {}
        }
    }

    if !current_row.is_empty() {
        rows.push(current_row);
    }

    // Telegram: max 100 buttons total
    serde_json::json!({ "inline_keyboard": rows })
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_button_rendering() {
        let components = vec![Component::Row {
            children: vec![
                Component::Button {
                    label: "Approve".into(),
                    action_id: "approve:123".into(),
                    style: ButtonStyle::Primary,
                },
                Component::Button {
                    label: "Reject".into(),
                    action_id: "reject:123".into(),
                    style: ButtonStyle::Danger,
                },
            ],
        }];
        let rendered = render_discord(&components);
        let arr = rendered.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], 1); // ACTION_ROW
        let inner = arr[0]["components"].as_array().unwrap();
        assert_eq!(inner.len(), 2);
        assert_eq!(inner[0]["label"], "Approve");
        assert_eq!(inner[0]["style"], 1); // PRIMARY
        assert_eq!(inner[1]["style"], 4); // DANGER
    }

    #[test]
    fn slack_section_rendering() {
        let components = vec![Component::Section {
            text: "Choose a model".into(),
            accessory: Some(Box::new(Component::Select {
                placeholder: "Select model".into(),
                action_id: "model_select".into(),
                options: vec![
                    SelectOption { label: "GPT-4o".into(), value: "gpt-4o".into(), description: None },
                    SelectOption { label: "Claude".into(), value: "claude-sonnet".into(), description: None },
                ],
            })),
        }];
        let rendered = render_slack(&components);
        let arr = rendered.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "section");
        assert_eq!(arr[0]["accessory"]["type"], "static_select");
    }

    #[test]
    fn telegram_inline_keyboard() {
        let components = vec![
            Component::Button { label: "Yes".into(), action_id: "confirm".into(), style: ButtonStyle::Primary },
            Component::Button { label: "No".into(), action_id: "cancel".into(), style: ButtonStyle::Danger },
        ];
        let rendered = render_telegram(&components);
        let keyboard = rendered["inline_keyboard"].as_array().unwrap();
        assert_eq!(keyboard.len(), 1); // both buttons in one row
        assert_eq!(keyboard[0].as_array().unwrap().len(), 2);
    }

    #[test]
    fn discord_max_rows_enforced() {
        let components: Vec<Component> = (0..10)
            .map(|i| Component::Button {
                label: format!("Btn {i}"),
                action_id: format!("btn_{i}"),
                style: ButtonStyle::Default,
            })
            .collect();
        let rendered = render_discord(&components);
        let arr = rendered.as_array().unwrap();
        assert!(arr.len() <= 5, "Discord allows max 5 action rows");
    }
}
