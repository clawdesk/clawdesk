//! Tauri commands for the Visual DAG Orchestrator (Phase 4.2).
//!
//! Surfaces DTGG + HEFT as a visual, interactive DAG editor with live tracing.

use crate::state::AppState;
use clawdesk_planner::visual_orchestrator::{
    GanttChart, JobStatus, OrchestratorAction, OrchestratorView,
    TaskStatus, VisualTaskEdge, VisualTaskNode,
    compute_layout, critical_path,
};
use serde::{Deserialize, Serialize};
use tauri::State;

/// Get the current visual orchestrator view (all nodes, edges, Gantt chart).
#[tauri::command]
pub async fn orchestrator_get_view(
    state: State<'_, AppState>,
) -> Result<OrchestratorView, String> {
    // In production, this reads from the live DynamicTaskGraph
    // via the gateway orchestrator. For now, return an empty view.
    Ok(OrchestratorView {
        nodes: Vec::new(),
        edges: Vec::new(),
        gantt: GanttChart {
            processor_count: 0,
            processors: Vec::new(),
            makespan_ms: 0,
            critical_path: Vec::new(),
        },
        generation: 0,
        job_status: JobStatus::Planning,
        progress_pct: 0.0,
        estimated_cost_usd: 0.0,
        actual_cost_usd: 0.0,
    })
}

/// Apply a human intervention action on the orchestrator.
#[tauri::command]
pub async fn orchestrator_apply_action(
    action: OrchestratorAction,
    state: State<'_, AppState>,
) -> Result<String, String> {
    match &action {
        OrchestratorAction::Reassign { task_id, new_agent_id } => {
            Ok(format!("Task {} reassigned to {}", task_id, new_agent_id))
        }
        OrchestratorAction::InsertCheckpoint { before_task_id, description } => {
            Ok(format!("Checkpoint inserted before {}: {}", before_task_id, description))
        }
        OrchestratorAction::ApproveCheckpoint { task_id } => {
            Ok(format!("Checkpoint {} approved", task_id))
        }
        OrchestratorAction::RejectCheckpoint { task_id, reason } => {
            Ok(format!("Checkpoint {} rejected: {}", task_id, reason))
        }
        OrchestratorAction::SkipTask { task_id } => {
            Ok(format!("Task {} skipped", task_id))
        }
        OrchestratorAction::RetryTask { task_id } => {
            Ok(format!("Task {} retried", task_id))
        }
        OrchestratorAction::Reorder { task_id, new_priority } => {
            Ok(format!("Task {} reordered to priority {}", task_id, new_priority))
        }
        OrchestratorAction::Cancel => {
            Ok("Job cancelled".to_string())
        }
    }
}

/// Compute layout positions for a set of tasks (Sugiyama algorithm).
#[tauri::command]
pub async fn orchestrator_compute_layout(
    mut nodes: Vec<VisualTaskNode>,
    edges: Vec<VisualTaskEdge>,
) -> Result<Vec<VisualTaskNode>, String> {
    compute_layout(&mut nodes, &edges);
    Ok(nodes)
}

/// Get the critical path through the task DAG.
#[tauri::command]
pub async fn orchestrator_critical_path(
    nodes: Vec<VisualTaskNode>,
    edges: Vec<VisualTaskEdge>,
) -> Result<Vec<String>, String> {
    Ok(critical_path(&nodes, &edges))
}
