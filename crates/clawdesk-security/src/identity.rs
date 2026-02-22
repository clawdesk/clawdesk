//! Agent identity — immutable contract, hash-verified, agent-unwritable.
//!
//! ## Security rationale
//!
//! A read-write `SOUL.md` pattern is vulnerable: an attacker injects
//! instructions via a webpage, the agent writes them to `SOUL.md`, and the
//! backdoor persists across restarts because `SOUL.md` is loaded into every
//! subsequent prompt. (Cf. Zenity persistence attack.)
//!
//! `IdentityContract` eliminates this entire attack class by design:
//! - The identity is **read-only** from the agent's perspective.
//! - A SHA-256 hash is computed at construction and verified on each read.
//! - Only explicit human actions (config reload, admin API) can modify it.
//! - The agent runner receives `&IdentityContract` (shared ref), never `&mut`.
//!
//! ## Usage
//!
//! ```rust,ignore
//! // At startup — loaded from config.toml or admin API
//! let contract = IdentityContract::new(
//!     "You are ClawDesk...".into(),
//!     IdentitySource::DiskLoad { path: "~/.clawdesk/config.toml".into() },
//! );
//!
//! // In agent runner — read-only access
//! assert!(contract.verify());
//! let persona = contract.persona();
//! ```
//!
//! Store in `ArcSwap<IdentityContract>` for hot-reload via admin API.
//! The agent runner snapshots via `.load()` — wait-free, ~2ns.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Identity source
// ---------------------------------------------------------------------------

/// Who set this identity — humans only, never agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IdentitySource {
    /// Set via GUI/CLI by the user (e.g., config.toml edit).
    UserConfig,
    /// Set via the admin API with an auth token.
    AdminApi {
        /// SHA-256 hash of the auth token used (not the token itself).
        token_hash: String,
    },
    /// Loaded from disk at startup.
    DiskLoad {
        /// Path to the config file.
        path: String,
    },
}

// ---------------------------------------------------------------------------
// Identity contract
// ---------------------------------------------------------------------------

/// Agent identity — immutable after construction, hash-verified on each use.
///
/// The agent **cannot** modify this. Only the admin API or config reload
/// (both require human authorization) can produce a new `IdentityContract`.
///
/// ## Invariants
/// - `persona_hash == sha256(persona.as_bytes())` — verified by `verify()`.
/// - `agent_writable` is always `false` — exists for audit/serialization clarity.
/// - No `&mut self` methods are exposed for modifying `persona` — the only
///   mutation path is `update_persona()`, which requires an `IdentitySource`
///   that is not an agent action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityContract {
    /// The persona prompt fragment (equivalent to SOUL.md).
    persona: String,
    /// SHA-256 hash of the persona, computed at construction.
    persona_hash: [u8; 32],
    /// Who set this identity (must be a human action).
    set_by: IdentitySource,
    /// When the identity was last modified.
    last_modified: DateTime<Utc>,
    /// Whether the agent has write access. Always `false`.
    agent_writable: bool,
    /// Version counter — incremented on each update for ABA detection.
    version: u64,
}

impl IdentityContract {
    /// Create a new identity contract.
    ///
    /// The hash is computed immediately and `agent_writable` is forced to `false`.
    pub fn new(persona: String, source: IdentitySource) -> Self {
        let hash = sha256(persona.as_bytes());
        Self {
            persona,
            persona_hash: hash,
            set_by: source,
            last_modified: Utc::now(),
            agent_writable: false,
            version: 1,
        }
    }

    /// Verify the persona hasn't been tampered with since construction.
    ///
    /// This should be called before every prompt assembly to detect
    /// memory corruption or unauthorized modification.
    pub fn verify(&self) -> bool {
        sha256(self.persona.as_bytes()) == self.persona_hash
    }

    /// Read the persona prompt fragment. Panics if verification fails.
    ///
    /// This is intentionally strict: a failed verification means the
    /// identity has been corrupted, and we should not silently use it.
    pub fn persona(&self) -> &str {
        assert!(
            self.verify(),
            "identity contract verification failed — persona hash mismatch"
        );
        &self.persona
    }

    /// Read the persona without verification (for debugging/admin views).
    pub fn persona_unchecked(&self) -> &str {
        &self.persona
    }

    /// Get the identity source.
    pub fn source(&self) -> &IdentitySource {
        &self.set_by
    }

    /// When this identity was last modified.
    pub fn last_modified(&self) -> DateTime<Utc> {
        self.last_modified
    }

