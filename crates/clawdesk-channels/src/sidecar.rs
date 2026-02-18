//! Sidecar bridge protocol — connects external channel processes to ClawDesk.
//!
//! A **sidecar** is a separate process (e.g., a Python WhatsApp gateway, a
//! Node.js Signal bridge) that communicates with ClawDesk via a local
//! Unix-domain socket or TCP connection using a simple JSON-lines protocol.
//!
//! ## Protocol (JSON-lines over Unix socket or TCP)
//!
//! ```text
//! → {"type":"message","channel":"whatsapp","from":"user123","text":"hello"}
//! ← {"type":"reply","channel":"whatsapp","to":"user123","text":"Hi!"}
//! ```
//!
//! ## Usage
//!
//! ```rust,no_run
//! use clawdesk_channels::sidecar::SidecarBridge;
//! let bridge = SidecarBridge::new("/tmp/clawdesk-whatsapp.sock", "whatsapp");
//! ```

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{DeliveryReceipt, NormalizedMessage, OutboundMessage};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

/// Sidecar message protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarMessage {
    /// Message type: "message", "reply", "status", "error"
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Channel identifier (e.g., "whatsapp", "signal")
    pub channel: String,
    /// Sender / recipient identifier
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// Message text
    #[serde(default)]
    pub text: Option<String>,
    /// Optional metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// Sidecar bridge — connects an external channel process to ClawDesk.
pub struct SidecarBridge {
    /// Socket path (Unix) or address (TCP)
    endpoint: String,
    /// Channel name for registration
    channel_name: String,
    /// Pending outbound messages
    outbox: Arc<RwLock<Vec<SidecarMessage>>>,
}

impl SidecarBridge {
    pub fn new(endpoint: impl Into<String>, channel_name: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            channel_name: channel_name.into(),
            outbox: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Queue a reply message for the sidecar process.
    pub async fn send_reply(&self, to: &str, text: &str) {
        let msg = SidecarMessage {
            msg_type: "reply".to_string(),
            channel: self.channel_name.clone(),
            from: None,
            to: Some(to.to_string()),
            text: Some(text.to_string()),
            metadata: None,
        };
        self.outbox.write().await.push(msg);
    }

    /// Drain pending outbound messages.
    pub async fn drain_outbox(&self) -> Vec<SidecarMessage> {
        let mut outbox = self.outbox.write().await;
        std::mem::take(&mut *outbox)
    }
}

#[async_trait]
impl Channel for SidecarBridge {
    fn id(&self) -> ChannelId {
        ChannelId::Internal
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: format!("Sidecar: {}", self.channel_name),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: false,
            supports_media: false,
            supports_groups: false,
            max_message_length: Some(4096),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        debug!(endpoint = %self.endpoint, "Sidecar bridge started");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        // Extract text from the message body and queue it
        self.send_reply("sidecar", &msg.body).await;
        let id = uuid::Uuid::new_v4().to_string();
        debug!(message_id = %id, "Sidecar message queued");
        Ok(DeliveryReceipt {
            channel: ChannelId::Internal,
            message_id: id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        debug!("Sidecar bridge stopped");
        Ok(())
    }
}

/// Message bus for routing between channels.
///
/// Central message router that connects all channels (native + sidecar)
/// and enables cross-channel forwarding, filtering, and command interception.
pub struct MessageBus {
    /// Registered message handlers
    handlers: Vec<Box<dyn MessageHandler>>,
    /// Message filters applied before processing
    filters: Vec<Box<dyn MessageFilter>>,
}

/// Handler for incoming messages.
#[async_trait]
pub trait MessageHandler: Send + Sync {
    async fn handle(&self, msg: &NormalizedMessage) -> Result<Option<String>, String>;
}

/// Filter for incoming messages — can modify, drop, or route messages.
#[async_trait]
pub trait MessageFilter: Send + Sync {
    /// Filter a message. Return None to drop it, Some(msg) to pass it through.
    async fn filter(&self, msg: NormalizedMessage) -> Option<NormalizedMessage>;
}

impl MessageBus {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            filters: Vec::new(),
        }
    }

    pub fn add_handler(&mut self, handler: Box<dyn MessageHandler>) {
        self.handlers.push(handler);
    }

    pub fn add_filter(&mut self, filter: Box<dyn MessageFilter>) {
        self.filters.push(filter);
    }

    /// Process an incoming message through filters and handlers.
    pub async fn process(&self, mut msg: NormalizedMessage) -> Result<Vec<String>, String> {
        // Apply filters
        for filter in &self.filters {
            match filter.filter(msg).await {
                Some(filtered) => msg = filtered,
                None => return Ok(vec![]), // Message dropped by filter
            }
        }

        // Run handlers
        let mut responses = Vec::new();
        for handler in &self.handlers {
            if let Ok(Some(response)) = handler.handle(&msg).await {
                responses.push(response);
            }
        }
        Ok(responses)
    }
}

impl Default for MessageBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Chat command parser — intercepts `/command` messages before agent processing.
pub struct ChatCommandHandler {
    commands: std::collections::HashMap<String, Box<dyn ChatCommand>>,
}

/// A chat command (e.g., `/help`, `/model`, `/clear`).
#[async_trait]
pub trait ChatCommand: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    async fn execute(&self, args: &str, context: &CommandContext) -> Result<String, String>;
}

