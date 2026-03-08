//! Tauri commands for orchestration — task DAG management and dispatch.
//!
//! Exposes the orchestration loop, task dispatcher, and capability index
//! to the frontend via IPC commands.

use crate::state::AppState;
use clawdesk_gateway::orchestrator::{
    build_dag_from_plan, OrchestratorConfig, OrchestrationEvent,
    OrchestrationLoop, OrchestrationResult, OrchestrationStatus, TaskPlan,
};
use clawdesk_gateway::task_dispatcher::{
    DispatchResult, DispatchRoute, DispatcherConfig, TaskDispatcher,
};
use clawdesk_planner::{DynamicTaskGraph, NodeStatus, TaskNode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{Emitter, State};
use tracing::{debug, error, info};

/// Tauri event channel for orchestration events.
pub const ORCHESTRATION_EVENT_NAME: &str = "orchestration-event";

// ═══════════════════════════════════════════════════════════════
// Frontend-facing types
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlanInput {
    pub tasks: Vec<TaskPlan>,
    pub edges: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationResultFrontend {
    pub status: String,
    pub outputs: HashMap<String, serde_json::Value>,
    pub duration_ms: u64,
    pub rewrite_count: usize,
    pub total_nodes: usize,
    pub completed_nodes: usize,
    pub failed_nodes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityInfo {
    pub action: String,
    pub skill_count: usize,
    pub skills: Vec<String>,
}

// ═══════════════════════════════════════════════════════════════
// Tauri Commands
// ═══════════════════════════════════════════════════════════════

/// List all registered capabilities in the capability index.
#[tauri::command]
pub async fn list_capabilities(
    state: State<'_, AppState>,
) -> Result<Vec<CapabilityInfo>, String> {
    let index = state.capability_index.read().map_err(|e| e.to_string())?;
    let actions = index.actions();
    let mut result = Vec::with_capacity(actions.len());
    for action in actions {
        let entries = index.find_by_action(action);
        result.push(CapabilityInfo {
            action: action.to_string(),
            skill_count: entries.len(),
            skills: entries.iter().map(|e| e.skill_id.clone()).collect(),
        });
    }
    Ok(result)
}

/// Get the current orchestration event channel status.
#[tauri::command]
pub async fn get_orchestration_status(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let index = state.capability_index.read().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "capabilities_registered": index.len(),
        "actions_available": index.actions().len(),
        "event_channel": ORCHESTRATION_EVENT_NAME,
    }))
}

/// Spawn the orchestration event bridge that forwards orchestration events
/// to the Tauri frontend via the `"orchestration-event"` channel.
///
/// Called once during app setup. Takes the receiver from AppState.
/// Spawns its own background thread with a dedicated Tokio runtime because
/// `.setup()` runs on the main thread before Tauri's async runtime is live.
pub fn spawn_orchestration_bridge(app: tauri::AppHandle, state: &AppState) {
    let rx = state
        .orchestration_event_rx
        .lock()
        .ok()
        .and_then(|mut guard| guard.take());

    let Some(rx) = rx else {
        tracing::warn!("Orchestration event bridge already spawned or rx unavailable");
        return;
    };

    let event_bus = state.event_bus.clone();

    std::thread::Builder::new()
        .name("orchestration-bridge".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("orchestration bridge runtime");
            rt.block_on(async move {
                let mut rx = rx;
                while let Some(event) = rx.recv().await {
                    // Emit to Tauri frontend
                    let event_json = serde_json::to_value(&event).unwrap_or_default();
                    if let Err(e) = app.emit(ORCHESTRATION_EVENT_NAME, &event_json) {
                        tracing::error!("Failed to emit orchestration event: {}", e);
                    }

                    // Also publish to the event bus for reactive pipeline triggers
                    let (topic, kind, priority) = match &event {
                        OrchestrationEvent::TaskCompleted { .. } => (
                            "orchestrator.task.completed",
                            clawdesk_bus::event::EventKind::PipelineCompleted,
                            clawdesk_bus::event::Priority::Standard,
                        ),
                        OrchestrationEvent::TaskFailed { .. } => (
                            "orchestrator.task.failed",
                            clawdesk_bus::event::EventKind::Custom(
                                "orchestrator_task_failed".into(),
                            ),
                            clawdesk_bus::event::Priority::Urgent,
                        ),
                        OrchestrationEvent::DagCreated { .. } => (
                            "orchestrator.dag.created",
                            clawdesk_bus::event::EventKind::Custom(
                                "orchestrator_dag_created".into(),
                            ),
                            clawdesk_bus::event::Priority::Standard,
                        ),
                        OrchestrationEvent::Finished { .. } => (
                            "orchestrator.finished",
                            clawdesk_bus::event::EventKind::PipelineCompleted,
                            clawdesk_bus::event::Priority::Standard,
                        ),
                        OrchestrationEvent::Escalated { .. } => (
                            "orchestrator.escalated",
                            clawdesk_bus::event::EventKind::ApprovalRequested,
                            clawdesk_bus::event::Priority::Urgent,
                        ),
                        _ => (
                            "orchestrator.event",
                            clawdesk_bus::event::EventKind::Custom(
                                "orchestrator_event".into(),
                            ),
                            clawdesk_bus::event::Priority::Batch,
                        ),
                    };

                    event_bus
                        .emit(topic, kind, priority, event_json, "orchestrator")
                        .await;
                }
                info!("Orchestration event bridge shut down");
            });
        })
        .expect("failed to spawn orchestration bridge thread");

    info!(
        "Orchestration event bridge spawned → Tauri channel '{}'",
        ORCHESTRATION_EVENT_NAME
    );
}
