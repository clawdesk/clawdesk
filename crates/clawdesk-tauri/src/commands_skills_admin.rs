//! Skills administration IPC commands — hot-reload, activate/deactivate.
//!
//! Surfaces the gateway's admin skills routes directly to the Tauri frontend
//! so the desktop UI can manage skills without going through HTTP.
//!
//! Previously, skill admin was only available via the gateway REST API
//! (`/api/v1/admin/skills/*`). This module provides native IPC equivalents.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::{info, warn};

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Response types
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Debug, Serialize)]
pub struct SkillAdminInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub active: bool,
    pub tools_count: usize,
    pub category: String,
}

#[derive(Debug, Serialize)]
pub struct SkillReloadResult {
    pub loaded: usize,
    pub errors: Vec<String>,
    pub skills_dir: String,
}

#[derive(Debug, Serialize)]
pub struct SkillActivateResult {
    pub skill_id: String,
    pub active: bool,
    pub message: String,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Admin commands
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// List all skills with admin metadata (active state, tool count, etc.)
#[tauri::command]
pub async fn admin_list_skills(
    state: State<'_, AppState>,
) -> Result<Vec<SkillAdminInfo>, String> {
    let reg = state.skill_registry.read().map_err(|e| e.to_string())?;
    let skills = reg.list();
    let infos = skills
        .iter()
        .map(|s| SkillAdminInfo {
            id: s.id.0.clone(),
            name: s.display_name.clone(),
            description: String::new(), // SkillInfo doesn't carry the full description
            version: s.version.clone(),
            active: matches!(s.state, clawdesk_skills::definition::SkillState::Active),
            tools_count: 0, // tool bindings are on Skill, not on SkillInfo
            category: String::new(),
        })
        .collect();
    Ok(infos)
}

/// Hot-reload skills from the filesystem (`~/.clawdesk/data/skills/`).
///
/// Scans the skills directory, loads/parses skill definitions, and
/// atomically replaces the in-memory skill registry.
#[tauri::command]
pub async fn admin_reload_skills(
    state: State<'_, AppState>,
) -> Result<SkillReloadResult, String> {
    let skills_dir = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".clawdesk")
        .join("data")
        .join("skills");

    info!(dir = %skills_dir.display(), "Hot-reloading skills from filesystem");

    // Use the skill loader to scan and rebuild
    let loader = clawdesk_skills::loader::SkillLoader::new(skills_dir.clone());
    let result = loader.load_fresh(true).await;

    let loaded = result.loaded;
    let errors = result.errors.clone();

    // Atomically replace the registry
    {
        let mut reg = state.skill_registry.write().map_err(|e| e.to_string())?;
        *reg = result.registry;
    }

    info!(loaded, errors = errors.len(), "Skills hot-reload complete");

    Ok(SkillReloadResult {
        loaded,
        errors,
        skills_dir: skills_dir.to_string_lossy().to_string(),
    })
}

/// Activate a skill by ID.
#[tauri::command]
pub async fn admin_activate_skill(
    skill_id: String,
    state: State<'_, AppState>,
) -> Result<SkillActivateResult, String> {
    let mut reg = state.skill_registry.write().map_err(|e| e.to_string())?;
    let sid = clawdesk_skills::SkillId(skill_id.clone());
    reg.activate(&sid)?;
    info!(skill_id = %skill_id, "Skill activated via admin");
    Ok(SkillActivateResult {
        skill_id,
        active: true,
        message: "Skill activated successfully".to_string(),
    })
}

/// Deactivate a skill by ID.
#[tauri::command]
pub async fn admin_deactivate_skill(
    skill_id: String,
    state: State<'_, AppState>,
) -> Result<SkillActivateResult, String> {
    let mut reg = state.skill_registry.write().map_err(|e| e.to_string())?;
    let sid = clawdesk_skills::SkillId(skill_id.clone());
    reg.deactivate(&sid)?;
    info!(skill_id = %skill_id, "Skill deactivated via admin");
    Ok(SkillActivateResult {
        skill_id,
        active: false,
        message: "Skill deactivated successfully".to_string(),
    })
}

/// Get the skills filesystem directory path and status.
#[tauri::command]
pub async fn admin_get_skills_dir() -> Result<serde_json::Value, String> {
    let skills_dir = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join(".clawdesk")
        .join("data")
        .join("skills");

    let exists = skills_dir.exists();
    let file_count = if exists {
        std::fs::read_dir(&skills_dir)
            .map(|entries| entries.count())
            .unwrap_or(0)
    } else {
        0
    };

    Ok(serde_json::json!({
        "path": skills_dir.to_string_lossy(),
        "exists": exists,
        "file_count": file_count,
    }))
}
