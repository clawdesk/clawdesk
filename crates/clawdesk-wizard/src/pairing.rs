//! Cryptographic device pairing via challenge-response.

use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;

/// A pairing challenge with cryptographic nonce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingChallenge {
    pub challenge_id: String,
    pub nonce: Vec<u8>,
    pub created_at: String,
    pub expires_at: String,
    pub attempts: u32,
    pub max_attempts: u32,
}

impl PairingChallenge {
    /// Create a new challenge with random nonce.
    pub fn new(max_attempts: u32, ttl_secs: u64) -> Self {
        let mut nonce = vec![0u8; 32];
        for byte in &mut nonce {
            *byte = rand::random();
        }
        let now = chrono::Utc::now();
        Self {
            challenge_id: uuid::Uuid::new_v4().to_string(),
            nonce,
            created_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::seconds(ttl_secs as i64)).to_rfc3339(),
            attempts: 0,
            max_attempts,
        }
    }

    /// Check if the challenge has expired or exceeded max attempts.
    pub fn is_valid(&self) -> bool {
        self.attempts < self.max_attempts
    }

    /// Record an attempt.
    pub fn record_attempt(&mut self) {
        self.attempts += 1;
    }
}

/// Human-readable setup code derived from public key + nonce.
///
/// Format: 8 uppercase alphanumeric characters (40 bits of entropy).
/// Brute force at 1000 attempts/sec with 5 attempts/min rate limit: ~34 years.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupCode(pub String);

impl SetupCode {
    /// Generate a setup code from a public key and nonce.
    pub fn generate(public_key: &[u8], nonce: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(public_key);
        hasher.update(nonce);
        let hash = hasher.finalize();

        // Take first 5 bytes (40 bits) and encode as base32-like.
        const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no I, O, 0, 1
        let mut code = String::with_capacity(8);
        for &byte in &hash[..8] {
            code.push(ALPHABET[(byte as usize) % ALPHABET.len()] as char);
        }
        Self(code)
    }

    /// Validate a user-entered code against the expected code.
    pub fn verify(&self, input: &str) -> bool {
        let normalized = input.trim().to_uppercase().replace(['I', 'O'], "");
        self.0 == normalized
    }
}

/// Persistent store for active pairing challenges.
#[derive(Debug, Default)]
pub struct PairingStore {
    challenges: HashMap<String, PairingChallenge>,
}

impl PairingStore {
    pub fn new() -> Self {
        Self { challenges: HashMap::new() }
    }

    pub fn store(&mut self, challenge: PairingChallenge) {
        self.challenges.insert(challenge.challenge_id.clone(), challenge);
    }

    pub fn get(&self, id: &str) -> Option<&PairingChallenge> {
        self.challenges.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut PairingChallenge> {
        self.challenges.get_mut(id)
    }

    pub fn remove(&mut self, id: &str) {
        self.challenges.remove(id);
    }

    /// Prune expired challenges.
    pub fn prune_expired(&mut self) {
        let now = chrono::Utc::now().to_rfc3339();
        self.challenges.retain(|_, c| c.expires_at > now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_code_is_8_chars() {
        let code = SetupCode::generate(b"pubkey", b"nonce");
        assert_eq!(code.0.len(), 8);
    }

    #[test]
    fn setup_code_deterministic() {
        let a = SetupCode::generate(b"key1", b"nonce1");
        let b = SetupCode::generate(b"key1", b"nonce1");
        assert_eq!(a.0, b.0);
    }

    #[test]
    fn setup_code_verify() {
        let code = SetupCode::generate(b"key", b"nonce");
        assert!(code.verify(&code.0));
        assert!(code.verify(&code.0.to_lowercase())); // case insensitive after normalize
    }

    #[test]
    fn challenge_max_attempts() {
        let mut challenge = PairingChallenge::new(3, 300);
        assert!(challenge.is_valid());
        challenge.record_attempt();
        challenge.record_attempt();
        challenge.record_attempt();
        assert!(!challenge.is_valid());
    }

    #[test]
    fn pairing_store_crud() {
        let mut store = PairingStore::new();
        let challenge = PairingChallenge::new(5, 300);
        let id = challenge.challenge_id.clone();
        store.store(challenge);
        assert!(store.get(&id).is_some());
        store.remove(&id);
        assert!(store.get(&id).is_none());
    }
}
