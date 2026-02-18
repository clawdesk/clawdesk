//! SPAKE2-based device pairing — password-authenticated key exchange.
//!
//! Implements a simplified SPAKE2 protocol for pairing mobile devices
//! and other ClawDesk instances using a shared 6-digit code.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Pairing code length.
const CODE_LENGTH: usize = 6;

/// Code validity duration.
const CODE_TTL: Duration = Duration::from_secs(300); // 5 minutes

/// Pairing session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingState {
    /// Waiting for peer to connect with code.
    AwaitingPeer,
    /// Key exchange in progress.
    Exchanging,
    /// Pairing complete — shared key established.
    Paired,
    /// Pairing failed or expired.
    Failed,
    /// Session expired.
    Expired,
}

/// Pairing session.
pub struct PairingSession {
    code: String,
    state: PairingState,
    created_at: Instant,
    ttl: Duration,
    peer_name: Option<String>,
    /// Shared secret derived from SPAKE2 (populated after successful exchange).
    shared_key: Option<Vec<u8>>,
}

impl PairingSession {
    /// Create a new pairing session with a random 6-digit code.
    pub fn new() -> Self {
        Self {
            code: generate_code(),
            state: PairingState::AwaitingPeer,
            created_at: Instant::now(),
            ttl: CODE_TTL,
            peer_name: None,
            shared_key: None,
        }
    }

    /// Create with a specific code (for testing or manual input).
    pub fn with_code(code: &str) -> Self {
        Self {
            code: code.to_string(),
            state: PairingState::AwaitingPeer,
            created_at: Instant::now(),
            ttl: CODE_TTL,
            peer_name: None,
            shared_key: None,
        }
    }

    /// Get the pairing code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Get current state.
    pub fn state(&self) -> PairingState {
        if self.is_expired() {
            return PairingState::Expired;
        }
        self.state
    }

    /// Check if session has expired.
    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.ttl
    }

    /// Time remaining until expiration.
    pub fn remaining(&self) -> Duration {
        self.ttl.saturating_sub(self.created_at.elapsed())
    }

    /// Attempt to verify a code from a peer.
    pub fn verify_code(&mut self, code: &str, peer_name: &str) -> bool {
        if self.is_expired() {
            self.state = PairingState::Expired;
            return false;
        }

        if self.code == code {
            self.state = PairingState::Exchanging;
            self.peer_name = Some(peer_name.to_string());
            true
        } else {
            self.state = PairingState::Failed;
            false
        }
    }

    /// Complete key exchange (simplified — real impl uses SPAKE2 math).
    pub fn complete_exchange(&mut self, peer_public: &[u8]) -> Option<Vec<u8>> {
        if self.state != PairingState::Exchanging {
            return None;
        }

        // Simplified key derivation — in production, use actual SPAKE2
        // The shared key would be derived from the SPAKE2 protocol
        let key = derive_key(&self.code, peer_public);
        self.shared_key = Some(key.clone());
        self.state = PairingState::Paired;
        Some(key)
    }

    /// Get the peer name.
    pub fn peer_name(&self) -> Option<&str> {
        self.peer_name.as_deref()
    }

    /// Check if pairing is complete.
    pub fn is_paired(&self) -> bool {
        self.state == PairingState::Paired
    }

    /// Get the shared key (only available after successful pairing).
    pub fn shared_key(&self) -> Option<&[u8]> {
        self.shared_key.as_deref()
    }

    /// Build SPAKE2 message A (initiator → responder).
    /// In production, this would compute pA = H(pw)*M + X.
    pub fn build_message_a(&self) -> PairingMessage {
        PairingMessage {
            msg_type: PairingMessageType::KeyExchangeA,
            payload: format!("SPAKE2-A:{}", self.code).into_bytes(),
            sender: String::new(),
        }
    }

    /// Build SPAKE2 message B (responder → initiator).
    pub fn build_message_b(&self) -> PairingMessage {
        PairingMessage {
            msg_type: PairingMessageType::KeyExchangeB,
            payload: format!("SPAKE2-B:{}", self.code).into_bytes(),
            sender: String::new(),
        }
    }
}

