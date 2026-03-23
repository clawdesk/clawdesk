//! GAP-H: Service adapter bridge — wraps any `Channel` as an `InboundAdapter`.
//!
//! The existing 9 channels all implement `Channel::start(sink)` but none
//! implement `InboundAdapter`. This module provides a generic bridge that
//! converts the `MessageSink`-based push model into the `InboundAdapter`
//! → `InboundEnvelope` → EventBus pipeline.
//!
//! ## Architecture
//!
//! ```text
//! Channel::start(BridgeSink) ──> NormalizedMessage
//!                                   │
//!                            BridgeSink::on_message()
//!                                   │
//!                            ┌─── construct ───┐
//!                            │  InboundEnvelope │
//!                            │  + ReplyPath     │
//!                            └────────┬────────┘
//!                                     │
//!                              tx.send(envelope)
//!                                     │
//!                              InboundAdapterRegistry → EventBus
//! ```

use async_trait::async_trait;
use clawdesk_channel::inbound_adapter::{
    AdapterError, AdapterErrorKind, AdapterStatus, InboundAdapter, InboundEnvelope, ReplyPath,
};
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::ChannelId;
use clawdesk_types::message::NormalizedMessage;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Status encoding for AtomicU8.
const STATUS_IDLE: u8 = 0;
const STATUS_RUNNING: u8 = 1;
const STATUS_STOPPED: u8 = 3;
const STATUS_FAILED: u8 = 4;

/// Generic bridge that wraps any `Channel` into an `InboundAdapter`.
///
/// When `start()` is called, it starts the wrapped channel with a `BridgeSink`
/// that converts each `NormalizedMessage` into an `InboundEnvelope` and
/// pushes it to the adapter's mpsc channel.
pub struct ChannelBridgeAdapter {
    /// Unique adapter ID.
    adapter_id: String,
    /// The wrapped channel.
    channel: Arc<dyn Channel>,
    /// Thread preference for reply routing.
    prefer_thread: bool,
    /// Streaming preference for reply routing.
    prefer_streaming: bool,
    /// Current status.
    status: AtomicU8,
}

impl ChannelBridgeAdapter {
    /// Create a new bridge adapter wrapping an existing channel.
    pub fn new(channel: Arc<dyn Channel>) -> Self {
        let channel_id = channel.id();
        Self {
            adapter_id: format!("bridge:{}", channel_id),
            channel,
            prefer_thread: false,
            prefer_streaming: false,
            status: AtomicU8::new(STATUS_IDLE),
        }
    }

    /// Set thread preference for reply routing.
    pub fn with_thread_preference(mut self, prefer: bool) -> Self {
        self.prefer_thread = prefer;
        self
    }

    /// Set streaming preference for reply routing.
    pub fn with_streaming_preference(mut self, prefer: bool) -> Self {
        self.prefer_streaming = prefer;
        self
    }

    fn status_from_u8(val: u8) -> AdapterStatus {
        match val {
            STATUS_IDLE => AdapterStatus::Idle,
            STATUS_RUNNING => AdapterStatus::Running,
            STATUS_STOPPED => AdapterStatus::Stopped,
            STATUS_FAILED => AdapterStatus::Failed,
            _ => AdapterStatus::Failed,
        }
    }
}

#[async_trait]
impl InboundAdapter for ChannelBridgeAdapter {
    fn id(&self) -> &str {
        &self.adapter_id
    }

    fn channel(&self) -> ChannelId {
        self.channel.id()
    }

    async fn start(
        &self,
        tx: mpsc::Sender<Result<InboundEnvelope, AdapterError>>,
    ) -> Result<(), AdapterError> {
        let channel_id = self.channel.id();
        let adapter_id = self.adapter_id.clone();
        let prefer_thread = self.prefer_thread;
        let prefer_streaming = self.prefer_streaming;

        let sink = Arc::new(BridgeSink {
            tx,
            channel_id,
            adapter_id: adapter_id.clone(),
            prefer_thread,
            prefer_streaming,
        });

        self.channel
            .start(sink)
            .await
            .map_err(|e| AdapterError {
                kind: AdapterErrorKind::Internal,
                message: format!("channel start failed: {}", e),
                retryable: true,
            })?;

        self.status.store(STATUS_RUNNING, Ordering::Release);
        Ok(())
    }

    async fn stop(&self) -> Result<(), AdapterError> {
        self.channel.stop().await.map_err(|e| AdapterError {
            kind: AdapterErrorKind::Internal,
            message: format!("channel stop failed: {}", e),
            retryable: false,
        })?;
        self.status.store(STATUS_STOPPED, Ordering::Release);
        Ok(())
    }

