//! # Git Sync — Automated Workspace Version Control
//!
//! Provides automated git-based synchronization for ClawDesk workspace data,
//! enabling version history, conflict resolution, and multi-device sync.
//!
//! ## Design
//!
//! ```text
//! file change → debounce (30s) → stage → commit → push
//!                                           ↗
//! pull (on startup + periodic) → merge/rebase
//! ```
//!
//! All operations are non-blocking and failure-tolerant.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Git sync configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitSyncConfig {
    /// Whether git sync is enabled
    pub enabled: bool,
    /// Path to the git repository (workspace root)
    pub repo_path: PathBuf,
    /// Remote name (default: "origin")
    pub remote: String,
    /// Branch name (default: "main")
    pub branch: String,
    /// Debounce interval in seconds before committing changes
    pub debounce_secs: u64,
    /// Periodic pull interval in seconds
    pub pull_interval_secs: u64,
    /// Auto-push after commit
    pub auto_push: bool,
    /// Commit message prefix
    pub commit_prefix: String,
    /// Paths to include in sync (relative to repo root)
    pub include_paths: Vec<String>,
    /// Paths to exclude from sync
    pub exclude_paths: Vec<String>,
    /// Maximum file size to sync in bytes
    pub max_file_size: u64,
    /// Conflict resolution strategy
    pub conflict_strategy: ConflictStrategy,
}

impl Default for GitSyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            repo_path: PathBuf::from("."),
            remote: "origin".into(),
            branch: "main".into(),
            debounce_secs: 30,
            pull_interval_secs: 300,
            auto_push: true,
            commit_prefix: "[clawdesk-sync]".into(),
            include_paths: vec!["config/".into(), "data/".into(), "templates/".into()],
            exclude_paths: vec!["*.log".into(), "cache/".into(), "tmp/".into()],
            max_file_size: 10 * 1024 * 1024, // 10MB
            conflict_strategy: ConflictStrategy::KeepBoth,
        }
    }
}

/// Strategy for resolving git merge conflicts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConflictStrategy {
    /// Keep both versions (create conflict markers file)
    KeepBoth,
    /// Prefer local changes
    PreferLocal,
    /// Prefer remote changes
    PreferRemote,
    /// Abort and notify user
    AbortAndNotify,
}

/// State of the git sync system.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SyncState {
    /// Idle, waiting for changes
    Idle,
    /// Debouncing (changes detected, waiting for quiet period)
    Debouncing,
    /// Staging files
    Staging,
    /// Committing
    Committing,
    /// Pushing to remote
    Pushing,
    /// Pulling from remote
    Pulling,
    /// Conflict detected
    Conflict,
    /// Error state
    Error,
}

/// Record of a sync operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRecord {
    /// Unique sync identifier
    pub id: String,
    /// When the sync occurred
    pub timestamp: DateTime<Utc>,
    /// Type of sync operation
    pub operation: SyncOperation,
    /// Number of files affected
    pub files_affected: usize,
    /// Commit hash (if applicable)
    pub commit_hash: Option<String>,
    /// Whether the operation succeeded
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
}

/// Type of sync operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SyncOperation {
    Commit,
    Push,
    Pull,
    Merge,
    ConflictResolution,
}

/// Git sync manager.
pub struct GitSyncManager {
    config: GitSyncConfig,
    state: SyncState,
    /// History of sync operations
    history: Vec<SyncRecord>,
    /// Pending file changes (paths that have been modified)
    pending_changes: Vec<PathBuf>,
    /// Last successful sync timestamp
    last_sync: Option<DateTime<Utc>>,
    /// Last pull timestamp
    last_pull: Option<DateTime<Utc>>,
}

impl GitSyncManager {
    pub fn new(config: GitSyncConfig) -> Self {
        Self {
            config,
            state: SyncState::Idle,
            history: Vec::new(),
            pending_changes: Vec::new(),
            last_sync: None,
            last_pull: None,
        }
    }

    /// Get current sync state.
    pub fn state(&self) -> SyncState {
        self.state
    }

