//! Tauri IPC commands for configuration hot-reload.
//!
//! Exposes the MVCC config reload subsystem (R1–R8) to the desktop UI:
//! - Trigger manual reloads
//! - Inspect current reload policy and canary status
//! - Roll back to a previous config generation
//! - Subscribe to config events via the Tauri event bus

use crate::state::AppState;
use serde::Serialize;
use tauri::State;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ReloadPolicyInfo {
    pub preset: String,
    pub debounce_ms: u64,
    pub canary_percentage: u8,
    pub canary_duration_secs: u64,
    pub auto_rollback: bool,
    pub buffer_capacity: usize,
}

#[derive(Debug, Serialize)]
pub struct ReloadStatusInfo {
    pub current_generation: u64,
    pub buffered_generations: usize,
    pub watcher_active: bool,
    pub last_event: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RollbackResult {
    pub rolled_back_to: u64,
    pub success: bool,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Get the current reload policy configuration.
#[tauri::command]
pub async fn config_get_reload_policy(
    state: State<'_, AppState>,
) -> Result<ReloadPolicyInfo, String> {
    let policy = &state.reload_policy;
    Ok(ReloadPolicyInfo {
        preset: format!("{:?}", policy.global.preset),
        debounce_ms: policy.watcher.debounce_ms,
        canary_percentage: 0, // canary uses window_secs/health_threshold, not percentage
        canary_duration_secs: policy.canary.window_secs,
        auto_rollback: policy.canary.auto_rollback,
        buffer_capacity: policy.rollback.buffer_capacity,
    })
}

/// Get the current reload status (generation, watcher, etc.).
#[tauri::command]
pub async fn config_get_reload_status(
    state: State<'_, AppState>,
) -> Result<ReloadStatusInfo, String> {
    let buffered = state.rollback_buffer.len();
    let watcher_active = state.native_watcher.is_watching();

    Ok(ReloadStatusInfo {
        current_generation: buffered as u64,
        buffered_generations: buffered,
        watcher_active,
        last_event: None,
    })
}

/// Trigger a manual configuration reload.
#[tauri::command]
pub async fn config_trigger_reload(
    state: State<'_, AppState>,
) -> Result<String, String> {
    // Publish a reload file-changed event on the config event bus
    state
        .config_event_bus
        .emit_file_changed(0, "manual_trigger".to_string(), "manual".to_string());
    Ok("reload requested".to_string())
}

/// Roll back to a previous config generation.
#[tauri::command]
pub async fn config_rollback(
    state: State<'_, AppState>,
    target_generation: u64,
) -> Result<RollbackResult, String> {
    match state.rollback_buffer.find(target_generation) {
        Some(entry) => {
            state
                .config_event_bus
                .emit_rolled_back(
                    entry.generation,
                    state.rollback_buffer.len() as u64,
                    entry.generation,
                    "manual_rollback".to_string(),
                );
            Ok(RollbackResult {
                rolled_back_to: entry.generation,
                success: true,
            })
        }
        None => Ok(RollbackResult {
            rolled_back_to: 0,
            success: false,
        }),
    }
}
