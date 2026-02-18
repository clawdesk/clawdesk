//! Daemon module — background service management for headless/server deployments.
//!
//! Manages ClawDesk as a long-running daemon process with:
//! - PID file management
//! - Signal handling (SIGTERM, SIGHUP for reload)  
//! - System service integration (systemd, launchd)
//! - Health check endpoint
//! - Graceful shutdown coordination

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{watch, RwLock};
use tracing::{info, warn};
use chrono::{DateTime, Utc};

/// Daemon configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Path to PID file.
    pub pid_file: PathBuf,
    /// Working directory.
    pub working_dir: PathBuf,
    /// Log file path.
    pub log_file: Option<PathBuf>,
    /// User to run as (Unix only).
    pub run_as_user: Option<String>,
    /// Group to run as (Unix only).
    pub run_as_group: Option<String>,
    /// Health check bind address.
    pub health_bind: Option<String>,
    /// Graceful shutdown timeout (seconds).
    pub shutdown_timeout_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            pid_file: PathBuf::from("/var/run/clawdesk.pid"),
            working_dir: PathBuf::from("/var/lib/clawdesk"),
            log_file: Some(PathBuf::from("/var/log/clawdesk/clawdesk.log")),
            run_as_user: None,
            run_as_group: None,
            health_bind: Some("127.0.0.1:9090".to_string()),
            shutdown_timeout_secs: 30,
        }
    }
}

/// Daemon lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    Starting,
    Running,
    Reloading,
    ShuttingDown,
    Stopped,
}

/// Health status report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub state: DaemonState,
    pub uptime_seconds: u64,
    pub pid: u32,
    pub version: String,
    pub started_at: DateTime<Utc>,
    pub last_health_check: DateTime<Utc>,
    pub active_connections: u32,
    pub active_agents: u32,
    pub memory_bytes: Option<u64>,
}

/// Signal received by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonSignal {
    /// Graceful shutdown (SIGTERM).
    Shutdown,
    /// Reload configuration (SIGHUP).
    Reload,
    /// User-defined signal (SIGUSR1).
    User1,
    /// User-defined signal (SIGUSR2).
    User2,
}

/// PID file manager.
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Write current PID to file.
    pub fn write(&self) -> Result<(), DaemonError> {
        let pid = std::process::id();
        std::fs::write(&self.path, pid.to_string()).map_err(|e| {
            DaemonError::PidFileError(format!(
                "failed to write PID file {}: {}",
                self.path.display(),
                e
            ))
        })?;
        info!(pid, path = %self.path.display(), "PID file written");
        Ok(())
    }

    /// Read PID from file.
    pub fn read(&self) -> Result<Option<u32>, DaemonError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&self.path).map_err(|e| {
            DaemonError::PidFileError(format!("failed to read PID file: {}", e))
        })?;
        let pid = content.trim().parse::<u32>().map_err(|e| {
            DaemonError::PidFileError(format!("invalid PID in file: {}", e))
        })?;
        Ok(Some(pid))
    }

    /// Remove PID file.
    pub fn remove(&self) -> Result<(), DaemonError> {
        if self.path.exists() {
            std::fs::remove_file(&self.path).map_err(|e| {
                DaemonError::PidFileError(format!("failed to remove PID file: {}", e))
            })?;
            info!(path = %self.path.display(), "PID file removed");
        }
        Ok(())
    }

    /// Check if another instance is running.
    pub fn is_running(&self) -> bool {
        match self.read() {
            Ok(Some(_pid)) => {
                // On Unix, this would check via kill(pid, 0). For now, we
                // consider the process running if the PID file exists and
                // contains a valid PID.
                true
            }
            _ => false,
        }
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

/// Daemon manager — coordinates startup, shutdown, and health.
pub struct DaemonManager {
    config: DaemonConfig,
    state: Arc<RwLock<DaemonState>>,
    started_at: DateTime<Utc>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl DaemonManager {
    pub fn new(config: DaemonConfig) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            config,
            state: Arc::new(RwLock::new(DaemonState::Starting)),
            started_at: Utc::now(),
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Get current daemon state.
    pub async fn state(&self) -> DaemonState {
        *self.state.read().await
    }

    /// Set daemon state.
    pub async fn set_state(&self, state: DaemonState) {
        let mut s = self.state.write().await;
        *s = state;
        info!(state = ?state, "daemon state changed");
    }

    /// Get a shutdown receiver (clone for multiple consumers).
    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    /// Initiate graceful shutdown.
    pub async fn initiate_shutdown(&self) {
        info!("initiating daemon shutdown");
        self.set_state(DaemonState::ShuttingDown).await;
        let _ = self.shutdown_tx.send(true);
    }

    /// Wait for shutdown signal.
    pub async fn wait_for_shutdown(&self) {
        let mut rx = self.shutdown_rx.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }

    /// Handle a daemon signal.
    pub async fn handle_signal(&self, signal: DaemonSignal) {
        match signal {
            DaemonSignal::Shutdown => {
                self.initiate_shutdown().await;
            }
            DaemonSignal::Reload => {
                info!("reload signal received");
                self.set_state(DaemonState::Reloading).await;
                // Configuration reload would happen here
                self.set_state(DaemonState::Running).await;
            }
            DaemonSignal::User1 => {
                info!("USR1 signal received — dumping status");
            }
            DaemonSignal::User2 => {
                info!("USR2 signal received");
            }
        }
    }

    /// Generate health status.
    pub async fn health_status(&self) -> HealthStatus {
        let now = Utc::now();
        let uptime = (now - self.started_at).num_seconds().max(0) as u64;

        HealthStatus {
            state: self.state().await,
            uptime_seconds: uptime,
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: self.started_at,
            last_health_check: now,
            active_connections: 0,
            active_agents: 0,
            memory_bytes: get_process_memory(),
        }
    }

    /// Get the daemon configuration.
    pub fn config(&self) -> &DaemonConfig {
        &self.config
    }

    /// Uptime in seconds.
    pub fn uptime_seconds(&self) -> u64 {
        let now = Utc::now();
        (now - self.started_at).num_seconds().max(0) as u64
    }
}

/// Get current process memory usage (platform-specific).
fn get_process_memory() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| {
                let fields: Vec<&str> = s.split_whitespace().collect();
                fields.first()?.parse::<u64>().ok().map(|pages| pages * 4096)
            })
    }

    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Generate a systemd unit file.
