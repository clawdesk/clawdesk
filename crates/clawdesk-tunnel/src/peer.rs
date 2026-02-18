//! Peer management — key exchange, state tracking, and lifecycle.
//!
//! Each peer represents a remote client (phone, laptop, another machine)
//! that has been authorized to connect to this ClawDesk gateway via
//! WireGuard. Peers are identified by their Curve25519 static public key.
//!
//! # Peer lifecycle
//!
//! 1. **Invited**: Admin generates a `PeerInvite` (QR code / invite link)
//! 2. **Pending**: Client scans invite, sends handshake initiation
//! 3. **Connected**: Handshake complete, encrypted tunnel established
//! 4. **Idle**: No traffic for keepalive_timeout, but session keys retained
//! 5. **Expired**: Session keys expired (2 minutes per WireGuard spec)
//! 6. **Revoked**: Admin removed the peer
//!
//! # Cache-line optimization
//!
//! Hot fields (counters, timestamps) are packed into the first 64 bytes
//! to fit in one cache line. Cold fields (keys, config) are on subsequent lines.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{debug, info};

// ── Peer State ───────────────────────────────────────────────

/// Per-peer connection state, optimized for cache-line access patterns.
///
/// Hot fields (accessed every packet) are packed into the first 64 bytes.
/// Cold fields (accessed on handshake/config) follow.
///
/// Memory: ~200 bytes per peer. For 50 peers: ~10KB (fits in L1 cache).
#[repr(C)]
pub struct PeerState {
    // ── Hot fields (accessed every packet) ──
    /// Receive counter for replay protection.
    pub rx_counter: AtomicU64,
    /// Transmit counter (nonce for encryption).
    pub tx_counter: AtomicU64,
    /// Timestamp of last handshake (nanoseconds since epoch).
    pub last_handshake_ns: AtomicU64,
    /// Total bytes received from this peer.
    pub rx_bytes: AtomicU64,
    /// Total bytes sent to this peer.
    pub tx_bytes: AtomicU64,
    /// Total packets received.
    pub rx_packets: AtomicU64,
    /// Total packets sent.
    pub tx_packets: AtomicU64,
    /// Whether this peer is currently active.
    pub is_active: AtomicBool,

    // ── Cold fields (accessed on handshake/config) ──
    /// Peer's static Curve25519 public key (32 bytes).
    pub public_key: [u8; 32],
    /// Optional pre-shared key for additional security layer.
    pub preshared_key: Option<[u8; 32]>,
    /// Human-readable label (e.g., "Alice's phone").
    pub label: String,
    /// Last known endpoint (IP:port).
    pub endpoint: RwLock<Option<SocketAddr>>,
    /// When this peer was added.
    pub created_at: u64,
    /// Current peer status.
    pub status: RwLock<PeerStatus>,
}

/// Peer connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerStatus {
    /// Invite sent, waiting for connection.
    Pending,
    /// Handshake complete, encrypted tunnel active.
    Connected,
    /// No recent traffic, but session keys retained.
    Idle,
    /// Session keys expired, needs new handshake.
    Expired,
    /// Admin revoked this peer's access.
    Revoked,
}

impl std::fmt::Display for PeerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Connected => write!(f, "connected"),
            Self::Idle => write!(f, "idle"),
            Self::Expired => write!(f, "expired"),
            Self::Revoked => write!(f, "revoked"),
        }
    }
}

