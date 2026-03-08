//! Tauri IPC commands for Canvas A2UI + Device capabilities.
//!
//! Commands:
//! 1. `canvas_a2ui_present`  — Show/position the canvas WebView
//! 2. `canvas_a2ui_hide`     — Hide the canvas WebView
//! 3. `canvas_a2ui_navigate` — Navigate canvas to URL
//! 4. `canvas_a2ui_eval`     — Execute JS in canvas WebView
//! 5. `canvas_a2ui_snapshot` — Screenshot the canvas
//! 6. `canvas_a2ui_push`     — Push A2UI JSONL to render components
//! 7. `canvas_a2ui_reset`    — Clear A2UI surface
//! 8. `canvas_a2ui_status`   — Get canvas + A2UI status
//! 9. `device_get_info`      — Structured device info
//! 10. `device_get_status`   — Dynamic device status
//! 11. `device_get_location` — GPS/IP-based location
//! 12. `device_capabilities` — Available capabilities

use clawdesk_canvas::commands::{CanvasCommand, CanvasCommandResult, CanvasManager};
use clawdesk_canvas::device::{DeviceCapabilities, DeviceInfo, DeviceManager, DeviceStatus, LocationData};
use clawdesk_canvas::capability::CapabilityStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Tauri-managed state for canvas A2UI + device capabilities.
pub struct CanvasA2uiState {
    pub canvas_manager: Arc<CanvasManager>,
    pub device_manager: Arc<RwLock<DeviceManager>>,
    pub capability_store: CapabilityStore,
}

impl CanvasA2uiState {
    pub fn new() -> Self {
        let host_url = "http://127.0.0.1:0".to_string(); // Will be updated after server starts
        let canvas_manager = Arc::new(CanvasManager::new(host_url.clone()));
        let device_manager = Arc::new(RwLock::new(DeviceManager::new()));
        let capability_store = CapabilityStore::new(host_url);
        Self {
            canvas_manager,
            device_manager,
            capability_store,
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Canvas A2UI commands
// ═══════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn canvas_a2ui_present(
    state: tauri::State<'_, CanvasA2uiState>,
    agent_id: String,
    url: Option<String>,
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
) -> Result<CanvasCommandResult, String> {
    let cmd = CanvasCommand::Present {
        url,
        x,
        y,
        width,
        height,
    };
    Ok(state.canvas_manager.execute(&agent_id, cmd).await)
}

#[tauri::command]
pub async fn canvas_a2ui_hide(
    state: tauri::State<'_, CanvasA2uiState>,
    agent_id: String,
) -> Result<CanvasCommandResult, String> {
    Ok(state.canvas_manager.execute(&agent_id, CanvasCommand::Hide).await)
}

#[tauri::command]
pub async fn canvas_a2ui_navigate(
    state: tauri::State<'_, CanvasA2uiState>,
    agent_id: String,
    url: String,
) -> Result<CanvasCommandResult, String> {
    let cmd = CanvasCommand::Navigate { url };
    Ok(state.canvas_manager.execute(&agent_id, cmd).await)
}

#[tauri::command]
pub async fn canvas_a2ui_eval(
    state: tauri::State<'_, CanvasA2uiState>,
    agent_id: String,
    javascript: String,
    timeout_ms: Option<u64>,
) -> Result<CanvasCommandResult, String> {
    let cmd = CanvasCommand::Eval {
        javascript,
        timeout_ms: timeout_ms.unwrap_or(5000),
    };
    Ok(state.canvas_manager.execute(&agent_id, cmd).await)
}

#[tauri::command]
pub async fn canvas_a2ui_snapshot(
    state: tauri::State<'_, CanvasA2uiState>,
    agent_id: String,
    format: Option<String>,
    max_width: Option<u32>,
    quality: Option<f64>,
) -> Result<CanvasCommandResult, String> {
    let cmd = CanvasCommand::Snapshot {
        format: format.unwrap_or_else(|| "png".into()),
        max_width,
        quality,
    };
    Ok(state.canvas_manager.execute(&agent_id, cmd).await)
}

#[tauri::command]
pub async fn canvas_a2ui_push(
    state: tauri::State<'_, CanvasA2uiState>,
    agent_id: String,
    jsonl: String,
) -> Result<CanvasCommandResult, String> {
    let cmd = CanvasCommand::A2uiPush { jsonl };
    Ok(state.canvas_manager.execute(&agent_id, cmd).await)
}

#[tauri::command]
pub async fn canvas_a2ui_reset(
    state: tauri::State<'_, CanvasA2uiState>,
    agent_id: String,
    surface_id: Option<String>,
) -> Result<CanvasCommandResult, String> {
    let cmd = CanvasCommand::A2uiReset { surface_id };
    Ok(state.canvas_manager.execute(&agent_id, cmd).await)
}

/// Canvas + A2UI status response.
#[derive(Debug, Serialize)]
pub struct CanvasA2uiStatus {
    pub canvas_host_url: String,
    pub surfaces_count: usize,
    pub has_backend: bool,
}

#[tauri::command]
pub async fn canvas_a2ui_status(
    state: tauri::State<'_, CanvasA2uiState>,
    agent_id: String,
) -> Result<CanvasA2uiStatus, String> {
    let canvas_state = state.canvas_manager.get_state(&agent_id);
    Ok(CanvasA2uiStatus {
        canvas_host_url: String::new(), // Will be filled by server
        surfaces_count: canvas_state.surfaces.len(),
        has_backend: true,
    })
}

// ═══════════════════════════════════════════════════════════════
// Device commands
// ═══════════════════════════════════════════════════════════════

#[tauri::command]
pub async fn device_get_info(
    state: tauri::State<'_, CanvasA2uiState>,
) -> Result<DeviceInfo, String> {
    let mgr = state.device_manager.read().await;
    Ok(mgr.device_info().clone())
}

#[tauri::command]
pub async fn device_get_status(
    state: tauri::State<'_, CanvasA2uiState>,
) -> Result<DeviceStatus, String> {
    let mgr = state.device_manager.read().await;
    Ok(mgr.device_status())
}

#[tauri::command]
pub async fn device_get_location(
    state: tauri::State<'_, CanvasA2uiState>,
) -> Result<LocationData, String> {
    let mgr = state.device_manager.read().await;
    mgr.get_location().await
}

#[tauri::command]
pub async fn device_capabilities(
    _state: tauri::State<'_, CanvasA2uiState>,
) -> Result<DeviceCapabilities, String> {
    Ok(DeviceCapabilities::detect())
}
