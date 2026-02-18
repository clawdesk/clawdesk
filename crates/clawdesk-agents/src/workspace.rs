//! Workspace isolation — chroot-style path confinement for agent file operations.
//!
//! When an agent has a `workspace_path` configured, all file-system operations
//! (read, write, list) are validated against the workspace root to prevent
//! path traversal attacks.
//!
//! ## Security Model
//! - Paths are canonicalized before validation (resolves `..`, symlinks)
//! - The workspace root itself is canonicalized on creation
//! - All operations must stay within the workspace subtree
//! - Fail-closed: if canonicalization fails, the path is rejected

use std::path::{Path, PathBuf};

/// Result of a path confinement check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfinementResult {
    /// Path is within the workspace.
    Allowed(PathBuf),
    /// Path escapes the workspace.
    Denied { requested: String, workspace: String },
    /// No workspace configured — all paths allowed.
    NoWorkspace,
}

/// Guards file-system access to a workspace directory.
#[derive(Debug, Clone)]
pub struct WorkspaceGuard {
    /// Canonicalized workspace root.
    root: PathBuf,
}

impl WorkspaceGuard {
    /// Create a new workspace guard for the given root directory.
    ///
    /// The root path is canonicalized to resolve symlinks and normalize the path.
    /// Returns `None` if the path doesn't exist or can't be canonicalized.
    pub fn new(root: &Path) -> Option<Self> {
        let canonical = root.canonicalize().ok()?;
        Some(Self { root: canonical })
    }

    /// Create a workspace guard, creating the directory if needed.
    pub fn ensure(root: &Path) -> Result<Self, std::io::Error> {
        std::fs::create_dir_all(root)?;
        let canonical = root.canonicalize()?;
        Ok(Self { root: canonical })
    }

    /// Check if a path is confined within this workspace.
    pub fn confine(&self, path: &Path) -> ConfinementResult {
        // Try to canonicalize the requested path.
        // If the path doesn't exist yet, canonicalize the longest existing prefix
        // and check that against the root.
        match path.canonicalize() {
            Ok(canonical) => {
                if canonical.starts_with(&self.root) {
                    ConfinementResult::Allowed(canonical)
                } else {
                    ConfinementResult::Denied {
                        requested: path.display().to_string(),
                        workspace: self.root.display().to_string(),
                    }
                }
            }
            Err(_) => {
                // Path doesn't exist yet — check the parent chain.
                if let Some(parent) = path.parent() {
                    match parent.canonicalize() {
                        Ok(canonical_parent) => {
                            if canonical_parent.starts_with(&self.root) {
                                // Parent is within workspace, so this new path would be too.
                                let file_name = path.file_name().unwrap_or_default();
                                ConfinementResult::Allowed(canonical_parent.join(file_name))
                            } else {
                                ConfinementResult::Denied {
                                    requested: path.display().to_string(),
                                    workspace: self.root.display().to_string(),
                                }
                            }
                        }
                        Err(_) => {
                            // Fail-closed: can't resolve parent.
                            ConfinementResult::Denied {
                                requested: path.display().to_string(),
                                workspace: self.root.display().to_string(),
                            }
                        }
                    }
                } else {
                    ConfinementResult::Denied {
                        requested: path.display().to_string(),
                        workspace: self.root.display().to_string(),
                    }
                }
            }
        }
    }

    /// Get the workspace root path.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Create a workspace guard from an optional path string (as in AgentConfig).
///
/// Returns `None` if no workspace path is configured or if it can't be resolved.
pub fn workspace_guard_from_config(workspace_path: Option<&str>) -> Option<WorkspaceGuard> {
    workspace_path.and_then(|p| WorkspaceGuard::new(Path::new(p)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn confine_within_workspace() {
        let dir = std::env::temp_dir().join("clawdesk_ws_test_confine");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let sub = dir.join("subdir");
        fs::create_dir_all(&sub).unwrap();

        let guard = WorkspaceGuard::new(&dir).unwrap();
        match guard.confine(&sub) {
            ConfinementResult::Allowed(_) => {} // expected
            other => panic!("expected Allowed, got {:?}", other),
        }
    }

    #[test]
    fn deny_outside_workspace() {
        let dir = std::env::temp_dir().join("clawdesk_ws_test_deny");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let guard = WorkspaceGuard::new(&dir).unwrap();
        // Attempt to access something outside
        match guard.confine(Path::new("/etc/passwd")) {
            ConfinementResult::Denied { .. } => {} // expected
            other => panic!("expected Denied, got {:?}", other),
        }
    }

    #[test]
    fn allow_new_file_in_workspace() {
        let dir = std::env::temp_dir().join("clawdesk_ws_test_newfile");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let guard = WorkspaceGuard::new(&dir).unwrap();
        let new_file = dir.join("new_file.txt");
        match guard.confine(&new_file) {
            ConfinementResult::Allowed(p) => {
                assert!(p.to_string_lossy().contains("new_file.txt"));
            }
            other => panic!("expected Allowed for new file, got {:?}", other),
        }
    }

    #[test]
    fn ensure_creates_directory() {
        let dir = std::env::temp_dir().join("clawdesk_ws_test_ensure");
        let _ = fs::remove_dir_all(&dir);

        let guard = WorkspaceGuard::ensure(&dir).unwrap();
        assert!(dir.exists());
        assert_eq!(guard.root(), dir.canonicalize().unwrap());
    }

    #[test]
    fn config_none_returns_none() {
        assert!(workspace_guard_from_config(None).is_none());
    }
}
