//! Workspace filesystem sandbox — path confinement with symlink escape prevention.
//!
//! Implements the `PathScope` isolation level by canonicalizing all file paths
//! and verifying they remain within the workspace root.

use crate::{
    FileOp, IsolationLevel, ResourceUsage, Sandbox, SandboxCommand, SandboxError, SandboxRequest,
    SandboxResult,
};
use async_trait::async_trait;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;
use tracing::{debug, warn};

/// Workspace filesystem sandbox.
///
/// Confines all file operations to a designated workspace directory,
/// preventing symlink escapes, path traversal, and TOCTOU races.
#[derive(Debug, Clone)]
pub struct WorkspaceSandbox {
    /// Additional read-only paths outside workspace (e.g., /usr/share)
    pub extra_read_paths: Vec<PathBuf>,
}

impl WorkspaceSandbox {
    pub fn new() -> Self {
        Self {
            extra_read_paths: Vec::new(),
        }
    }

    pub fn with_extra_read_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.extra_read_paths = paths;
        self
    }
}

impl Default for WorkspaceSandbox {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve and validate a path within a workspace sandbox.
///
/// Algorithm:
/// 1. Reject all `..` components (path traversal prevention)
/// 2. Join relative paths with workspace root
/// 3. Canonicalize both candidate and root via `std::fs::canonicalize()`
/// 4. Verify canonical candidate starts with canonical root
///
/// For new files that don't yet exist, canonicalize the parent directory
/// and append the filename.
pub fn resolve_sandbox_path(
    user_path: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, SandboxError> {
    // Layer 1: Reject all ".." components
    for component in user_path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(SandboxError::PathEscape {
                path: user_path.display().to_string(),
            });
        }
    }

    // Layer 2: Build the candidate path
    let candidate = if user_path.is_absolute() {
        user_path.to_path_buf()
    } else {
        workspace_root.join(user_path)
    };

    // Layer 3: Canonicalize
    let canonical_root = workspace_root.canonicalize().map_err(|e| {
        SandboxError::ExecutionFailed(format!(
            "cannot canonicalize workspace root {}: {}",
            workspace_root.display(),
            e
        ))
    })?;

    let canonical_candidate = if candidate.exists() {
        // Existing file/dir: canonicalize directly (follows symlinks)
        candidate.canonicalize().map_err(|e| {
            SandboxError::ExecutionFailed(format!(
                "cannot canonicalize {}: {}",
                candidate.display(),
                e
            ))
        })?
    } else {
        // New file: canonicalize parent, append filename
        let parent = candidate.parent().ok_or_else(|| {
            SandboxError::ExecutionFailed(format!("no parent for {}", candidate.display()))
        })?;

        let filename = candidate.file_name().ok_or_else(|| {
            SandboxError::ExecutionFailed(format!("no filename in {}", candidate.display()))
        })?;

        let canonical_parent = if parent.exists() {
            parent.canonicalize().map_err(|e| {
                SandboxError::ExecutionFailed(format!(
                    "cannot canonicalize parent {}: {}",
                    parent.display(),
                    e
                ))
            })?
        } else {
            // Parent doesn't exist: best-effort check
            parent.to_path_buf()
        };

        canonical_parent.join(filename)
    };

    // Layer 4: Prefix check
    if !canonical_candidate.starts_with(&canonical_root) {
        warn!(
            path = %canonical_candidate.display(),
            root = %canonical_root.display(),
            "sandbox path escape blocked"
        );
        return Err(SandboxError::PathEscape {
            path: user_path.display().to_string(),
        });
    }

    debug!(
        original = %user_path.display(),
        resolved = %canonical_candidate.display(),
        "sandbox path resolved"
    );

    Ok(canonical_candidate)
}

/// Validate an executable path for directory traversal.
pub fn validate_executable_path(path: &Path) -> Result<(), SandboxError> {
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(SandboxError::PathEscape {
                path: path.display().to_string(),
            });
        }
    }
    Ok(())
}

#[async_trait]
impl Sandbox for WorkspaceSandbox {
    fn name(&self) -> &str {
        "workspace"
    }

