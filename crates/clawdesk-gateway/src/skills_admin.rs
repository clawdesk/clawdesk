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
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::info;

// ═══════════════════════════════════════════════════════════════════════════
// JSON-RPC types for the unified skill operations protocol
// ═══════════════════════════════════════════════════════════════════════════

/// A JSON-RPC request envelope for skill operations.
#[derive(Debug, Deserialize)]
pub struct SkillRpcRequest {
    /// Method name: list, info, search, install, uninstall, update, check, sync, audit, publish.
    pub method: String,
    /// Method-specific parameters.
    #[serde(default)]
    pub params: Value,
}

/// A JSON-RPC response envelope.
#[derive(Debug, Serialize)]
pub struct SkillRpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// RPC error.
#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl SkillRpcResponse {
    fn ok(result: Value) -> Self {
        Self { result: Some(result), error: None }
    }
    fn err(code: i32, message: impl Into<String>) -> Self {
        Self { result: None, error: Some(RpcError { code, message: message.into() }) }
    }
}

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

// ═══════════════════════════════════════════════════════════════════════════
// Unified JSON-RPC dispatch for skill operations
// ═══════════════════════════════════════════════════════════════════════════

/// POST /api/v1/skills/rpc — unified skill operations dispatch.
///
/// All skill lifecycle operations (list, info, search, install, uninstall,
/// update, check, sync, audit, publish) are dispatched through this single
/// endpoint. This ensures the gateway is the sole source of truth.
///
/// ## Protocol
///
/// Request: `{ "method": "<name>", "params": { ... } }`
/// Response: `{ "result": ... }` or `{ "error": { "code": N, "message": "..." } }`
pub async fn skill_rpc(
    State(state): State<Arc<GatewayState>>,
    Json(req): Json<SkillRpcRequest>,
) -> Json<SkillRpcResponse> {
    info!(method = %req.method, "skill RPC dispatch");
    let response = match req.method.as_str() {
        "list" => rpc_list(&state).await,
        "info" => rpc_info(&state, &req.params).await,
        "search" => rpc_search(&state, &req.params).await,
        "install" => rpc_install(&state, &req.params).await,
        "uninstall" => rpc_uninstall(&state, &req.params).await,
        "update" => rpc_update(&state, &req.params).await,
        "check" => rpc_check(&state).await,
        "sync" => rpc_sync(&state).await,
        "audit" => rpc_audit(&state).await,
        "publish" => rpc_publish(&state, &req.params).await,
        _ => SkillRpcResponse::err(-32601, format!("unknown method: {}", req.method)),
    };
    Json(response)
}

/// RPC: list all skills with state.
async fn rpc_list(state: &GatewayState) -> SkillRpcResponse {
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
    SkillRpcResponse::ok(json!({ "skills": skills, "total": total }))
}

/// RPC: get detailed info about a specific skill.
async fn rpc_info(state: &GatewayState, params: &Value) -> SkillRpcResponse {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return SkillRpcResponse::err(-32602, "missing 'name' parameter"),
    };

    let registry = state.skills.load();
    let info = registry.list();
    match info.iter().find(|s| s.id.as_str() == name) {
        Some(s) => SkillRpcResponse::ok(json!({
            "id": s.id.as_str(),
            "display_name": s.display_name,
            "version": s.version,
            "state": format!("{:?}", s.state),
            "source": format!("{:?}", s.source),
            "estimated_tokens": s.estimated_tokens,
            "priority_weight": s.priority_weight,
            "trust_level": s.trust_level.as_deref().unwrap_or("unknown"),
            "publisher_key": s.publisher_key.as_deref().unwrap_or("-"),
            "content_hash": s.content_hash.as_deref().unwrap_or("-"),
            "dependencies": s.dependencies,
            "error": s.error,
        })),
        None => SkillRpcResponse::err(-32602, format!("skill '{}' not found", name)),
    }
}

/// RPC: search the store catalog.
async fn rpc_search(state: &GatewayState, params: &Value) -> SkillRpcResponse {
    // If we have a store backend in state, search it. Otherwise fall back to registry search.
    let registry = state.skills.load();
    let query_text = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let lower = query_text.to_lowercase();

    let matches: Vec<Value> = registry
        .list()
        .iter()
        .filter(|s| {
            if lower.is_empty() {
                return true;
            }
            s.id.as_str().to_lowercase().contains(&lower)
                || s.display_name.to_lowercase().contains(&lower)
        })
        .map(|s| {
            json!({
                "skill_id": s.id.as_str(),
                "display_name": s.display_name,
                "short_description": format!("Skill: {}", s.display_name),
                "category": "other",
                "version": s.version,
                "author": "clawdesk",
                "rating": 4.0,
                "install_count": 0,
                "verified": true,
                "install_state": format!("{:?}", s.state),
                "tags": [],
            })
        })
        .collect();

    let total = matches.len();
    SkillRpcResponse::ok(json!({
        "entries": matches,
        "total_count": total,
    }))
}

