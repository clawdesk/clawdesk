//! Tauri IPC commands for service preview (live preview of web apps built by agents).
//!
//! When an agent runs a dev server (e.g. `npm run dev`, `python -m http.server`),
//! these commands allow the frontend to discover and display the running service
//! in an iframe preview panel.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use tauri::State;
use tracing::info;

/// A running service that can be previewed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewService {
    /// Unique ID (typically the bg session ID from shell_exec).
    pub id: String,
    /// Human-readable label (e.g. "Next.js Dev Server").
    pub label: String,
    /// The URL to render in the iframe.
    pub url: String,
    /// The port number.
    pub port: u16,
    /// When the service was registered.
    pub created_at: String,
}

/// Global registry of previewable services.
pub struct PreviewRegistry {
    services: RwLock<HashMap<String, PreviewService>>,
}

impl PreviewRegistry {
    pub fn new() -> Self {
        Self {
            services: RwLock::new(HashMap::new()),
        }
    }
}

// ── Register a preview service ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegisterPreviewRequest {
    pub id: String,
    pub label: String,
    pub port: u16,
}

#[tauri::command]
pub async fn preview_register(
    request: RegisterPreviewRequest,
    registry: State<'_, PreviewRegistry>,
) -> Result<PreviewService, String> {
    let service = PreviewService {
        id: request.id.clone(),
        label: request.label,
        url: format!("http://localhost:{}", request.port),
        port: request.port,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    registry
        .services
        .write()
        .map_err(|e| e.to_string())?
        .insert(request.id.clone(), service.clone());

    info!(id = request.id.as_str(), port = request.port, "preview service registered");
    Ok(service)
}

// ── List active preview services ─────────────────────────────────────────

#[tauri::command]
pub async fn preview_list(
    registry: State<'_, PreviewRegistry>,
) -> Result<Vec<PreviewService>, String> {
    let services = registry.services.read().map_err(|e| e.to_string())?;
    Ok(services.values().cloned().collect())
}

// ── Remove a preview service ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RemovePreviewRequest {
    pub id: String,
}

#[tauri::command]
pub async fn preview_remove(
    request: RemovePreviewRequest,
    registry: State<'_, PreviewRegistry>,
) -> Result<(), String> {
    registry
        .services
        .write()
        .map_err(|e| e.to_string())?
        .remove(&request.id);

    info!(id = request.id.as_str(), "preview service removed");
    Ok(())
}

// ── Check if a port is serving HTTP ──────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CheckPortRequest {
    pub port: u16,
}

#[tauri::command]
pub async fn preview_check_port(
    request: CheckPortRequest,
) -> Result<bool, String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], request.port));
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    {
        Ok(Ok(_)) => Ok(true),
        _ => Ok(false),
    }
}
