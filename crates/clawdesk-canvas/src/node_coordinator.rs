//! Node Mode coordinator — capability advertisement and role management.
//!
//! When ClawDesk runs in "node mode" it connects to a remote gateway and
//! advertises platform capabilities (camera, screen, location, device
//! commands, etc.) so the orchestrator knows what tools are available.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Role a node plays in the ClawDesk network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    /// Full desktop application with UI.
    Desktop,
    /// Mobile node (iOS/Android) providing device sensors.
    Mobile,
    /// Headless server node for compute.
    Server,
    /// Remote node connected over the network.
    Remote,
}

/// A capability that a node can expose.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NodeCapability {
    // Core capabilities
    Camera,
    ScreenCapture,
    ScreenRecording,
    Location,
    Microphone,
    Speakers,

    // Device-specific capabilities
    Sms,
    Photos,
    Contacts,
    Calendar,
    Motion,

    // Compute capabilities
    SystemRun,
    FileAccess,
    BrowserAutomation,
    Canvas,
    A2UI,

    // Network capabilities
    Notifications,
    VoiceWake,
    TalkMode,

    /// Custom capability with a string identifier.
    Custom(String),
}

/// Information about this node's identity and capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAdvertisement {
    /// Unique node identifier.
    pub node_id: String,
    /// Human-readable name.
    pub display_name: String,
    /// Role of this node.
    pub role: NodeRole,
    /// Platform string (e.g. "macos-aarch64", "ios", "android", "linux-x86_64").
    pub platform: String,
    /// App version.
    pub version: String,
    /// Set of capabilities this node exposes.
    pub capabilities: HashSet<NodeCapability>,
    /// Metadata (key-value pairs for extra information).
    pub metadata: HashMap<String, String>,
}

/// State of the coordinator's registration with the gateway.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationState {
    /// Not connected to any gateway.
    Disconnected,
    /// Connecting to gateway.
    Connecting,
    /// Connected and capabilities advertised.
    Registered,
    /// Connection lost, will retry.
    Reconnecting,
    /// Explicitly deregistered.
    Deregistered,
}

/// Configuration for node mode behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Gateway URL to connect to (ws:// or wss://).
    pub gateway_url: Option<String>,
    /// Whether to auto-connect on startup.
    pub auto_connect: bool,
    /// Reconnect interval in seconds.
    pub reconnect_interval_secs: u64,
    /// Maximum reconnect attempts (0 = infinite).
    pub max_reconnect_attempts: u32,
    /// Which role this node plays.
    pub role: NodeRole,
    /// Explicit capabilities override (auto-detected if empty).
    pub explicit_capabilities: HashSet<NodeCapability>,
    /// Human-readable name for this node.
    pub display_name: Option<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            gateway_url: None,
            auto_connect: false,
            reconnect_interval_secs: 5,
            max_reconnect_attempts: 0,
            role: NodeRole::Desktop,
            explicit_capabilities: HashSet::new(),
            display_name: None,
        }
    }
}

/// Node coordinator manages this device's participation as a node.
pub struct NodeCoordinator {
    config: Arc<RwLock<NodeConfig>>,
    state: Arc<RwLock<RegistrationState>>,
    detected_capabilities: Arc<RwLock<HashSet<NodeCapability>>>,
    node_id: String,
    reconnect_attempts: Arc<RwLock<u32>>,
}

