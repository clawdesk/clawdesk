//! Discord Application Commands and Interactions (API v10).
//!
//! Implements Discord's Interactions protocol:
//! - Application Commands (Slash commands, context menus)
//! - Message Components (Buttons, Select Menus)
//! - Modal dialogs (Text Inputs)
//!
//! ## Interaction lifecycle
//!
//! 1. User triggers interaction (slash command, button click, etc.)
//! 2. Discord sends INTERACTION_CREATE via Gateway WebSocket
//! 3. Bot must ACK within 3 seconds or the interaction fails
//! 4. Bot can follow up / edit for 15 minutes via the interaction token
//!
//! ## Custom ID routing
//!
//! Components use `custom_id` for routing: `{prefix}:{action}:{payload}`
//! - prefix: subsystem identifier (e.g. "mdl" for model picker)
//! - action: enum variant
//! - payload: base64url-encoded state (≤100 chars total)

use clawdesk_channel::interactive::{
    ButtonStyle, CommandDefinition, CommandOption, CommandOptionType, Component,
    Interaction, InteractionResponse, SelectOption,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::DISCORD_API_BASE;

// ─────────────────────────────────────────────────────────────
// Discord Interaction types (from Gateway INTERACTION_CREATE)
// ─────────────────────────────────────────────────────────────

/// Discord interaction type (from the API).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InteractionType {
    Ping = 1,
    ApplicationCommand = 2,
    MessageComponent = 3,
    ApplicationCommandAutocomplete = 4,
    ModalSubmit = 5,
}

impl InteractionType {
    pub fn from_u64(v: u64) -> Option<Self> {
        match v {
            1 => Some(Self::Ping),
            2 => Some(Self::ApplicationCommand),
            3 => Some(Self::MessageComponent),
            4 => Some(Self::ApplicationCommandAutocomplete),
            5 => Some(Self::ModalSubmit),
            _ => None,
        }
    }
}

/// Callback type for interaction responses.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InteractionCallbackType {
    Pong = 1,
    ChannelMessageWithSource = 4,
    DeferredChannelMessageWithSource = 5,
    DeferredUpdateMessage = 6,
    UpdateMessage = 7,
    ApplicationCommandAutocompleteResult = 8,
    Modal = 9,
}

// ─────────────────────────────────────────────────────────────
// Interaction token store (for follow-up / edit)
// ─────────────────────────────────────────────────────────────

/// Active interaction token with expiry tracking.
#[derive(Debug, Clone)]
struct ActiveInteraction {
    token: String,
    application_id: String,
    created_at: std::time::Instant,
}

impl ActiveInteraction {
    /// Interaction tokens are valid for 15 minutes.
    fn is_expired(&self) -> bool {
        self.created_at.elapsed() > std::time::Duration::from_secs(15 * 60)
    }
}

/// Manages active interaction tokens for follow-up messages.
pub struct InteractionStore {
    active: RwLock<HashMap<String, ActiveInteraction>>,
}

impl InteractionStore {
    pub fn new() -> Self {
        Self {
            active: RwLock::new(HashMap::new()),
        }
    }

    /// Store a new interaction token.
    pub async fn insert(&self, interaction_id: &str, token: &str, application_id: &str) {
        let mut map = self.active.write().await;
        map.insert(
            interaction_id.to_string(),
            ActiveInteraction {
                token: token.to_string(),
                application_id: application_id.to_string(),
                created_at: std::time::Instant::now(),
            },
        );
    }

    /// Get a token if still valid, removing expired ones lazily.
    pub async fn get(&self, interaction_id: &str) -> Option<(String, String)> {
        let map = self.active.read().await;
        map.get(interaction_id)
            .filter(|i| !i.is_expired())
            .map(|i| (i.token.clone(), i.application_id.clone()))
    }

    /// Remove expired interactions (call periodically).
    pub async fn gc(&self) {
        let mut map = self.active.write().await;
        map.retain(|_, v| !v.is_expired());
    }
}

// ─────────────────────────────────────────────────────────────
// Interaction handler
// ─────────────────────────────────────────────────────────────

/// Handles Discord INTERACTION_CREATE events from the Gateway.
pub struct InteractionHandler {
    client: Client,
    application_id: String,
    bot_token: String,
    store: Arc<InteractionStore>,
    /// Registered interaction handlers by custom_id prefix.
    handlers: RwLock<HashMap<String, Arc<dyn InteractionCallback>>>,
}

