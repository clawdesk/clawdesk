//! DM pairing protocol — short-code verification for unknown senders.
//!
//! Implements `dmPolicy="pairing"`: unknown senders receive a challenge code,
//! messages aren't processed until approved by the device owner.
//!
//! ## Security Properties
//! - Codes generated from OS CSPRNG (`rand::rngs::OsRng`)
//! - 6-digit numeric code: collision probability ≈ 10⁻⁶ per concurrent pair
//! - Codes expire after τ = 300 seconds (configurable)
//! - Queue bounded at Q_max = 100 to prevent memory exhaustion from pairing spam
//! - Queue eviction: LRU (oldest pending pairing evicted when full)

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use rand::Rng;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Maximum pending pairings (prevents memory exhaustion from spam).
const MAX_PENDING: usize = 100;

/// Default code expiration in seconds.
const DEFAULT_EXPIRY_SECS: u64 = 300;

/// A pending pairing challenge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingChallenge {
    /// The generated pairing code (e.g., "482917").
    pub code: String,
    /// The sender's platform-specific ID.
    pub sender_id: String,
    /// The channel this pairing request came from.
    pub channel: String,
    /// Display name if available.
    pub display_name: Option<String>,
    /// When the challenge was created (serialized as epoch millis).
    #[serde(skip)]
    pub created_at: Option<Instant>,
    /// ISO timestamp for serialization.
    pub created_at_iso: String,
}

/// Result of a pairing attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingResult {
    /// Pairing approved — sender added to allowlist.
    Approved { sender_id: String },
    /// Code doesn't match any pending pairing.
    InvalidCode,
    /// Pairing code has expired.
    Expired,
    /// Pairing rejected by owner.
    Rejected,
}

/// DM pairing manager.
///
/// Maintains a bounded queue of pending pairings with LRU eviction.
/// Thread-safe: designed to be wrapped in `Arc<RwLock<DmPairingManager>>`.
pub struct DmPairingManager {
    /// Pending pairings keyed by code.
    pending: HashMap<String, PairingChallenge>,
    /// Insertion order for LRU eviction.
    order: VecDeque<String>,
    /// Code expiration duration.
    expiry: Duration,
}

impl DmPairingManager {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            order: VecDeque::new(),
            expiry: Duration::from_secs(DEFAULT_EXPIRY_SECS),
        }
    }

    pub fn with_expiry(mut self, secs: u64) -> Self {
        self.expiry = Duration::from_secs(secs);
        self
    }

    /// Generate a pairing challenge for an unknown sender.
    ///
    /// If the sender already has a pending challenge, returns the existing one.
    /// If the queue is full, evicts the oldest pending challenge.
    pub fn challenge(&mut self, sender_id: &str, channel: &str, display_name: Option<String>) -> PairingChallenge {
        // Check if sender already has a pending challenge
        for (code, challenge) in &self.pending {
            if challenge.sender_id == sender_id && challenge.channel == channel {
                debug!(%sender_id, %code, "returning existing pairing challenge");
                return challenge.clone();
            }
        }

        // Evict expired entries first
        self.evict_expired();

        // Evict oldest if at capacity
        while self.pending.len() >= MAX_PENDING {
            if let Some(old_code) = self.order.pop_front() {
                self.pending.remove(&old_code);
                warn!("evicted oldest pairing challenge (queue full)");
            }
        }

        // Generate a unique 6-digit code
        let code = self.generate_code();

        let now_iso = {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            format!("{}", secs)
        };

        let challenge = PairingChallenge {
            code: code.clone(),
            sender_id: sender_id.to_string(),
            channel: channel.to_string(),
            display_name,
            created_at: Some(Instant::now()),
            created_at_iso: now_iso,
        };

        self.pending.insert(code.clone(), challenge.clone());
        self.order.push_back(code);

        info!(%sender_id, channel, "new pairing challenge created");
        challenge
    }

    /// Approve a pending pairing by code. Returns the sender_id on success.
    pub fn approve(&mut self, code: &str) -> PairingResult {
        match self.pending.remove(code) {
            Some(challenge) => {
                self.order.retain(|c| c != code);

                // Check expiration
                if let Some(created) = challenge.created_at {
                    if created.elapsed() > self.expiry {
                        info!(sender = %challenge.sender_id, "pairing code expired");
                        return PairingResult::Expired;
                    }
                }

                info!(sender = %challenge.sender_id, "pairing approved");
                PairingResult::Approved {
                    sender_id: challenge.sender_id,
                }
            }
            None => PairingResult::InvalidCode,
        }
    }

    /// Reject a pending pairing by code.
    pub fn reject(&mut self, code: &str) -> PairingResult {
        match self.pending.remove(code) {
            Some(challenge) => {
                self.order.retain(|c| c != code);
                info!(sender = %challenge.sender_id, "pairing rejected");
                PairingResult::Rejected
            }
            None => PairingResult::InvalidCode,
        }
    }

    /// List all pending pairing challenges.
    pub fn list_pending(&self) -> Vec<&PairingChallenge> {
        self.pending.values().collect()
    }

    /// Number of pending challenges.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Remove expired challenges.
    pub fn evict_expired(&mut self) {
        let expired_codes: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, c)| {
                c.created_at
                    .map(|t| t.elapsed() > self.expiry)
                    .unwrap_or(false)
            })
            .map(|(code, _)| code.clone())
            .collect();

        for code in &expired_codes {
            self.pending.remove(code);
            debug!(%code, "evicted expired pairing challenge");
        }

        self.order.retain(|c| !expired_codes.contains(c));
    }

    /// Generate a CSPRNG-backed 6-digit code using OS entropy.
    fn generate_code(&mut self) -> String {
        let mut rng = rand::rngs::OsRng;
        loop {
            let code = format!("{:06}", rng.gen_range(0..1_000_000u32));

            // Ensure uniqueness
            if !self.pending.contains_key(&code) {
                return code;
            }
        }
    }
}