impl PeerState {
    /// Create a new peer state with default values.
    pub fn new(public_key: [u8; 32], preshared_key: Option<[u8; 32]>) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            rx_counter: AtomicU64::new(0),
            tx_counter: AtomicU64::new(0),
            last_handshake_ns: AtomicU64::new(0),
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            rx_packets: AtomicU64::new(0),
            tx_packets: AtomicU64::new(0),
            is_active: AtomicBool::new(false),
            public_key,
            preshared_key,
            label: String::new(),
            endpoint: RwLock::new(None),
            created_at: now,
            status: RwLock::new(PeerStatus::Pending),
        }
    }

    /// Create a new peer with a label.
    pub fn with_label(public_key: [u8; 32], label: impl Into<String>) -> Self {
        let mut peer = Self::new(public_key, None);
        peer.label = label.into();
        peer
    }

    /// Get the public key as hex string.
    pub fn public_key_hex(&self) -> String {
        self.public_key
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }

    /// Record received bytes.
    pub fn record_rx(&self, bytes: u64) {
        self.rx_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.rx_packets.fetch_add(1, Ordering::Relaxed);
    }

    /// Record sent bytes.
    pub fn record_tx(&self, bytes: u64) {
        self.tx_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.tx_packets.fetch_add(1, Ordering::Relaxed);
    }

    /// Update the peer's endpoint.
    pub async fn update_endpoint(&self, addr: SocketAddr) {
        let mut ep = self.endpoint.write().await;
        *ep = Some(addr);
    }

    /// Get the peer's current endpoint.
    pub async fn get_endpoint(&self) -> Option<SocketAddr> {
        *self.endpoint.read().await
    }

    /// Mark the peer as connected.
    pub async fn set_connected(&self) {
        self.is_active.store(true, Ordering::Release);
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        self.last_handshake_ns.store(now_ns, Ordering::Release);
        *self.status.write().await = PeerStatus::Connected;
    }

    /// Mark the peer as idle.
    pub async fn set_idle(&self) {
        self.is_active.store(false, Ordering::Release);
        *self.status.write().await = PeerStatus::Idle;
    }

    /// Revoke the peer.
    pub async fn revoke(&self) {
        self.is_active.store(false, Ordering::Release);
        *self.status.write().await = PeerStatus::Revoked;
    }

    /// Get a snapshot of the peer's state for API responses.
    pub async fn snapshot(&self) -> PeerSnapshot {
        PeerSnapshot {
            public_key_hex: self.public_key_hex(),
            label: self.label.clone(),
            status: *self.status.read().await,
            endpoint: self.get_endpoint().await.map(|a| a.to_string()),
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            rx_packets: self.rx_packets.load(Ordering::Relaxed),
            tx_packets: self.tx_packets.load(Ordering::Relaxed),
            created_at: self.created_at,
            last_handshake_ns: self.last_handshake_ns.load(Ordering::Relaxed),
        }
    }
}

/// Serializable snapshot of a peer's state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSnapshot {
    pub public_key_hex: String,
    pub label: String,
    pub status: PeerStatus,
    pub endpoint: Option<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub created_at: u64,
    pub last_handshake_ns: u64,
}

// ── Peer Configuration ──────────────────────────────────────

/// Static configuration for a peer (stored on disk).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    /// Peer's static public key (hex-encoded).
    pub public_key: String,
    /// Human-readable label.
    pub label: String,
    /// Optional pre-shared key (hex-encoded).
    pub preshared_key: Option<String>,
    /// Allowed IP ranges (CIDR notation).
    #[serde(default)]
    pub allowed_ips: Vec<String>,
    /// Persistent keepalive interval in seconds (0 = disabled).
    #[serde(default)]
    pub persistent_keepalive: u64,
    /// When this peer was added (Unix timestamp).
    pub added_at: u64,
    /// Who added this peer.
    pub added_by: String,
}

// ── Peer Manager ─────────────────────────────────────────────

/// Manages the lifecycle of WireGuard peers.
///
/// Responsibilities:
/// - Add/remove peers
/// - Track peer state transitions
/// - Persist peer configs to disk
/// - Prune expired/idle peers
pub struct PeerManager {
    /// Active peer states, indexed by public key.
    peers: RwLock<HashMap<[u8; 32], Arc<PeerState>>>,
    /// Maximum number of peers.
    max_peers: usize,
    /// Idle timeout before marking a peer as idle.
    idle_timeout: Duration,
    /// Session expiry (WireGuard rekey interval: 120 seconds).
    #[allow(dead_code)]
    rekey_after: Duration,
}

