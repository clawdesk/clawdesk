//! GAP-D: Event Bus Integration — materializes the reactive event bus.
//!
//! Wires `clawdesk-bus::EventBus` into the application lifecycle:
//! - Initializes the bus at startup
//! - Publishes lifecycle events (message sent, cron executed, etc.)
//! - Spawns the inbound bridge to convert channel messages → bus events
//! - Registers default subscriptions for pipeline triggers
//!
//! ## Event Flow
//!
//! ```text
//! Channel Adapter → InboundBridge → EventBus.publish()
//!                                      ↓
//!                               Subscription match
//!                                      ↓
//!                               Pipeline trigger
//! ```

use clawdesk_bus::dispatch::EventBus;
use clawdesk_bus::event::{EventKind, Priority};
use clawdesk_bus::inbound::{InboundBridge, InboundBridgeConfig, InboundEnvelopeEvent, InboundError};
use clawdesk_bus::subscription::Subscription;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

/// Default bus topic capacity (128 events per topic ring buffer).
const DEFAULT_BUS_CAPACITY: usize = 128;

/// Initialize the event bus with standard topics and subscriptions.
///
/// Returns an `Arc<EventBus>` ready for use in AppState/GatewayState.
pub async fn init_event_bus() -> Arc<EventBus> {
    let bus = EventBus::new(DEFAULT_BUS_CAPACITY);

    // Pre-create standard topics to avoid lazy creation on the hot path.
    let standard_topics = [
        "channel.inbound.telegram",
        "channel.inbound.discord",
        "channel.inbound.slack",
        "channel.inbound.webchat",
        "channel.inbound.internal",
        "agent.message.sent",
        "cron.task.executed",
        "cron.heartbeat.fired",
        "pipeline.completed",
        "memory.stored",
        "skill.lifecycle",
        "system.startup",
        "system.shutdown",
    ];

    for topic in &standard_topics {
        bus.topic(topic).await;
    }

    info!(topics = standard_topics.len(), "Event bus initialized");
    bus
}

/// Publish a "message sent" event to the bus.
///
/// Call this after the agent produces a response.
pub async fn emit_message_sent(
    bus: &EventBus,
    session_id: &str,
    agent_id: &str,
    message_preview: &str,
    tokens_used: Option<u64>,
) {
    let payload = serde_json::json!({
        "session_id": session_id,
        "agent_id": agent_id,
        "preview": if message_preview.len() > 200 {
            &message_preview[..200]
        } else {
            message_preview
        },
        "tokens_used": tokens_used,
    });

    bus.emit(
        "agent.message.sent",
        EventKind::MessageSent,
        Priority::Standard,
        payload,
        format!("agent:{}", agent_id),
    )
    .await;
}

/// Publish a "cron executed" event to the bus.
pub async fn emit_cron_executed(
    bus: &EventBus,
    task_id: &str,
    task_name: &str,
    success: bool,
    duration_ms: u64,
) {
    let payload = serde_json::json!({
        "task_id": task_id,
        "task_name": task_name,
        "success": success,
        "duration_ms": duration_ms,
    });

    bus.emit(
        "cron.task.executed",
        EventKind::CronExecuted,
        Priority::Standard,
        payload,
        format!("cron:{}", task_id),
    )
    .await;
}

/// Publish a "memory stored" event to the bus.
pub async fn emit_memory_stored(
    bus: &EventBus,
    session_id: &str,
    memory_type: &str,
    content_preview: &str,
) {
    let payload = serde_json::json!({
        "session_id": session_id,
        "memory_type": memory_type,
        "preview": if content_preview.len() > 100 {
            &content_preview[..100]
        } else {
            content_preview
        },
    });

    bus.emit(
        "memory.stored",
        EventKind::MemoryStored,
        Priority::Batch,
        payload,
        "memory-manager",
    )
    .await;
}

/// Publish a "pipeline completed" event to the bus.
pub async fn emit_pipeline_completed(
    bus: &EventBus,
    pipeline_id: &str,
    pipeline_name: &str,
    success: bool,
    step_count: usize,
) {
    let payload = serde_json::json!({
        "pipeline_id": pipeline_id,
        "pipeline_name": pipeline_name,
        "success": success,
        "step_count": step_count,
    });

    bus.emit(
        "pipeline.completed",
        EventKind::PipelineCompleted,
        Priority::Standard,
        payload,
        format!("pipeline:{}", pipeline_id),
    )
    .await;
}

/// Emit a startup event that external subscriptions can react to.
pub async fn emit_startup(bus: &EventBus) {
    let payload = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    bus.emit(
        "system.startup",
        EventKind::Custom("SystemStartup".into()),
        Priority::Urgent,
        payload,
        "system",
    )
    .await;
}

/// Spawn the inbound bridge — converts channel adapter envelopes into bus events.
///
/// Returns the sender channel for injecting `InboundEnvelopeEvent`s.
/// The bridge task runs until the cancel token is triggered.
pub fn spawn_inbound_bridge(
    bus: Arc<EventBus>,
    cancel: CancellationToken,
) -> mpsc::Sender<Result<InboundEnvelopeEvent, InboundError>> {
    let (tx, rx) = mpsc::channel(256);

    let config = InboundBridgeConfig::default();
    InboundBridge::spawn(bus, rx, config, cancel);

    info!("Inbound bridge spawned");
    tx
}

/// Register a subscription linking a topic pattern to a pipeline.
///
/// This is the reactive trigger: when events matching the pattern arrive,
/// the subscription's pipeline_id is returned by `EventBus::publish()`.
pub async fn register_trigger(
    bus: &EventBus,
    name: &str,
    topic_pattern: &str,
    event_kind: Option<EventKind>,
    pipeline_id: &str,
) {
    let sub = Subscription {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        topic_patterns: vec![topic_pattern.to_string()],
        event_kinds: event_kind.map(|k| vec![k]).unwrap_or_default(),
        min_priority: None,
        pipeline_id: pipeline_id.to_string(),
        enabled: true,
        batch_size: 1,
        flush_interval_secs: 0,
    };

    info!(
        name = name,
        topic = topic_pattern,
        pipeline = pipeline_id,
        "Registered reactive trigger"
    );
    bus.subscribe(sub).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_init_event_bus() {
        let bus = init_event_bus().await;
        let topics = bus.list_topics().await;
        assert!(topics.len() >= 10, "Expected standard topics to be pre-created");
    }

    #[tokio::test]
    async fn test_emit_message_sent() {
        let bus = init_event_bus().await;
        // Should not panic
        emit_message_sent(&bus, "s1", "agent1", "Hello world", Some(50)).await;
    }

    #[tokio::test]
    async fn test_emit_cron_executed() {
        let bus = init_event_bus().await;
        emit_cron_executed(&bus, "t1", "Daily check", true, 1234).await;
    }

    #[tokio::test]
    async fn test_spawn_bridge_cancel() {
        let bus = init_event_bus().await;
        let cancel = CancellationToken::new();
        let tx = spawn_inbound_bridge(bus, cancel.clone());
        cancel.cancel();
        // Sender should still be valid
        assert!(!tx.is_closed());
    }

    #[tokio::test]
    async fn test_register_trigger() {
        let bus = init_event_bus().await;
        register_trigger(
            &bus,
            "test-trigger",
            "channel.inbound.*",
            Some(EventKind::MessageReceived),
            "pipeline-123",
        )
        .await;

        let subs = bus.list_subscriptions().await;
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].pipeline_id, "pipeline-123");
    }
}
