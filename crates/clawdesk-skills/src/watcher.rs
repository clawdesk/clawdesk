//! Filesystem watcher for hot-reloading skill files.
//!
//! ## File Watcher (P2)
//!
//! Watches `*/SKILL.md` in configured skill directories and triggers reload
//! when files change. Replicates OpenClaw's 250ms debounce behavior and
//! `DEFAULT_SKILLS_WATCH_IGNORED` patterns.
//!
//! ## Architecture
//!
//! Since `notify` is not in our dependency tree, we use a polling strategy:
//! a tokio task periodically stats skill directories and compares mtimes.
//! This is efficient because:
//! - Skill files are small (max 256KB each)
//! - Skill dirs contain < 300 entries
//! - Poll interval is configurable (default 2s)
//!
//! ## Integration
//!
//! ```text
//! SkillWatcher (poll loop)
//!   → detects mtime change
//!   → debounce (250ms)
//!   → emits SkillChangeEvent via channel
//!   → consumer calls load_fresh() + bump_version()
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Default poll interval.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Default debounce window (matches OpenClaw's 250ms).
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(250);

/// Patterns to ignore when watching (replicates OpenClaw's DEFAULT_SKILLS_WATCH_IGNORED).
pub const IGNORED_PATTERNS: &[&str] = &[
    ".git",
    "node_modules",
    ".DS_Store",
    "thumbs.db",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    ".venv",
    "target",
];

/// A change detected in the skills filesystem.
#[derive(Debug, Clone)]
pub struct SkillChangeEvent {
    /// Path that changed.
    pub path: PathBuf,
    /// Kind of change.
    pub kind: ChangeKind,
    /// Timestamp of detection.
    pub detected_at: SystemTime,
}

/// Kind of filesystem change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    /// File was created or appeared.
    Created,
    /// File content was modified (mtime changed).
    Modified,
    /// File was deleted or disappeared.
    Deleted,
}

impl std::fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Modified => write!(f, "modified"),
            Self::Deleted => write!(f, "deleted"),
        }
    }
}

/// Configuration for the skill watcher.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Directories to watch for skill files.
    pub watch_dirs: Vec<PathBuf>,
    /// Poll interval between filesystem scans.
    pub poll_interval: Duration,
    /// Debounce window — changes within this window are coalesced.
    pub debounce: Duration,
    /// Filename pattern to watch (e.g., "SKILL.md").
    pub skill_filename: String,
    /// Maximum depth to scan (0 = watch_dir only, 1 = one level of subdirs).
    pub max_depth: usize,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            watch_dirs: vec![],
            poll_interval: DEFAULT_POLL_INTERVAL,
            debounce: DEFAULT_DEBOUNCE,
            skill_filename: "SKILL.md".to_string(),
            max_depth: 2,
        }
    }
}

/// State of a single watched file.
#[derive(Debug, Clone)]
struct WatchedFile {
    mtime: SystemTime,
    size: u64,
}

/// Polling-based skill file watcher.
///
/// Scans configured directories and tracks mtime+size for each SKILL.md.
/// Emits `SkillChangeEvent`s via a channel when changes are detected.
pub struct SkillWatcher {
    config: WatcherConfig,
    /// Known file states from last scan.
    known_files: HashMap<PathBuf, WatchedFile>,
}

impl SkillWatcher {
    /// Create a new watcher with the given config.
    pub fn new(config: WatcherConfig) -> Self {
        Self {
            config,
            known_files: HashMap::new(),
        }
    }

    /// Check if a path component should be ignored.
    fn is_ignored(component: &str) -> bool {
        IGNORED_PATTERNS
            .iter()
            .any(|pat| component.eq_ignore_ascii_case(pat))
    }

    /// Scan all watch directories and collect SKILL.md files.
    fn scan_directories(&self) -> HashMap<PathBuf, WatchedFile> {
        let mut found = HashMap::new();

        for dir in &self.config.watch_dirs {
            if !dir.exists() {
                continue;
            }
            self.scan_dir(dir, 0, &mut found);
        }

        found
    }