    fn status(&self) -> AdapterStatus {
        Self::status_from_u8(self.status.load(Ordering::Acquire))
    }

    fn description(&self) -> String {
        format!(
            "Bridge adapter for {} (channel: {})",
            self.adapter_id,
            self.channel.id()
        )
    }
}

/// Internal message sink that bridges `Channel::start()` output into
/// the `InboundAdapter` mpsc pipeline.
struct BridgeSink {
    tx: mpsc::Sender<Result<InboundEnvelope, AdapterError>>,
    channel_id: ChannelId,
    adapter_id: String,
    prefer_thread: bool,
    prefer_streaming: bool,
}

#[async_trait]
impl MessageSink for BridgeSink {
    async fn on_message(&self, msg: NormalizedMessage) {
        let reply_path = ReplyPath {
            channel: self.channel_id,
            origin: msg.origin.clone(),
            prefer_thread: self.prefer_thread,
            prefer_streaming: self.prefer_streaming,
        };

        let envelope = InboundEnvelope {
            message: msg,
            reply_path,
            deduplicated: false,
            source_adapter: self.adapter_id.clone(),
        };

        // Best effort — if the receiver dropped, we silently discard.
        let _ = self.tx.send(Ok(envelope)).await;
    }
}

// ---------------------------------------------------------------------------
// Per-channel specialized builders
// ---------------------------------------------------------------------------

/// Create a bridge adapter for Telegram with thread preference enabled.
pub fn telegram_adapter(channel: Arc<dyn Channel>) -> ChannelBridgeAdapter {
    ChannelBridgeAdapter::new(channel).with_thread_preference(true)
}

/// Create a bridge adapter for Discord with thread + streaming preferences.
pub fn discord_adapter(channel: Arc<dyn Channel>) -> ChannelBridgeAdapter {
    ChannelBridgeAdapter::new(channel)
        .with_thread_preference(true)
        .with_streaming_preference(true)
}

/// Create a bridge adapter for Slack with thread preference enabled.
pub fn slack_adapter(channel: Arc<dyn Channel>) -> ChannelBridgeAdapter {
    ChannelBridgeAdapter::new(channel).with_thread_preference(true)
}

/// Create a bridge adapter for WebChat with streaming preference.
pub fn webchat_adapter(channel: Arc<dyn Channel>) -> ChannelBridgeAdapter {
    ChannelBridgeAdapter::new(channel).with_streaming_preference(true)
}

/// Create a bridge adapter for Email (no special preferences).
pub fn email_adapter(channel: Arc<dyn Channel>) -> ChannelBridgeAdapter {
    ChannelBridgeAdapter::new(channel)
}

/// Create a bridge adapter for WhatsApp.
pub fn whatsapp_adapter(channel: Arc<dyn Channel>) -> ChannelBridgeAdapter {
    ChannelBridgeAdapter::new(channel)
}

/// Create a bridge adapter for iMessage.
pub fn imessage_adapter(channel: Arc<dyn Channel>) -> ChannelBridgeAdapter {
    ChannelBridgeAdapter::new(channel)
}

/// Create a bridge adapter for IRC.
pub fn irc_adapter(channel: Arc<dyn Channel>) -> ChannelBridgeAdapter {
    ChannelBridgeAdapter::new(channel)
}

// ---------------------------------------------------------------------------
// Factory integration
// ---------------------------------------------------------------------------

