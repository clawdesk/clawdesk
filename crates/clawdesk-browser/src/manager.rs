//! BrowserManager — Per-agent session pool with idle reaper.
//!
//! Owns Chrome processes. Provides `Arc<Mutex<ManagedSession>>` per agent.
//! DashMap provides concurrent O(1) reads across non-colliding agent IDs.
//! Each session is behind `Arc<Mutex>` to serialize CDP commands per agent
//! (CDP protocol is inherently sequential per page).

use crate::cdp::{CdpConfig, CdpSession};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, info};

/// Per-agent browser session: CdpSession + Chrome child process.
pub struct ManagedSession {
    pub cdp: CdpSession,
    pub child: std::process::Child,
    pub last_active: Instant,
    pub pages_visited: u32,
    pub port: u16,
}

/// Centralized browser configuration.
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    pub headless: bool,
    pub browser_path: Option<String>,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub timeout_secs: u32,
    pub max_sessions: usize,
    pub idle_timeout_secs: u64,
    pub base_debug_port: u16,
    pub max_pages_per_task: u32,
    // Security
    pub allowed_ports: Vec<u16>,
    pub ssrf_allow_private: Vec<String>,
    // Content
    pub max_content_chars: usize,
    pub wrap_external_content: bool,
    // Approval
    pub require_purchase_approval: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            headless: true,
            browser_path: None,
            viewport_width: 1280,
            viewport_height: 720,
            timeout_secs: 30,
            max_sessions: 5,
            idle_timeout_secs: 300,
            base_debug_port: 19222,
            max_pages_per_task: 20,
            allowed_ports: vec![80, 443, 8080, 8443],
            ssrf_allow_private: vec![],
            max_content_chars: 50_000,
            wrap_external_content: true,
            require_purchase_approval: true,
        }
    }
}

/// Session pool keyed by agent_id.
pub struct BrowserManager {
    sessions: DashMap<String, Arc<Mutex<ManagedSession>>>,
    pub config: BrowserConfig,
    next_port: AtomicU16,
}

impl BrowserManager {
    pub fn new(config: BrowserConfig) -> Arc<Self> {
        let base = config.base_debug_port;
        let mgr = Arc::new(Self {
            sessions: DashMap::new(),
            config,
            next_port: AtomicU16::new(base),
        });
        // Only start the idle-session reaper if we're inside a Tokio runtime.
        // During Tauri's synchronous setup phase there is no reactor yet, so we
        // defer the spawn and let callers invoke `start_reaper()` later if needed.
        if tokio::runtime::Handle::try_current().is_ok() {
            mgr.start_reaper();
        }
        mgr
    }

    /// Start the background idle-session reaper.
    ///
    /// Safe to call multiple times — each call spawns one additional reaper,
    /// but in practice this should only be called once after construction
    /// when a Tokio runtime is available.
    pub fn ensure_reaper(self: &Arc<Self>) {
        self.start_reaper();
    }

    /// Get or create a session for an agent.
    ///
    /// - If session exists: return it (O(1) DashMap lookup)
    /// - If at capacity: return error
    /// - Otherwise: spawn Chrome, connect CDP, insert session
    pub async fn get_or_create(
        &self,
        agent_id: &str,
    ) -> Result<Arc<Mutex<ManagedSession>>, String> {
        // Fast path: existing session
        if let Some(entry) = self.sessions.get(agent_id) {
            return Ok(Arc::clone(entry.value()));
        }

        // Check capacity
        if self.sessions.len() >= self.config.max_sessions {
            return Err(format!(
                "browser session limit reached ({}/{})",
                self.sessions.len(),
                self.config.max_sessions
            ));
        }

        // Allocate port (wait-free atomic increment)
        let port = self.next_port.fetch_add(1, Ordering::Relaxed);

        // Build CdpConfig
        let cdp_config = CdpConfig {
            ws_url: String::new(), // Discovered after launch
            timeout_ms: (self.config.timeout_secs as u64) * 1000,
            headless: self.config.headless,
            browser_path: self.config.browser_path.clone(),
        };

        // Spawn Chrome with sandboxed environment
        let (child, cdp, actual_port) = self.spawn_chrome_sandboxed(&cdp_config, port).await?;

        let session = Arc::new(Mutex::new(ManagedSession {
            cdp,
            child,
            last_active: Instant::now(),
            pages_visited: 0,
            port: actual_port,
        }));

        self.sessions
            .insert(agent_id.to_string(), Arc::clone(&session));
        info!(agent_id, port = actual_port, "browser session created");

        Ok(session)
    }

