//! Orchestrator Bridge — bidirectional connection between the Event Bus
//! and the Orchestration Loop.
//!
//! ## Inbound: Events → Planner
//!
//! When the bus receives an event (webhook, cron trigger, channel message),
//! the bridge can spawn a new task DAG or inject a node into an existing one.
//!
//! ## Outbound: Planner → Bus
//!
//! When the planner marks a task as completed, the bridge emits a
//! `TaskCompletedEvent` so downstream listeners can react.
//!
//! ## Priority Mapping
//!
//! The `WfqScheduler` priority classes map to task urgency:
//! - Urgent (w=8)   → P0: user-facing real-time responses
//! - Standard (w=4) → P1: background tasks with SLA
//! - Batch (w=1)    → P2: batch processing, speculative work

use crate::dispatch::EventBus;
use crate::event::{Event, EventKind, Priority};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Orchestrator event types
// ═══════════════════════════════════════════════════════════════════════════

/// Events emitted by the orchestration loop for bus consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrchestratorBusEvent {
    /// A task DAG was created from an inbound event.
    DagSpawned {
        dag_id: String,
        trigger_event_id: String,
        total_nodes: usize,
    },
    /// A task node completed execution.
    TaskCompleted {
        dag_id: String,
        node_id: String,
        duration_ms: u64,
        output_summary: String,
    },
    /// A task node failed.
    TaskFailed {
        dag_id: String,
        node_id: String,
        error: String,
    },
    /// A rewrite was applied to the DAG.
    DagRewritten {
        dag_id: String,
        rule: String,
        nodes_affected: usize,
    },
    /// The entire orchestration completed.
    OrchestrationCompleted {
        dag_id: String,
        completed_nodes: usize,
        failed_nodes: usize,
        duration_ms: u64,
    },
    /// A task was escalated to a human.
    TaskEscalated {
        dag_id: String,
        node_id: String,
        reason: String,
    },
}

/// Maps an event kind to orchestration priority.
///
/// Priority mapping:
/// - Urgent:   user messages, security alerts, approval responses
/// - Standard: webhook triggers, cron results, pipeline completions
/// - Batch:    digests, backups, catalog syncs
pub fn event_to_task_priority(kind: &EventKind) -> TaskPriority {
    match kind {
        EventKind::MessageReceived => TaskPriority::Realtime,
        EventKind::ApprovalRequested | EventKind::ApprovalResolved => TaskPriority::Realtime,
        EventKind::EmailIngested => TaskPriority::Background,
        EventKind::CalendarEvent => TaskPriority::Background,
        EventKind::TranscriptReady => TaskPriority::Background,
        EventKind::PipelineCompleted => TaskPriority::Background,
        EventKind::CronExecuted => TaskPriority::Background,
        EventKind::HeartbeatFired => TaskPriority::Background,
        EventKind::SkillInstalled
        | EventKind::SkillUninstalled
        | EventKind::SkillUpdated
        | EventKind::SkillActivated
        | EventKind::SkillDeactivated => TaskPriority::Background,
        EventKind::DigestWindowClosed => TaskPriority::Batch,
        EventKind::BackupCompleted => TaskPriority::Batch,
        EventKind::CatalogSynced => TaskPriority::Batch,
        EventKind::SocialMetricSnapshot => TaskPriority::Batch,
        EventKind::RelationshipAlert => TaskPriority::Background,
        _ => TaskPriority::Background,
    }
}

/// Task urgency level (maps to WFQ priority classes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskPriority {
    /// P0: User-facing real-time (maps to Urgent/w=8)
    Realtime,
    /// P1: Background with SLA (maps to Standard/w=4)
    Background,
    /// P2: Batch/speculative (maps to Batch/w=1)
    Batch,
}

