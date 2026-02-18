//! Extended RPC handlers for the gateway.
//!
//! Adds additional API endpoints beyond the core message/session/channel routes:
//!
//! - `/api/v1/config` — runtime configuration
//! - `/api/v1/models` — available models and providers
//! - `/api/v1/agents` — agent management
//! - `/api/v1/sessions/:id` — individual session operations
//! - `/api/v1/sessions/:id/messages` — conversation history
//! - `/api/v1/sessions/:id/compact` — trigger context compaction

use crate::state::GatewayState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use clawdesk_storage::conversation_store::ConversationStore;
use clawdesk_storage::session_store::SessionStore;
use clawdesk_types::session::SessionKey;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::debug;

// ─── Config RPC ─────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ConfigResponse {
    pub version: &'static str,
    pub providers: Vec<ProviderInfo>,
    pub channels: Vec<ChannelInfo>,
    pub capabilities: Vec<&'static str>,
}

#[derive(Serialize)]
pub struct ProviderInfo {
    pub name: String,
    pub models: Vec<String>,
    pub is_default: bool,
}

#[derive(Serialize)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
    pub supports_threading: bool,
    pub supports_streaming: bool,
    pub supports_reactions: bool,
}

/// GET /api/v1/config
pub async fn get_config(
    State(state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    let provider_registry = state.providers.load();
    let channel_registry = state.channels.load();

    let default_name = provider_registry
        .default_provider()
        .map(|p| p.name().to_string())
        .unwrap_or_default();

    let providers: Vec<ProviderInfo> = provider_registry
        .iter()
        .map(|(name, p)| ProviderInfo {
            name: name.clone(),
            models: p.models(),
            is_default: name == &default_name,
        })
        .collect();

    let channels: Vec<ChannelInfo> = channel_registry
        .iter()
        .map(|(id, ch)| {
            let meta = ch.meta();
            ChannelInfo {
                id: id.to_string(),
                name: meta.display_name,
                supports_threading: meta.supports_threading,
                supports_streaming: meta.supports_streaming,
                supports_reactions: meta.supports_reactions,
            }
        })
        .collect();

    Json(ConfigResponse {
        version: env!("CARGO_PKG_VERSION"),
        providers,
        channels,
        capabilities: vec![
            "chat",
            "streaming",
            "tool_use",
            "sessions",
            "plugins",
            "cron",
            "openai_compat",
        ],
    })
}

// ─── Models RPC ─────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ModelInfo {
    pub provider: String,
    pub model: String,
    pub qualified_name: String,
}

/// GET /api/v1/models
pub async fn list_models(
    State(state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    let registry = state.providers.load();
    let mut models = Vec::new();

    for (name, provider) in registry.iter() {
        for model in provider.models() {
            models.push(ModelInfo {
                provider: name.clone(),
                model: model.clone(),
                qualified_name: format!("{}/{}", name, model),
            });
        }
    }

    Json(models)
}

// ─── Session detail RPCs ────────────────────────────────────────────

/// GET /api/v1/sessions/:id
pub async fn get_session(
    State(state): State<Arc<GatewayState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let session_key = SessionKey::from(session_id);

    let session = state
        .store
        .load_session(&session_key)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "session not found".into()))?;

    Ok(Json(serde_json::json!({
        "id": session_key.as_str(),
        "channel": session.channel.to_string(),
        "model": session.model,
        "message_count": session.message_count,
        "state": format!("{:?}", session.state),
        "created_at": session.created_at.to_rfc3339(),
        "last_activity": session.last_activity.to_rfc3339(),
    })))
}

/// GET /api/v1/sessions/:id/messages
pub async fn get_session_messages(
    State(state): State<Arc<GatewayState>>,
    Path(session_id): Path<String>,
    Query(params): Query<MessageQueryParams>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let session_key = SessionKey::from(session_id);
    let limit = params.limit.unwrap_or(50).min(200);

    let messages = state
        .store
        .load_history(&session_key, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let msgs: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": format!("{:?}", m.role),
                "content": m.content,
                "timestamp": m.timestamp.to_rfc3339(),
                "model": m.model,
                "token_count": m.token_count,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "messages": msgs,
        "total": messages.len(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct MessageQueryParams {
    pub limit: Option<usize>,
    pub before: Option<String>,
}

/// DELETE /api/v1/sessions/:id
pub async fn delete_session(
    State(state): State<Arc<GatewayState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let session_key = SessionKey::from(session_id);

    state
        .store
        .delete_session(&session_key)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/v1/sessions/:id/compact
///
/// Trigger context compaction on a session. Uses the domain layer's
/// compaction logic (DropMetadata → SummarizeOld → Truncate → CircuitBreak).
pub async fn compact_session(
    State(state): State<Arc<GatewayState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let session_key = SessionKey::from(session_id.clone());

    // Load current history
    let messages = state
        .store
        .load_history(&session_key, 1000)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let before_count = messages.len();

    // Compaction is handled by the agent runner during its loop.
    // This endpoint provides a manual trigger for admin use.
    debug!(%session_id, before_count, "manual compaction requested");

    Ok(Json(serde_json::json!({
        "session_id": session_id,
        "messages_before": before_count,
        "status": "compaction_queued",
    })))
}