/// Callback for handling interactions routed by custom_id prefix.
#[async_trait::async_trait]
pub trait InteractionCallback: Send + Sync + 'static {
    async fn handle(&self, interaction: &Interaction) -> InteractionResponse;
}

impl InteractionHandler {
    pub fn new(client: Client, application_id: &str, bot_token: &str) -> Self {
        Self {
            client,
            application_id: application_id.to_string(),
            bot_token: bot_token.to_string(),
            store: Arc::new(InteractionStore::new()),
            handlers: RwLock::new(HashMap::new()),
        }
    }

    /// Register a handler for a custom_id prefix.
    pub async fn register_handler(&self, prefix: &str, handler: Arc<dyn InteractionCallback>) {
        let mut handlers = self.handlers.write().await;
        handlers.insert(prefix.to_string(), handler);
    }

    /// Process a raw INTERACTION_CREATE event from the Gateway.
    pub async fn handle_event(&self, event: &serde_json::Value) -> Result<(), String> {
        let interaction_type = event
            .get("type")
            .and_then(|v| v.as_u64())
            .and_then(InteractionType::from_u64)
            .ok_or("unknown interaction type")?;

        let interaction_id = event
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or("missing interaction id")?;

        let token = event
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or("missing interaction token")?;

        // Store token for follow-up
        self.store
            .insert(interaction_id, token, &self.application_id)
            .await;

        match interaction_type {
            InteractionType::Ping => {
                self.respond_to_interaction(interaction_id, token, InteractionCallbackType::Pong, None)
                    .await
            }
            InteractionType::ApplicationCommand => {
                self.handle_command(event, interaction_id, token).await
            }
            InteractionType::MessageComponent => {
                self.handle_component(event, interaction_id, token).await
            }
            InteractionType::ModalSubmit => {
                self.handle_modal_submit(event, interaction_id, token).await
            }
            InteractionType::ApplicationCommandAutocomplete => {
                // Defer for now — send empty results
                self.respond_to_interaction(
                    interaction_id,
                    token,
                    InteractionCallbackType::ApplicationCommandAutocompleteResult,
                    Some(json!({ "choices": [] })),
                )
                .await
            }
        }
    }

