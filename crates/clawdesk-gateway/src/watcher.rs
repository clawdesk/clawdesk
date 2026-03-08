//! Filesystem watcher — debounced auto-reload for skills and config.
//!
//! ## Design rationale
//!
//! The legacy system watches `~/.legacy/openclaw.json` and applies changes
//! automatically, but users report inconsistencies where changes don't
//! take effect until manual restart. Skills don't hot-reload at all.
//!
//! ClawDesk's `ArcSwap` pattern makes atomic hot-reload safe and wait-free.
//! This module adds a filesystem watcher that debounces changes and
//! triggers the existing `GatewayState::reload_skills()` method.
//!
//! ## Architecture
//!
//! ```text
//! notify::Watcher ──→ mpsc channel ──→ debounce timer ──→ reload
//!                                      (500ms window)
//! ```
//!
//! - Uses `notify` crate for cross-platform filesystem events.
//! - Debounces with a 500ms window to coalesce rapid saves.
//! - Runs as a background task via `tokio::spawn`.
//! - Logs reload results but never panics — the watcher is best-effort.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Watcher event types
// ---------------------------------------------------------------------------

/// Type of filesystem change detected.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// A skill file was modified/created/deleted.
    SkillChange { path: PathBuf },
    /// The config file was modified.
    ConfigChange { path: PathBuf },
    /// An agent definition (agent.toml) was modified/created/deleted.
    AgentChange { path: PathBuf },
}

/// Configuration for the filesystem watcher.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Directory to watch for skill changes (recursive).
    pub skills_dir: PathBuf,
    /// Config file to watch for changes (non-recursive).
    pub config_path: PathBuf,
    /// Directory to watch for agent definition changes (recursive).
    pub agents_dir: PathBuf,
    /// Debounce window — minimum time between reloads.
    pub debounce: Duration,
    /// Whether to watch skills directory.
    pub watch_skills: bool,
    /// Whether to watch config file.
    pub watch_config: bool,
    /// Whether to watch agents directory.
    pub watch_agents: bool,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        let home = dirs_path();
        Self {
            skills_dir: home.join("skills"),
            config_path: home.join("config.toml"),
            agents_dir: home.join("agents"),
            debounce: Duration::from_millis(500),
            watch_skills: true,
            watch_config: true,
            watch_agents: true,
        }
    }
}

/// Returns `~/.clawdesk/` path.
fn dirs_path() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".clawdesk")
}

// ---------------------------------------------------------------------------
// Reload handler trait
// ---------------------------------------------------------------------------

/// Trait for handling reload events — abstracts over `GatewayState`.
///
/// Allows testing without the full gateway dependency.
#[async_trait::async_trait]
pub trait ReloadHandler: Send + Sync + 'static {
    /// Reload skills from the filesystem.
    async fn reload_skills(&self) -> (usize, Vec<String>);
    /// Reload configuration (if supported).
    async fn reload_config(&self) -> Result<(), String>;
    /// Reload agent definitions from the filesystem.
    /// Returns `(loaded, changed, errors)`.
    async fn reload_agents(&self) -> (usize, usize, Vec<String>);
}

// ---------------------------------------------------------------------------
// Config watcher
// ---------------------------------------------------------------------------

/// Filesystem watcher with debounced auto-reload.
///
/// Watches `~/.clawdesk/skills/` and `~/.clawdesk/config.toml` for changes,
/// debounces rapid modifications, and triggers atomic reload via `ArcSwap`.
pub struct ConfigWatcher {
    config: WatcherConfig,
}

impl ConfigWatcher {
    pub fn new(config: WatcherConfig) -> Self {
        Self { config }
    }

    /// Start watching in the background. Returns a handle to stop the watcher.
    ///
    /// This spawns a background task that:
    /// 1. Sets up a `notify` filesystem watcher
    /// 2. Receives events via an mpsc channel
    /// 3. Debounces with a configurable window
    /// 4. Calls the reload handler for skill/config changes
    ///
    /// The watcher stops when the returned `WatcherHandle` is dropped
    /// or when the cancellation token fires.
    pub fn start<H: ReloadHandler>(
        self,
        handler: Arc<H>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> WatcherHandle {
        let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
        let config = self.config;

        tokio::spawn(async move {
            if let Err(e) = Self::watch_loop(config, handler, cancel, stop_rx).await {
                error!(%e, "filesystem watcher failed");
            }
        });

        WatcherHandle { _stop: stop_tx }
    }

    /// Internal watch loop with debouncing.
    async fn watch_loop<H: ReloadHandler>(
        config: WatcherConfig,
        handler: Arc<H>,
        cancel: tokio_util::sync::CancellationToken,
        mut stop_rx: mpsc::Receiver<()>,
    ) -> Result<(), String> {
        let (tx, mut rx) = mpsc::channel::<WatchEvent>(64);

        // Set up the native filesystem watcher.
        // Note: `notify` is optional — if not available, we fall back to
        // periodic polling. This implementation uses a polling approach
        // that works on all platforms without the `notify` crate dependency.
        let skills_dir = config.skills_dir.clone();
        let config_path = config.config_path.clone();
        let agents_dir = config.agents_dir.clone();
        let poll_interval = config.debounce;

        // Polling-based watcher (cross-platform, no native dep).
        // Tracks file modification times and detects changes.
        let tx_clone = tx.clone();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            let mut last_skills_mtime = get_dir_mtime(&skills_dir);
            let mut last_config_mtime = get_file_mtime(&config_path);
            let mut last_agents_mtime = get_dir_mtime(&agents_dir);

            loop {
                tokio::select! {
                    _ = cancel_clone.cancelled() => break,
                    _ = tokio::time::sleep(poll_interval) => {}
                }

                // Check skills directory.
                if config.watch_skills {
                    let current = get_dir_mtime(&skills_dir);
                    if current != last_skills_mtime {
                        last_skills_mtime = current;
                        let _ = tx_clone
                            .send(WatchEvent::SkillChange {
                                path: skills_dir.clone(),
                            })
                            .await;
                    }
                }

                // Check config file.
                if config.watch_config {
                    let current = get_file_mtime(&config_path);
                    if current != last_config_mtime {
                        last_config_mtime = current;
                        let _ = tx_clone
                            .send(WatchEvent::ConfigChange {
                                path: config_path.clone(),
                            })
                            .await;
                    }
                }

                // Check agents directory.
                if config.watch_agents {
                    let current = get_dir_mtime(&agents_dir);
                    if current != last_agents_mtime {
                        last_agents_mtime = current;
                        let _ = tx_clone
                            .send(WatchEvent::AgentChange {
                                path: agents_dir.clone(),
                            })
                            .await;
                    }
                }
            }
        });