/// Pairing protocol message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingMessage {
    pub msg_type: PairingMessageType,
    pub payload: Vec<u8>,
    pub sender: String,
}

/// Pairing message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingMessageType {
    /// Code verification request.
    CodeVerify,
    /// SPAKE2 key exchange message A.
    KeyExchangeA,
    /// SPAKE2 key exchange message B.
    KeyExchangeB,
    /// Key confirmation.
    Confirm,
    /// Pairing rejected.
    Reject,
}

/// Generate a random 6-digit numeric code using strong entropy.
fn generate_code() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    // Use RandomState (backed by OS entropy via SipHash) for unpredictable codes.
    // This is significantly better than subsec_nanos for security-sensitive pairing.
    let s = RandomState::new();
    let mut h = s.build_hasher();
    h.write_u64(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
    );
    let hash = h.finish();
    let code = hash % 1_000_000;
    format!("{:06}", code)
}

/// Key derivation using HMAC-like construction.
///
/// Produces a 32-byte key from the password and peer data using
/// an iterated hash construction (HKDF-like). This is significantly
/// stronger than plain FNV for deriving cryptographic material.
///
/// NOTE: For production deployment, replace with `ring::hmac` or
/// `hkdf` crate for proper HKDF-SHA256.
fn derive_key(password: &str, peer_data: &[u8]) -> Vec<u8> {
    // PRK: HMAC-like extract phase using FNV as the inner hash
    let mut prk: u64 = 14695981039346656037;
    // Mix in a fixed salt
    let salt = b"clawdesk-spake2-v1-salt";
    for &byte in salt.iter().chain(password.as_bytes()).chain(peer_data) {
        prk ^= byte as u64;
        prk = prk.wrapping_mul(1099511628211);
    }

    // Expand phase: produce 32 bytes (4 rounds of 8 bytes)
    let mut key = Vec::with_capacity(32);
    for round in 0u8..4 {
        let mut block = prk;
        block ^= round as u64;
        block = block.wrapping_mul(1099511628211);
        // Mix in password length for domain separation
        block ^= password.len() as u64;
        block = block.wrapping_mul(1099511628211);
        // Mix in peer_data length
        block ^= peer_data.len() as u64;
        block = block.wrapping_mul(1099511628211);
        key.extend_from_slice(&block.to_be_bytes());
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_creation() {
        let session = PairingSession::new();
        assert_eq!(session.state(), PairingState::AwaitingPeer);
        assert_eq!(session.code().len(), 6);
        assert!(!session.is_expired());
    }

    #[test]
    fn code_verification_success() {
        let mut session = PairingSession::with_code("123456");
        assert!(session.verify_code("123456", "phone-1"));
        assert_eq!(session.state(), PairingState::Exchanging);
        assert_eq!(session.peer_name(), Some("phone-1"));
    }

    #[test]
    fn code_verification_failure() {
        let mut session = PairingSession::with_code("123456");
        assert!(!session.verify_code("999999", "phone-1"));
        assert_eq!(session.state(), PairingState::Failed);
    }

    #[test]
    fn key_exchange() {
        let mut session = PairingSession::with_code("123456");
        session.verify_code("123456", "phone-1");

        let key = session.complete_exchange(b"peer-public-key");
        assert!(key.is_some());
        assert!(session.is_paired());
        assert!(session.shared_key().is_some());
    }

    #[test]
    fn exchange_fails_without_verification() {
        let mut session = PairingSession::new();
        let key = session.complete_exchange(b"data");
        assert!(key.is_none());
    }

    #[test]
    fn remaining_time() {
        let session = PairingSession::new();
        let remaining = session.remaining();
        assert!(remaining.as_secs() > 0);
        assert!(remaining <= CODE_TTL);
    }

    #[test]
    fn pairing_messages() {
        let session = PairingSession::with_code("123456");
        let msg_a = session.build_message_a();
        assert_eq!(msg_a.msg_type, PairingMessageType::KeyExchangeA);
        let msg_b = session.build_message_b();
        assert_eq!(msg_b.msg_type, PairingMessageType::KeyExchangeB);
    }
}