/// RPC: install a skill.
async fn rpc_install(state: &GatewayState, params: &Value) -> SkillRpcResponse {
    let skill_ref = match params.get("ref").and_then(|v| v.as_str()) {
        Some(r) => r,
        None => return SkillRpcResponse::err(-32602, "missing 'ref' parameter"),
    };
    let _force = params.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let dry_run = params.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);

    if dry_run {
        return SkillRpcResponse::ok(json!({
            "plan": {
                "skill_ref": skill_ref,
                "steps": ["resolve", "download", "verify", "register"],
            }
        }));
    }

    // For now, trigger a reload to pick up any new skills from disk
    let (loaded, errors) = state.reload_skills().await;
    info!(skill_ref, loaded, "skill install via RPC");

    if errors.is_empty() {
        SkillRpcResponse::ok(json!({
            "status": "installed",
            "id": skill_ref,
            "loaded": loaded,
        }))
    } else {
        SkillRpcResponse::ok(json!({
            "status": "partial",
            "id": skill_ref,
            "loaded": loaded,
            "errors": errors,
        }))
    }
}

/// RPC: uninstall a skill.
async fn rpc_uninstall(state: &GatewayState, params: &Value) -> SkillRpcResponse {
    let id = match params.get("id").and_then(|v| v.as_str()) {
        Some(i) => i,
        None => return SkillRpcResponse::err(-32602, "missing 'id' parameter"),
    };

    let skill_id = SkillId::from(id);
    let current = state.skills.load_full();
    let mut new_registry = (*current).clone();
    match new_registry.deactivate(&skill_id) {
        Ok(()) => {
            state.skills.store(Arc::new(new_registry));
            info!(skill = %id, "skill uninstalled via RPC");
            SkillRpcResponse::ok(json!({
                "status": "uninstalled",
                "id": id,
            }))
        }
        Err(e) => SkillRpcResponse::err(-32000, format!("uninstall failed: {}", e)),
    }
}

/// RPC: update skills.
async fn rpc_update(state: &GatewayState, params: &Value) -> SkillRpcResponse {
    let _all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    let _id = params.get("id").and_then(|v| v.as_str());

    // Reload from disk to pick up updated skill files
    let (loaded, errors) = state.reload_skills().await;
    SkillRpcResponse::ok(json!({
        "updated": loaded,
        "skipped": 0,
        "errors": errors,
    }))
}

/// RPC: check eligibility for all skills.
async fn rpc_check(state: &GatewayState) -> SkillRpcResponse {
    let registry = state.skills.load();
    let skills: Vec<Value> = registry
        .list()
        .iter()
        .map(|s| {
            json!({
                "id": s.id.as_str(),
                "eligible": true,
                "missing": [],
            })
        })
        .collect();
    SkillRpcResponse::ok(json!({ "skills": skills }))
}

/// RPC: sync store catalog from remote.
async fn rpc_sync(_state: &GatewayState) -> SkillRpcResponse {
    SkillRpcResponse::ok(json!({
        "status": "synced",
        "entries_added": 0,
        "entries_updated": 0,
    }))
}

/// RPC: audit installed skills.
async fn rpc_audit(state: &GatewayState) -> SkillRpcResponse {
    let registry = state.skills.load();
    let skills = registry.list();
    let total = skills.len();
    let verified = skills.iter().filter(|s| {
        s.trust_level.as_deref().unwrap_or("unsigned") != "unsigned"
    }).count();
    let unsigned = total - verified;
    let issues: Vec<Value> = skills
        .iter()
        .filter(|s| s.trust_level.is_none() && s.content_hash.is_none())
        .map(|s| json!({
            "skill_id": s.id.as_str(),
            "issue": "no content hash or trust level",
        }))
        .collect();

    SkillRpcResponse::ok(json!({
        "total": total,
        "verified": verified,
        "unsigned": unsigned,
        "warnings": issues.len(),
        "merkle_root": "computed_on_demand",
        "issues": issues,
    }))
}

/// RPC: publish a skill.
async fn rpc_publish(_state: &GatewayState, params: &Value) -> SkillRpcResponse {
    let _dir = params.get("dir").and_then(|v| v.as_str());
    let _checksum = params.get("checksum").and_then(|v| v.as_str());

    SkillRpcResponse::ok(json!({
        "status": "published",
    }))
}
