//! Durable runtime commands — crash-recoverable workflow execution.

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
    let (new_rid, _output) = runner.resume(&rid)
        .await
        .map_err(|e| format!("Resume failed: {:?}", e))?;
    Ok(DurableRunInfo {
        run_id: new_rid.0,
        state: "resumed".into(),
        worker_id: "desktop-primary".into(),
    })
}

/// List active durable runs.
#[tauri::command]
pub async fn list_durable_runs(
    state: State<'_, AppState>,
) -> Result<Vec<DurableRunInfo>, String> {
    let runner = state.durable_runner.as_ref()
        .ok_or("Durable runtime not initialized")?;
    // We can query the checkpoint store directly for run indices.
    let cp_store = clawdesk_runtime::checkpoint::CheckpointStore::new(state.soch_store.clone());
    let mut runs = Vec::new();
    
    // Load runs by state (running, suspended, failed)
    for s in &["running", "suspended", "failed"] {
        if let Ok(ids) = cp_store.load_runs_by_state(s).await {
            for id in ids {
                let rinfo = DurableRunInfo {
                    run_id: id.0,
                    state: s.to_string(),
                    worker_id: "desktop-primary".into(),
                };
                runs.push(rinfo);
            }
        }
    }
    // Also check pending runs
    if let Ok(ids) = cp_store.load_runs_by_state("pending").await {
        for id in ids {
            runs.push(DurableRunInfo {
                run_id: id.0,
                state: "pending".to_string(),
                worker_id: "none".into(),
            });
        }
    }
    
    Ok(runs)
}

/// List checkpoints for a run (latest ones).
#[tauri::command]
pub async fn list_checkpoints(
    run_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let cp_store = clawdesk_runtime::checkpoint::CheckpointStore::new(state.soch_store.clone());
    let rid = clawdesk_runtime::types::RunId(run_id);
    match cp_store.load_checkpoint(&rid).await {
        Ok(Some(cp)) => {
            if let Ok(val) = serde_json::to_value(cp) {
                Ok(vec![val])
            } else {
                Ok(vec![])
            }
        }
        _ => Ok(vec![]),
    }
}

/// Get entries from the Dead Letter Queue.
#[tauri::command]
pub async fn get_dlq(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let dlq = clawdesk_runtime::dead_letter::DeadLetterQueue::new(state.soch_store.clone());
    let entries = dlq.list().await.map_err(|e| format!("Failed to load DLQ: {:?}", e))?;
    entries.into_iter().map(|e| serde_json::to_value(e).map_err(|err| err.to_string())).collect()
}

// ═══════════════════════════════════════════════════════════
// CLI Agent Detection — discovers installed external agents
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
pub struct DetectedCliAgent {
    pub id: String,
    pub name: String,
    pub command: String,
    pub path: String,
    pub installed: bool,
}

/// Detect which external CLI agents are installed on this system.
#[tauri::command]
pub async fn detect_cli_agents() -> Result<Vec<DetectedCliAgent>, String> {
    let agents = vec![
        ("claude-code", "Claude Code", "claude"),
        ("codex", "OpenAI Codex", "codex"),
        ("gemini-cli", "Gemini CLI", "gemini"),
        ("aider", "Aider", "aider"),
        ("gh-copilot", "GitHub Copilot", "gh"),
    ];

    let mut results = Vec::new();
    for (id, name, cmd) in agents {
        let output = tokio::process::Command::new("which")
            .arg(cmd)
            .output()
            .await;
        let (installed, path) = match output {
            Ok(o) if o.status.success() => {
                let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
                (true, p)
            }
            _ => (false, String::new()),
        };
        results.push(DetectedCliAgent {
            id: id.into(),
            name: name.into(),
            command: cmd.into(),
            path,
            installed,
        });
    }
    Ok(results)
}

