//! Internal channel — in-process testing channel.
//!
//! Used for unit tests, cron task delivery, and agent-to-agent
//! communication without network I/O.

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{DeliveryReceipt, OutboundMessage};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::debug;

/// Internal in-process channel for testing and inter-agent communication.
pub struct InternalChannel {
    /// Sent messages accumulate here for inspection.
    sent_tx: mpsc::UnboundedSender<OutboundMessage>,
    sent_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<OutboundMessage>>,
    sink: tokio::sync::RwLock<Option<Arc<dyn MessageSink>>>,
}

impl InternalChannel {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            sent_tx: tx,
            sent_rx: tokio::sync::Mutex::new(rx),
            sink: tokio::sync::RwLock::new(None),
        }
    }

    /// Inject a message into the channel (simulates inbound).
    pub async fn inject(
        &self,
        msg: clawdesk_types::message::NormalizedMessage,
    ) {
        let sink = self.sink.read().await;
        if let Some(ref s) = *sink {
            s.on_message(msg).await;
        }
    }

    /// Drain all sent messages (for test assertions).
    pub async fn drain_sent(&self) -> Vec<OutboundMessage> {
        let mut rx = self.sent_rx.lock().await;
        let mut msgs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            msgs.push(msg);
        }
        msgs
    }
}

#[async_trait]
impl Channel for InternalChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Internal
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Internal".into(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: false,
            supports_media: false,
            supports_groups: false,
            max_message_length: None,
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        *self.sink.write().await = Some(sink);
        debug!("Internal channel started");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let msg_id = uuid::Uuid::new_v4().to_string();
        let _ = self.sent_tx.send(msg);

        Ok(DeliveryReceipt {
            channel: ChannelId::Internal,
            message_id: msg_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        *self.sink.write().await = None;
        debug!("Internal channel stopped");
        Ok(())
    }
}

impl Default for InternalChannel {
    fn default() -> Self {
        Self::new()
    }
}
