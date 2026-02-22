//! Inbound event bridge — wires inbound adapters to the event bus.
//!
//! Consumes the merged `InboundEnvelope` stream from `InboundAdapterRegistry`
//! and publishes each envelope as a source-attributed `MessageReceived` event
//! on the event bus.
//!
//! ## Source Attribution
//!
//! Each published event preserves the origin channel identity in its payload,
//! enabling the reply dispatcher to route responses back to the correct
//! channel and thread. The `correlation_id` links the inbound event to
//! any downstream agent events for end-to-end tracing.
//!
//! ## Backpressure
//!
//! The bridge respects the bus's bounded channel capacity. When the agent
//! runner falls behind, backpressure propagates through the mpsc channel
//! to the individual adapters, which handle it according to their design
//! (e.g., buffering, dropping, or pausing the external subscription).
//!
//! ## Throughput Model
//!
//! Under Poisson arrivals (rate λ_in) with service rate μ:
//! - Steady-state queue length L = λ_in / (μ - λ_in)  [M/M/1]
//! - Stable when λ_in < μ

use crate::dispatch::EventBus;
use crate::event::{Event, EventKind, Priority};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Configuration for the inbound bridge.
#[derive(Debug, Clone)]
pub struct InboundBridgeConfig {
    /// Topic prefix for inbound message events.
    pub topic_prefix: String,
    /// Default priority for inbound messages.
    pub default_priority: Priority,
    /// Whether to include full message body in event payload.
    pub include_body: bool,
}

impl Default for InboundBridgeConfig {
    fn default() -> Self {
        Self {
            topic_prefix: "channel.inbound".to_string(),
            default_priority: Priority::Standard,
            include_body: true,
        }
    }
}

/// Bridges inbound channel adapters to the event bus.
///
/// Spawns a Tokio task that reads from the merged adapter stream and
/// publishes events. Cancellation is cooperative via `CancellationToken`.
pub struct InboundBridge;

impl InboundBridge {
    /// Start the bridge, consuming envelopes and publishing to the bus.
    ///
    /// Returns a `JoinHandle` for the bridge task. The task runs until:
    /// - The receiver is exhausted (all adapters stopped)
    /// - The cancellation token is triggered
    pub fn spawn(
        bus: Arc<EventBus>,
        mut rx: mpsc::Receiver<Result<InboundEnvelopeEvent, InboundError>>,
        config: InboundBridgeConfig,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<BridgeStats> {
        tokio::spawn(async move {
            let mut stats = BridgeStats::default();

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        info!("Inbound bridge cancelled");
                        break;
                    }
                    msg = rx.recv() => {
                        match msg {
                            Some(Ok(envelope)) => {
                                let topic = format!(
                                    "{}.{}",
                                    config.topic_prefix,
                                    envelope.channel
                                );

                                let correlation_id = Uuid::new_v4();
                                let payload = serde_json::json!({
                                    "message_id": envelope.message_id,
                                    "channel": envelope.channel.to_string(),
                                    "sender_id": envelope.sender_id,
                                    "sender_name": envelope.sender_name,
                                    "body": if config.include_body {
                                        Some(&envelope.body)
                                    } else {
                                        None
                                    },
                                    "reply_path": envelope.reply_path_json,
                                    "source_adapter": envelope.source_adapter,
                                    "timestamp": envelope.timestamp,
                                });

                                let event = Event::new(
                                    &topic,
                                    EventKind::MessageReceived,
                                    config.default_priority,
                                    payload,
                                    format!("adapter:{}", envelope.source_adapter),
                                ).with_correlation(correlation_id);

                                let matched = bus.publish(event).await;
                                stats.events_published += 1;
                                stats.subscriptions_triggered += matched.len() as u64;

                                debug!(
                                    channel = %envelope.channel,
                                    message_id = %envelope.message_id,
                                    matched = matched.len(),
                                    "Inbound message published to bus"
                                );
                            }
                            Some(Err(err)) => {
                                warn!(
                                    error = %err.message,
                                    adapter = %err.adapter,
                                    "Inbound adapter error"
                                );
                                stats.adapter_errors += 1;
                            }
                            None => {
                                info!("All inbound adapters stopped, bridge shutting down");
                                break;
                            }
                        }
                    }
                }
            }

            stats
        })
    }
}

/// Flattened envelope for bus publishing (avoids clawdesk-channel dependency).
#[derive(Debug, Clone)]
pub struct InboundEnvelopeEvent {
    pub message_id: String,
    pub channel: clawdesk_types::channel::ChannelId,
    pub sender_id: String,
    pub sender_name: String,
    pub body: String,
    pub reply_path_json: serde_json::Value,
    pub source_adapter: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Error from an inbound adapter, tagged with the adapter ID.
#[derive(Debug, Clone)]
pub struct InboundError {
    pub adapter: String,
    pub message: String,
    pub retryable: bool,
}

/// Statistics from the inbound bridge.
#[derive(Debug, Clone, Default)]
pub struct BridgeStats {
    pub events_published: u64,
    pub adapter_errors: u64,
    pub subscriptions_triggered: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bridge_stats_default() {
        let stats = BridgeStats::default();
        assert_eq!(stats.events_published, 0);
        assert_eq!(stats.adapter_errors, 0);
    }

    #[tokio::test]
    async fn test_bridge_config_default() {
        let config = InboundBridgeConfig::default();
        assert_eq!(config.topic_prefix, "channel.inbound");
        assert!(config.include_body);
    }

    #[tokio::test]
    async fn test_bridge_shutdown_on_cancel() {
        let bus = EventBus::new(100);
        let (tx, rx) = mpsc::channel(10);
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        let handle = InboundBridge::spawn(
            bus,
            rx,
            InboundBridgeConfig::default(),
            cancel_clone,
        );

        // Cancel immediately
        cancel.cancel();
        let stats = handle.await.unwrap();
        assert_eq!(stats.events_published, 0);
    }

    #[tokio::test]
    async fn test_bridge_shutdown_on_channel_close() {
        let bus = EventBus::new(100);
        let (tx, rx) = mpsc::channel(10);
        let cancel = CancellationToken::new();

        let handle = InboundBridge::spawn(
            bus,
            rx,
            InboundBridgeConfig::default(),
            cancel,
        );

        // Drop sender to close channel
        drop(tx);
        let stats = handle.await.unwrap();
        assert_eq!(stats.events_published, 0);
    }
}
