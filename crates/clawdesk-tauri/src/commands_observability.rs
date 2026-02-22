//! Observability commands — config + metrics.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Serialize)]
pub struct ObservabilityStatus {
    pub enabled: bool,
    pub service_name: String,
    pub endpoint: String,
    pub environment: String,
    pub version: String,
    pub api_key_set: bool,
    pub project: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConfigureObservabilityRequest {
    pub enabled: Option<bool>,
    pub endpoint: Option<String>,
    pub service_name: Option<String>,
    pub environment: Option<String>,
    pub api_key: Option<String>,
    pub project: Option<String>,
}

/// Get the current observability configuration.
#[tauri::command]
pub async fn get_observability_config(
    state: State<'_, AppState>,
) -> Result<ObservabilityStatus, String> {
    let config = state.observability_config.read().map_err(|e| e.to_string())?;
    Ok(ObservabilityStatus {
        enabled: config.enabled,
        service_name: config.service_name.clone(),
        endpoint: config.clawdesk_endpoint.clone(),
        environment: config.environment.clone(),
        version: config.version.clone(),
        api_key_set: config.api_key.is_some(),
        project: config.project.clone(),
    })
}

/// Configure the observability pipeline.
#[tauri::command]
pub async fn configure_observability(
    request: ConfigureObservabilityRequest,
    state: State<'_, AppState>,
) -> Result<ObservabilityStatus, String> {
    let mut config = state.observability_config.write().map_err(|e| e.to_string())?;
    if let Some(enabled) = request.enabled {
        config.enabled = enabled;
    }
    if let Some(endpoint) = request.endpoint {
        config.clawdesk_endpoint = endpoint;
    }
    if let Some(name) = request.service_name {
        config.service_name = name;
    }
    if let Some(env) = request.environment {
        config.environment = env;
    }
    if let Some(key) = request.api_key {
        config.api_key = Some(key);
    }
    if let Some(project) = request.project {
        config.project = Some(project);
    }

    // Re-initialize observability if enabled
    if config.enabled {
        let fresh = config.clone();
        if let Err(e) = clawdesk_observability::init_observability(fresh) {
            tracing::warn!("Failed to reinitialize observability: {e}");
        }
    }

    Ok(ObservabilityStatus {
        enabled: config.enabled,
        service_name: config.service_name.clone(),
        endpoint: config.clawdesk_endpoint.clone(),
        environment: config.environment.clone(),
        version: config.version.clone(),
        api_key_set: config.api_key.is_some(),
        project: config.project.clone(),
    })
}
