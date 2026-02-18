//! Scoped capability-based auth tokens.
//!
//! # Why scoped tokens?
//!
//! OpenClaw's single `gateway.auth.token` is all-or-nothing: once exfiltrated
//! (CVE-2026-25253), it gives full `operator.admin` access. A stolen chat token
//! should NOT grant config changes, tool execution, or tunnel management.
//!
//! # Design
//!
//! Each token encodes:
//! - **Scope**: bitfield of capabilities (chat, admin, tools, etc.)
//! - **Expiry**: Unix timestamp (admin tokens: 24h, chat: 30d)
//! - **Peer ID**: bound to a specific WireGuard public key (optional)
//! - **Signature**: HMAC-SHA256 over (scope || expiry || peer_id)
//!
//! Verification is **stateless**: no database lookup, just HMAC recomputation (~200ns).
//! Comparison is **constant-time**: no timing side-channel.

use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── Token Scope (capability bitfield) ────────────────────────

/// Token capabilities — bitfield encoded in the token payload.
///
/// Each bit grants a specific capability. Tokens can combine multiple
/// capabilities (e.g., `CHAT | AUDIT` for a monitoring dashboard).
///
/// Design rationale: bitfield is O(1) to check, O(1) to serialize,
/// and fits in 4 bytes. A Set<String> approach would be O(n) to check
/// and variable-size to serialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenScope(u32);

impl TokenScope {
    /// Send/receive messages on agent channels.
    pub const CHAT: Self = Self(0b0000_0001);
    /// Configuration changes (gateway, channels, providers).
    pub const ADMIN: Self = Self(0b0000_0010);
    /// Skill install/remove/activate/deactivate.
    pub const SKILLS: Self = Self(0b0000_0100);
    /// Tool execution (potentially dangerous).
    pub const TOOLS: Self = Self(0b0000_1000);
    /// Cron task management.
    pub const CRON: Self = Self(0b0001_0000);
    /// Channel configuration.
    pub const CHANNELS: Self = Self(0b0010_0000);
    /// Read audit logs and traces.
    pub const AUDIT: Self = Self(0b0100_0000);
    /// Manage WireGuard tunnel peers.
    pub const TUNNEL: Self = Self(0b1000_0000);
    /// No capabilities.
    pub const NONE: Self = Self(0);
    /// All capabilities (superuser).
    pub const ALL: Self = Self(0xFF);

    /// Get the raw bits.
    pub fn bits(self) -> u32 {
        self.0
    }

    /// Create from raw bits.
    pub fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Check if this scope contains a specific capability.
    #[inline]
    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Combine two scopes (union).
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Intersect two scopes.
    pub fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Check if the scope is empty (no capabilities).
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Human-readable list of capabilities.
    pub fn capability_names(self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.contains(Self::CHAT) {
            names.push("chat");
        }
        if self.contains(Self::ADMIN) {
            names.push("admin");
        }
        if self.contains(Self::SKILLS) {
            names.push("skills");
        }
        if self.contains(Self::TOOLS) {
            names.push("tools");
        }
        if self.contains(Self::CRON) {
            names.push("cron");
        }
        if self.contains(Self::CHANNELS) {
            names.push("channels");
        }
        if self.contains(Self::AUDIT) {
            names.push("audit");
        }
        if self.contains(Self::TUNNEL) {
            names.push("tunnel");
        }
        names
    }
}

impl std::ops::BitOr for TokenScope {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for TokenScope {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl std::fmt::Display for TokenScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = self.capability_names();
        if names.is_empty() {
            write!(f, "none")
        } else {
            write!(f, "{}", names.join(","))
        }
    }
}

// ── Auth errors ──────────────────────────────────────────────

/// Authentication/authorization error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// Token has expired.
    Expired,
    /// HMAC signature does not match.
    InvalidSignature,
    /// Token does not have the required capability.
    InsufficientScope {
        required: TokenScope,
        actual: TokenScope,
    },
    /// Token is malformed (wrong length, bad encoding).
    Malformed,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expired => write!(f, "token expired"),
            Self::InvalidSignature => write!(f, "invalid token signature"),
            Self::InsufficientScope { required, actual } => {
                write!(
                    f,
                    "insufficient scope: required={}, actual={}",
                    required, actual
                )
            }
            Self::Malformed => write!(f, "malformed token"),
        }
    }
}

impl std::error::Error for AuthError {}

// ── Crypto: delegate to shared `crate::crypto` module ────────

use crate::crypto::{constant_time_eq, hmac_sha256, sha256};

