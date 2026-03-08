//! Tauri IPC commands for local model management.
//!
//! These commands expose the `clawdesk-local-models` crate functionality
//! to the frontend, enabling hardware detection, model recommendations,
//! downloading, starting/stopping inference servers, and more.

use crate::state::AppState;
use clawdesk_local_models::{
    DownloadEvent, LocalModelsStatus, ModelFit, RunningModel, ScannedModel, SystemSpecs,
};
use serde::Deserialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tracing::{error, info};

/// Helper: extract an Arc<ServerManager> from state without holding the RwLock across await.
fn get_server(state: &AppState) -> Result<Arc<clawdesk_local_models::ServerManager>, String> {
    let guard = state.local_model_manager.read().map_err(|e| e.to_string())?;
    match &*guard {
        Some(m) => Ok(Arc::clone(&m.server)),
        None => Err("Local model manager not initialized".to_string()),
    }
}

/// Helper: get models_dir + system from state without holding lock across await.
fn get_manager_info(state: &AppState) -> Result<(std::path::PathBuf, SystemSpecs, Arc<clawdesk_local_models::ServerManager>), String> {
    let guard = state.local_model_manager.read().map_err(|e| e.to_string())?;
    match &*guard {
        Some(m) => Ok((m.models_dir.clone(), m.system.clone(), Arc::clone(&m.server))),
        None => Err("Local model manager not initialized".to_string()),
    }
}

// ── Get full local models status ──────────────────────────────────────────

#[tauri::command]
pub async fn local_models_status(
    state: State<'_, AppState>,
) -> Result<LocalModelsStatus, String> {
    let (models_dir, system, server) = get_manager_info(&state)?;

    let downloaded = server.list_downloaded_models();
    let running = server.status().await;
    let llama_server_available = server.is_llama_server_available().await;

    // Build recommendations
    let downloaded_names: std::collections::HashSet<String> = downloaded
        .iter()
        .map(|m| m.name.clone())
        .collect();

    let mut recommended: Vec<ModelFit> = clawdesk_local_models::builtin_models()
        .iter()
        .map(|model| {
            let installed = downloaded_names
                .iter()
                .any(|d| d.to_lowercase().contains(&model.name.to_lowercase()));
            ModelFit::analyze(model, &system, installed)
        })
        .collect();
    recommended.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    Ok(LocalModelsStatus {
        system,
        llama_server_available,
        models_dir: models_dir.display().to_string(),
        downloaded_models: downloaded,
        running_models: running,
        recommended_models: recommended,
    })
}

// ── Get system hardware info ──────────────────────────────────────────────

#[tauri::command]
pub async fn local_models_system_info(
    state: State<'_, AppState>,
) -> Result<SystemSpecs, String> {
    let guard = state.local_model_manager.read().map_err(|e| e.to_string())?;
    match &*guard {
        Some(m) => Ok(m.system.clone()),
        None => Ok(SystemSpecs::detect()),
    }
}

// ── Get model recommendations ─────────────────────────────────────────────

#[tauri::command]
pub async fn local_models_recommend(
    state: State<'_, AppState>,
) -> Result<Vec<ModelFit>, String> {
    let guard = state.local_model_manager.read().map_err(|e| e.to_string())?;
    match &*guard {
        Some(m) => Ok(m.recommend_models()),
        None => Err("Local model manager not initialized".to_string()),
    }
}

// ── Start a local model ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct StartModelRequest {
    pub model_name: String,
}

#[tauri::command]
pub async fn local_models_start(
    request: StartModelRequest,
    state: State<'_, AppState>,
) -> Result<u16, String> {
    // Extract what we need before any await
    let (models_dir, _system, server) = get_manager_info(&state)?;

    // Find the model file
    let downloaded = server.list_downloaded_models();
    let model_file = downloaded
        .iter()
        .find(|m| m.name.to_lowercase().contains(&request.model_name.to_lowercase()))
        .ok_or_else(|| {
            format!(
                "Model '{}' not found in {}. Download it first.",
                request.model_name,
                models_dir.display()
            )
        })?;

    let model_path = model_file.path.clone();
    let port = server.start_model(&model_path, &request.model_name).await?;

    info!(
        model = request.model_name.as_str(),
        port,
        "model started and available as local provider"
    );

    Ok(port)
}

