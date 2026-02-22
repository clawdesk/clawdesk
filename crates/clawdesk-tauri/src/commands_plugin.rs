//! Plugin system commands — lifecycle, install, hooks.

use crate::state::AppState;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct PluginSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub state: String,
}

#[tauri::command]
pub async fn list_plugins(state: State<'_, AppState>) -> Result<Vec<PluginSummary>, String> {
    let host = state.plugin_host.as_ref()
        .ok_or("Plugin system not initialized")?;
    let plugins = host.list_plugins().await;
    Ok(plugins.into_iter().map(|p| PluginSummary {
        id: p.manifest.name.clone(),
        name: p.manifest.name.clone(),
        version: p.manifest.version.clone(),
        description: p.manifest.description.clone(),
        state: format!("{:?}", p.state),
    }).collect())
}

#[tauri::command]
pub async fn get_plugin_info(
    plugin_id: String,
    state: State<'_, AppState>,
) -> Result<PluginSummary, String> {
    let host = state.plugin_host.as_ref()
        .ok_or("Plugin system not initialized")?;
    let info = host.get_plugin(&plugin_id).await
        .ok_or_else(|| format!("Plugin '{}' not found", plugin_id))?;
    Ok(PluginSummary {
        id: info.manifest.name.clone(),
        name: info.manifest.name.clone(),
        version: info.manifest.version.clone(),
        description: info.manifest.description.clone(),
        state: format!("{:?}", info.state),
    })
}

#[tauri::command]
pub async fn enable_plugin(
    plugin_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let host = state.plugin_host.as_ref()
        .ok_or("Plugin system not initialized")?;
    host.activate(&plugin_id).await
        .map_err(|e| format!("{:?}", e))?;
    Ok(true)
}

#[tauri::command]
pub async fn disable_plugin(
    plugin_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let host = state.plugin_host.as_ref()
        .ok_or("Plugin system not initialized")?;
    host.deactivate(&plugin_id).await
        .map_err(|e| format!("{:?}", e))?;
    Ok(true)
}