pub fn generate_systemd_unit(config: &DaemonConfig) -> String {
    format!(
        r#"[Unit]
Description=ClawDesk AI Assistant Daemon
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/clawdesk daemon
WorkingDirectory={working_dir}
PIDFile={pid_file}
Restart=on-failure
RestartSec=5
{user}{group}
StandardOutput=journal
StandardError=journal
SyslogIdentifier=clawdesk

[Install]
WantedBy=multi-user.target
"#,
        working_dir = config.working_dir.display(),
        pid_file = config.pid_file.display(),
        user = config
            .run_as_user
            .as_ref()
            .map(|u| format!("User={}\n", u))
            .unwrap_or_default(),
        group = config
            .run_as_group
            .as_ref()
            .map(|g| format!("Group={}\n", g))
            .unwrap_or_default(),
    )
}

/// Generate a macOS launchd plist.
pub fn generate_launchd_plist(config: &DaemonConfig) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.clawdesk.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/clawdesk</string>
        <string>daemon</string>
    </array>
    <key>WorkingDirectory</key>
    <string>{working_dir}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_path}</string>
    <key>StandardErrorPath</key>
    <string>{log_path}</string>
</dict>
</plist>
"#,
        working_dir = config.working_dir.display(),
        log_path = config
            .log_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "/var/log/clawdesk.log".to_string()),
    )
}

/// Daemon error.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("PID file error: {0}")]
    PidFileError(String),
    #[error("already running (PID {0})")]
    AlreadyRunning(u32),
    #[error("not running")]
    NotRunning,
    #[error("shutdown timeout")]
    ShutdownTimeout,
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pid_file_write_read() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("clawdesk_test_{}.pid", std::process::id()));
        let pf = PidFile::new(&path);

        pf.write().unwrap();
        let pid = pf.read().unwrap().unwrap();
        assert_eq!(pid, std::process::id());

        pf.remove().unwrap();
        assert!(pf.read().unwrap().is_none());
    }

    #[test]
    fn test_pid_file_missing() {
        let pf = PidFile::new("/nonexistent/test.pid");
        assert!(pf.read().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_daemon_manager_states() {
        let config = DaemonConfig::default();
        let mgr = DaemonManager::new(config);

        assert_eq!(mgr.state().await, DaemonState::Starting);

        mgr.set_state(DaemonState::Running).await;
        assert_eq!(mgr.state().await, DaemonState::Running);
    }

    #[tokio::test]
    async fn test_shutdown() {
        let config = DaemonConfig::default();
        let mgr = DaemonManager::new(config);

        mgr.set_state(DaemonState::Running).await;
        mgr.initiate_shutdown().await;

        assert_eq!(mgr.state().await, DaemonState::ShuttingDown);
    }

    #[tokio::test]
    async fn test_health_status() {
        let config = DaemonConfig::default();
        let mgr = DaemonManager::new(config);
        mgr.set_state(DaemonState::Running).await;

        let health = mgr.health_status().await;
        assert_eq!(health.state, DaemonState::Running);
        assert_eq!(health.pid, std::process::id());
        assert!(health.uptime_seconds < 5);
    }

    #[test]
    fn test_generate_systemd_unit() {
        let config = DaemonConfig {
            run_as_user: Some("clawdesk".to_string()),
            ..Default::default()
        };
        let unit = generate_systemd_unit(&config);
        assert!(unit.contains("ExecStart=/usr/local/bin/clawdesk daemon"));
        assert!(unit.contains("User=clawdesk"));
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn test_generate_launchd_plist() {
        let config = DaemonConfig::default();
        let plist = generate_launchd_plist(&config);
        assert!(plist.contains("com.clawdesk.daemon"));
        assert!(plist.contains("<true/>"));
    }

    #[tokio::test]
    async fn test_reload_signal() {
        let config = DaemonConfig::default();
        let mgr = DaemonManager::new(config);
        mgr.set_state(DaemonState::Running).await;

        mgr.handle_signal(DaemonSignal::Reload).await;
        assert_eq!(mgr.state().await, DaemonState::Running); // Returns to running after reload
    }

    #[test]
    fn test_daemon_config_default() {
        let config = DaemonConfig::default();
        assert_eq!(config.shutdown_timeout_secs, 30);
        assert_eq!(config.pid_file, PathBuf::from("/var/run/clawdesk.pid"));
    }

    #[test]
    fn test_uptime() {
        let config = DaemonConfig::default();
        let mgr = DaemonManager::new(config);
        let uptime = mgr.uptime_seconds();
        assert!(uptime < 5);
    }
}