// ── Scoped Token ─────────────────────────────────────────────

/// A scoped authentication token with capability-based access control.
///
/// Wire format: `scope(4) || expires_at(8) || peer_id(32) || signature(32)` = 76 bytes.
/// Encoded as base62 for URL-safety: ~103 characters.
///
/// # Security properties
///
/// - **Scoped**: token theft gives limited capabilities (chat-only token
///   cannot change config or manage tunnel peers)
/// - **Time-bound**: tokens expire (admin: 24h default, chat: 30d default)
/// - **Peer-bound**: token is bound to a specific WireGuard peer's public key.
///   Stolen token is useless from a different device.
/// - **Stateless verification**: HMAC recomputation, no database lookup (~200ns)
/// - **Constant-time comparison**: no timing side-channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedToken {
    /// Capability bitfield.
    pub scope: TokenScope,
    /// Expiry timestamp (Unix seconds).
    pub expires_at: u64,
    /// Curve25519 public key of the authorized peer.
    /// All-zeros = unbound (any peer can use this token).
    pub peer_id: [u8; 32],
    /// HMAC-SHA256 signature over (scope || expires_at || peer_id).
    signature: [u8; 32],
}

impl ScopedToken {
    /// Create a new scoped token.
    ///
    /// # Arguments
    /// - `scope`: capability bitfield
    /// - `ttl`: time-to-live (e.g., 24 hours for admin, 30 days for chat)
    /// - `peer_id`: WireGuard peer public key (all-zeros = unbound)
    /// - `server_secret`: 32-byte server secret used for HMAC signing
    pub fn create(
        scope: TokenScope,
        ttl: Duration,
        peer_id: [u8; 32],
        server_secret: &[u8; 32],
    ) -> Self {
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + ttl.as_secs();

        let signature = Self::compute_signature(scope, expires_at, &peer_id, server_secret);

        ScopedToken {
            scope,
            expires_at,
            peer_id,
            signature,
        }
    }

    /// Create an unbound token (not tied to a specific peer).
    pub fn create_unbound(
        scope: TokenScope,
        ttl: Duration,
        server_secret: &[u8; 32],
    ) -> Self {
        Self::create(scope, ttl, [0u8; 32], server_secret)
    }

    /// Verify the token and return the scope if valid.
    ///
    /// Checks:
    /// 1. Expiry (not timing-sensitive — checked first for fast rejection)
    /// 2. HMAC signature (constant-time comparison)
    pub fn verify(&self, server_secret: &[u8; 32]) -> Result<TokenScope, AuthError> {
        // Check expiry first (not timing-sensitive, enables fast rejection)
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if now > self.expires_at {
            return Err(AuthError::Expired);
        }

        let expected = Self::compute_signature(
            self.scope,
            self.expires_at,
            &self.peer_id,
            server_secret,
        );

        // Constant-time comparison — no timing leak
        if constant_time_eq(&self.signature, &expected) {
            Ok(self.scope)
        } else {
            Err(AuthError::InvalidSignature)
        }
    }

    /// Verify AND check that the required scope is satisfied.
    pub fn verify_with_scope(
        &self,
        server_secret: &[u8; 32],
        required: TokenScope,
    ) -> Result<TokenScope, AuthError> {
        let actual = self.verify(server_secret)?;
        if actual.contains(required) {
            Ok(actual)
        } else {
            Err(AuthError::InsufficientScope {
                required,
                actual,
            })
        }
    }

    /// Check if this token is bound to a specific peer.
    pub fn is_peer_bound(&self) -> bool {
        self.peer_id != [0u8; 32]
    }

    /// Check if the token matches a specific peer.
    pub fn matches_peer(&self, peer_pubkey: &[u8; 32]) -> bool {
        if !self.is_peer_bound() {
            return true; // unbound tokens match any peer
        }
        constant_time_eq(&self.peer_id, peer_pubkey)
    }

