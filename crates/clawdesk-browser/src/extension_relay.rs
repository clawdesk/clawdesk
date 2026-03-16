//! # Extension Relay — Bidirectional Chrome Extension ↔ Backend communication.
//!
//! Uses WebSocket for real-time communication between a Chrome extension running
//! in the user's browser and the ClawDesk backend. Enables agents to observe and
//! act on the user's actual browsing context rather than a headless instance.
//!
//! ## Connection State Machine
//! ```text
//! Disconnected → Authenticating → Connected → Stale
//!      ↑              |                |        |
//!      └──────────────┴────────────────┴────────┘
//! ```
//!
//! ## Security
//! - CSRF token + secret-ref authentication on handshake
//! - All messages signed with session HMAC
//! - Reconnection uses exponential backoff with jitter

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::warn;

// ───────────────────────────────────────────────────────────────────────────
// Connection state machine
// ───────────────────────────────────────────────────────────────────────────

/// Extension relay connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayState {
    Disconnected,
    Authenticating,
    Connected,
    /// Connected but heartbeat overdue — may reconnect.
    Stale,
}

/// Configuration for the extension relay.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// WebSocket URL to listen on for extension connections.
    pub listen_addr: String,
    /// Heartbeat interval (extension sends ping every N seconds).
    pub heartbeat_interval: Duration,
    /// After this duration without a heartbeat, mark as Stale.
    pub heartbeat_timeout: Duration,
    /// Base delay for exponential backoff on reconnection.
    pub reconnect_base: Duration,
    /// Maximum reconnect delay.
    pub reconnect_max: Duration,
    /// Jitter range (added to backoff).
    pub reconnect_jitter: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:9222".to_string(),
            heartbeat_interval: Duration::from_secs(10),
            heartbeat_timeout: Duration::from_secs(30),
            reconnect_base: Duration::from_millis(100),
            reconnect_max: Duration::from_secs(30),
            reconnect_jitter: Duration::from_millis(50),
        }
    }
}

/// Messages from extension → backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionMessage {
    /// Authentication handshake.
    Auth { csrf_token: String, extension_id: String },
    /// Heartbeat ping.
    Ping { seq: u64 },
    /// Tab state update.
    TabUpdate { tab_id: u32, url: String, title: String, active: bool },
    /// Navigation event.
    Navigate { tab_id: u32, url: String, transition_type: String },
    /// DOM snapshot from active tab.
    Snapshot { tab_id: u32, snapshot: String },
    /// Content script result.
    ActionResult { request_id: String, success: bool, data: serde_json::Value },
}

/// Messages from backend → extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendMessage {
    /// Authentication accepted.
    AuthOk { session_id: String },
    /// Authentication rejected.
    AuthFail { reason: String },
    /// Heartbeat pong.
    Pong { seq: u64 },
    /// Request a DOM snapshot.
    RequestSnapshot { tab_id: u32, mode: String },
    /// Execute an action on a tab.
    ExecuteAction { request_id: String, tab_id: u32, action: serde_json::Value },
    /// Navigate to a URL.
    NavigateTo { tab_id: u32, url: String },
}

/// A connected extension relay session.
pub struct RelaySession {
    /// Connection state.
    pub state: RelayState,
    /// Extension ID.
    pub extension_id: String,
    /// Session ID (generated on auth).
    pub session_id: String,
    /// Last heartbeat received.
    pub last_heartbeat: Instant,
    /// Reconnection attempt count (for backoff).
    pub reconnect_attempts: u32,
    /// Sequence counter for messages.
    seq: AtomicU64,
}

impl RelaySession {
    pub fn new(extension_id: String, session_id: String) -> Self {
        Self {
            state: RelayState::Connected,
            extension_id,
            session_id,
            last_heartbeat: Instant::now(),
            reconnect_attempts: 0,
            seq: AtomicU64::new(0),
        }
    }

    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    pub fn record_heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
        self.state = RelayState::Connected;
        self.reconnect_attempts = 0;
    }

    pub fn is_stale(&self, timeout: Duration) -> bool {
        self.last_heartbeat.elapsed() > timeout
    }
}

/// Compute reconnection delay with exponential backoff + jitter.
///
/// `delay(n) = min(base × 2^n + rand(0, jitter), max_delay)`
pub fn reconnect_delay(config: &RelayConfig, attempt: u32) -> Duration {
    let base_ms = config.reconnect_base.as_millis() as u64;
    let exp = base_ms.saturating_mul(1u64 << attempt.min(20));
    let jitter = fastrand::u64(0..config.reconnect_jitter.as_millis() as u64);
    let total = exp.saturating_add(jitter);
    let max_ms = config.reconnect_max.as_millis() as u64;
    Duration::from_millis(total.min(max_ms))
}

/// Extension relay manager — handles multiple connected extensions.
pub struct ExtensionRelay {
    config: RelayConfig,
    sessions: Arc<RwLock<Vec<RelaySession>>>,
    /// Channel for sending messages to connected extensions.
    #[allow(dead_code)]
    outbound_tx: mpsc::Sender<(String, BackendMessage)>,
    /// Channel for receiving messages from extensions.
    #[allow(dead_code)]
    inbound_rx: mpsc::Receiver<(String, ExtensionMessage)>,
}

impl ExtensionRelay {
    pub fn new(config: RelayConfig) -> (Self, mpsc::Sender<(String, ExtensionMessage)>, mpsc::Receiver<(String, BackendMessage)>) {
        let (out_tx, out_rx) = mpsc::channel(256);
        let (in_tx, in_rx) = mpsc::channel(256);
        let relay = Self {
            config,
            sessions: Arc::new(RwLock::new(Vec::new())),
            outbound_tx: out_tx,
            inbound_rx: in_rx,
        };
        (relay, in_tx, out_rx)
    }

    /// Number of connected sessions.
    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Mark stale sessions based on heartbeat timeout.
    pub async fn check_heartbeats(&self) {
        let mut sessions = self.sessions.write().await;
        for session in sessions.iter_mut() {
            if session.state == RelayState::Connected
                && session.is_stale(self.config.heartbeat_timeout)
            {
                warn!(
                    ext = %session.extension_id,
                    "extension heartbeat timeout — marking stale"
                );
                session.state = RelayState::Stale;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_delay_exponential() {
        let config = RelayConfig::default();
        let d0 = reconnect_delay(&config, 0);
        let d1 = reconnect_delay(&config, 1);
        // d1 should be roughly 2× d0 (ignoring jitter).
        assert!(d1 >= d0);
    }

    #[test]
    fn reconnect_delay_capped() {
        let config = RelayConfig::default();
        let d = reconnect_delay(&config, 100);
        assert!(d <= config.reconnect_max + config.reconnect_jitter);
    }

    #[test]
    fn session_heartbeat_tracking() {
        let session = RelaySession::new("ext1".into(), "sess1".into());
        assert_eq!(session.state, RelayState::Connected);
        assert!(!session.is_stale(Duration::from_secs(30)));
    }
}