impl NodeCoordinator {
    pub fn new(node_id: String, config: NodeConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            state: Arc::new(RwLock::new(RegistrationState::Disconnected)),
            detected_capabilities: Arc::new(RwLock::new(HashSet::new())),
            node_id,
            reconnect_attempts: Arc::new(RwLock::new(0)),
        }
    }

    /// Detect capabilities based on the current platform.
    pub async fn detect_capabilities(&self) -> HashSet<NodeCapability> {
        let mut caps = HashSet::new();

        // Core desktop capabilities always present
        caps.insert(NodeCapability::FileAccess);
        caps.insert(NodeCapability::SystemRun);
        caps.insert(NodeCapability::Canvas);
        caps.insert(NodeCapability::A2UI);
        caps.insert(NodeCapability::Notifications);

        // Platform-specific detection
        #[cfg(target_os = "macos")]
        {
            caps.insert(NodeCapability::Camera);
            caps.insert(NodeCapability::ScreenCapture);
            caps.insert(NodeCapability::ScreenRecording);
            caps.insert(NodeCapability::Location);
            caps.insert(NodeCapability::Microphone);
            caps.insert(NodeCapability::Speakers);
            caps.insert(NodeCapability::VoiceWake);
            caps.insert(NodeCapability::TalkMode);
            caps.insert(NodeCapability::BrowserAutomation);
            // macOS can also access contacts/calendar via EventKit framework
            caps.insert(NodeCapability::Contacts);
            caps.insert(NodeCapability::Calendar);
        }

        #[cfg(target_os = "linux")]
        {
            caps.insert(NodeCapability::Camera);
            caps.insert(NodeCapability::ScreenCapture);
            caps.insert(NodeCapability::ScreenRecording);
            caps.insert(NodeCapability::Microphone);
            caps.insert(NodeCapability::Speakers);
            caps.insert(NodeCapability::BrowserAutomation);
        }

        #[cfg(target_os = "windows")]
        {
            caps.insert(NodeCapability::Camera);
            caps.insert(NodeCapability::ScreenCapture);
            caps.insert(NodeCapability::ScreenRecording);
            caps.insert(NodeCapability::Location);
            caps.insert(NodeCapability::Microphone);
            caps.insert(NodeCapability::Speakers);
            caps.insert(NodeCapability::BrowserAutomation);
            caps.insert(NodeCapability::Contacts);
            caps.insert(NodeCapability::Calendar);
        }

        // Mobile-specific
        #[cfg(target_os = "ios")]
        {
            caps.insert(NodeCapability::Camera);
            caps.insert(NodeCapability::ScreenCapture);
            caps.insert(NodeCapability::Location);
            caps.insert(NodeCapability::Microphone);
            caps.insert(NodeCapability::Speakers);
            caps.insert(NodeCapability::Photos);
            caps.insert(NodeCapability::Contacts);
            caps.insert(NodeCapability::Calendar);
            caps.insert(NodeCapability::Motion);
            caps.insert(NodeCapability::VoiceWake);
            caps.insert(NodeCapability::TalkMode);
        }

        #[cfg(target_os = "android")]
        {
            caps.insert(NodeCapability::Camera);
            caps.insert(NodeCapability::ScreenCapture);
            caps.insert(NodeCapability::ScreenRecording);
            caps.insert(NodeCapability::Location);
            caps.insert(NodeCapability::Microphone);
            caps.insert(NodeCapability::Speakers);
            caps.insert(NodeCapability::Sms);
            caps.insert(NodeCapability::Photos);
            caps.insert(NodeCapability::Contacts);
            caps.insert(NodeCapability::Calendar);
            caps.insert(NodeCapability::Motion);
            caps.insert(NodeCapability::VoiceWake);
            caps.insert(NodeCapability::TalkMode);
        }

        // Store detected capabilities
        let mut stored = self.detected_capabilities.write().await;
        *stored = caps.clone();
        caps
    }

    /// Build the advertisement message.
    pub async fn build_advertisement(&self) -> NodeAdvertisement {
        let config = self.config.read().await;
        let detected = self.detected_capabilities.read().await;

        // Use explicit capabilities if set, otherwise use detected
        let capabilities = if config.explicit_capabilities.is_empty() {
            detected.clone()
        } else {
            config.explicit_capabilities.clone()
        };

        let platform = format!(
            "{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );

        NodeAdvertisement {
            node_id: self.node_id.clone(),
            display_name: config
                .display_name
                .clone()
                .unwrap_or_else(|| hostname::get()
                    .ok()
                    .and_then(|h| h.into_string().ok())
                    .unwrap_or_else(|| "ClawDesk Node".into())),
            role: config.role.clone(),
            platform,
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities,
            metadata: HashMap::new(),
        }
    }

    /// Convert ws:// / wss:// gateway URLs to http:// / https:// for REST calls.
    fn ws_to_http(url: &str) -> String {
        if url.starts_with("wss://") {
            format!("https://{}", &url[6..])
        } else if url.starts_with("ws://") {
            format!("http://{}", &url[5..])
        } else {
            url.to_string()
        }
    }

    /// Connect to gateway and register this node's capabilities.
    pub async fn connect(&self) -> Result<(), String> {
        let url = {
            let config = self.config.read().await;
            config
                .gateway_url
                .clone()
                .ok_or_else(|| "no gateway URL configured".to_string())?
        };

        // Detect capabilities before advertising
        self.detect_capabilities().await;

        *self.state.write().await = RegistrationState::Connecting;
        info!(gateway = %url, node_id = %self.node_id, "connecting to gateway");

        // Build advertisement
        let adv = self.build_advertisement().await;
        debug!(
            capabilities = adv.capabilities.len(),
            role = ?adv.role,
            "prepared node advertisement"
        );

        // POST registration to gateway REST endpoint
        let http_base = Self::ws_to_http(&url);
        let register_url = format!(
            "{}/api/v1/nodes/register",
            http_base.trim_end_matches('/')
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| format!("HTTP client error: {e}"))?;

        match client.post(&register_url).json(&adv).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!(node_id = %self.node_id, "node registered via REST");
            }
            Ok(resp) => {
                debug!(
                    status = resp.status().as_u16(),
                    "gateway registration returned non-success (endpoint may not exist yet)"
                );
            }
            Err(e) => {
                debug!(error = %e, "failed to POST registration (gateway may not support node registration yet)");
            }
        }

        // Spawn a heartbeat loop that periodically pings the gateway
        let heartbeat_url = format!(
            "{}/api/v1/nodes/{}/heartbeat",
            http_base.trim_end_matches('/'),
            self.node_id
        );
        let heartbeat_client = client.clone();
        let heartbeat_state = Arc::clone(&self.state);
        let node_id = self.node_id.clone();
        let reconnect_secs = {
            let config = self.config.read().await;
            config.reconnect_interval_secs
        };

        tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(reconnect_secs.max(5));
            loop {
                tokio::time::sleep(interval).await;
                let current = heartbeat_state.read().await.clone();
                if current == RegistrationState::Deregistered
                    || current == RegistrationState::Disconnected
                {
                    debug!(node_id = %node_id, "heartbeat loop ending — node deregistered");
                    break;
                }
                match heartbeat_client.post(&heartbeat_url).send().await {
                    Ok(_) => {
                        debug!(node_id = %node_id, "heartbeat sent");
                    }
                    Err(e) => {
                        debug!(node_id = %node_id, error = %e, "heartbeat failed");
                    }
                }
            }
        });

        *self.state.write().await = RegistrationState::Registered;
        info!("registered with gateway");
        Ok(())
    }

    /// Disconnect from gateway.
    pub async fn disconnect(&self) {
        *self.state.write().await = RegistrationState::Deregistered;
        info!(node_id = %self.node_id, "deregistered from gateway");
    }

    /// Get current registration state.
    pub async fn state(&self) -> RegistrationState {
        self.state.read().await.clone()
    }

    /// Get the set of effective capabilities.
    pub async fn capabilities(&self) -> HashSet<NodeCapability> {
        let config = self.config.read().await;
        if config.explicit_capabilities.is_empty() {
            self.detected_capabilities.read().await.clone()
        } else {
            config.explicit_capabilities.clone()
        }
    }

    /// Update config at runtime.
    pub async fn update_config(&self, new_config: NodeConfig) {
        *self.config.write().await = new_config;
    }

    /// Increment reconnect counter, returns false if max reached.
    pub async fn can_reconnect(&self) -> bool {
        let config = self.config.read().await;
        let max = config.max_reconnect_attempts;
        if max == 0 {
            return true; // infinite
        }
        let mut attempts = self.reconnect_attempts.write().await;
        if *attempts >= max {
            false
        } else {
            *attempts += 1;
            true
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_config_default() {
        let cfg = NodeConfig::default();
        assert!(!cfg.auto_connect);
        assert_eq!(cfg.role, NodeRole::Desktop);
        assert!(cfg.explicit_capabilities.is_empty());
    }

    #[tokio::test]
    async fn detect_capabilities_non_empty() {
        let coord = NodeCoordinator::new("test-node".into(), NodeConfig::default());
        let caps = coord.detect_capabilities().await;
        // Should always have at least the core desktop capabilities
        assert!(caps.contains(&NodeCapability::FileAccess));
        assert!(caps.contains(&NodeCapability::Canvas));
    }

    #[tokio::test]
    async fn build_advertisement() {
        let coord = NodeCoordinator::new("nd-1".into(), NodeConfig::default());
        coord.detect_capabilities().await;
        let adv = coord.build_advertisement().await;
        assert_eq!(adv.node_id, "nd-1");
        assert!(!adv.capabilities.is_empty());
        assert_eq!(adv.role, NodeRole::Desktop);
    }

    #[tokio::test]
    async fn connect_without_url_fails() {
        let coord = NodeCoordinator::new("nd-2".into(), NodeConfig::default());
        let result = coord.connect().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn explicit_capabilities_override() {
        let mut cfg = NodeConfig::default();
        cfg.explicit_capabilities.insert(NodeCapability::Camera);
        cfg.explicit_capabilities.insert(NodeCapability::Sms);

        let coord = NodeCoordinator::new("nd-3".into(), cfg);
        coord.detect_capabilities().await;
        let caps = coord.capabilities().await;
        assert_eq!(caps.len(), 2);
        assert!(caps.contains(&NodeCapability::Camera));
        assert!(caps.contains(&NodeCapability::Sms));
    }

    #[tokio::test]
    async fn disconnect_sets_deregistered() {
        let coord = NodeCoordinator::new("nd-4".into(), NodeConfig::default());
        coord.disconnect().await;
        assert_eq!(coord.state().await, RegistrationState::Deregistered);
    }

    #[tokio::test]
    async fn reconnect_limiter() {
        let mut cfg = NodeConfig::default();
        cfg.max_reconnect_attempts = 2;
        let coord = NodeCoordinator::new("nd-5".into(), cfg);
        assert!(coord.can_reconnect().await); // attempt 1
        assert!(coord.can_reconnect().await); // attempt 2
        assert!(!coord.can_reconnect().await); // exceeded
    }

    #[test]
    fn node_role_serialization() {
        let json = serde_json::to_string(&NodeRole::Mobile).unwrap();
        assert_eq!(json, "\"mobile\"");
    }

    #[test]
    fn capability_serialization() {
        let json = serde_json::to_string(&NodeCapability::Sms).unwrap();
        assert_eq!(json, "\"sms\"");

        let custom = NodeCapability::Custom("lidar".into());
        let json2 = serde_json::to_string(&custom).unwrap();
        assert!(json2.contains("lidar"));
    }
}
