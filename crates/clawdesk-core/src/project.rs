//! # Project workspace isolation service
//!
//! Each chat session gets an isolated project directory. This service
//! manages the lifecycle of per-chat project directories and provides
//! file operations scoped to them.
//!
//! ## Directory Structure
//!
//! ```text
//! ~/.clawdesk/workspace/
//! ├── projects/
//! │   ├── {chat_id_1}/        ← "Build a todo app"
//! │   │   ├── package.json
//! │   │   ├── src/
//! │   │   └── ...
//! │   ├── {chat_id_2}/        ← "Build an address book"
//! │   │   ├── package.json
//! │   │   └── ...
//! │   └── ...
//! └── shared/                 ← Global shared files (templates, config)
//! ```

use std::path::{Path, PathBuf};
use serde::Serialize;
use tracing::{error, info};

/// Metadata about a file entry in a project workspace.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectFileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
}

/// Manages per-chat project directories.
pub struct ProjectService {
    workspace_root: PathBuf,
}

impl ProjectService {
    /// Create a new project service with the given workspace root.
    pub fn new(workspace_root: PathBuf) -> Self {
        let projects_dir = workspace_root.join("projects");
        if !projects_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&projects_dir) {
                error!(path = ?projects_dir, error = %e, "Failed to create projects directory");
            }
        }
        Self { workspace_root }
    }

    /// Get the workspace root path.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Resolve the project directory for a specific chat.
    /// Creates the directory if it doesn't exist.
    pub fn project_dir(&self, chat_id: &str) -> PathBuf {
        let dir = self.workspace_root.join("projects").join(chat_id);
        if !dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                error!(path = ?dir, error = %e, "Failed to create project directory for chat");
            } else {
                info!(chat_id = %chat_id, path = ?dir, "Created project directory");
            }
        }
        dir
    }

    /// List files in a chat's project directory.
    pub fn list_files(
        &self,
        chat_id: &str,
        relative_path: Option<&str>,
    ) -> Result<Vec<ProjectFileEntry>, String> {
        let project_root = self.project_dir(chat_id);
        let dir = match relative_path {
            Some(rel) if !rel.is_empty() => {
                safe_resolve(&project_root, rel)?
            }
            _ => project_root
                .canonicalize()
                .map_err(|e| format!("Cannot resolve project root: {e}"))?,
        };

        if !dir.is_dir() {
            return Err("Not a directory".into());
        }

        let root_canonical = project_root
            .canonicalize()
            .map_err(|e| format!("Cannot resolve project root: {e}"))?;

        let mut entries = Vec::new();
        let read_dir = std::fs::read_dir(&dir)
            .map_err(|e| format!("Cannot read directory: {e}"))?;

        for entry in read_dir.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().to_string();
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

            entries.push(ProjectFileEntry {
                name,
                path: rel,
                is_dir: meta.is_dir(),
                size: meta.len(),
                modified,
            });
        }

        entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name))
        });
        Ok(entries)
    }

    /// Read a file from a chat's project workspace.
    pub fn read_file(
        &self,
        chat_id: &str,
        relative_path: &str,
    ) -> Result<String, String> {
        let project_root = self.project_dir(chat_id);
        let resolved = safe_resolve(&project_root, relative_path)?;

        if !resolved.is_file() {
            return Err("Not a file".into());
        }

        let meta = std::fs::metadata(&resolved)
            .map_err(|e| format!("Cannot stat file: {e}"))?;
        if meta.len() > 1_048_576 {
            return Err("File too large (>1 MB)".into());
        }

        std::fs::read_to_string(&resolved)
            .map_err(|e| format!("Cannot read file: {e}"))
    }

    /// Build a scoped tool registry for a specific chat's project directory.
    ///
    /// This re-creates file/shell tools confined to the per-chat workspace,
    /// ensuring each chat's agent can only operate within its own project.
    pub fn scoped_tool_registry(
        &self,
        chat_id: &str,
        base_registry: &clawdesk_agents::tools::ToolRegistry,
    ) -> clawdesk_agents::tools::ToolRegistry {
        let project_dir = self.project_dir(chat_id);
        let mut registry = clawdesk_agents::tools::ToolRegistry::new();

        // Register builtin tools scoped to the per-chat workspace
        clawdesk_agents::builtin_tools::register_builtin_tools(
            &mut registry,
            Some(project_dir),
        );

        // Copy non-filesystem tools from the base registry
        let scoped_names: std::collections::HashSet<&str> = [
            "shell_exec", "file_read", "file_write", "file_edit",
            "file_list", "grep", "process_start", "process_poll",
            "process_write", "process_kill", "process_list",
        ].iter().copied().collect();

        for schema in base_registry.schemas() {
            if !scoped_names.contains(schema.name.as_str()) {
                if let Some(tool) = base_registry.get(&schema.name) {
                    if registry.get(&schema.name).is_none() {
                        registry.register(tool);
                    }
                }
            }
        }

        registry
    }
}

/// Path traversal prevention — validates that a relative path stays within root.
fn safe_resolve(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.starts_with('/') || relative.starts_with('\\') || relative.contains("..") {
        return Err("Path traversal not allowed".into());
    }
    let joined = root.join(relative);

    // For existing paths, canonicalize and verify
    if joined.exists() {
        let canonical = joined
            .canonicalize()
            .map_err(|e| format!("Cannot resolve path: {e}"))?;
        let root_canonical = root
            .canonicalize()
            .map_err(|e| format!("Cannot resolve root: {e}"))?;
        if !canonical.starts_with(&root_canonical) {
            return Err("Path escapes workspace".into());
        }
        return Ok(canonical);
    }

    // For non-existent paths (writes), check the parent
    if let Some(parent) = joined.parent() {
        if parent.exists() {
            let parent_canonical = parent
                .canonicalize()
                .map_err(|e| format!("Cannot resolve parent: {e}"))?;
            let root_canonical = root
                .canonicalize()
                .map_err(|e| format!("Cannot resolve root: {e}"))?;
            if !parent_canonical.starts_with(&root_canonical) {
                return Err("Path escapes workspace boundary".into());
            }
        }
    }

    Ok(joined)
}