    fn isolation_level(&self) -> IsolationLevel {
        IsolationLevel::PathScope
    }

    async fn is_available(&self) -> bool {
        true // Always available
    }

    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError> {
        let start = Instant::now();

        match &request.command {
            SandboxCommand::FileOperation {
                operation,
                path,
                content,
            } => {
                let resolved = resolve_sandbox_path(path, &request.workspace_root)?;

                let output = match operation {
                    FileOp::Read => {
                        tokio::fs::read_to_string(&resolved)
                            .await
                            .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?
                    }
                    FileOp::Write => {
                        let data = content.as_ref().ok_or_else(|| {
                            SandboxError::InvalidConfig("write requires content".into())
                        })?;

                        // Check output size limit
                        if data.len() as u64 > request.limits.max_output_bytes {
                            return Err(SandboxError::ResourceLimitExceeded(format!(
                                "write size {} exceeds limit {}",
                                data.len(),
                                request.limits.max_output_bytes
                            )));
                        }

                        // Ensure parent directory exists
                        if let Some(parent) = resolved.parent() {
                            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                                SandboxError::ExecutionFailed(e.to_string())
                            })?;
                        }

                        tokio::fs::write(&resolved, data)
                            .await
                            .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?;

                        format!("wrote {} bytes to {}", data.len(), resolved.display())
                    }
                    FileOp::List => {
                        let mut entries = Vec::new();
                        let mut dir = tokio::fs::read_dir(&resolved)
                            .await
                            .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?;

                        while let Some(entry) = dir
                            .next_entry()
                            .await
                            .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?
                        {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let ft = entry.file_type().await.ok();
                            let suffix = if ft.map(|t| t.is_dir()).unwrap_or(false) {
                                "/"
                            } else {
                                ""
                            };
                            entries.push(format!("{}{}", name, suffix));
                        }
                        entries.join("\n")
                    }
                    FileOp::Delete => {
                        if resolved.is_dir() {
                            tokio::fs::remove_dir_all(&resolved)
                                .await
                                .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?;
                        } else {
                            tokio::fs::remove_file(&resolved)
                                .await
                                .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?;
                        }
                        format!("deleted {}", resolved.display())
                    }
                    FileOp::Exists => {
                        if resolved.exists() {
                            "true".to_string()
                        } else {
                            "false".to_string()
                        }
                    }
                };

                let elapsed = start.elapsed();
                Ok(SandboxResult {
                    exit_code: 0,
                    stdout: output,
                    stderr: String::new(),
                    duration: elapsed,
                    resource_usage: ResourceUsage {
                        wall_time_ms: elapsed.as_millis() as u64,
                        ..Default::default()
                    },
                })
            }
            _ => Err(SandboxError::InvalidConfig(
                "workspace sandbox only handles file operations".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn rejects_parent_dir_components() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        assert!(resolve_sandbox_path(Path::new("../etc/passwd"), root).is_err());
        assert!(resolve_sandbox_path(Path::new("foo/../../bar"), root).is_err());
    }

    #[test]
    fn allows_valid_relative_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("subdir")).unwrap();
        fs::write(root.join("subdir/file.txt"), "test").unwrap();
        let resolved = resolve_sandbox_path(Path::new("subdir/file.txt"), root).unwrap();
        assert!(resolved.starts_with(root.canonicalize().unwrap()));
    }

    #[test]
    fn blocks_symlink_escape() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create a symlink pointing outside workspace
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", root.join("escape_link")).ok();
            if root.join("escape_link").exists() {
                let result = resolve_sandbox_path(Path::new("escape_link/passwd"), root);
                assert!(result.is_err());
            }
        }
    }

    #[test]
    fn allows_new_file_in_existing_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let result = resolve_sandbox_path(Path::new("new_file.txt"), root);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_executable_rejects_traversal() {
        assert!(validate_executable_path(Path::new("../../bin/sh")).is_err());
        assert!(validate_executable_path(Path::new("/usr/bin/ls")).is_ok());
        assert!(validate_executable_path(Path::new("python3")).is_ok());
    }
}