impl Default for DmPairingManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_generates_6_digit_code() {
        let mut mgr = DmPairingManager::new();
        let c = mgr.challenge("user1", "telegram", Some("User One".into()));
        assert_eq!(c.code.len(), 6);
        assert!(c.code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn same_sender_gets_same_code() {
        let mut mgr = DmPairingManager::new();
        let c1 = mgr.challenge("user1", "telegram", None);
        let c2 = mgr.challenge("user1", "telegram", None);
        assert_eq!(c1.code, c2.code);
    }

    #[test]
    fn different_senders_get_different_codes() {
        let mut mgr = DmPairingManager::new();
        let c1 = mgr.challenge("user1", "telegram", None);
        let c2 = mgr.challenge("user2", "telegram", None);
        assert_ne!(c1.code, c2.code);
    }

    #[test]
    fn approve_valid_code() {
        let mut mgr = DmPairingManager::new();
        let c = mgr.challenge("user1", "telegram", None);
        let result = mgr.approve(&c.code);
        assert_eq!(
            result,
            PairingResult::Approved {
                sender_id: "user1".to_string()
            }
        );
        assert_eq!(mgr.pending_count(), 0);
    }

    #[test]
    fn approve_invalid_code() {
        let mut mgr = DmPairingManager::new();
        mgr.challenge("user1", "telegram", None);
        let result = mgr.approve("000000");
        assert_eq!(result, PairingResult::InvalidCode);
    }

    #[test]
    fn reject_removes_challenge() {
        let mut mgr = DmPairingManager::new();
        let c = mgr.challenge("user1", "telegram", None);
        let result = mgr.reject(&c.code);
        assert_eq!(result, PairingResult::Rejected);
        assert_eq!(mgr.pending_count(), 0);
    }

    #[test]
    fn expired_code_returns_expired() {
        let mut mgr = DmPairingManager::new().with_expiry(0); // immediate expiry
        let c = mgr.challenge("user1", "telegram", None);
        std::thread::sleep(Duration::from_millis(10));
        let result = mgr.approve(&c.code);
        assert_eq!(result, PairingResult::Expired);
    }

    #[test]
    fn eviction_when_full() {
        let mut mgr = DmPairingManager::new();
        // Fill to capacity
        for i in 0..MAX_PENDING {
            mgr.challenge(&format!("user_{i}"), "telegram", None);
        }
        assert_eq!(mgr.pending_count(), MAX_PENDING);

        // Adding one more should evict the oldest
        mgr.challenge("overflow_user", "telegram", None);
        assert_eq!(mgr.pending_count(), MAX_PENDING);
    }

    #[test]
    fn list_pending_returns_all() {
        let mut mgr = DmPairingManager::new();
        mgr.challenge("user1", "telegram", None);
        mgr.challenge("user2", "telegram", None);
        assert_eq!(mgr.list_pending().len(), 2);
    }
}