    /// Spawn Chrome with env_clear() for secret isolation.
    async fn spawn_chrome_sandboxed(
        &self,
        config: &CdpConfig,
        port: u16,
    ) -> Result<(std::process::Child, CdpSession, u16), String> {
        let browser_path = config
            .browser_path
            .clone()
            .or_else(CdpSession::detect_browser)
            .ok_or("no browser found — install Chrome, Chromium, or Edge")?;

        let data_dir = format!("/tmp/clawdesk-browser-{}", port);
        let args = CdpSession::browser_launch_args(port, config.headless, &data_dir);

        // SECURITY: Clear environment, pass through only what Chrome needs
        let mut cmd = std::process::Command::new(&browser_path);
        cmd.env_clear();

        // Platform-specific env passthrough
        for var in &[
            "PATH",
            "HOME",
            "TMPDIR",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            if let Ok(v) = std::env::var(var) {
                cmd.env(var, v);
            }
        }
        #[cfg(target_os = "windows")]
        for var in &[
            "SYSTEMROOT",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "LOCALAPPDATA",
        ] {
            if let Ok(v) = std::env::var(var) {
                cmd.env(var, v);
            }
        }

        // Additional hardening flags beyond browser_launch_args
        cmd.args(&args)
            .arg("--disable-extensions")
            .arg("--disable-plugins")
            .arg("--disable-component-update")
            .arg("about:blank")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let child = cmd
            .spawn()
            .map_err(|e| format!("Chrome launch failed: {}", e))?;

        // Wait for Chrome to start listening (retry with backoff)
        let mut ws_url = Err(String::new());
        for delay_ms in [200, 400, 800, 1600] {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            ws_url = CdpSession::discover_ws_url("127.0.0.1", port).await;
            if ws_url.is_ok() {
                break;
            }
        }
        let ws_url =
            ws_url.map_err(|e| format!("Chrome discovery failed after retries: {}", e))?;

        let mut session = CdpSession::new(CdpConfig {
            ws_url,
            ..config.clone()
        });
        session.connect().await?;

        // Enable required CDP domains
        let _ = session.send(session.enable_page()).await;
        let _ = session.send(session.enable_dom()).await;

        Ok((child, session, port))
    }

    /// Close and clean up a session.
    pub async fn close_session(&self, agent_id: &str) {
        if let Some((_, session)) = self.sessions.remove(agent_id) {
            let mut s = session.lock().await;
            let _ = s.child.kill();
            let _ = s.child.wait();
            debug!(agent_id, "browser session closed");
        }
    }

    /// List active session agent IDs (for API/debug).
    pub fn list_sessions(&self) -> Vec<String> {
        self.sessions.iter().map(|e| e.key().clone()).collect()
    }

    /// Number of active sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Background idle reaper task.
    ///
    /// Uses `Weak<Self>` so it doesn't prevent `BrowserManager::drop()`.
    /// Scans every 60s, closes sessions idle beyond `idle_timeout_secs`.
    fn start_reaper(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let Some(mgr) = weak.upgrade() else {
                    debug!("BrowserManager dropped, reaper exiting");
                    break;
                };
                let idle_limit = Duration::from_secs(mgr.config.idle_timeout_secs);
                let mut to_remove = Vec::new();

                for entry in mgr.sessions.iter() {
                    if let Ok(session) = entry.value().try_lock() {
                        if session.last_active.elapsed() > idle_limit {
                            to_remove.push(entry.key().clone());
                        }
                    }
                }

                for agent_id in to_remove {
                    mgr.close_session(&agent_id).await;
                    info!(agent_id, "reaped idle browser session");
                }
            }
        });
    }
}

impl Drop for BrowserManager {
    fn drop(&mut self) {
        for entry in self.sessions.iter_mut() {
            if let Ok(mut session) = entry.value().try_lock() {
                let _ = session.child.kill();
                let _ = session.child.wait();
            }
        }
        info!("BrowserManager dropped, all Chrome processes killed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_config_defaults() {
        let config = BrowserConfig::default();
        assert!(config.headless);
        assert_eq!(config.max_sessions, 5);
        assert_eq!(config.idle_timeout_secs, 300);
        assert_eq!(config.base_debug_port, 19222);
        assert_eq!(config.max_pages_per_task, 20);
        assert!(config.allowed_ports.contains(&80));
        assert!(config.allowed_ports.contains(&443));
        assert!(config.wrap_external_content);
        assert!(config.require_purchase_approval);
    }

    #[tokio::test]
    async fn test_manager_capacity_check() {
        let config = BrowserConfig {
            max_sessions: 0, // Zero capacity — all creates should fail
            ..Default::default()
        };
        // We can't call BrowserManager::new() because it spawns a reaper.
        // Instead, test the struct directly.
        let mgr = BrowserManager {
            sessions: DashMap::new(),
            config,
            next_port: AtomicU16::new(19222),
        };
        let result = mgr.get_or_create("agent-1").await;
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.contains("session limit reached"));
    }

    #[test]
    fn test_port_allocation() {
        let counter = AtomicU16::new(19222);
        assert_eq!(counter.fetch_add(1, Ordering::Relaxed), 19222);
        assert_eq!(counter.fetch_add(1, Ordering::Relaxed), 19223);
        assert_eq!(counter.fetch_add(1, Ordering::Relaxed), 19224);
    }
}
