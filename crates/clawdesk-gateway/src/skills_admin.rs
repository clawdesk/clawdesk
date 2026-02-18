//! Skill administration API endpoints.
//!
//! Provides HTTP endpoints for managing the skill lifecycle at runtime:
//! listing, reloading, activating, deactivating, and hot-swapping skills
//! without restarting the gateway.
//!
//! ## Endpoints
//!
//! - `GET    /api/v1/admin/skills`              — list all skills + state
//! - `POST   /api/v1/admin/skills/reload`       — re-scan filesystem
//! - `POST   /api/v1/admin/skills/:id/activate` — activate a skill
//! - `POST   /api/v1/admin/skills/:id/deactivate` — deactivate a skill
//! - `POST   /api/v1/admin/channels/reload`     — re-read config and rebuild channels

use crate::bootstrap::ClawDeskConfig;
use crate::state::GatewayState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use clawdesk_channel::registry::ChannelRegistry;
use clawdesk_channels::factory::ChannelConfig;
use clawdesk_skills::definition::SkillId;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::info;

/// GET /api/v1/admin/skills — list all skills with their state.
pub async fn list_skills(
    State(state): State<Arc<GatewayState>>,
) -> Json<Value> {
    let registry = state.skills.load();
    let skills: Vec<Value> = registry
        .list()
        .iter()
        .map(|s| {
            json!({
                "id": s.id.as_str(),
                "display_name": s.display_name,
                "version": s.version,
                "state": format!("{:?}", s.state),
                "source": format!("{:?}", s.source),
                "estimated_tokens": s.estimated_tokens,
                "priority_weight": s.priority_weight,
                "error": s.error,
            })
        })
        .collect();
    let total = skills.len();
    Json(json!({ "skills": skills, "total": total }))
}

/// POST /api/v1/admin/skills/reload — re-scan filesystem and reload all skills.
///
/// This performs a full hot-reload:
/// 1. SkillLoader re-scans `~/.clawdesk/skills/`
/// 2. A fresh SkillRegistry is built
/// 3. ArcSwap atomically replaces the old registry
pub async fn reload_skills(
    State(state): State<Arc<GatewayState>>,
) -> Json<Value> {
    let (loaded, errors) = state.reload_skills().await;
    let total = state.skills.load().len();
    info!(loaded, total, errors = errors.len(), "skills reloaded");
    Json(json!({
        "loaded": loaded,
        "errors": errors,
        "total": total,
    }))
}

/// POST /api/v1/admin/skills/:id/activate — activate a specific skill.
///
/// Uses the ArcSwap COW (copy-on-write) pattern:
/// 1. Clone the current registry
/// 2. Mutate the clone (activate the skill)
/// 3. Atomically swap via ArcSwap
pub async fn activate_skill(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let skill_id = SkillId::from(id.as_str());
    let current = state.skills.load_full();
    let mut new_registry = (*current).clone();
    match new_registry.activate(&skill_id) {
        Ok(()) => {
            state.skills.store(Arc::new(new_registry));
            info!(skill = %id, "skill activated via admin API");
            (StatusCode::OK, Json(json!({ "status": "activated", "id": id })))
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))),
    }
}

/// POST /api/v1/admin/skills/:id/deactivate — deactivate a specific skill.
pub async fn deactivate_skill(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let skill_id = SkillId::from(id.as_str());
    let current = state.skills.load_full();
    let mut new_registry = (*current).clone();
    match new_registry.deactivate(&skill_id) {
        Ok(()) => {
            state.skills.store(Arc::new(new_registry));
            info!(skill = %id, "skill deactivated via admin API");
            (
                StatusCode::OK,
                Json(json!({ "status": "deactivated", "id": id })),
            )
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))),
    }
}

/// POST /api/v1/admin/channels/reload — re-read config and rebuild channels.
///
/// Re-reads the config file (from `CLAWDESK_CONFIG` or default path),
/// constructs new channel instances via the `ChannelFactory`, and
/// atomically swaps the `ChannelRegistry` via ArcSwap.
pub async fn reload_channels(
    State(state): State<Arc<GatewayState>>,
) -> (StatusCode, Json<Value>) {
    let config = ClawDeskConfig::load_or_default();

    let mut registry = ChannelRegistry::new();
    let mut errors: Vec<String> = Vec::new();
    let mut created = 0usize;

    let factory = state.channel_factory.load();
    for (kind, entry) in &config.channels {
        if !entry.enabled {
            continue;
        }
        let ch_config = ChannelConfig::new(kind.as_str(), entry.settings.clone());
        match factory.create(&ch_config) {
            Ok(ch) => {
                match registry.register(ch) {
                    clawdesk_channel::registry::RegistrationResult::Ok { .. } => {
                        created += 1;
                    }
                    clawdesk_channel::registry::RegistrationResult::Rejected { reason } => {
                        errors.push(format!("channel '{}' registration rejected: {}", kind, reason));
                    }
                }
            }
            Err(e) => errors.push(format!("{}", e)),
        }
    }

    state.channels.store(Arc::new(registry));
    info!(created, errors = errors.len(), "channels reloaded from config");

    (
        StatusCode::OK,
        Json(json!({
            "created": created,
            "errors": errors,
            "available_kinds": factory.available_kinds(),
        })),
    )
}