impl PeerManager {
    /// Create a new peer manager.
    pub fn new(max_peers: usize) -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
            max_peers,
            idle_timeout: Duration::from_secs(300), // 5 minutes
            rekey_after: Duration::from_secs(120),   // 2 minutes (WireGuard spec)
        }
    }

    /// Add a new peer from a config.
    pub async fn add_peer(&self, config: &PeerConfig) -> Result<Arc<PeerState>, PeerManagerError> {
        let mut peers = self.peers.write().await;

        if peers.len() >= self.max_peers {
            return Err(PeerManagerError::PeerLimitReached {
                max: self.max_peers,
            });
        }

        // Decode public key from hex
        let pubkey = hex_to_key(&config.public_key)
            .map_err(|_| PeerManagerError::InvalidKey(config.public_key.clone()))?;

        if peers.contains_key(&pubkey) {
            return Err(PeerManagerError::DuplicatePeer(config.public_key.clone()));
        }

        let psk = config
            .preshared_key
            .as_ref()
            .and_then(|s| hex_to_key(s).ok());

        let mut state = PeerState::new(pubkey, psk);
        state.label = config.label.clone();

        let arc_state = Arc::new(state);
        peers.insert(pubkey, arc_state.clone());

        info!(
            peer = %config.label,
            public_key = %config.public_key,
            "peer added"
        );

        Ok(arc_state)
    }

    /// Remove a peer by public key.
    pub async fn remove_peer(&self, public_key_hex: &str) -> Result<(), PeerManagerError> {
        let pubkey = hex_to_key(public_key_hex)
            .map_err(|_| PeerManagerError::InvalidKey(public_key_hex.to_string()))?;

        let mut peers = self.peers.write().await;
        if let Some(peer) = peers.remove(&pubkey) {
            peer.revoke().await;
            info!(public_key = %public_key_hex, "peer removed");
            Ok(())
        } else {
            Err(PeerManagerError::NotFound(public_key_hex.to_string()))
        }
    }

    /// Get a peer by public key.
    pub async fn get_peer(&self, public_key: &[u8; 32]) -> Option<Arc<PeerState>> {
        self.peers.read().await.get(public_key).cloned()
    }

    /// List all peers with their current state.
    pub async fn list_peers(&self) -> Vec<PeerSnapshot> {
        let peers = self.peers.read().await;
        let mut snapshots = Vec::with_capacity(peers.len());
        for peer in peers.values() {
            snapshots.push(peer.snapshot().await);
        }
        snapshots
    }

    /// Count total peers.
    pub async fn peer_count(&self) -> usize {
        self.peers.read().await.len()
    }

    /// Prune expired and idle peers, updating their status.
    ///
    /// Call this periodically (e.g., every 30 seconds) from a background task.
    pub async fn prune_idle(&self) {
        let peers = self.peers.read().await;
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        for peer in peers.values() {
            let status = *peer.status.read().await;
            if status == PeerStatus::Revoked {
                continue;
            }

            let last_hs = peer.last_handshake_ns.load(Ordering::Relaxed);
            if last_hs == 0 {
                continue; // Never connected
            }

            let elapsed_ns = now_ns.saturating_sub(last_hs);
            let elapsed = Duration::from_nanos(elapsed_ns);

            if elapsed > self.idle_timeout && status == PeerStatus::Connected {
                debug!(
                    peer = %peer.label,
                    elapsed_secs = elapsed.as_secs(),
                    "marking peer as idle"
                );
                peer.set_idle().await;
            }
        }
    }
}

/// Peer manager errors.
#[derive(Debug)]
pub enum PeerManagerError {
    PeerLimitReached { max: usize },
    InvalidKey(String),
    DuplicatePeer(String),
    NotFound(String),
}

impl std::fmt::Display for PeerManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PeerLimitReached { max } => write!(f, "peer limit reached ({} max)", max),
            Self::InvalidKey(key) => write!(f, "invalid key: {}", key),
            Self::DuplicatePeer(key) => write!(f, "duplicate peer: {}", key),
            Self::NotFound(key) => write!(f, "peer not found: {}", key),
        }
    }
}

impl std::error::Error for PeerManagerError {}

// ── Helper: hex to 32-byte key ──────────────────────────────

