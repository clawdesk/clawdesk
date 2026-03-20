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
        // Phase 1: Security & health
        "security.health.check",
        "security.permission.denied",
        "security.permission.granted",
        "diagnostics.check.completed",
        "diagnostics.fix.applied",
        // Phase 2: Skill ecosystem
        "skill.builder.compiled",
        "skill.builder.deployed",
        "skill.marketplace.installed",
        "skill.signature.verified",
        "skill.signature.failed",
        // Phase 3: Voice & proactive
        "voice.speech.detected",
        "voice.transcription.ready",
        "voice.tts.started",
        "voice.tts.completed",
        "voice.barge_in",
        "proactive.notification.pushed",
        "proactive.notification.dismissed",
        "memory.entry.created",
        "memory.entry.edited",
        "memory.entry.deleted",
        // Phase 4: Orchestration
        "orchestrator.task.started",
        "orchestrator.task.completed",
        "orchestrator.task.failed",
        "orchestrator.graph.rewritten",
        "orchestrator.checkpoint.awaiting",
        // Architecture: resource monitoring
        "resource.snapshot.updated",
        "daemon.sleep.entered",
        "daemon.wake.triggered",
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

// ── Strategic feature event emitters ─────────────────────────

/// Emit a security health check event.
pub async fn emit_security_health_check(
    bus: &EventBus,
    score: u32,
    grade: &str,
    critical_issues: usize,
) {
    let payload = serde_json::json!({
        "score": score,
        "grade": grade,
        "critical_issues": critical_issues,
    });
    bus.emit("security.health.check", EventKind::MemoryStored, Priority::Standard, payload, "security").await;
}

/// Emit a sandbox permission event.
pub async fn emit_permission_event(
    bus: &EventBus,
    tool_name: &str,
    granted: bool,
    capabilities: &str,
) {
    let topic = if granted { "security.permission.granted" } else { "security.permission.denied" };
    let payload = serde_json::json!({
        "tool": tool_name,
        "granted": granted,
        "capabilities": capabilities,
    });
    let priority = if granted { Priority::Standard } else { Priority::Urgent };
    bus.emit(topic, EventKind::ApprovalResolved, priority, payload, "sandbox").await;
}

/// Emit a voice pipeline event.
pub async fn emit_voice_event(
    bus: &EventBus,
    event_type: &str,
    detail: &str,
) {
    let topic = format!("voice.{}", event_type);
    let payload = serde_json::json!({ "detail": detail });
    bus.emit(&topic, EventKind::MessageReceived, Priority::Standard, payload, "voice").await;
}

/// Emit a skill builder event (compiled or deployed).
pub async fn emit_skill_builder_event(
    bus: &EventBus,
    action: &str,
    skill_id: &str,
    node_count: usize,
) {
    let topic = format!("skill.builder.{}", action);
    let payload = serde_json::json!({
        "skill_id": skill_id,
        "node_count": node_count,
    });
    bus.emit(&topic, EventKind::SkillActivated, Priority::Standard, payload, "skill_builder").await;
}

/// Emit an orchestrator task status change.
pub async fn emit_orchestrator_event(
    bus: &EventBus,
    event_type: &str,
    task_id: &str,
    agent_id: Option<&str>,
) {
    let topic = format!("orchestrator.{}", event_type);
    let payload = serde_json::json!({
        "task_id": task_id,
        "agent_id": agent_id,
    });
    bus.emit(&topic, EventKind::PipelineCompleted, Priority::Standard, payload, "orchestrator").await;
}

/// Emit a daemon power state change.
pub async fn emit_daemon_power_event(
    bus: &EventBus,
    event_type: &str,
) {
    let topic = format!("daemon.{}", event_type);
    bus.emit(&topic, EventKind::HeartbeatFired, Priority::Standard, serde_json::json!({}), "daemon").await;
}

/// Emit a memory transparency event (entry created, edited, deleted).
pub async fn emit_memory_event(
    bus: &EventBus,
    action: &str,
    entry_id: &str,
    category: &str,
) {
    let topic = format!("memory.entry.{}", action);
    let payload = serde_json::json!({
        "entry_id": entry_id,
        "category": category,
    });
    bus.emit(&topic, EventKind::MemoryStored, Priority::Standard, payload, "memory").await;
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
