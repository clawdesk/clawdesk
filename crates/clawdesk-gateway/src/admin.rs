//! Admin API routes for plugin, cron, and system management.
//!
//! Routes:
//! - `GET    /api/v1/admin/plugins` — list installed plugins
//! - `POST   /api/v1/admin/plugins/:name/reload` — reload a plugin
//! - `GET    /api/v1/admin/cron/tasks` — list cron tasks
//! - `POST   /api/v1/admin/cron/tasks` — create a cron task
//! - `DELETE /api/v1/admin/cron/tasks/:id` — delete a cron task
//! - `POST   /api/v1/admin/cron/tasks/:id/trigger` — manually trigger a task
//! - `GET    /api/v1/admin/metrics` — gateway metrics snapshot

use crate::state::GatewayState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use clawdesk_storage::SessionStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

// ── Plugin admin ─────────────────────────────────────────────

#[derive(Serialize)]
pub struct PluginInfoResponse {
    pub name: String,
    pub version: String,
    pub state: String,
    pub capabilities: PluginCapsResponse,
    pub load_time_ms: u64,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct PluginCapsResponse {
    pub tools: Vec<String>,
    pub hooks: Vec<String>,
    pub channels: Vec<String>,
    pub commands: Vec<String>,
}

/// GET /api/v1/admin/plugins — list all plugins.
pub async fn list_plugins(
    State(state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    let list: Vec<PluginInfoResponse> = state
        .plugin_host
        .list_plugins()
        .await
        .into_iter()
        .map(|info| PluginInfoResponse {
            name: info.manifest.name.clone(),
            version: info.manifest.version.clone(),
            state: format!("{:?}", info.state),
            capabilities: PluginCapsResponse {
                tools: info.manifest.capabilities.tools.clone(),
                hooks: info.manifest.capabilities.hooks.clone(),
                channels: info.manifest.capabilities.channels.clone(),
                commands: info.manifest.capabilities.commands.clone(),
            },
            load_time_ms: info.load_time_ms,
            error: info.error.clone(),
        })
        .collect();
    Json(list)
}

/// POST /api/v1/admin/plugins/:name/reload — reload (deactivate + activate).
pub async fn reload_plugin(
    State(state): State<Arc<GatewayState>>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // Deactivate then re-activate
    state
        .plugin_host
        .deactivate(&name)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, format!("Deactivate failed: {e}")))?;

    state
        .plugin_host
        .activate(&name)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Activate failed: {e}")))?;

    info!(%name, "plugin reloaded via admin API");
    Ok(Json(serde_json::json!({ "status": "reloaded", "name": name })))
}

// ── Cron admin ───────────────────────────────────────────────

#[derive(Serialize)]
pub struct CronTaskResponse {
    pub id: String,
    pub name: String,
    pub schedule: String,
    pub enabled: bool,
    pub prompt_preview: String,
    pub delivery_targets: usize,
}

#[derive(Deserialize)]
pub struct CreateCronTaskRequest {
    pub name: String,
    pub schedule: String,
    pub prompt: String,
    pub agent_id: Option<String>,
    pub enabled: Option<bool>,
    /// Delivery targets: where to send results on completion.
    /// Each entry specifies a channel and optional conversation ID.
    #[serde(default)]
    pub delivery_targets: Vec<DeliveryTargetRequest>,
}

#[derive(Deserialize)]
pub struct DeliveryTargetRequest {
    /// Channel name: "telegram", "slack", "discord", "email", etc.
    pub channel: String,
    /// Target conversation/chat ID, or "default" for the channel's default target.
    #[serde(default = "default_target")]
    pub to: String,
}

fn default_target() -> String {
    "default".to_string()
}