impl TaskPriority {
    /// Convert to bus priority for event emission.
    pub fn to_bus_priority(self) -> Priority {
        match self {
            TaskPriority::Realtime => Priority::Urgent,
            TaskPriority::Background => Priority::Standard,
            TaskPriority::Batch => Priority::Batch,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Orchestrator plan request (inbound event → task DAG)
// ═══════════════════════════════════════════════════════════════════════════

/// Request to create a task DAG from an inbound event.
///
/// The orchestrator bridge converts inbound bus events into these
/// requests, which the orchestration loop then processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRequest {
    /// Unique ID for this plan request.
    pub id: String,
    /// The event that triggered this request.
    pub trigger_event_id: String,
    /// Natural language description of what to accomplish.
    pub goal: String,
    /// Priority level.
    pub priority: TaskPriority,
    /// Additional context from the triggering event.
    pub context: serde_json::Value,
}

// ═══════════════════════════════════════════════════════════════════════════
// Orchestrator Bridge
// ═══════════════════════════════════════════════════════════════════════════

/// Bidirectional bridge between the EventBus and the orchestration loop.
///
/// - **Inbound**: Converts bus events into `PlanRequest`s.
/// - **Outbound**: Publishes `OrchestratorBusEvent`s back to the bus.
pub struct OrchestratorBridge {
    bus: Arc<EventBus>,
    /// Channel for sending plan requests to the orchestration loop.
    plan_tx: mpsc::UnboundedSender<PlanRequest>,
    /// Event patterns that trigger orchestration.
    trigger_patterns: Vec<TriggerPattern>,
}

/// Defines which events should trigger orchestration.
#[derive(Debug, Clone)]
pub struct TriggerPattern {
    /// Event kind to match.
    pub event_kind: EventKind,
    /// Goal template — the `{payload}` placeholder is replaced with event data.
    pub goal_template: String,
    /// Priority override (if None, derived from event kind).
    pub priority: Option<TaskPriority>,
}

impl OrchestratorBridge {
    pub fn new(
        bus: Arc<EventBus>,
        plan_tx: mpsc::UnboundedSender<PlanRequest>,
    ) -> Self {
        Self {
            bus,
            plan_tx,
            trigger_patterns: Vec::new(),
        }
    }

    /// Register a trigger pattern that spawns orchestration on matching events.
    pub fn add_trigger(&mut self, pattern: TriggerPattern) {
        self.trigger_patterns.push(pattern);
    }

    /// Evaluate an inbound event against registered trigger patterns.
    ///
    /// If a pattern matches, a `PlanRequest` is sent to the orchestration loop.
    pub fn evaluate_event(&self, event: &Event) {
        for pattern in &self.trigger_patterns {
            if pattern.event_kind == event.kind {
                let priority = pattern
                    .priority
                    .unwrap_or_else(|| event_to_task_priority(&event.kind));

                // Substitute payload into goal template
                let goal = pattern
                    .goal_template
                    .replace("{payload}", &event.payload.to_string());

                let request = PlanRequest {
                    id: uuid::Uuid::new_v4().to_string(),
                    trigger_event_id: event.id.to_string(),
                    goal,
                    priority,
                    context: event.payload.clone(),
                };

                debug!(
                    event_id = %event.id,
                    plan_id = %request.id,
                    kind = ?event.kind,
                    "orchestrator bridge: event triggered plan request"
                );

                if let Err(e) = self.plan_tx.send(request) {
                    error!("failed to send plan request: {}", e);
                }
            }
        }
    }

    /// Publish an orchestrator event back to the bus.
    ///
    /// This closes the feedback loop: orchestration results are visible
    /// to all bus subscribers (cron, telemetry, other skills).
    pub async fn publish_orchestrator_event(&self, event: OrchestratorBusEvent) {
        let (topic, kind, priority) = match &event {
            OrchestratorBusEvent::TaskCompleted { .. } => (
                "orchestrator.task.completed",
                EventKind::PipelineCompleted,
                Priority::Standard,
            ),
            OrchestratorBusEvent::TaskFailed { .. } => (
                "orchestrator.task.failed",
                EventKind::Custom("orchestrator_task_failed".to_string()),
                Priority::Urgent,
            ),
            OrchestratorBusEvent::DagSpawned { .. } => (
                "orchestrator.dag.spawned",
                EventKind::Custom("orchestrator_dag_spawned".to_string()),
                Priority::Standard,
            ),
            OrchestratorBusEvent::DagRewritten { .. } => (
                "orchestrator.dag.rewritten",
                EventKind::Custom("orchestrator_dag_rewritten".to_string()),
                Priority::Batch,
            ),
            OrchestratorBusEvent::OrchestrationCompleted { .. } => (
                "orchestrator.completed",
                EventKind::PipelineCompleted,
                Priority::Standard,
            ),
            OrchestratorBusEvent::TaskEscalated { .. } => (
                "orchestrator.task.escalated",
                EventKind::ApprovalRequested,
                Priority::Urgent,
            ),
        };

        let payload =
            serde_json::to_value(&event).unwrap_or(serde_json::json!({"error": "serialization failed"}));

        let bus_event =
            Event::new(topic, kind, priority, payload, "orchestrator");

        self.bus.publish(bus_event).await;
    }
}
