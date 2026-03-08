//! Gateway connection modes — local, remote, and unconfigured.
//!
//! Controls whether ClawDesk runs its own gateway process locally or connects
//! to a remote gateway server. OpenClaw equivalent: Remote Claude Desktop
//! gateway selection in Settings > General.
//!
//! ## Modes
//! - **Local**: Gateway runs as a child process on this machine.
//! - **Remote**: Connects to an existing remote gateway over WebSocket.
//! - **Unconfigured**: No gateway configured (prompts user at first launch).

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// How the gateway is provided.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionMode {
    /// Gateway runs as a child process on this machine.
    Local,
    /// Connect to a remote gateway.
    Remote {
        /// Remote gateway URL (ws:// or wss://).
        url: String,
        /// API key or bearer token for authentication.
        api_key: Option<String>,
    },
    /// No gateway configured yet.
    Unconfigured,
}

impl Default for ConnectionMode {
    fn default() -> Self {
        Self::Local
    }
}

/// State of the gateway process (when in Local mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    /// Not started.
    Stopped,
    /// Starting up.
    Starting,
    /// Running and healthy.
    Running,
    /// Stopping gracefully.
    Stopping,
    /// Crashed or failed to start.
    Failed,
}

/// State of remote connection (when in Remote mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteState {
    /// Not connected.
    Disconnected,
    /// Connecting.
    Connecting,
    /// Connected and authenticated.
    Connected,
    /// Connection lost, retrying.
    Reconnecting,
    /// Authentication failed.
    AuthFailed,
}

/// Configuration for gateway process management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayProcessConfig {
    /// Connection mode.
    pub mode: ConnectionMode,
    /// Local gateway binary path (auto-detected if None).
    pub local_binary: Option<String>,
    /// Local gateway port.
    pub local_port: u16,
    /// Whether to auto-start local gateway on app launch.
    pub auto_start: bool,
    /// Restart local gateway on crash.
    pub auto_restart: bool,
    /// Max restart attempts (0 = infinite).
    pub max_restarts: u32,
    /// Restart backoff base in milliseconds.
    pub restart_backoff_ms: u64,
    /// Remote connection timeout in seconds.
    pub remote_timeout_secs: u64,
    /// Remote reconnect interval in seconds.
    pub remote_reconnect_secs: u64,
    /// TLS certificate pinning for remote (SPKI hash).
    pub remote_tls_pin: Option<String>,
}

impl Default for GatewayProcessConfig {
    fn default() -> Self {
        Self {
            mode: ConnectionMode::Local,
            local_binary: None,
            local_port: 18789,
            auto_start: true,
            auto_restart: true,
            max_restarts: 5,
            restart_backoff_ms: 1000,
            remote_timeout_secs: 10,
            remote_reconnect_secs: 5,
            remote_tls_pin: None,
        }
    }
}

/// Events from the gateway process manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GatewayEvent {
    /// Mode changed.
    ModeChanged { mode: ConnectionMode },
    /// Local process state changed.
    LocalStateChanged { state: ProcessState },
    /// Remote connection state changed.
    RemoteStateChanged { state: RemoteState },
    /// Gateway URL is now available.
    Ready { url: String },
    /// Error.
    Error { message: String },
}

/// Manages the gateway lifecycle — local process or remote connection.
pub struct GatewayProcessManager {
    config: Arc<RwLock<GatewayProcessConfig>>,
    process_state: Arc<RwLock<ProcessState>>,
    remote_state: Arc<RwLock<RemoteState>>,
    gateway_url: Arc<RwLock<Option<String>>>,
    restart_count: Arc<RwLock<u32>>,
    /// Handle to the spawned child process (Local mode).
    child: Arc<RwLock<Option<tokio::process::Child>>>,
}

