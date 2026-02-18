//! Peer registry — track discovered ClawDesk instances.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Peer health status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerStatus {
    Discovered,
    Paired,
    Connected,
    Unreachable,
}

/// A discovered peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub version: String,
    pub status: PeerStatus,
    pub capabilities: Vec<String>,
    #[serde(skip)]
    pub last_seen: Option<Instant>,
    pub paired: bool,
}

/// Peer registry with automatic stale peer removal.
pub struct PeerRegistry {
    peers: HashMap<String, Peer>,
    stale_timeout: Duration,
}

impl PeerRegistry {
    /// Create a new registry.
    pub fn new(stale_timeout: Duration) -> Self {
        Self {
            peers: HashMap::new(),
            stale_timeout,
        }
    }

    /// Register or update a peer.
    pub fn upsert(&mut self, mut peer: Peer) {
        peer.last_seen = Some(Instant::now());
        self.peers.insert(peer.id.clone(), peer);
    }

    /// Get a peer by ID.
    pub fn get(&self, id: &str) -> Option<&Peer> {
        self.peers.get(id)
    }

    /// Remove a peer.
    pub fn remove(&mut self, id: &str) -> Option<Peer> {
        self.peers.remove(id)
    }

    /// Get all active (non-stale) peers.
    pub fn active_peers(&self) -> Vec<&Peer> {
        self.peers
            .values()
            .filter(|p| {
                p.last_seen
                    .map(|t| t.elapsed() < self.stale_timeout)
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Get all paired peers.
    pub fn paired_peers(&self) -> Vec<&Peer> {
        self.peers.values().filter(|p| p.paired).collect()
    }

    /// Remove stale peers (not seen within timeout).
    pub fn prune_stale(&mut self) -> usize {
        let before = self.peers.len();
        self.peers.retain(|_, p| {
            p.last_seen
                .map(|t| t.elapsed() < self.stale_timeout)
                .unwrap_or(false)
        });
        before - self.peers.len()
    }

    /// Total peer count.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Mark a peer as paired.
    pub fn mark_paired(&mut self, id: &str) -> bool {
        if let Some(peer) = self.peers.get_mut(id) {
            peer.paired = true;
            peer.status = PeerStatus::Paired;
            true
        } else {
            false
        }
    }

    /// Update peer status.
    pub fn set_status(&mut self, id: &str, status: PeerStatus) -> bool {
        if let Some(peer) = self.peers.get_mut(id) {
            peer.status = status;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_peer(id: &str, name: &str) -> Peer {
        Peer {
            id: id.to_string(),
            name: name.to_string(),
            host: "192.168.1.100".to_string(),
            port: 18789,
            version: "0.1.0".to_string(),
            status: PeerStatus::Discovered,
            capabilities: vec!["chat".into()],
            last_seen: None,
            paired: false,
        }
    }

    #[test]
    fn upsert_and_get() {
        let mut reg = PeerRegistry::new(Duration::from_secs(60));
        reg.upsert(make_peer("p1", "Node 1"));
        assert_eq!(reg.len(), 1);
        assert!(reg.get("p1").is_some());
        assert_eq!(reg.get("p1").unwrap().name, "Node 1");
    }

    #[test]
    fn active_peers_filter() {
        let mut reg = PeerRegistry::new(Duration::from_secs(60));
        reg.upsert(make_peer("p1", "Node 1"));
        assert_eq!(reg.active_peers().len(), 1);
    }

    #[test]
    fn mark_paired() {
        let mut reg = PeerRegistry::new(Duration::from_secs(60));
        reg.upsert(make_peer("p1", "Node 1"));
        assert!(reg.mark_paired("p1"));
        assert!(reg.get("p1").unwrap().paired);
        assert_eq!(reg.paired_peers().len(), 1);
    }

    #[test]
    fn remove_peer() {
        let mut reg = PeerRegistry::new(Duration::from_secs(60));
        reg.upsert(make_peer("p1", "Node 1"));
        assert!(reg.remove("p1").is_some());
        assert!(reg.is_empty());
    }

    #[test]
    fn set_status() {
        let mut reg = PeerRegistry::new(Duration::from_secs(60));
        reg.upsert(make_peer("p1", "Node 1"));
        reg.set_status("p1", PeerStatus::Connected);
        assert_eq!(reg.get("p1").unwrap().status, PeerStatus::Connected);
    }
}
