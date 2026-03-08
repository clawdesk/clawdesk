//! Workspace file browser IPC commands.
//!
//! Lets the frontend list and read files within the workspace root.
//! All paths are resolved relative to `AppState::workspace_root` and
//! validated to prevent path-traversal attacks.

use crate::state::AppState;
use serde::Serialize;
use std::path::PathBuf;
use tauri::State;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Types
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[derive(Debug, Serialize)]
pub struct WorkspaceFileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Helpers
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Resolve a user-provided relative path against the workspace root,
/// canonicalise both, and confirm the result stays inside the root.
fn safe_resolve(workspace_root: &std::path::Path, relative: &str) -> Result<PathBuf, String> {
    // Reject absolute paths and obvious traversal attempts early.
    if relative.starts_with('/') || relative.starts_with('\\') || relative.contains("..") {
        return Err("Path traversal not allowed".into());
    }
    let joined = workspace_root.join(relative);
    let canonical = joined
        .canonicalize()
        .map_err(|e| format!("Cannot resolve path: {e}"))?;
    let root_canonical = workspace_root
        .canonicalize()
        .map_err(|e| format!("Cannot resolve workspace root: {e}"))?;
    if !canonical.starts_with(&root_canonical) {
        return Err("Path escapes workspace".into());
    }
    Ok(canonical)
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Commands
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Return the workspace root path (for display in the UI).
#[tauri::command]
pub async fn get_workspace_root(
    state: State<'_, AppState>,
) -> Result<String, String> {
    Ok(state.workspace_root.display().to_string())
}

/// List files & directories under a relative path within the workspace.
#[tauri::command]
pub async fn list_workspace_files(
    relative_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<WorkspaceFileEntry>, String> {
    let dir = match &relative_path {
        Some(rel) if !rel.is_empty() => safe_resolve(&state.workspace_root, rel)?,
        _ => state
            .workspace_root
            .canonicalize()
            .map_err(|e| format!("Cannot resolve workspace root: {e}"))?,
    };

    if !dir.is_dir() {
        return Err("Not a directory".into());
    }

    let root_canonical = state
        .workspace_root
        .canonicalize()
        .map_err(|e| format!("Cannot resolve workspace root: {e}"))?;

    let mut entries = Vec::new();
    let read_dir = std::fs::read_dir(&dir).map_err(|e| format!("Cannot read directory: {e}"))?;

    for entry in read_dir.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden files
        if name.starts_with('.') {
            continue;
        }
        let full_path = entry.path();
        let rel = full_path
            .strip_prefix(&root_canonical)
            .unwrap_or(&full_path)
            .to_string_lossy()
            .to_string();
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        entries.push(WorkspaceFileEntry {
            name,
            path: rel,
            is_dir: meta.is_dir(),
            size: meta.len(),
            modified,
        });
    }

    // Sort: directories first, then alphabetical
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

    Ok(entries)
}

/// Read a file within the workspace (text only, capped to 1 MB).
#[tauri::command]
pub async fn read_workspace_file(
    relative_path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let resolved = safe_resolve(&state.workspace_root, &relative_path)?;

    if !resolved.is_file() {
        return Err("Not a file".into());
    }

    let meta = std::fs::metadata(&resolved).map_err(|e| format!("Cannot stat file: {e}"))?;
    if meta.len() > 1_048_576 {
        return Err("File too large (>1 MB)".into());
    }

    std::fs::read_to_string(&resolved).map_err(|e| format!("Cannot read file: {e}"))
}