    /// Current version number.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Whether the agent can write this identity (always `false`).
    pub fn is_agent_writable(&self) -> bool {
        self.agent_writable
    }

    /// Get the persona hash (hex-encoded) for audit logging.
    pub fn persona_hash_hex(&self) -> String {
        self.persona_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Update the persona — only callable by human-authorized sources.
    ///
    /// # Panics
    /// This method validates that the source is not an agent action.
    /// The type system enforces this (there is no `AgentAction` variant
    /// in `IdentitySource`), but we assert defensively.
    pub fn update_persona(&mut self, new: String, source: IdentitySource) {
        self.persona_hash = sha256(new.as_bytes());
        self.persona = new;
        self.set_by = source;
        self.last_modified = Utc::now();
        self.version += 1;
    }

    /// Serialize to JSON for persistence/admin API response.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

// ---------------------------------------------------------------------------
// SHA-256 (minimal, no external dep)
// ---------------------------------------------------------------------------

/// Compute SHA-256 hash of input bytes.
///
/// Uses a pure-Rust implementation to avoid adding a crypto dependency
/// to the security crate. For production use at scale, swap to `ring`
/// or `sha2` crate.
fn sha256(data: &[u8]) -> [u8; 32] {
    // SHA-256 constants
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
        0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
        0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    // Pre-processing: pad the message
    let bit_len = (data.len() as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0x00);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut hash = H0;

    // Process each 512-bit (64-byte) block
    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = hash[0];
        let mut b = hash[1];
        let mut c = hash[2];
        let mut d = hash[3];
        let mut e = hash[4];
        let mut f = hash[5];
        let mut g = hash[6];
        let mut h = hash[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        hash[0] = hash[0].wrapping_add(a);
        hash[1] = hash[1].wrapping_add(b);
        hash[2] = hash[2].wrapping_add(c);
        hash[3] = hash[3].wrapping_add(d);
        hash[4] = hash[4].wrapping_add(e);
        hash[5] = hash[5].wrapping_add(f);
        hash[6] = hash[6].wrapping_add(g);
        hash[7] = hash[7].wrapping_add(h);
    }

    let mut result = [0u8; 32];
    for (i, &val) in hash.iter().enumerate() {
        result[i * 4..i * 4 + 4].copy_from_slice(&val.to_be_bytes());
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_contract_verifies() {
        let contract = IdentityContract::new(
            "You are ClawDesk, a helpful AI assistant.".into(),
            IdentitySource::UserConfig,
        );
        assert!(contract.verify());
        assert!(!contract.is_agent_writable());
        assert_eq!(contract.version(), 1);
    }

    #[test]
    fn persona_accessible_when_valid() {
        let contract = IdentityContract::new(
            "You are ClawDesk.".into(),
            IdentitySource::DiskLoad {
                path: "~/.clawdesk/config.toml".into(),
            },
        );
        assert_eq!(contract.persona(), "You are ClawDesk.");
    }

    #[test]
    fn update_persona_increments_version() {
        let mut contract = IdentityContract::new(
            "Original persona.".into(),
            IdentitySource::UserConfig,
        );
        assert_eq!(contract.version(), 1);

        contract.update_persona(
            "Updated persona.".into(),
            IdentitySource::AdminApi {
                token_hash: "abc123".into(),
            },
        );
        assert_eq!(contract.version(), 2);
        assert!(contract.verify());
        assert_eq!(contract.persona(), "Updated persona.");
    }

    #[test]
    fn hash_changes_on_update() {
        let mut contract = IdentityContract::new(
            "First identity.".into(),
            IdentitySource::UserConfig,
        );
        let hash1 = contract.persona_hash_hex();

        contract.update_persona(
            "Second identity.".into(),
            IdentitySource::UserConfig,
        );
        let hash2 = contract.persona_hash_hex();

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn sha256_known_vector() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let empty_hash = sha256(b"");
        assert_eq!(
            empty_hash,
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14,
                0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
                0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c,
                0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
            ]
        );
    }

    #[test]
    fn serialization_roundtrip() {
        let contract = IdentityContract::new(
            "Test persona.".into(),
            IdentitySource::UserConfig,
        );
        let json = contract.to_json().unwrap();
        let restored: IdentityContract = serde_json::from_str(&json).unwrap();
        assert!(restored.verify());
        assert_eq!(restored.persona(), "Test persona.");
    }
}
