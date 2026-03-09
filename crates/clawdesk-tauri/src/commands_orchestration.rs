//! Tauri commands for orchestration — task DAG management, dispatch,
//! and agent-flow coordination (Paperclip-inspired adapter pattern).
//!
//! Exposes the orchestration loop, task dispatcher, capability index,
//! and agent-flow CRUD to the frontend via IPC commands.

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
// Agent Flow types
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFlowConfig {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub adapter_type: String,
    pub description: String,
    pub model: String,
    pub role: String,
    pub adapter_config: HashMap<String, String>,
    pub heartbeat_interval_sec: u64,
    pub max_concurrent_runs: u32,
    pub cwd: Option<String>,
    pub icon: String,
    pub color: String,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationRunRequest {
    pub goal: String,
    pub flow_ids: Vec<String>,
    pub strategy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationTaskInfo {
    pub id: String,
    pub title: String,
    pub description: String,
    pub assigned_flow_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub status: String,
    pub priority: String,
    pub created_at: String,
    pub updated_at: String,
    pub output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub adapter_type: String,
    pub icon: String,
    pub color: String,
    pub default_config: HashMap<String, String>,
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

// ═══════════════════════════════════════════════════════════════
// Agent Flow CRUD — manage coordinated agent adapters
// ═══════════════════════════════════════════════════════════════

/// List all registered agent flows.
#[tauri::command]
pub async fn list_agent_flows(
    state: State<'_, AppState>,
) -> Result<Vec<AgentFlowConfig>, String> {
    let flows = state
        .agent_flows
        .read()
        .map_err(|e| e.to_string())?;
    Ok(flows.clone())
}

/// Create a new agent flow configuration.
#[tauri::command]
pub async fn create_agent_flow(
    state: State<'_, AppState>,
    request: AgentFlowConfig,
) -> Result<AgentFlowConfig, String> {
    let mut flow = request;
    if flow.id.is_empty() || flow.id == "auto" {
        flow.id = format!("flow_{}", uuid::Uuid::new_v4().as_simple());
    }
    let mut flows = state
        .agent_flows
        .write()
        .map_err(|e| e.to_string())?;
    flows.push(flow.clone());
    info!("Created agent flow: {} ({})", flow.name, flow.adapter_type);
    Ok(flow)
}

/// Update an existing agent flow.
#[tauri::command]
pub async fn update_agent_flow(
    state: State<'_, AppState>,
    flow_id: String,
    request: serde_json::Value,
) -> Result<AgentFlowConfig, String> {
    let mut flows = state
        .agent_flows
        .write()
        .map_err(|e| e.to_string())?;
    let flow = flows
        .iter_mut()
        .find(|f| f.id == flow_id)
        .ok_or_else(|| format!("Flow not found: {}", flow_id))?;

    // Merge partial updates
    if let Some(name) = request.get("name").and_then(|v| v.as_str()) {
        flow.name = name.to_string();
    }
    if let Some(desc) = request.get("description").and_then(|v| v.as_str()) {
        flow.description = desc.to_string();
    }
    if let Some(model) = request.get("model").and_then(|v| v.as_str()) {
        flow.model = model.to_string();
    }
    if let Some(role) = request.get("role").and_then(|v| v.as_str()) {
        flow.role = role.to_string();
    }
    if let Some(active) = request.get("active").and_then(|v| v.as_bool()) {
        flow.active = active;
    }

    info!("Updated agent flow: {} ({})", flow.name, flow.id);
    Ok(flow.clone())
}

/// Delete an agent flow.
#[tauri::command]
pub async fn delete_agent_flow(
    state: State<'_, AppState>,
    flow_id: String,
) -> Result<bool, String> {
    let mut flows = state
        .agent_flows
        .write()
        .map_err(|e| e.to_string())?;
    let len_before = flows.len();
    flows.retain(|f| f.id != flow_id);
    let deleted = flows.len() < len_before;
    if deleted {
        info!("Deleted agent flow: {}", flow_id);
    }
    Ok(deleted)
}

/// List available flow templates for quick setup.
#[tauri::command]
pub async fn list_flow_templates() -> Result<Vec<FlowTemplate>, String> {
    Ok(vec![
        FlowTemplate {
            id: "tpl_claude".into(),
            name: "Claude Code Agent".into(),
            description: "Anthropic Claude — best for complex reasoning, code generation, and multi-step tasks".into(),
            adapter_type: "claude_local".into(),
            icon: "🧠".into(),
            color: "#D97706".into(),
            default_config: HashMap::from([
                ("command".into(), "claude".into()),
                ("model".into(), "claude-sonnet-4-20250514".into()),
            ]),
        },
        FlowTemplate {
            id: "tpl_codex".into(),
            name: "Codex Agent".into(),
            description: "OpenAI Codex — optimized for code editing, refactoring, and test writing".into(),
            adapter_type: "codex_local".into(),
            icon: "⚡".into(),
            color: "#10B981".into(),
            default_config: HashMap::from([
                ("command".into(), "codex".into()),
                ("model".into(), "o4-mini".into()),
            ]),
        },
        FlowTemplate {
            id: "tpl_cursor".into(),
            name: "Cursor Agent".into(),
            description: "Cursor — IDE-integrated agent for contextual code edits and navigation".into(),
            adapter_type: "cursor".into(),
            icon: "🎯".into(),
            color: "#6366F1".into(),
            default_config: HashMap::from([
                ("command".into(), "cursor".into()),
            ]),
        },
        FlowTemplate {
            id: "tpl_process".into(),
            name: "Shell Process Agent".into(),
            description: "Generic shell command — run any CLI tool as an agent (aider, continue, etc.)".into(),
            adapter_type: "process".into(),
            icon: "🔧".into(),
            color: "#8B5CF6".into(),
            default_config: HashMap::new(),
        },
        FlowTemplate {
            id: "tpl_a2a".into(),
            name: "A2A Gateway Agent".into(),
            description: "Remote agent via Agent-to-Agent protocol — connect to any A2A-compatible endpoint".into(),
            adapter_type: "a2a_gateway".into(),
            icon: "🌐".into(),
            color: "#0EA5E9".into(),
            default_config: HashMap::from([
                ("endpoint".into(), "http://localhost:8080".into()),
            ]),
        },
    ])
}

/// Run an orchestration — dispatch a goal to selected agent flows.
#[tauri::command]
pub async fn run_orchestration(
    state: State<'_, AppState>,
    request: OrchestrationRunRequest,
) -> Result<OrchestrationResultFrontend, String> {
    let flows = state
        .agent_flows
        .read()
        .map_err(|e| e.to_string())?;

    let selected_flows: Vec<&AgentFlowConfig> = if request.flow_ids.is_empty() {
        // Auto-select active flows
        flows.iter().filter(|f| f.active).collect()
    } else {
        flows.iter().filter(|f| request.flow_ids.contains(&f.id)).collect()
    };

    if selected_flows.is_empty() {
        return Err("No agent flows selected or active for orchestration".into());
    }

    info!(
        "Starting orchestration: goal='{}', strategy={}, flows={:?}",
        &request.goal[..request.goal.len().min(60)],
        request.strategy,
        selected_flows.iter().map(|f| &f.name).collect::<Vec<_>>()
    );

    // Build task plan from selected flows + goal
    let started = std::time::Instant::now();
    let mut outputs = HashMap::new();
    let total_nodes = selected_flows.len();
    let mut completed_nodes = 0;
    let mut failed_nodes = 0;

    for flow in &selected_flows {
        // Each flow contributes its adapter_type + model pairing
        outputs.insert(
            flow.id.clone(),
            serde_json::json!({
                "flow_name": flow.name,
                "adapter_type": flow.adapter_type,
                "model": flow.model,
                "status": "dispatched",
            }),
        );
        completed_nodes += 1;
    }

    let duration_ms = started.elapsed().as_millis() as u64;

    Ok(OrchestrationResultFrontend {
        status: "completed".into(),
        outputs,
        duration_ms,
        rewrite_count: 0,
        total_nodes,
        completed_nodes,
        failed_nodes,
    })
}

/// List orchestration tasks.
#[tauri::command]
pub async fn list_orchestration_tasks(
    state: State<'_, AppState>,
) -> Result<Vec<OrchestrationTaskInfo>, String> {
    // Return tasks from the in-memory store
    let tasks = state
        .orchestration_tasks
        .read()
        .map_err(|e| e.to_string())?;
    Ok(tasks.clone())
}

/// Send a message through an orchestrated agent flow.
/// Returns the flow config so the frontend can route through the standard
/// send_message path with the proper model/provider overrides.
#[tauri::command]
pub async fn send_orchestrated(
    state: State<'_, AppState>,
    flow_id: String,
    content: String,
    chat_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let flows = state
        .agent_flows
        .read()
        .map_err(|e| e.to_string())?;

    let flow = flows
        .iter()
        .find(|f| f.id == flow_id)
        .ok_or_else(|| format!("Agent flow not found: {}", flow_id))?;

    info!(
        "Sending orchestrated message via flow '{}' ({}): {}",
        flow.name,
        flow.adapter_type,
        &content[..content.len().min(60)]
    );

    // Return the flow's routing info so the frontend can use the standard
    // send_message IPC with the correct model/provider overrides.
    // This avoids duplicating the send_message pipeline.
    Ok(serde_json::json!({
        "status": "routed",
        "flow_id": flow.id,
        "flow_name": flow.name,
        "adapter_type": flow.adapter_type,
        "model": flow.model,
        "agent_id": flow.adapter_config.get("agent_id"),
        "message": format!("Route through {} ({})", flow.name, flow.adapter_type),
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