    /// Handle a slash command interaction.
    async fn handle_command(
        &self,
        event: &serde_json::Value,
        interaction_id: &str,
        token: &str,
    ) -> Result<(), String> {
        let data = event.get("data").ok_or("missing data")?;
        let command_name = data
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let user_id = event
            .get("member")
            .and_then(|m| m.get("user"))
            .or_else(|| event.get("user"))
            .and_then(|u| u.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("0");

        let channel_id = event
            .get("channel_id")
            .and_then(|v| v.as_str())
            .unwrap_or("0");

        info!(
            command = command_name,
            user_id,
            channel_id,
            "Discord: slash command received"
        );

        let interaction = Interaction {
            interaction_id: interaction_id.to_string(),
            action_id: format!("cmd:{command_name}"),
            user_id: user_id.to_string(),
            values: extract_command_options(data),
            channel_id: clawdesk_types::channel::ChannelId::Discord,
            message_id: None,
        };

        let response = self.route_interaction(&interaction).await;
        self.send_response(interaction_id, token, &response).await
    }

    /// Handle a message component interaction (button, select menu).
    async fn handle_component(
        &self,
        event: &serde_json::Value,
        interaction_id: &str,
        token: &str,
    ) -> Result<(), String> {
        let data = event.get("data").ok_or("missing data")?;
        let custom_id = data
            .get("custom_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let user_id = event
            .get("member")
            .and_then(|m| m.get("user"))
            .or_else(|| event.get("user"))
            .and_then(|u| u.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("0");

        let values: Vec<String> = data
            .get("values")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let message_id = event
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);

        debug!(custom_id, user_id, "Discord: component interaction");

        let interaction = Interaction {
            interaction_id: interaction_id.to_string(),
            action_id: custom_id.to_string(),
            user_id: user_id.to_string(),
            values,
            channel_id: clawdesk_types::channel::ChannelId::Discord,
            message_id,
        };

        let response = self.route_interaction(&interaction).await;
        self.send_response(interaction_id, token, &response).await
    }

    /// Handle a modal submission.
    async fn handle_modal_submit(
        &self,
        event: &serde_json::Value,
        interaction_id: &str,
        token: &str,
    ) -> Result<(), String> {
        let data = event.get("data").ok_or("missing data")?;
        let custom_id = data
            .get("custom_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let user_id = event
            .get("member")
            .and_then(|m| m.get("user"))
            .or_else(|| event.get("user"))
            .and_then(|u| u.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("0");

        // Extract submitted values from modal components
        let values = extract_modal_values(data);

        let interaction = Interaction {
            interaction_id: interaction_id.to_string(),
            action_id: custom_id.to_string(),
            user_id: user_id.to_string(),
            values,
            channel_id: clawdesk_types::channel::ChannelId::Discord,
            message_id: None,
        };

        let response = self.route_interaction(&interaction).await;
        self.send_response(interaction_id, token, &response).await
    }

    /// Route an interaction to the appropriate handler by custom_id prefix.
    async fn route_interaction(&self, interaction: &Interaction) -> InteractionResponse {
        let prefix = interaction
            .action_id
            .split(':')
            .next()
            .unwrap_or("");

        let handlers = self.handlers.read().await;
        if let Some(handler) = handlers.get(prefix) {
            handler.handle(interaction).await
        } else {
            debug!(prefix, "no handler registered for interaction prefix");
            InteractionResponse::Ack
        }
    }

    /// Send a response to Discord's Interaction endpoint.
    async fn send_response(
        &self,
        interaction_id: &str,
        token: &str,
        response: &InteractionResponse,
    ) -> Result<(), String> {
        match response {
            InteractionResponse::Ack => {
                self.respond_to_interaction(
                    interaction_id,
                    token,
                    InteractionCallbackType::DeferredUpdateMessage,
                    None,
                )
                .await
            }
            InteractionResponse::Message { content, ephemeral } => {
                let mut data = json!({ "content": content });
                if *ephemeral {
                    data["flags"] = json!(64); // EPHEMERAL flag
                }
                self.respond_to_interaction(
                    interaction_id,
                    token,
                    InteractionCallbackType::ChannelMessageWithSource,
                    Some(data),
                )
                .await
            }
            InteractionResponse::UpdateMessage { content, components } => {
                let mut data = json!({});
                if let Some(text) = content {
                    data["content"] = json!(text);
                }
                if !components.is_empty() {
                    data["components"] =
                        clawdesk_channel::interactive::render_discord(components);
                }
                self.respond_to_interaction(
                    interaction_id,
                    token,
                    InteractionCallbackType::UpdateMessage,
                    Some(data),
                )
                .await
            }
            InteractionResponse::ShowModal { modal } => {
                if let Component::Modal {
                    title,
                    submit_label,
                    fields,
                } = modal
                {
                    let modal_data = build_discord_modal(title, submit_label.as_deref(), fields);
                    self.respond_to_interaction(
                        interaction_id,
                        token,
                        InteractionCallbackType::Modal,
                        Some(modal_data),
                    )
                    .await
                } else {
                    Err("ShowModal response requires a Modal component".into())
                }
            }
        }
    }

    /// Send the raw interaction callback to Discord.
    async fn respond_to_interaction(
        &self,
        interaction_id: &str,
        token: &str,
        callback_type: InteractionCallbackType,
        data: Option<serde_json::Value>,
    ) -> Result<(), String> {
        let url = format!(
            "{DISCORD_API_BASE}/interactions/{interaction_id}/{token}/callback"
        );

        let mut body = json!({ "type": callback_type as u8 });
        if let Some(d) = data {
            body["data"] = d;
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("interaction callback failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(
                status = %status,
                body = %body,
                "Discord: interaction callback error"
            );
            return Err(format!("interaction callback {status}: {body}"));
        }

        Ok(())
    }

    /// Register application commands with Discord's API.
    pub async fn register_commands(
        &self,
        commands: &[CommandDefinition],
    ) -> Result<usize, String> {
        let url = format!(
            "{DISCORD_API_BASE}/applications/{}/commands",
            self.application_id
        );

        let discord_commands: Vec<serde_json::Value> = commands
            .iter()
            .map(|cmd| {
                let options: Vec<serde_json::Value> = cmd
                    .options
                    .iter()
                    .map(|opt| {
                        json!({
                            "name": opt.name,
                            "description": opt.description,
                            "type": command_option_type_to_discord(&opt.option_type),
                            "required": opt.required,
                        })
                    })
                    .collect();

                json!({
                    "name": cmd.name,
                    "description": cmd.description,
                    "type": 1, // CHAT_INPUT
                    "options": options,
                })
            })
            .collect();

        let resp = self
            .client
            .put(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&discord_commands)
            .send()
            .await
            .map_err(|e| format!("register commands failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("register commands {status}: {body}"));
        }

        info!(count = commands.len(), "Discord: registered application commands");
        Ok(commands.len())
    }
}

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

fn command_option_type_to_discord(opt: &CommandOptionType) -> u8 {
    match opt {
        CommandOptionType::String => 3,
        CommandOptionType::Integer => 4,
        CommandOptionType::Boolean => 5,
        CommandOptionType::User => 6,
        CommandOptionType::Channel => 7,
    }
}

fn extract_command_options(data: &serde_json::Value) -> Vec<String> {
    data.get("options")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|opt| {
                    let name = opt.get("name")?.as_str()?;
                    let value = opt.get("value")?;
                    Some(format!("{name}={value}"))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn extract_modal_values(data: &serde_json::Value) -> Vec<String> {
    let mut values = Vec::new();
    if let Some(components) = data.get("components").and_then(|v| v.as_array()) {
        for row in components {
            if let Some(row_components) = row.get("components").and_then(|v| v.as_array()) {
                for component in row_components {
                    if let (Some(id), Some(value)) = (
                        component.get("custom_id").and_then(|v| v.as_str()),
                        component.get("value").and_then(|v| v.as_str()),
                    ) {
                        values.push(format!("{id}={value}"));
                    }
                }
            }
        }
    }
    values
}

fn build_discord_modal(
    title: &str,
    submit_label: Option<&str>,
    fields: &[Component],
) -> serde_json::Value {
    let custom_id = format!("modal:{}", title.to_lowercase().replace(' ', "_"));
    let components: Vec<serde_json::Value> = fields
        .iter()
        .filter_map(|field| match field {
            Component::TextInput {
                label,
                action_id,
                multiline,
                placeholder,
            } => Some(json!({
                "type": 1, // ACTION_ROW
                "components": [{
                    "type": 4, // TEXT_INPUT
                    "custom_id": action_id,
                    "label": label,
                    "style": if *multiline { 2 } else { 1 }, // PARAGRAPH : SHORT
                    "placeholder": placeholder.as_deref().unwrap_or(""),
                    "required": true,
                }]
            })),
            _ => None,
        })
        .take(5) // Discord: max 5 action rows in a modal
        .collect();

    let mut modal = json!({
        "title": title,
        "custom_id": custom_id,
        "components": components,
    });

    if let Some(label) = submit_label {
        modal["submit_label"] = json!(label);
    }

    modal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_type_from_u64() {
        assert_eq!(
            InteractionType::from_u64(2),
            Some(InteractionType::ApplicationCommand)
        );
        assert_eq!(
            InteractionType::from_u64(3),
            Some(InteractionType::MessageComponent)
        );
        assert_eq!(InteractionType::from_u64(99), None);
    }

    #[test]
    fn extract_slash_command_options() {
        let data = json!({
            "options": [
                {"name": "model", "value": "gpt-4o"},
                {"name": "temperature", "value": 0.7}
            ]
        });
        let opts = extract_command_options(&data);
        assert_eq!(opts.len(), 2);
        assert!(opts[0].contains("model="));
    }

    #[test]
    fn modal_values_extraction() {
        let data = json!({
            "components": [{
                "components": [{
                    "custom_id": "feedback_text",
                    "value": "Great work!"
                }]
            }]
        });
        let values = extract_modal_values(&data);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0], "feedback_text=Great work!");
    }

    #[test]
    fn discord_modal_building() {
        let fields = vec![Component::TextInput {
            label: "Feedback".into(),
            action_id: "fb_input".into(),
            multiline: true,
            placeholder: Some("Enter feedback...".into()),
        }];
        let modal = build_discord_modal("Submit Feedback", Some("Send"), &fields);
        assert_eq!(modal["title"], "Submit Feedback");
        assert!(modal["components"].as_array().unwrap().len() == 1);
    }

    #[tokio::test]
    async fn interaction_store_lifecycle() {
        let store = InteractionStore::new();
        store.insert("int-1", "token-abc", "app-123").await;

        let result = store.get("int-1").await;
        assert!(result.is_some());
        let (token, app_id) = result.unwrap();
        assert_eq!(token, "token-abc");
        assert_eq!(app_id, "app-123");

        // Non-existent ID returns None
        assert!(store.get("int-999").await.is_none());
    }
}