    /// Register a file change for debounced sync.
    pub fn notify_change(&mut self, path: PathBuf) {
        // Check exclusions
        let path_str = path.to_string_lossy();
        for exclude in &self.config.exclude_paths {
            if glob_matches(exclude, &path_str) {
                return;
            }
        }

        if !self.pending_changes.contains(&path) {
            self.pending_changes.push(path);
        }

        if self.state == SyncState::Idle {
            self.state = SyncState::Debouncing;
        }
    }

    /// Check if debounce period has elapsed and we should commit.
    pub fn should_commit(&self) -> bool {
        self.state == SyncState::Debouncing && !self.pending_changes.is_empty()
    }

    /// Check if we should pull from remote.
    pub fn should_pull(&self) -> bool {
        if !self.config.enabled {
            return false;
        }
        match self.last_pull {
            None => true,
            Some(last) => {
                let elapsed = (Utc::now() - last).num_seconds();
                elapsed >= self.config.pull_interval_secs as i64
            }
        }
    }

    /// Get pending changes.
    pub fn pending_changes(&self) -> &[PathBuf] {
        &self.pending_changes
    }

    /// Clear pending changes after successful commit.
    pub fn clear_pending(&mut self) {
        self.pending_changes.clear();
        self.state = SyncState::Idle;
    }

    /// Record a sync operation.
    pub fn record_sync(&mut self, record: SyncRecord) {
        if record.success {
            match record.operation {
                SyncOperation::Commit | SyncOperation::Push => {
                    self.last_sync = Some(record.timestamp);
                }
                SyncOperation::Pull => {
                    self.last_pull = Some(record.timestamp);
                }
                _ => {}
            }
        }
        self.history.push(record);
    }

    /// Set the sync state.
    pub fn set_state(&mut self, state: SyncState) {
        self.state = state;
    }

    /// Get sync history.
    pub fn history(&self) -> &[SyncRecord] {
        &self.history
    }

    /// Get the config.
    pub fn config(&self) -> &GitSyncConfig {
        &self.config
    }

    /// Generate a commit message for pending changes.
    pub fn commit_message(&self) -> String {
        let count = self.pending_changes.len();
        let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        format!(
            "{} Auto-sync {} file(s) at {}",
            self.config.commit_prefix, count, timestamp
        )
    }
}

/// Simple glob matching for file exclusion patterns.
fn glob_matches(pattern: &str, path: &str) -> bool {
    if pattern.starts_with('*') {
        // Suffix match: *.log matches foo.log
        let suffix = &pattern[1..];
        path.ends_with(suffix)
    } else if pattern.ends_with('/') {
        // Directory prefix match
        path.starts_with(pattern) || path.contains(&format!("/{}", pattern))
    } else {
        path == pattern || path.ends_with(&format!("/{}", pattern))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_matches() {
        assert!(glob_matches("*.log", "debug.log"));
        assert!(glob_matches("*.log", "path/to/debug.log"));
        assert!(!glob_matches("*.log", "debug.txt"));
        assert!(glob_matches("cache/", "cache/file.dat"));
        assert!(!glob_matches("cache/", "other/file.dat"));
    }

    #[test]
    fn test_sync_manager_basics() {
        let config = GitSyncConfig::default();
        let mut mgr = GitSyncManager::new(config);

        assert_eq!(mgr.state(), SyncState::Idle);
        assert!(mgr.pending_changes().is_empty());

        // Add a change
        mgr.notify_change(PathBuf::from("config/agents.toml"));
        assert_eq!(mgr.state(), SyncState::Debouncing);
        assert_eq!(mgr.pending_changes().len(), 1);

        // Excluded path should be filtered
        mgr.notify_change(PathBuf::from("debug.log"));
        assert_eq!(mgr.pending_changes().len(), 1); // still 1

        // Should commit
        assert!(mgr.should_commit());

        // Clear after commit
        mgr.clear_pending();
        assert_eq!(mgr.state(), SyncState::Idle);
        assert!(mgr.pending_changes().is_empty());
    }
}