/// GET /api/v1/admin/cron/tasks — list all cron tasks.
pub async fn list_cron_tasks(
    State(state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    let tasks_raw = state.cron_manager.list_tasks().await;
    let tasks: Vec<CronTaskResponse> = tasks_raw
        .iter()
        .map(|t| CronTaskResponse {
            id: t.id.clone(),
            name: t.name.clone(),
            schedule: t.schedule.clone(),
            enabled: t.enabled,
            prompt_preview: t.prompt.chars().take(100).collect(),
            delivery_targets: t.delivery_targets.len(),
        })
        .collect();
    Json(tasks)
}

/// POST /api/v1/admin/cron/tasks — create a new cron task.
pub async fn create_cron_task(
    State(state): State<Arc<GatewayState>>,
    Json(req): Json<CreateCronTaskRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    use chrono::Utc;
    use clawdesk_types::cron::CronTask;

    // Validate schedule expression
    clawdesk_cron::parse_cron_expression(&req.schedule)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid schedule: {e}")))?;

    let task = CronTask {
        id: uuid::Uuid::new_v4().to_string(),
        name: req.name.clone(),
        schedule: req.schedule,
        prompt: req.prompt,
        agent_id: req.agent_id,
        delivery_targets: req.delivery_targets.iter().map(|dt| {
            clawdesk_types::cron::DeliveryTarget::Channel {
                channel_id: dt.channel.clone(),
                conversation_id: dt.to.clone(),
            }
        }).collect(),
        skip_if_running: true,
        timeout_secs: 300,
        enabled: req.enabled.unwrap_or(true),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        depends_on: vec![],
        chain_mode: Default::default(),
        max_retained_logs: 0,
    };

    let id = task.id.clone();
    state
        .cron_manager
        .upsert_task(task)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed to create task: {e}")))?;

    info!(task_id = %id, name = %req.name, "cron task created via admin API");
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "status": "created" })),
    ))
}

/// POST /api/v1/admin/cron/tasks/:id/trigger — manually trigger a task.
pub async fn trigger_cron_task(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let log = state
        .cron_manager
        .trigger(&id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, format!("Trigger failed: {e}")))?;

    info!(task_id = %id, run_id = %log.run_id, "cron task manually triggered");
    Ok(Json(serde_json::json!({
        "id": id,
        "run_id": log.run_id,
        "status": format!("{:?}", log.status),
    })))
}

/// DELETE /api/v1/admin/cron/tasks/:id — delete a cron task.
pub async fn delete_cron_task(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let removed = state
        .cron_manager
        .remove_task(&id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, format!("Task not found: {e}")))?;

    info!(task_id = %id, name = %removed.name, "cron task deleted via admin API");
    Ok(StatusCode::NO_CONTENT)
}

// ── Metrics ──────────────────────────────────────────────────

#[derive(Serialize)]
pub struct MetricsSnapshot {
    pub uptime_secs: u64,
    pub total_sessions: u64,
    pub active_channels: usize,
    pub loaded_plugins: usize,
    pub cron_tasks: usize,
    pub registered_tools: usize,
}

/// GET /api/v1/admin/metrics — basic runtime metrics.
pub async fn metrics_snapshot(
    State(state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    let channels = state.channels.load();
    let (plugins, tasks) = tokio::join!(
        state.plugin_host.list_plugins(),
        state.cron_manager.list_tasks()
    );

    let session_count = state
        .store
        .list_sessions(clawdesk_types::session::SessionFilter::default())
        .await
        .map(|s| s.len() as u64)
        .unwrap_or(0);

    let snapshot = MetricsSnapshot {
        uptime_secs: state.uptime_secs(),
        total_sessions: session_count,
        active_channels: channels.len(),
        loaded_plugins: plugins.len(),
        cron_tasks: tasks.len(),
        registered_tools: state.tools.total_count(),
    };
    Json(snapshot)
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_task_response_serializes() {
        let resp = CronTaskResponse {
            id: "t1".into(),
            name: "daily-summary".into(),
            schedule: "0 9 * * *".into(),
            enabled: true,
            prompt_preview: "Summarize today's...".into(),
            delivery_targets: 2,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["id"], "t1");
        assert_eq!(json["delivery_targets"], 2);
    }

    #[test]
    fn metrics_snapshot_serializes() {
        let snap = MetricsSnapshot {
            uptime_secs: 120,
            total_sessions: 5,
            active_channels: 3,
            loaded_plugins: 2,
            cron_tasks: 1,
            registered_tools: 10,
        };
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["uptime_secs"], 120);
    }
}