// ── Stop a local model ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct StopModelRequest {
    pub model_name: String,
}

#[tauri::command]
pub async fn local_models_stop(
    request: StopModelRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let server = get_server(&state)?;
    server.stop_model(&request.model_name).await
}

// ── Get running models ───────────────────────────────────────────────────

#[tauri::command]
pub async fn local_models_running(
    state: State<'_, AppState>,
) -> Result<Vec<RunningModel>, String> {
    let server = get_server(&state)?;
    Ok(server.status().await)
}

// ── Download a model ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DownloadModelRequest {
    pub model_name: String,
    pub download_url: String,
}

#[tauri::command]
pub async fn local_models_download(
    request: DownloadModelRequest,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let (models_dir, _, _) = get_manager_info(&state)?;

    let model_name = request.model_name.clone();
    let url = request.download_url.clone();

    // Spawn download in background, emit progress events
    let app_handle = app.clone();
    tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DownloadEvent>(32);

        let dl_name = model_name.clone();
        let dl_url = url.clone();
        let dl_dir = models_dir.clone();
        let download_handle = tokio::spawn(async move {
            clawdesk_local_models::download_model(&dl_url, &dl_dir, &dl_name, tx).await
        });

        // Forward download events to frontend
        while let Some(event) = rx.recv().await {
            let _ = app_handle.emit("local-model-download", &event);
        }

        match download_handle.await {
            Ok(Ok(path)) => {
                info!(path = %path.display(), "model download complete");
            }
            Ok(Err(e)) => {
                error!(error = %e, "model download failed");
                let _ = app_handle.emit(
                    "local-model-download",
                    DownloadEvent::Error {
                        model_name,
                        message: e,
                    },
                );
            }
            Err(e) => {
                error!(error = %e, "download task panicked");
            }
        }
    });

    Ok(())
}

// ── Delete a downloaded model ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DeleteModelRequest {
    pub model_name: String,
}

#[tauri::command]
pub async fn local_models_delete(
    request: DeleteModelRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let server = get_server(&state)?;

    // Stop if running
    let _ = server.stop_model(&request.model_name).await;

    // Find and delete the file
    let models = server.list_downloaded_models();
    let model = models
        .iter()
        .find(|m| m.name.to_lowercase().contains(&request.model_name.to_lowercase()))
        .ok_or("Model file not found")?;

    clawdesk_local_models::delete_model(&model.path).await
}

// ── Set llama-server binary path ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SetServerPathRequest {
    pub path: String,
}

#[tauri::command]
pub async fn local_models_set_server_path(
    request: SetServerPathRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let server = get_server(&state)?;
    server
        .set_llama_server_path(std::path::PathBuf::from(request.path))
        .await;
    Ok(())
}

// ── Scan a directory for GGUF files ──────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ScanDirectoryRequest {
    pub directory: String,
}

#[tauri::command]
pub async fn local_models_scan_directory(
    request: ScanDirectoryRequest,
    state: State<'_, AppState>,
) -> Result<Vec<ScannedModel>, String> {
    let server = get_server(&state)?;
    let dir = std::path::PathBuf::from(&request.directory);
    if !dir.is_dir() {
        return Err(format!("'{}' is not a directory", request.directory));
    }
    Ok(server.scan_directory(&dir))
}

// ── Import a GGUF model from external path ───────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ImportModelRequest {
    pub source_path: String,
}

#[tauri::command]
pub async fn local_models_import(
    request: ImportModelRequest,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let server = get_server(&state)?;
    let source = std::path::PathBuf::from(&request.source_path);
    server.import_model(&source).await
}

// ── Set TTL for auto-unloading idle models ───────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SetTtlRequest {
    pub ttl_secs: u64,
}

#[tauri::command]
pub async fn local_models_set_ttl(
    request: SetTtlRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let server = get_server(&state)?;
    server.set_default_ttl(request.ttl_secs).await;
    info!(ttl_secs = request.ttl_secs, "updated model auto-unload TTL");
    Ok(())
}