fn hex_to_key(hex: &str) -> Result<[u8; 32], ()> {
    if hex.len() != 64 {
        return Err(());
    }
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| ())?;
    }
    Ok(bytes)
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_state_creation() {
        let peer = PeerState::new([42u8; 32], None);
        assert_eq!(peer.public_key, [42u8; 32]);
        assert_eq!(peer.rx_bytes.load(Ordering::Relaxed), 0);
        assert_eq!(peer.tx_bytes.load(Ordering::Relaxed), 0);
        assert!(!peer.is_active.load(Ordering::Relaxed));
    }

    #[test]
    fn peer_state_with_label() {
        let peer = PeerState::with_label([1u8; 32], "Alice's phone");
        assert_eq!(peer.label, "Alice's phone");
    }

    #[test]
    fn peer_record_traffic() {
        let peer = PeerState::new([1u8; 32], None);
        peer.record_rx(1024);
        peer.record_rx(2048);
        peer.record_tx(512);

        assert_eq!(peer.rx_bytes.load(Ordering::Relaxed), 3072);
        assert_eq!(peer.tx_bytes.load(Ordering::Relaxed), 512);
        assert_eq!(peer.rx_packets.load(Ordering::Relaxed), 2);
        assert_eq!(peer.tx_packets.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn peer_public_key_hex() {
        let mut key = [0u8; 32];
        key[0] = 0xab;
        key[1] = 0xcd;
        key[31] = 0xef;
        let peer = PeerState::new(key, None);
        let hex = peer.public_key_hex();
        assert!(hex.starts_with("abcd"));
        assert!(hex.ends_with("ef"));
        assert_eq!(hex.len(), 64);
    }

    #[tokio::test]
    async fn peer_state_transitions() {
        let peer = PeerState::new([1u8; 32], None);

        // Initial: pending
        assert_eq!(*peer.status.read().await, PeerStatus::Pending);

        // Connect
        peer.set_connected().await;
        assert_eq!(*peer.status.read().await, PeerStatus::Connected);
        assert!(peer.is_active.load(Ordering::Relaxed));

        // Idle
        peer.set_idle().await;
        assert_eq!(*peer.status.read().await, PeerStatus::Idle);
        assert!(!peer.is_active.load(Ordering::Relaxed));

        // Revoke
        peer.revoke().await;
        assert_eq!(*peer.status.read().await, PeerStatus::Revoked);
    }

    #[tokio::test]
    async fn peer_endpoint_tracking() {
        let peer = PeerState::new([1u8; 32], None);
        assert!(peer.get_endpoint().await.is_none());

        let addr: SocketAddr = "203.0.113.1:51820".parse().unwrap();
        peer.update_endpoint(addr).await;
        assert_eq!(peer.get_endpoint().await, Some(addr));
    }

    #[tokio::test]
    async fn peer_manager_add_remove() {
        let manager = PeerManager::new(10);

        let config = PeerConfig {
            public_key: "aa".repeat(32),
            label: "Test Peer".into(),
            preshared_key: None,
            allowed_ips: vec![],
            persistent_keepalive: 0,
            added_at: 0,
            added_by: "admin".into(),
        };

        let peer = manager.add_peer(&config).await.unwrap();
        assert_eq!(peer.label, "Test Peer");
        assert_eq!(manager.peer_count().await, 1);

        // Remove
        manager.remove_peer(&"aa".repeat(32)).await.unwrap();
        assert_eq!(manager.peer_count().await, 0);
    }

    #[tokio::test]
    async fn peer_manager_limit() {
        let manager = PeerManager::new(1);

        let config1 = PeerConfig {
            public_key: "aa".repeat(32),
            label: "Peer 1".into(),
            preshared_key: None,
            allowed_ips: vec![],
            persistent_keepalive: 0,
            added_at: 0,
            added_by: "admin".into(),
        };
        let config2 = PeerConfig {
            public_key: "bb".repeat(32),
            label: "Peer 2".into(),
            preshared_key: None,
            allowed_ips: vec![],
            persistent_keepalive: 0,
            added_at: 0,
            added_by: "admin".into(),
        };

        manager.add_peer(&config1).await.unwrap();
        let result = manager.add_peer(&config2).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn peer_manager_no_duplicates() {
        let manager = PeerManager::new(10);

        let config = PeerConfig {
            public_key: "cc".repeat(32),
            label: "Peer".into(),
            preshared_key: None,
            allowed_ips: vec![],
            persistent_keepalive: 0,
            added_at: 0,
            added_by: "admin".into(),
        };

        manager.add_peer(&config).await.unwrap();
        let result = manager.add_peer(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn peer_manager_list() {
        let manager = PeerManager::new(10);

        let config = PeerConfig {
            public_key: "dd".repeat(32),
            label: "Listed Peer".into(),
            preshared_key: None,
            allowed_ips: vec![],
            persistent_keepalive: 0,
            added_at: 0,
            added_by: "admin".into(),
        };

        manager.add_peer(&config).await.unwrap();
        let list = manager.list_peers().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label, "Listed Peer");
    }

    #[test]
    fn hex_to_key_valid() {
        let hex = "aa".repeat(32);
        let key = hex_to_key(&hex).unwrap();
        assert_eq!(key, [0xaa; 32]);
    }

    #[test]
    fn hex_to_key_invalid_length() {
        assert!(hex_to_key("aabb").is_err());
    }

    #[test]
    fn hex_to_key_invalid_chars() {
        assert!(hex_to_key(&"zz".repeat(32)).is_err());
    }
}