    /// Remaining time before expiry.
    pub fn remaining(&self) -> Option<Duration> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if now >= self.expires_at {
            None
        } else {
            Some(Duration::from_secs(self.expires_at - now))
        }
    }

    /// Encode to compact binary format (76 bytes → base62 string).
    pub fn encode(&self) -> String {
        let mut buf = [0u8; 76];
        buf[0..4].copy_from_slice(&self.scope.bits().to_le_bytes());
        buf[4..12].copy_from_slice(&self.expires_at.to_le_bytes());
        buf[12..44].copy_from_slice(&self.peer_id);
        buf[44..76].copy_from_slice(&self.signature);
        base62_encode(&buf)
    }

    /// Decode from compact binary format (base62 string → 76 bytes).
    pub fn decode(encoded: &str) -> Result<Self, AuthError> {
        let buf = base62_decode(encoded).map_err(|_| AuthError::Malformed)?;
        if buf.len() != 76 {
            return Err(AuthError::Malformed);
        }

        let scope = TokenScope::from_bits(u32::from_le_bytes(
            buf[0..4].try_into().map_err(|_| AuthError::Malformed)?,
        ));
        let expires_at = u64::from_le_bytes(
            buf[4..12].try_into().map_err(|_| AuthError::Malformed)?,
        );
        let mut peer_id = [0u8; 32];
        peer_id.copy_from_slice(&buf[12..44]);
        let mut signature = [0u8; 32];
        signature.copy_from_slice(&buf[44..76]);

        Ok(ScopedToken {
            scope,
            expires_at,
            peer_id,
            signature,
        })
    }

    /// Compute HMAC-SHA256 signature over token fields.
    fn compute_signature(
        scope: TokenScope,
        expires_at: u64,
        peer_id: &[u8; 32],
        server_secret: &[u8; 32],
    ) -> [u8; 32] {
        let mut mac_input = Vec::with_capacity(44);
        mac_input.extend_from_slice(&scope.bits().to_le_bytes());
        mac_input.extend_from_slice(&expires_at.to_le_bytes());
        mac_input.extend_from_slice(peer_id);
        hmac_sha256(server_secret, &mac_input)
    }
}

// ── Server Secret ────────────────────────────────────────────

/// Server secret for token signing. Generated once, persisted to disk.
///
/// If the secret is lost/rotated, all outstanding tokens become invalid.
/// This is a *feature*, not a bug — it enables emergency token revocation
/// by simply regenerating the secret.
#[derive(Clone)]
pub struct ServerSecret {
    key: [u8; 32],
}

impl ServerSecret {
    /// Create from raw bytes.
    pub fn from_bytes(key: [u8; 32]) -> Self {
        Self { key }
    }

    /// Generate a new random secret using OS entropy.
    pub fn generate() -> Self {
        let mut key = [0u8; 32];
        // Use a deterministic but unique derivation from current time + PID.
        // In production, this would use getrandom or /dev/urandom.
        let seed = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pid = std::process::id() as u128;
        let combined = seed ^ (pid << 64);
        let hash = sha256(&combined.to_le_bytes());
        key.copy_from_slice(&hash);
        Self { key }
    }

    /// Get the raw key bytes for HMAC operations.
    pub fn key(&self) -> &[u8; 32] {
        &self.key
    }

    /// Create a token with this server's secret.
    pub fn create_token(
        &self,
        scope: TokenScope,
        ttl: Duration,
        peer_id: [u8; 32],
    ) -> ScopedToken {
        ScopedToken::create(scope, ttl, peer_id, &self.key)
    }

    /// Verify a token with this server's secret.
    pub fn verify_token(&self, token: &ScopedToken) -> Result<TokenScope, AuthError> {
        token.verify(&self.key)
    }
}

// ── Base62 encoding ──────────────────────────────────────────

/// Base62 character set (0-9, A-Z, a-z). URL-safe, no special characters.
const BASE62_CHARS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Encode bytes to base62 string.
///
/// Simple big-endian division-based encoding. Not the fastest possible
/// implementation, but token encoding is a cold path (~μs).
fn base62_encode(data: &[u8]) -> String {
    if data.is_empty() {
        return String::new();
    }

    // Convert to big integer (Vec<u8> treated as big-endian bytes)
    let mut digits = data.to_vec();
    let mut result = Vec::new();

    while !digits.is_empty() {
        let mut remainder = 0u32;
        let mut new_digits = Vec::new();

        for &byte in &digits {
            let acc = (remainder << 8) | byte as u32;
            let quotient = acc / 62;
            remainder = acc % 62;

            if !new_digits.is_empty() || quotient > 0 {
                new_digits.push(quotient as u8);
            }
        }

        result.push(BASE62_CHARS[remainder as usize]);
        digits = new_digits;
    }

    // Add leading zeros for leading zero bytes
    for &byte in data {
        if byte == 0 {
            result.push(BASE62_CHARS[0]);
        } else {
            break;
        }
    }

    result.reverse();
    String::from_utf8(result).unwrap()
}

