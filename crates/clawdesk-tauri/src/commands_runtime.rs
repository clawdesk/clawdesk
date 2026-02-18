//! Durable runtime commands — crash-recoverable workflow execution (Task 12).

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Serialize)]
pub struct DurableRunInfo {
    pub run_id: String,
    pub state: String,
    pub worker_id: String,
}

#[derive(Debug, Serialize)]
pub struct DurableRunStatus {
    pub run_id: String,
    pub state: String,
    pub checkpoint_count: usize,
    pub journal_entries: usize,
}

/// Get the status of the durable runtime subsystem.
/// Returns whether the durable runner is available and its configuration.
#[tauri::command]
pub async fn get_runtime_status(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let has_durable = state.durable_runner.is_some();
    Ok(serde_json::json!({
        "durable_runner_available": has_durable,
        "worker_id": "desktop-primary",
        "checkpoint_store": if has_durable { "sochdb" } else { "none" },
        "journal": if has_durable { "wal" } else { "none" },
        "lease_manager": if has_durable { "local" } else { "none" },
    }))
}

/// Cancel a durable run by its run ID.
#[tauri::command]
pub async fn cancel_durable_run(
    run_id: String,
    reason: Option<String>,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let runner = state.durable_runner.as_ref()
        .ok_or("Durable runtime not initialized (requires SochDB)")?;
    let rid = clawdesk_runtime::types::RunId(run_id);
    runner.cancel(&rid, reason.unwrap_or_else(|| "User cancelled".into()))
        .await
        .map_err(|e| format!("Cancel failed: {:?}", e))?;
    Ok(true)
}

/// Get the status of a specific durable run.
#[tauri::command]
pub async fn get_durable_run_status(
    run_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let runner = state.durable_runner.as_ref()
        .ok_or("Durable runtime not initialized (requires SochDB)")?;
    let rid = clawdesk_runtime::types::RunId(run_id);
    let status = runner.get_status(&rid)
        .await
        .map_err(|e| format!("Status query failed: {:?}", e))?;
    Ok(format!("{:?}", status))
}

/// Resume a previously interrupted durable run.
#[tauri::command]
pub async fn resume_durable_run(
    run_id: String,
    state: State<'_, AppState>,
) -> Result<DurableRunInfo, String> {
    let runner = state.durable_runner.as_ref()
        .ok_or("Durable runtime not initialized (requires SochDB)")?;
    let rid = clawdesk_runtime::types::RunId(run_id);
    let (new_rid, output) = runner.resume(&rid)
        .await
        .map_err(|e| format!("Resume failed: {:?}", e))?;
    Ok(DurableRunInfo {
        run_id: new_rid.0,
        state: "resumed".into(),
        worker_id: "desktop-primary".into(),
    })
}