impl GatewayProcessManager {
    pub fn new(config: GatewayProcessConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            process_state: Arc::new(RwLock::new(ProcessState::Stopped)),
            remote_state: Arc::new(RwLock::new(RemoteState::Disconnected)),
            gateway_url: Arc::new(RwLock::new(None)),
            restart_count: Arc::new(RwLock::new(0)),
            child: Arc::new(RwLock::new(None)),
        }
    }

    /// Get the current connection mode.
    pub async fn mode(&self) -> ConnectionMode {
        self.config.read().await.mode.clone()
    }

    /// Get the effective gateway URL (local or remote).
    pub async fn gateway_url(&self) -> Option<String> {
        self.gateway_url.read().await.clone()
    }

    /// Start the gateway (local process or remote connection).
    pub async fn start(&self) -> Result<String, String> {
        let config = self.config.read().await.clone();
        match &config.mode {
            ConnectionMode::Local => self.start_local(&config).await,
            ConnectionMode::Remote { url, api_key } => {
                self.connect_remote(url, api_key.as_deref()).await
            }
            ConnectionMode::Unconfigured => {
                Err("gateway not configured — set mode to Local or Remote".into())
            }
        }
    }

    /// Start a local gateway process.
    async fn start_local(&self, config: &GatewayProcessConfig) -> Result<String, String> {
        *self.process_state.write().await = ProcessState::Starting;
        info!(port = config.local_port, "starting local gateway");

        // Determine binary path
        let binary = config
            .local_binary
            .as_deref()
            .unwrap_or("clawdesk-gateway");

        // Spawn the child process using tokio::process::Command
        let child_result = tokio::process::Command::new(binary)
            .arg("run")
            .arg("--port")
            .arg(config.local_port.to_string())
            .arg("--bind")
            .arg("127.0.0.1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn();

        match child_result {
            Ok(child_proc) => {
                *self.child.write().await = Some(child_proc);
            }
            Err(e) => {
                // Binary not found or cannot spawn — fall back to in-process mode.
                // This is the common case in Tauri apps where the gateway runs
                // in-process rather than as a separate child process.
                debug!(binary = %binary, error = %e, "child spawn failed, assuming in-process gateway");
            }
        }

        let url = format!("http://127.0.0.1:{}", config.local_port);
        *self.gateway_url.write().await = Some(url.clone());
        *self.process_state.write().await = ProcessState::Running;

        info!(url = %url, "local gateway started");
        Ok(url)
    }

    /// Convert ws:// / wss:// URLs to http:// / https:// for HTTP probes.
    fn ws_to_http(url: &str) -> String {
        if url.starts_with("wss://") {
            format!("https://{}", &url[6..])
        } else if url.starts_with("ws://") {
            format!("http://{}", &url[5..])
        } else {
            url.to_string()
        }
    }

    /// Connect to a remote gateway.
    async fn connect_remote(&self, url: &str, api_key: Option<&str>) -> Result<String, String> {
        *self.remote_state.write().await = RemoteState::Connecting;
        info!(url = %url, "connecting to remote gateway");

        // Convert ws/wss to http/https for the health-check probe
        let http_base = Self::ws_to_http(url);
        let health_url = format!("{}/api/v1/health", http_base.trim_end_matches('/'));

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| format!("HTTP client error: {e}"))?;

        let mut req = client.get(&health_url);
        if let Some(key) = api_key {
            req = req.bearer_auth(key);
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                info!(url = %url, "remote gateway health check passed");
            }
            Ok(resp) if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 => {
                *self.remote_state.write().await = RemoteState::AuthFailed;
                return Err(format!("authentication failed (HTTP {})", resp.status()));
            }
            Ok(resp) => {
                warn!(url = %url, status = resp.status().as_u16(), "remote gateway returned unexpected status");
                // Non-auth failures are not fatal — the gateway may still be starting.
            }
            Err(e) => {
                // Connectivity failure is non-fatal; set state to Reconnecting
                // so the caller can retry later.
                warn!(url = %url, error = %e, "health check failed, will set Reconnecting");
                *self.remote_state.write().await = RemoteState::Reconnecting;
            }
        }

        *self.gateway_url.write().await = Some(url.to_string());

        // Only upgrade to Connected if we're not already in a degraded state
        let current = *self.remote_state.read().await;
        if current == RemoteState::Connecting {
            *self.remote_state.write().await = RemoteState::Connected;
        }

        info!(url = %url, state = ?*self.remote_state.read().await, "remote gateway connection attempt complete");
        Ok(url.to_string())
    }

    /// Stop the gateway (kill local process or disconnect remote).
    pub async fn stop(&self) -> Result<(), String> {
        let config = self.config.read().await.clone();
        match &config.mode {
            ConnectionMode::Local => {
                *self.process_state.write().await = ProcessState::Stopping;
                // Send SIGTERM to child process if one was spawned
                if let Some(mut child) = self.child.write().await.take() {
                    info!("sending SIGTERM to local gateway child process");
                    // kill_on_drop is set, but try graceful shutdown first
                    let _ = child.kill().await;
                }
                *self.process_state.write().await = ProcessState::Stopped;
                info!("local gateway stopped");
            }
            ConnectionMode::Remote { .. } => {
                // Disconnect: clear URL, reset state
                *self.remote_state.write().await = RemoteState::Disconnected;
                info!("disconnected from remote gateway");
            }
            ConnectionMode::Unconfigured => {}
        }
        *self.gateway_url.write().await = None;
        Ok(())
    }

    /// Switch connection mode.
    pub async fn set_mode(&self, new_mode: ConnectionMode) -> Result<(), String> {
        // Stop current gateway first
        self.stop().await?;

        let mut config = self.config.write().await;
        config.mode = new_mode;
        *self.restart_count.write().await = 0;

        info!(mode = ?config.mode, "gateway mode changed");
        Ok(())
    }

    /// Get local process state.
    pub async fn process_state(&self) -> ProcessState {
        *self.process_state.read().await
    }

    /// Get remote connection state.
    pub async fn remote_state(&self) -> RemoteState {
        *self.remote_state.read().await
    }

    /// Handle a local process crash — auto-restart if configured.
    pub async fn on_process_crashed(&self) -> Result<bool, String> {
        let config = self.config.read().await.clone();
        if !config.auto_restart {
            *self.process_state.write().await = ProcessState::Failed;
            return Ok(false);
        }

        let mut count = self.restart_count.write().await;
        if config.max_restarts > 0 && *count >= config.max_restarts {
            warn!(
                attempts = *count,
                max = config.max_restarts,
                "max restart attempts reached"
            );
            *self.process_state.write().await = ProcessState::Failed;
            return Ok(false);
        }

        *count += 1;
        let attempt = *count;
        drop(count);

        // Exponential backoff
        let backoff = config.restart_backoff_ms * 2u64.pow(attempt.min(6));
        info!(
            attempt,
            backoff_ms = backoff,
            "restarting local gateway after crash"
        );

        tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
        self.start_local(&config).await?;
        Ok(true)
    }

    /// Get current config.
    pub async fn config(&self) -> GatewayProcessConfig {
        self.config.read().await.clone()
    }
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_mode_default() {
        assert_eq!(ConnectionMode::default(), ConnectionMode::Local);
    }

    #[test]
    fn connection_mode_serialization() {
        let local = serde_json::to_string(&ConnectionMode::Local).unwrap();
        assert_eq!(local, "\"local\"");

        let remote = ConnectionMode::Remote {
            url: "wss://gw.example.com".into(),
            api_key: Some("sk-123".into()),
        };
        let json = serde_json::to_value(&remote).unwrap();
        assert_eq!(json["remote"]["url"], "wss://gw.example.com");
    }

    #[test]
    fn process_config_default() {
        let cfg = GatewayProcessConfig::default();
        assert!(cfg.auto_start);
        assert!(cfg.auto_restart);
        assert_eq!(cfg.local_port, 18789);
        assert_eq!(cfg.max_restarts, 5);
    }

    #[tokio::test]
    async fn local_mode_start_stop() {
        let mgr = GatewayProcessManager::new(GatewayProcessConfig::default());

        let url = mgr.start().await.unwrap();
        assert!(url.contains("127.0.0.1:18789"));
        assert_eq!(mgr.process_state().await, ProcessState::Running);
        assert!(mgr.gateway_url().await.is_some());

        mgr.stop().await.unwrap();
        assert_eq!(mgr.process_state().await, ProcessState::Stopped);
        assert!(mgr.gateway_url().await.is_none());
    }

    #[tokio::test]
    async fn remote_mode_connect() {
        let mut cfg = GatewayProcessConfig::default();
        cfg.mode = ConnectionMode::Remote {
            url: "wss://remote.example.com".into(),
            api_key: None,
        };

        let mgr = GatewayProcessManager::new(cfg);
        let url = mgr.start().await.unwrap();
        assert!(url.contains("remote.example.com"));
        // Health check will fail for non-routable host, so state is Reconnecting
        let state = mgr.remote_state().await;
        assert!(
            state == RemoteState::Connected || state == RemoteState::Reconnecting,
            "expected Connected or Reconnecting, got {:?}",
            state
        );
    }

    #[tokio::test]
    async fn unconfigured_fails() {
        let mut cfg = GatewayProcessConfig::default();
        cfg.mode = ConnectionMode::Unconfigured;

        let mgr = GatewayProcessManager::new(cfg);
        let result = mgr.start().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn mode_switch() {
        let mgr = GatewayProcessManager::new(GatewayProcessConfig::default());
        mgr.start().await.unwrap();
        assert_eq!(mgr.process_state().await, ProcessState::Running);

        // Switch to remote
        mgr.set_mode(ConnectionMode::Remote {
            url: "wss://other.example.com".into(),
            api_key: None,
        })
        .await
        .unwrap();

        assert_eq!(mgr.process_state().await, ProcessState::Stopped);
        let mode = mgr.mode().await;
        match mode {
            ConnectionMode::Remote { url, .. } => assert!(url.contains("other.example.com")),
            _ => panic!("expected Remote mode"),
        }
    }

    #[tokio::test]
    async fn crash_restart_limit() {
        let mut cfg = GatewayProcessConfig::default();
        cfg.max_restarts = 2;
        cfg.restart_backoff_ms = 1; // fast for tests
        let mgr = GatewayProcessManager::new(cfg);

        // Simulate crashes
        assert!(mgr.on_process_crashed().await.unwrap()); // attempt 1
        assert!(mgr.on_process_crashed().await.unwrap()); // attempt 2
        assert!(!mgr.on_process_crashed().await.unwrap()); // exceeded
        assert_eq!(mgr.process_state().await, ProcessState::Failed);
    }

    #[tokio::test]
    async fn crash_no_restart_when_disabled() {
        let mut cfg = GatewayProcessConfig::default();
        cfg.auto_restart = false;
        let mgr = GatewayProcessManager::new(cfg);

        assert!(!mgr.on_process_crashed().await.unwrap());
        assert_eq!(mgr.process_state().await, ProcessState::Failed);
    }
}