        // Debounce + dispatch loop.
        let mut debounce_timer = tokio::time::interval(config.debounce);
        let mut pending_skills = false;
        let mut pending_config = false;
        let mut pending_agents = false;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("filesystem watcher shutting down");
                    break;
                }
                _ = stop_rx.recv() => {
                    info!("filesystem watcher stopped");
                    break;
                }
                Some(event) = rx.recv() => {
                    match event {
                        WatchEvent::SkillChange { path } => {
                            debug!(?path, "skill filesystem change detected");
                            pending_skills = true;
                        }
                        WatchEvent::ConfigChange { path } => {
                            debug!(?path, "config filesystem change detected");
                            pending_config = true;
                        }
                        WatchEvent::AgentChange { path } => {
                            debug!(?path, "agent filesystem change detected");
                            pending_agents = true;
                        }
                    }
                }
                _ = debounce_timer.tick() => {
                    if pending_skills {
                        pending_skills = false;
                        let (loaded, errors) = handler.reload_skills().await;
                        if errors.is_empty() {
                            info!(loaded, "auto-reloaded skills after filesystem change");
                        } else {
                            warn!(loaded, errors = ?errors, "auto-reloaded skills with errors");
                        }
                    }
                    if pending_config {
                        pending_config = false;
                        match handler.reload_config().await {
                            Ok(()) => info!("auto-reloaded config after filesystem change"),
                            Err(e) => warn!(%e, "config reload failed"),
                        }
                    }
                    if pending_agents {
                        pending_agents = false;
                        let (loaded, changed, errors) = handler.reload_agents().await;
                        if errors.is_empty() {
                            if changed > 0 {
                                info!(loaded, changed, "auto-reloaded agents after filesystem change");
                            }
                        } else {
                            warn!(loaded, changed, errors = ?errors, "auto-reloaded agents with errors");
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Handle for the filesystem watcher. Dropping this stops the watcher.
pub struct WatcherHandle {
    _stop: mpsc::Sender<()>,
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

/// Get the most recent modification time for any file in a directory (recursive).
fn get_dir_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    if !dir.exists() {
        return None;
    }

    let mut latest: Option<std::time::SystemTime> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(sub_mtime) = get_dir_mtime(&path) {
                    latest = Some(match latest {
                        Some(current) => current.max(sub_mtime),
                        None => sub_mtime,
                    });
                }
            } else if let Ok(meta) = path.metadata() {
                if let Ok(mtime) = meta.modified() {
                    latest = Some(match latest {
                        Some(current) => current.max(mtime),
                        None => mtime,
                    });
                }
            }
        }
    }
    latest
}

/// Get the modification time of a single file.
fn get_file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock reload handler for testing.
    struct MockHandler {
        skills_reload_count: AtomicUsize,
        config_reload_count: AtomicUsize,
        agents_reload_count: AtomicUsize,
    }

    impl MockHandler {
        fn new() -> Self {
            Self {
                skills_reload_count: AtomicUsize::new(0),
                config_reload_count: AtomicUsize::new(0),
                agents_reload_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl ReloadHandler for MockHandler {
        async fn reload_skills(&self) -> (usize, Vec<String>) {
            self.skills_reload_count.fetch_add(1, Ordering::SeqCst);
            (3, vec![])
        }

        async fn reload_config(&self) -> Result<(), String> {
            self.config_reload_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn reload_agents(&self) -> (usize, usize, Vec<String>) {
            self.agents_reload_count.fetch_add(1, Ordering::SeqCst);
            (2, 1, vec![])
        }
    }

    #[test]
    fn default_config_paths() {
        let config = WatcherConfig::default();
        assert!(config.skills_dir.to_string_lossy().contains(".clawdesk"));
        assert!(config.config_path.to_string_lossy().contains("config.toml"));
        assert_eq!(config.debounce, Duration::from_millis(500));
    }

    #[test]
    fn file_mtime_returns_none_for_missing() {
        let result = get_file_mtime(Path::new("/nonexistent/file.toml"));
        assert!(result.is_none());
    }

    #[test]
    fn dir_mtime_returns_none_for_missing() {
        let result = get_dir_mtime(Path::new("/nonexistent/dir"));
        assert!(result.is_none());
    }
}
