//! # clawdesk-channel
//!
//! Layered capability trait system for channel plugins.
//!
//! Layer 0: `Channel` — the only required trait (receive + send)
//! Layer 1: Optional capabilities (`Threaded`, `Streaming`, `Reactions`, etc.)
//! Layer 2: Composed rich capabilities
//!
//! Additional modules:
//! - `health` — per-channel health monitoring (liveness, latency, error rates)
//! - `rate_limit` — per-channel outbound rate limiting (token bucket)
//! - `registry` — channel registration and lookup
//!
//! A minimal channel implementation is ~50 lines. The compiler prevents
//! calling capabilities that a channel doesn't implement.

pub mod channel_bridge;
pub mod channel_dock;
pub mod health;
pub mod inbound_adapter;
pub mod rate_limit;
pub mod registry;
pub mod reply_formatter;

use async_trait::async_trait;
use clawdesk_types::{
    channel::{ChannelId, ChannelMeta},
    message::{DeliveryReceipt, NormalizedMessage, OutboundMessage},
};
use std::sync::Arc;

/// Callback sink for inbound messages from a channel.
#[async_trait]
pub trait MessageSink: Send + Sync + 'static {
    /// Called when a channel receives an inbound message.
    async fn on_message(&self, msg: NormalizedMessage);
}

/// Streaming handle for partial message updates.
pub struct StreamHandle {
    pub message_id: String,
    pub update_fn: Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
}

impl StreamHandle {
    pub fn update(&self, text: &str) -> Result<(), String> {
        (self.update_fn)(text)
    }
}

/// Member info from a group channel.
#[derive(Debug, Clone)]
pub struct MemberInfo {
    pub id: String,
    pub display_name: String,
    pub role: Option<String>,
}

/// Directory lookup entry.
#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub id: String,
    pub display_name: String,
    pub channel: ChannelId,
}

/// Pairing session for channels that require device pairing (e.g., WhatsApp, Signal).
#[derive(Debug, Clone)]
pub struct PairingSession {
    pub session_id: String,
    pub qr_code: Option<String>,
    pub instructions: String,
}

#[derive(Debug, Clone)]
pub enum PairingResult {
    Success { device_id: String },
    Failed { reason: String },
    Timeout,
}

// ===========================================================================
// Layer 0: Required trait — the only thing a channel MUST implement
// ===========================================================================

/// Layer 0: The only required trait. A channel that can receive and send.
///
/// Implementing this trait is sufficient for a fully functional channel.
/// All other capabilities are opt-in.
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// Unique channel identifier.
    fn id(&self) -> ChannelId;

    /// Channel metadata (capabilities, display name, limits).
    fn meta(&self) -> ChannelMeta;

    /// Start receiving messages. Calls `sink.on_message()` for each inbound.
    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String>;

    /// Send an outbound reply.
    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String>;

    /// Stop the channel gracefully.
    async fn stop(&self) -> Result<(), String>;
}

// ===========================================================================
// Layer 1: Optional capabilities (each is an independent trait)
// ===========================================================================

/// Opt-in capability: thread support.
#[async_trait]
pub trait Threaded: Channel {
    async fn send_to_thread(
        &self,
        thread_id: &str,
        msg: OutboundMessage,
    ) -> Result<DeliveryReceipt, String>;

    async fn create_thread(
        &self,
        parent_msg_id: &str,
        title: &str,
    ) -> Result<String, String>;
}

/// Opt-in capability: streaming message updates.
#[async_trait]
pub trait Streaming: Channel {
    async fn send_streaming(
        &self,
        initial: OutboundMessage,
    ) -> Result<StreamHandle, String>;
}

/// Opt-in capability: message reactions.
#[async_trait]
pub trait Reactions: Channel {
    async fn add_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String>;
    async fn remove_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String>;
}

/// Opt-in capability: group member management.
#[async_trait]
pub trait GroupManagement: Channel {
    async fn list_members(&self, group_id: &str) -> Result<Vec<MemberInfo>, String>;
    async fn resolve_mention(&self, mention: &str) -> Result<Option<MemberInfo>, String>;
}

/// Opt-in capability: user directory lookup.
#[async_trait]
pub trait Directory: Channel {
    async fn lookup(&self, query: &str) -> Result<Vec<DirectoryEntry>, String>;
}

/// Opt-in capability: device pairing (WhatsApp, Signal, etc.).
#[async_trait]
pub trait Pairing: Channel {
    async fn start_pairing(&self) -> Result<PairingSession, String>;
    async fn complete_pairing(&self, code: &str) -> Result<PairingResult, String>;
}

// ===========================================================================
// Layer 2: Rich capabilities composed from Layer 1
// ===========================================================================

/// A channel that supports all rich capabilities.
pub trait RichChannel: Channel + Threaded + Streaming + Reactions {}

/// Auto-implement RichChannel for types that have all the traits.
impl<T: Channel + Threaded + Streaming + Reactions> RichChannel for T {}
