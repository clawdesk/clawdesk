//! File watcher for hot-reloading agent configs.
//!
//! Uses kqueue (macOS) / inotify (Linux) via the `notify` crate.
//! O(1) amortized notification delivery per file change event.

use crate::error::AgentConfigError;
use crate::loader::AgentLoader;
use crate::registry::AgentRegistry;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

/// Watches an agents directory for TOML file changes and hot-reloads configs.
pub struct AgentWatcher {
    _watcher: RecommendedWatcher,
    watch_dir: PathBuf,
}

impl AgentWatcher {
    /// Start watching a directory for agent config changes.
    ///
    /// - File created/modified → upsert into registry
    /// - File deleted → remove from registry
    pub fn start(
        dir: &Path,
        registry: Arc<AgentRegistry>,
    ) -> Result<Self, AgentConfigError> {
        let watch_dir = dir.to_path_buf();
        let dir_clone = dir.to_path_buf();

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => handle_event(&dir_clone, &registry, &event),
                Err(e) => warn!("File watcher error: {}", e),
            }
        })
        .map_err(|e| AgentConfigError::IoError(format!("Failed to create watcher: {}", e)))?;

        watcher
            .watch(dir, RecursiveMode::NonRecursive)
            .map_err(|e| AgentConfigError::IoError(format!("Failed to watch directory: {}", e)))?;

        info!(dir = %dir.display(), "Agent config watcher started");

        Ok(Self {
            _watcher: watcher,
            watch_dir,
        })
    }

    /// The directory being watched.
    pub fn watch_dir(&self) -> &Path {
        &self.watch_dir
    }
}

fn handle_event(dir: &Path, registry: &AgentRegistry, event: &Event) {
    let toml_paths: Vec<&Path> = event
        .paths
        .iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .map(|p| p.as_path())
        .collect();

    if toml_paths.is_empty() {
        return;
    }

    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) => {
            for path in toml_paths {
                match AgentLoader::load_file(path) {
                    Ok(config) => {
                        let name = config.agent.name.clone();
                        registry.upsert(config);
                        info!(agent = %name, "Hot-reloaded agent config");
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "Failed to reload agent config");
                    }
                }
            }
        }
        EventKind::Remove(_) => {
            for path in toml_paths {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if registry.remove(stem).is_some() {
                        info!(agent = %stem, "Removed agent config (file deleted)");
                    }
                }
            }
        }
        _ => {}
    }
}