/// Create the appropriate bridge adapter for a channel based on its ChannelId.
///
/// Uses per-channel builder functions that set the correct thread/streaming
/// preferences for each platform.
pub fn bridge_adapter_for(channel: Arc<dyn Channel>) -> ChannelBridgeAdapter {
    match channel.id() {
        ChannelId::Telegram => telegram_adapter(channel),
        ChannelId::Discord => discord_adapter(channel),
        ChannelId::Slack => slack_adapter(channel),
        ChannelId::WebChat => webchat_adapter(channel),
        ChannelId::Email => email_adapter(channel),
        ChannelId::WhatsApp => whatsapp_adapter(channel),
        ChannelId::IMessage => imessage_adapter(channel),
        ChannelId::Irc => irc_adapter(channel),
        ChannelId::Internal => ChannelBridgeAdapter::new(channel),
        ChannelId::Teams
        | ChannelId::Matrix
        | ChannelId::Signal
        | ChannelId::Webhook
        | ChannelId::Mastodon
        | ChannelId::Line => ChannelBridgeAdapter::new(channel),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_types::channel::ChannelMeta;
    use clawdesk_types::message::{
        DeliveryReceipt, MessageOrigin, OutboundMessage, SenderIdentity,
    };
    use clawdesk_types::session::SessionKey;

    /// Minimal test channel that pushes a message on start.
    struct TestChannel {
        id: ChannelId,
    }

    #[async_trait]
    impl Channel for TestChannel {
        fn id(&self) -> ChannelId {
            self.id
        }

        fn meta(&self) -> ChannelMeta {
            ChannelMeta::basic("test")
        }

        async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
            let channel_id = self.id;
            tokio::spawn(async move {
                let msg = NormalizedMessage {
                    id: uuid::Uuid::new_v4(),
                    session_key: SessionKey::new(channel_id, "test-user"),
                    body: "Hello from test channel".to_string(),
                    body_for_agent: None,
                    sender: SenderIdentity {
                        id: "test-user".to_string(),
                        display_name: "Test User".to_string(),
                        channel: channel_id,
                    },
                    media: vec![],
                    artifact_refs: vec![],
                    reply_context: None,
                    origin: MessageOrigin::Internal {
                        source: "test".to_string(),
                    },
                    timestamp: chrono::Utc::now(),
                };
                sink.on_message(msg).await;
            });
            Ok(())
        }

        async fn send(
            &self,
            _msg: OutboundMessage,
        ) -> Result<DeliveryReceipt, String> {
            Ok(DeliveryReceipt {
                message_id: "test-receipt".to_string(),
                channel: self.id,
                timestamp: chrono::Utc::now(),
                success: true,
                error: None,
            })
        }

        async fn stop(&self) -> Result<(), String> {
            Ok(())
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[tokio::test]
    async fn test_bridge_adapter_lifecycle() {
        let channel: Arc<dyn Channel> = Arc::new(TestChannel {
            id: ChannelId::Internal,
        });
        let adapter = ChannelBridgeAdapter::new(channel);

        assert_eq!(adapter.status(), AdapterStatus::Idle);

        let (tx, mut rx) = mpsc::channel(16);
        adapter.start(tx).await.unwrap();
        assert_eq!(adapter.status(), AdapterStatus::Running);

        // Wait for the test message
        let envelope = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed")
            .expect("adapter error");

        assert_eq!(envelope.message.body, "Hello from test channel");
        assert_eq!(envelope.reply_path.channel, ChannelId::Internal);
        assert_eq!(envelope.source_adapter, "bridge:internal");

        adapter.stop().await.unwrap();
        assert_eq!(adapter.status(), AdapterStatus::Stopped);
    }

    #[tokio::test]
    async fn test_bridge_adapter_preferences() {
        let channel: Arc<dyn Channel> = Arc::new(TestChannel {
            id: ChannelId::Telegram,
        });
        let adapter = telegram_adapter(channel);
        assert!(adapter.prefer_thread);
        assert!(!adapter.prefer_streaming);

        let (tx, mut rx) = mpsc::channel(16);
        adapter.start(tx).await.unwrap();

        let envelope = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed")
            .expect("adapter error");

        assert!(envelope.reply_path.prefer_thread);
        assert!(!envelope.reply_path.prefer_streaming);
    }

    #[tokio::test]
    async fn test_bridge_adapter_for_factory() {
        let channel: Arc<dyn Channel> = Arc::new(TestChannel {
            id: ChannelId::Discord,
        });
        let adapter = bridge_adapter_for(channel);
        assert!(adapter.prefer_thread);
        assert!(adapter.prefer_streaming);
        assert_eq!(adapter.id(), "bridge:discord");
    }

    #[tokio::test]
    async fn test_bridge_adapter_description() {
        let channel: Arc<dyn Channel> = Arc::new(TestChannel {
            id: ChannelId::Slack,
        });
        let adapter = slack_adapter(channel);
        let desc = adapter.description();
        assert!(desc.contains("bridge:slack"));
    }

    #[tokio::test]
    async fn test_all_channel_id_bridge_mapping() {
        // Ensure bridge_adapter_for handles all ChannelId variants
        let channel_ids = [
            ChannelId::Telegram,
            ChannelId::Discord,
            ChannelId::Slack,
            ChannelId::WebChat,
            ChannelId::Email,
            ChannelId::WhatsApp,
            ChannelId::IMessage,
            ChannelId::Irc,
            ChannelId::Internal,
        ];
        for cid in &channel_ids {
            let channel: Arc<dyn Channel> = Arc::new(TestChannel { id: *cid });
            let adapter = bridge_adapter_for(channel);
            assert_eq!(adapter.channel(), *cid);
        }
    }
}