    /// Recursively scan a directory up to max_depth.
    fn scan_dir(&self, dir: &Path, depth: usize, found: &mut HashMap<PathBuf, WatchedFile>) {
        if depth > self.config.max_depth {
            return;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Skip ignored patterns
            if Self::is_ignored(&name_str) {
                continue;
            }

            if path.is_dir() {
                self.scan_dir(&path, depth + 1, found);
            } else if name_str == self.config.skill_filename {
                if let Ok(meta) = std::fs::metadata(&path) {
                    if let Ok(mtime) = meta.modified() {
                        found.insert(
                            path,
                            WatchedFile {
                                mtime,
                                size: meta.len(),
                            },
                        );
                    }
                }
            }
        }
    }

    /// Perform one scan cycle and return detected changes.
    pub fn poll(&mut self) -> Vec<SkillChangeEvent> {
        let current = self.scan_directories();
        let mut events = Vec::new();
        let now = SystemTime::now();

        // Check for new or modified files
        for (path, state) in &current {
            match self.known_files.get(path) {
                None => {
                    // New file
                    events.push(SkillChangeEvent {
                        path: path.clone(),
                        kind: ChangeKind::Created,
                        detected_at: now,
                    });
                }
                Some(prev) => {
                    // Check if modified (mtime or size changed)
                    if prev.mtime != state.mtime || prev.size != state.size {
                        events.push(SkillChangeEvent {
                            path: path.clone(),
                            kind: ChangeKind::Modified,
                            detected_at: now,
                        });
                    }
                }
            }
        }

        // Check for deleted files
        for path in self.known_files.keys() {
            if !current.contains_key(path) {
                events.push(SkillChangeEvent {
                    path: path.clone(),
                    kind: ChangeKind::Deleted,
                    detected_at: now,
                });
            }
        }

        // Update known state
        self.known_files = current;

        if !events.is_empty() {
            info!(
                count = events.len(),
                "skill filesystem changes detected"
            );
        }

        events
    }

    /// Start the watch loop, sending events to the provided channel.
    ///
    /// This runs until the sender is dropped or the task is cancelled.
    pub async fn watch_loop(mut self, tx: mpsc::Sender<Vec<SkillChangeEvent>>) {
        // Initial scan to populate known_files
        self.known_files = self.scan_directories();
        debug!(
            files = self.known_files.len(),
            dirs = self.config.watch_dirs.len(),
            "skill watcher initialized"
        );

        loop {
            tokio::time::sleep(self.config.poll_interval).await;

            let events = self.poll();
            if !events.is_empty() {
                // Debounce: wait a bit more to coalesce rapid changes
                tokio::time::sleep(self.config.debounce).await;

                // Re-poll to capture any writes that occurred during debounce
                let final_events = self.poll();
                let all_events = if final_events.is_empty() {
                    events
                } else {
                    // Merge: use the latest events
                    let mut merged = events;
                    merged.extend(final_events);
                    merged
                };

                if tx.send(all_events).await.is_err() {
                    debug!("skill watcher channel closed, stopping");
                    break;
                }
            }
        }
    }

    /// Get the number of currently tracked files.
    pub fn tracked_count(&self) -> usize {
        self.known_files.len()
    }

    /// Get the paths of all tracked files.
    pub fn tracked_paths(&self) -> Vec<&Path> {
        self.known_files.keys().map(|p| p.as_path()).collect()
    }
}

