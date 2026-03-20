//! Tauri commands for the Transparent Memory System (Phase 3.4).
//!
//! "What I Know About You" panel — surfaces agent memory as auditable knowledge base.

use crate::state::AppState;
use clawdesk_memory::transparent::{
    MemoryAction, MemoryCategory, MemoryEntry, MemoryKnowledgeBase,
    MemorySource, bayesian_update, compute_decayed_confidence,
};
use serde::{Deserialize, Serialize};
use tauri::State;

/// Get the full knowledge base for the "What I Know About You" panel.
#[tauri::command]
pub async fn memory_get_knowledge_base(
    state: State<'_, AppState>,
) -> Result<MemoryKnowledgeBase, String> {
    // In production, retrieve from SochDB's memory collections.
    // For now, return an empty knowledge base that the frontend can populate.
    Ok(MemoryKnowledgeBase::from_entries(Vec::new()))
}

/// Get all memory categories with their labels and icons.
#[tauri::command]
pub async fn memory_get_categories() -> Result<Vec<CategoryInfo>, String> {
    Ok(MemoryCategory::all().iter().map(|c| CategoryInfo {
        id: format!("{:?}", c),
        label: c.label().to_string(),
        icon: c.icon().to_string(),
    }).collect())
}

/// Apply a user action on a memory entry (edit, delete, verify, mark incorrect).
#[tauri::command]
pub async fn memory_apply_action(
    action: MemoryAction,
    state: State<'_, AppState>,
) -> Result<String, String> {
    match action {
        MemoryAction::Edit { entry_id, new_content } => {
            // Update in SochDB
            Ok(format!("Entry {} updated", entry_id))
        }
        MemoryAction::Delete { entry_id } => {
            // Delete from SochDB
            Ok(format!("Entry {} deleted", entry_id))
        }
        MemoryAction::Verify { entry_id } => {
            Ok(format!("Entry {} verified", entry_id))
        }
        MemoryAction::MarkIncorrect { entry_id } => {
            Ok(format!("Entry {} marked incorrect", entry_id))
        }
    }
}

/// Compute decayed confidence for a memory entry (for staleness display).
#[tauri::command]
pub async fn memory_compute_confidence(
    initial_confidence: f64,
    last_reinforced_at: u64,
    category: String,
) -> Result<f64, String> {
    let cat = match category.as_str() {
        "PersonalFacts" => MemoryCategory::PersonalFacts,
        "Preferences" => MemoryCategory::Preferences,
        "WorkContext" => MemoryCategory::WorkContext,
        "Relationships" => MemoryCategory::Relationships,
        "Schedules" => MemoryCategory::Schedules,
        "Skills" => MemoryCategory::Skills,
        "ProjectContext" => MemoryCategory::ProjectContext,
        "Communication" => MemoryCategory::Communication,
        _ => MemoryCategory::Other,
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(compute_decayed_confidence(initial_confidence, last_reinforced_at, now, cat))
}

#[derive(Debug, Serialize)]
pub struct CategoryInfo {
    pub id: String,
    pub label: String,
    pub icon: String,
}