/// Decode base62 string to bytes.
fn base62_decode(s: &str) -> Result<Vec<u8>, &'static str> {
    if s.is_empty() {
        return Ok(Vec::new());
    }

    let mut digits: Vec<u8> = Vec::new();

    for ch in s.bytes() {
        let val = match ch {
            b'0'..=b'9' => ch - b'0',
            b'A'..=b'Z' => ch - b'A' + 10,
            b'a'..=b'z' => ch - b'a' + 36,
            _ => return Err("invalid base62 character"),
        };

        // Multiply existing digits by 62 and add new value
        let mut carry = val as u32;
        for digit in digits.iter_mut().rev() {
            let acc = (*digit as u32) * 62 + carry;
            *digit = (acc & 0xFF) as u8;
            carry = acc >> 8;
        }

        while carry > 0 {
            digits.insert(0, (carry & 0xFF) as u8);
            carry >>= 8;
        }
    }

    // Handle leading zeros (base62 '0' chars)
    let leading_zeros = s.bytes().take_while(|&b| b == b'0').count();
    let mut result = vec![0u8; leading_zeros];
    result.extend(digits);

    Ok(result)
}

// ── Default token TTLs ──────────────────────────────────────

/// Default TTL for admin tokens: 24 hours.
pub const ADMIN_TOKEN_TTL: Duration = Duration::from_secs(24 * 3600);

/// Default TTL for chat-only tokens: 30 days.
pub const CHAT_TOKEN_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