/// Convenience: create a watcher for standard ClawDesk skill dirs.
///
/// Watches:
/// - `~/.clawdesk/skills/`  (managed skills)
/// - `~/.agents/skills/`    (personal agents)
/// - Workspace skills dir if provided
pub fn default_watch_dirs(workspace_root: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let managed = home.join(".clawdesk").join("skills");
        if managed.exists() {
            dirs.push(managed);
        }

        let personal = home.join(".agents").join("skills");
        if personal.exists() {
            dirs.push(personal);
        }
    }

    if let Some(ws) = workspace_root {
        let ws_skills = ws.join("skills");
        if ws_skills.exists() {
            dirs.push(ws_skills);
        }

        let ws_agents = ws.join(".agents").join("skills");
        if ws_agents.exists() {
            dirs.push(ws_agents);
        }
    }

    dirs
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a temp dir that cleans up on drop.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("clawdesk_watcher_test_{}_{}", name, std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn create_skill_file(dir: &Path, skill_name: &str, content: &str) -> PathBuf {
        let skill_dir = dir.join(skill_name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let file_path = skill_dir.join("SKILL.md");
        std::fs::write(&file_path, content).unwrap();
        file_path
    }

    #[test]
    fn detects_new_skill_files() {
        let tmp = TestDir::new("new");
        create_skill_file(tmp.path(), "skill-a", "# Skill A\nTest content");

        let config = WatcherConfig {
            watch_dirs: vec![tmp.path().to_path_buf()],
            ..Default::default()
        };
        let mut watcher = SkillWatcher::new(config);

        let events = watcher.poll();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, ChangeKind::Created);
        assert!(events[0].path.ends_with("skill-a/SKILL.md"));
    }

    #[test]
    fn detects_modified_files() {
        let tmp = TestDir::new("mod");
        let path = create_skill_file(tmp.path(), "skill-b", "# Skill B\nOriginal");

        let config = WatcherConfig {
            watch_dirs: vec![tmp.path().to_path_buf()],
            ..Default::default()
        };
        let mut watcher = SkillWatcher::new(config);

        watcher.poll();

        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(&path, "# Skill B\nModified content").unwrap();

        let events = watcher.poll();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, ChangeKind::Modified);
    }

    #[test]
    fn detects_deleted_files() {
        let tmp = TestDir::new("del");
        let path = create_skill_file(tmp.path(), "skill-c", "# Skill C");

        let config = WatcherConfig {
            watch_dirs: vec![tmp.path().to_path_buf()],
            ..Default::default()
        };
        let mut watcher = SkillWatcher::new(config);

        watcher.poll();

        std::fs::remove_file(&path).unwrap();

        let events = watcher.poll();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, ChangeKind::Deleted);
    }

    #[test]
    fn ignores_hidden_dirs() {
        let tmp = TestDir::new("ign");
        create_skill_file(&tmp.path().join(".git"), "ignored", "# Ignored");
        create_skill_file(tmp.path(), "normal", "# Normal");

        let config = WatcherConfig {
            watch_dirs: vec![tmp.path().to_path_buf()],
            ..Default::default()
        };
        let mut watcher = SkillWatcher::new(config);

        let events = watcher.poll();
        assert_eq!(events.len(), 1);
        assert!(events[0].path.to_string_lossy().contains("normal"));
    }

    #[test]
    fn no_events_when_unchanged() {
        let tmp = TestDir::new("nochg");
        create_skill_file(tmp.path(), "skill-d", "# Skill D");

        let config = WatcherConfig {
            watch_dirs: vec![tmp.path().to_path_buf()],
            ..Default::default()
        };
        let mut watcher = SkillWatcher::new(config);

        let events = watcher.poll();
        assert_eq!(events.len(), 1);

        let events = watcher.poll();
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn multiple_watch_dirs() {
        let tmp1 = TestDir::new("multi1");
        let tmp2 = TestDir::new("multi2");
        create_skill_file(tmp1.path(), "skill-1", "# One");
        create_skill_file(tmp2.path(), "skill-2", "# Two");

        let config = WatcherConfig {
            watch_dirs: vec![tmp1.path().to_path_buf(), tmp2.path().to_path_buf()],
            ..Default::default()
        };
        let mut watcher = SkillWatcher::new(config);

        let events = watcher.poll();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn tracked_count() {
        let tmp = TestDir::new("track");
        create_skill_file(tmp.path(), "skill-e", "# Skill E");
        create_skill_file(tmp.path(), "skill-f", "# Skill F");

        let config = WatcherConfig {
            watch_dirs: vec![tmp.path().to_path_buf()],
            ..Default::default()
        };
        let mut watcher = SkillWatcher::new(config);

        watcher.poll();
        assert_eq!(watcher.tracked_count(), 2);
    }

    #[test]
    fn change_kind_display() {
        assert_eq!(ChangeKind::Created.to_string(), "created");
        assert_eq!(ChangeKind::Modified.to_string(), "modified");
        assert_eq!(ChangeKind::Deleted.to_string(), "deleted");
    }

    #[test]
    fn nonexistent_watch_dir_is_ok() {
        let config = WatcherConfig {
            watch_dirs: vec![PathBuf::from("/nonexistent/path/abc123")],
            ..Default::default()
        };
        let mut watcher = SkillWatcher::new(config);

        let events = watcher.poll();
        assert!(events.is_empty());
        assert_eq!(watcher.tracked_count(), 0);
    }
}