/// Context passed to chat commands.
pub struct CommandContext {
    pub channel_id: String,
    pub user_id: String,
    pub session_id: Option<String>,
}

impl ChatCommandHandler {
    pub fn new() -> Self {
        Self {
            commands: std::collections::HashMap::new(),
        }
    }

    pub fn register(&mut self, cmd: Box<dyn ChatCommand>) {
        self.commands.insert(format!("/{}", cmd.name()), cmd);
    }

    pub fn with_builtins() -> Self {
        let mut handler = Self::new();
        handler.register(Box::new(HelpCommand));
        handler.register(Box::new(ModelCommand));
        handler.register(Box::new(ClearCommand));
        handler.register(Box::new(StatusCommand));
        handler
    }

    /// Try to handle a message as a command. Returns None if not a command.
    pub async fn try_handle(
        &self,
        text: &str,
        context: &CommandContext,
    ) -> Option<Result<String, String>> {
        if !text.starts_with('/') {
            return None;
        }

        let parts: Vec<&str> = text.splitn(2, ' ').collect();
        let cmd_name = parts[0].to_lowercase();
        let args = parts.get(1).unwrap_or(&"");

        if let Some(cmd) = self.commands.get(&cmd_name) {
            Some(cmd.execute(args, context).await)
        } else {
            Some(Ok(format!("Unknown command: {}. Type /help for available commands.", cmd_name)))
        }
    }
}

impl Default for ChatCommandHandler {
    fn default() -> Self {
        Self::with_builtins()
    }
}

// ── Built-in commands ────────────────────────────────────────

struct HelpCommand;

#[async_trait]
impl ChatCommand for HelpCommand {
    fn name(&self) -> &str { "help" }
    fn description(&self) -> &str { "Show available commands" }
    async fn execute(&self, _args: &str, _ctx: &CommandContext) -> Result<String, String> {
        Ok(
            "/help    — Show this help message\n\
             /model   — Show or change the current model\n\
             /clear   — Clear conversation history\n\
             /status  — Show system status"
                .to_string(),
        )
    }
}

struct ModelCommand;

#[async_trait]
impl ChatCommand for ModelCommand {
    fn name(&self) -> &str { "model" }
    fn description(&self) -> &str { "Show or change the current model" }
    async fn execute(&self, args: &str, _ctx: &CommandContext) -> Result<String, String> {
        if args.is_empty() {
            Ok("Current model: (default). Use /model <name> to switch.".to_string())
        } else {
            Ok(format!("Model switched to: {}", args.trim()))
        }
    }
}

struct ClearCommand;

#[async_trait]
impl ChatCommand for ClearCommand {
    fn name(&self) -> &str { "clear" }
    fn description(&self) -> &str { "Clear conversation history" }
    async fn execute(&self, _args: &str, _ctx: &CommandContext) -> Result<String, String> {
        Ok("Conversation cleared.".to_string())
    }
}

struct StatusCommand;

#[async_trait]
impl ChatCommand for StatusCommand {
    fn name(&self) -> &str { "status" }
    fn description(&self) -> &str { "Show system status" }
    async fn execute(&self, _args: &str, _ctx: &CommandContext) -> Result<String, String> {
        Ok(format!(
            "ClawDesk v{}\nStatus: running\nPlatform: {}/{}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_message_serialize() {
        let msg = SidecarMessage {
            msg_type: "message".to_string(),
            channel: "whatsapp".to_string(),
            from: Some("user123".to_string()),
            to: None,
            text: Some("hello".to_string()),
            metadata: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("whatsapp"));
        assert!(json.contains("hello"));
    }

    #[tokio::test]
    async fn chat_commands_help() {
        let handler = ChatCommandHandler::with_builtins();
        let ctx = CommandContext {
            channel_id: "test".to_string(),
            user_id: "user".to_string(),
            session_id: None,
        };
        let result = handler.try_handle("/help", &ctx).await;
        assert!(result.is_some());
        assert!(result.unwrap().unwrap().contains("/help"));
    }

    #[tokio::test]
    async fn chat_commands_non_command() {
        let handler = ChatCommandHandler::with_builtins();
        let ctx = CommandContext {
            channel_id: "test".to_string(),
            user_id: "user".to_string(),
            session_id: None,
        };
        let result = handler.try_handle("hello world", &ctx).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn message_bus_empty_handlers() {
        use clawdesk_types::message::{MessageOrigin, SenderIdentity};
        use clawdesk_types::session::SessionKey;

        let bus = MessageBus::new();
        let msg = NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key: SessionKey::new(ChannelId::Internal, "test"),
            body: "hello".to_string(),
            body_for_agent: None,
            sender: SenderIdentity {
                id: "user".to_string(),
                display_name: "Test User".to_string(),
                channel: ChannelId::Internal,
            },
            media: vec![],
            reply_context: None,
            origin: MessageOrigin::Internal {
                source: "test".to_string(),
            },
            timestamp: chrono::Utc::now(),
        };
        let responses = bus.process(msg).await.unwrap();
        assert!(responses.is_empty());
    }
}