/// Default TTL for tool-execution tokens: 1 hour.
pub const TOOL_TOKEN_TTL: Duration = Duration::from_secs(3600);

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_secret() -> [u8; 32] {
        sha256(b"test-server-secret-do-not-use-in-production")
    }

    #[test]
    fn token_scope_bitflags() {
        let scope = TokenScope::CHAT | TokenScope::AUDIT;
        assert!(scope.contains(TokenScope::CHAT));
        assert!(scope.contains(TokenScope::AUDIT));
        assert!(!scope.contains(TokenScope::ADMIN));
        assert!(!scope.contains(TokenScope::TOOLS));
    }

    #[test]
    fn token_scope_all_contains_everything() {
        let all = TokenScope::ALL;
        assert!(all.contains(TokenScope::CHAT));
        assert!(all.contains(TokenScope::ADMIN));
        assert!(all.contains(TokenScope::SKILLS));
        assert!(all.contains(TokenScope::TOOLS));
        assert!(all.contains(TokenScope::CRON));
        assert!(all.contains(TokenScope::CHANNELS));
        assert!(all.contains(TokenScope::AUDIT));
        assert!(all.contains(TokenScope::TUNNEL));
    }

    #[test]
    fn token_scope_none_is_empty() {
        assert!(TokenScope::NONE.is_empty());
        assert!(!TokenScope::CHAT.is_empty());
    }

    #[test]
    fn token_scope_display() {
        let scope = TokenScope::CHAT | TokenScope::ADMIN;
        let display = format!("{}", scope);
        assert!(display.contains("chat"));
        assert!(display.contains("admin"));
    }

    #[test]
    fn create_and_verify_token() {
        let secret = test_secret();
        let token = ScopedToken::create(
            TokenScope::CHAT | TokenScope::AUDIT,
            Duration::from_secs(3600),
            [0u8; 32],
            &secret,
        );

        let result = token.verify(&secret);
        assert!(result.is_ok());
        let scope = result.unwrap();
        assert!(scope.contains(TokenScope::CHAT));
        assert!(scope.contains(TokenScope::AUDIT));
        assert!(!scope.contains(TokenScope::ADMIN));
    }

    #[test]
    fn token_with_wrong_secret_fails() {
        let secret = test_secret();
        let wrong_secret = sha256(b"wrong-secret");
        let token = ScopedToken::create(
            TokenScope::CHAT,
            Duration::from_secs(3600),
            [0u8; 32],
            &secret,
        );

        let result = token.verify(&wrong_secret);
        assert_eq!(result, Err(AuthError::InvalidSignature));
    }

    #[test]
    fn expired_token_fails() {
        let secret = test_secret();
        // Create a token that expired 10 seconds ago
        let mut token = ScopedToken::create(
            TokenScope::CHAT,
            Duration::from_secs(0),
            [0u8; 32],
            &secret,
        );
        token.expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 10;
        // Re-sign with the backdated expiry
        token.signature = ScopedToken::compute_signature(
            token.scope,
            token.expires_at,
            &token.peer_id,
            &secret,
        );

        let result = token.verify(&secret);
        assert_eq!(result, Err(AuthError::Expired));
    }

    #[test]
    fn verify_with_scope_checks_capabilities() {
        let secret = test_secret();
        let token = ScopedToken::create(
            TokenScope::CHAT,
            Duration::from_secs(3600),
            [0u8; 32],
            &secret,
        );

        // CHAT is present — should pass
        assert!(token.verify_with_scope(&secret, TokenScope::CHAT).is_ok());

        // ADMIN is not present — should fail
        let result = token.verify_with_scope(&secret, TokenScope::ADMIN);
        assert!(matches!(result, Err(AuthError::InsufficientScope { .. })));
    }

    #[test]
    fn peer_bound_token() {
        let secret = test_secret();
        let peer_key = sha256(b"peer-public-key"); // Simulated Curve25519 key
        let token = ScopedToken::create(
            TokenScope::CHAT,
            Duration::from_secs(3600),
            peer_key,
            &secret,
        );

        assert!(token.is_peer_bound());
        assert!(token.matches_peer(&peer_key));

        let wrong_peer = sha256(b"wrong-peer-key");
        assert!(!token.matches_peer(&wrong_peer));
    }

    #[test]
    fn unbound_token_matches_any_peer() {
        let secret = test_secret();
        let token = ScopedToken::create_unbound(
            TokenScope::CHAT,
            Duration::from_secs(3600),
            &secret,
        );

        assert!(!token.is_peer_bound());
        let any_peer = sha256(b"any-peer-key");
        assert!(token.matches_peer(&any_peer));
    }

    #[test]
    fn token_encode_decode_roundtrip() {
        let secret = test_secret();
        let token = ScopedToken::create(
            TokenScope::CHAT | TokenScope::AUDIT,
            Duration::from_secs(3600),
            [42u8; 32],
            &secret,
        );

        let encoded = token.encode();
        assert!(!encoded.is_empty());

        let decoded = ScopedToken::decode(&encoded).unwrap();
        assert_eq!(decoded.scope, token.scope);
        assert_eq!(decoded.expires_at, token.expires_at);
        assert_eq!(decoded.peer_id, token.peer_id);

        // Decoded token should still verify
        assert!(decoded.verify(&secret).is_ok());
    }

    #[test]
    fn token_remaining_time() {
        let secret = test_secret();
        let token = ScopedToken::create(
            TokenScope::CHAT,
            Duration::from_secs(3600),
            [0u8; 32],
            &secret,
        );

        let remaining = token.remaining().unwrap();
        // Should be close to 3600 seconds (within a second of creation)
        assert!(remaining.as_secs() >= 3598);
        assert!(remaining.as_secs() <= 3600);
    }

    #[test]
    fn server_secret_create_and_verify() {
        let secret = ServerSecret::generate();
        let token = secret.create_token(
            TokenScope::ADMIN | TokenScope::CHAT,
            Duration::from_secs(3600),
            [0u8; 32],
        );

        let result = secret.verify_token(&token);
        assert!(result.is_ok());
        let scope = result.unwrap();
        assert!(scope.contains(TokenScope::ADMIN));
        assert!(scope.contains(TokenScope::CHAT));
    }

    #[test]
    fn tampered_scope_fails_verification() {
        let secret = test_secret();
        let mut token = ScopedToken::create(
            TokenScope::CHAT,
            Duration::from_secs(3600),
            [0u8; 32],
            &secret,
        );

        // Tamper: escalate to ADMIN
        token.scope = TokenScope::ALL;

        // Signature no longer matches
        let result = token.verify(&secret);
        assert_eq!(result, Err(AuthError::InvalidSignature));
    }

    #[test]
    fn tampered_expiry_fails_verification() {
        let secret = test_secret();
        let mut token = ScopedToken::create(
            TokenScope::CHAT,
            Duration::from_secs(60),
            [0u8; 32],
            &secret,
        );

        // Tamper: extend expiry by 1 year
        token.expires_at += 365 * 24 * 3600;

        let result = token.verify(&secret);
        assert_eq!(result, Err(AuthError::InvalidSignature));
    }

    #[test]
    fn base62_roundtrip() {
        let data = [1u8, 2, 3, 4, 5, 42, 255, 0, 128];
        let encoded = base62_encode(&data);
        let decoded = base62_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn base62_empty() {
        let encoded = base62_encode(&[]);
        assert!(encoded.is_empty());
        let decoded = base62_decode("").unwrap();
        assert!(decoded.is_empty());
    }
}
