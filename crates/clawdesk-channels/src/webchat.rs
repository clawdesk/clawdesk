//! WebChat channel — gateway WebSocket-based chat.
//!
//! The simplest channel: messages come in via the gateway's WebSocket
//! handler and go out via the same connection. This channel bridges
//! the gap between the generic channel trait system and the gateway's
//! built-in WS handler.
//!
//! This channel is always available (no external API keys needed)
//! and serves as the default testing channel.

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, StreamHandle, Streaming};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{DeliveryReceipt, NormalizedMessage, OutboundMessage};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::info;

/// WebChat channel — gateway-integrated WebSocket chat.
pub struct WebChatChannel {
    /// Broadcast sender for outbound messages.
    /// WS handler subscribes to receive messages to forward to clients.
    outbound_tx: broadcast::Sender<OutboundMessage>,
    /// Inbound message sink (set during start).
    sink: tokio::sync::RwLock<Option<Arc<dyn MessageSink>>>,
}

impl WebChatChannel {
    pub fn new() -> (Self, broadcast::Receiver<OutboundMessage>) {
        let (tx, rx) = broadcast::channel(256);
        (
            Self {
                outbound_tx: tx,
                sink: tokio::sync::RwLock::new(None),
            },
            rx,
        )
    }

    /// Inject an inbound message (called by the gateway WS handler).
    pub async fn inject_message(&self, msg: NormalizedMessage) {
        let sink = self.sink.read().await;
        if let Some(ref s) = *sink {
            s.on_message(msg).await;
        }
    }

    /// Subscribe to outbound messages.
    pub fn subscribe(&self) -> broadcast::Receiver<OutboundMessage> {
        self.outbound_tx.subscribe()
    }
}

#[async_trait]
impl Channel for WebChatChannel {
    fn id(&self) -> ChannelId {
        ChannelId::WebChat
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "WebChat".into(),
            supports_threading: false,
            supports_streaming: true,
            supports_reactions: false,
            supports_media: true,
            supports_groups: false,
            max_message_length: None, // no limit
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        *self.sink.write().await = Some(sink);
        info!("WebChat channel started");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let msg_id = uuid::Uuid::new_v4().to_string();

        // Broadcast to all subscribed WS connections
        let _ = self.outbound_tx.send(msg);

        Ok(DeliveryReceipt {
            channel: ChannelId::WebChat,
            message_id: msg_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        *self.sink.write().await = None;
        info!("WebChat channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait]
impl Streaming for WebChatChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        let msg_id = uuid::Uuid::new_v4().to_string();
        let tx = self.outbound_tx.clone();

        // Send the initial message
        let _ = tx.send(initial);

        // Return a handle that sends updates via the broadcast channel
        let _id = msg_id.clone();
        Ok(StreamHandle {
            message_id: msg_id,
            update_fn: Box::new(move |_text: &str| {
                // In a real impl, send a StreamUpdate message type
                Ok(())
            }),
        })
    }
}
