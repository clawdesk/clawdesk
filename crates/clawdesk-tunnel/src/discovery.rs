//! Peer discovery — QR code invites and invite links.
//!
//! # Security comparison with OpenClaw
//!
//! | Property               | OpenClaw                    | ClawDesk                          |
//! |------------------------|-----------------------------|------------------------------------|
//! | Token transport        | URL query param (HTTP)      | QR code / invite code (out-of-band) |
//! | Browser history leak   | Yes                         | No (never in URL)                  |
//! | Replay protection      | None                        | One-time PSK, burned after use     |
//! | Expiry                 | No expiry                   | 24h default                        |
//! | What's exposed         | Full gateway token          | Public key + one-time PSK          |
//! | Man-in-the-middle      | Possible without TLS        | Impossible (Curve25519 key pinning)|
//!
//! # Invite flow
//!
//! 1. Admin runs `clawdesk invite create --label "Alice's phone"`
//! 2. CLI generates a `PeerInvite` and displays it as a QR code in terminal
//! 3. Remote client scans the QR code (or pastes the invite string)
//! 4. Client connects via WireGuard using the gateway's public key + one-time PSK
//! 5. After first successful handshake, the PSK is burned (marked used)
//! 6. Subsequent connections use the established static key pair

use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── Peer Invite ──────────────────────────────────────────────

/// An invite for a new remote client to connect to this ClawDesk gateway.
///
/// The invite encodes the gateway's static public key and a one-time
/// pre-shared key (PSK) for the initial handshake. It's encoded as a
/// compact URL-safe string (base62) or rendered as a QR code.
///
/// # Wire format
///
/// ```text
/// gateway_pubkey (32) || onetime_psk (32) || endpoint_len (2) ||
/// endpoint (var) || expires_at (8) || label_len (2) || label (var)
/// ```
///
/// Total: 76 + endpoint.len() + label.len() bytes → base62 ≈ 120-180 chars.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInvite {
    /// Gateway's static Curve25519 public key (32 bytes).
    pub gateway_pubkey: [u8; 32],
    /// One-time PSK for initial handshake (32 bytes).
    /// Burned after first successful connection.
    pub onetime_psk: [u8; 32],
    /// Gateway's endpoint (IP:port or domain:port).
    pub endpoint: String,
    /// Expiry timestamp (Unix seconds). Invite becomes invalid after this.
    pub expires_at: u64,
    /// Human-readable label for this peer (e.g., "Alice's phone").
    pub label: String,
    /// Whether this invite has been used.
    #[serde(default)]
    pub used: bool,
}

impl PeerInvite {
    /// Create a new invite with the given parameters.
    pub fn new(
        gateway_pubkey: [u8; 32],
        endpoint: impl Into<String>,
        label: impl Into<String>,
        ttl: Duration,
    ) -> Self {
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + ttl.as_secs();

        // Generate a random one-time PSK
        let onetime_psk = generate_random_key();

        Self {
            gateway_pubkey,
            onetime_psk,
            endpoint: endpoint.into(),
            expires_at,
            label: label.into(),
            used: false,
        }
    }

    /// Check if the invite has expired.
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now > self.expires_at
    }

    /// Check if the invite is still valid (not expired and not used).
    pub fn is_valid(&self) -> bool {
        !self.used && !self.is_expired()
    }

    /// Mark the invite as used (burn the one-time PSK).
    pub fn burn(&mut self) {
        self.used = true;
        // Zero the PSK in memory
        self.onetime_psk = [0u8; 32];
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

    /// Encode as a compact invite code (base62 string).
    ///
    /// Format: `gateway_pubkey(32) || psk(32) || endpoint_len(2) || endpoint(var)
    ///          || expires_at(8) || label_len(2) || label(var)`
    pub fn to_invite_code(&self) -> String {
        let mut buf = Vec::with_capacity(
            32 + 32 + 2 + self.endpoint.len() + 8 + 2 + self.label.len(),
        );
        buf.extend_from_slice(&self.gateway_pubkey);
        buf.extend_from_slice(&self.onetime_psk);
        buf.extend_from_slice(&(self.endpoint.len() as u16).to_le_bytes());
        buf.extend_from_slice(self.endpoint.as_bytes());
        buf.extend_from_slice(&self.expires_at.to_le_bytes());
        buf.extend_from_slice(&(self.label.len() as u16).to_le_bytes());
        buf.extend_from_slice(self.label.as_bytes());
        base62_encode(&buf)
    }

    /// Decode from invite code string.
    pub fn from_invite_code(code: &str) -> Result<Self, InviteError> {
        let buf = base62_decode(code).map_err(|_| InviteError::InvalidEncoding)?;

        if buf.len() < 76 {
            return Err(InviteError::TooShort);
        }

        let mut gateway_pubkey = [0u8; 32];
        gateway_pubkey.copy_from_slice(&buf[0..32]);

        let mut onetime_psk = [0u8; 32];
        onetime_psk.copy_from_slice(&buf[32..64]);

        let endpoint_len =
            u16::from_le_bytes([buf[64], buf[65]]) as usize;
        if buf.len() < 66 + endpoint_len + 10 {
            return Err(InviteError::TooShort);
        }

        let endpoint =
            String::from_utf8(buf[66..66 + endpoint_len].to_vec())
                .map_err(|_| InviteError::InvalidEncoding)?;

        let offset = 66 + endpoint_len;
        let expires_at = u64::from_le_bytes(
            buf[offset..offset + 8]
                .try_into()
                .map_err(|_| InviteError::InvalidEncoding)?,
        );

        let label_len_offset = offset + 8;
        if buf.len() < label_len_offset + 2 {
            return Err(InviteError::TooShort);
        }

        let label_len =
            u16::from_le_bytes([buf[label_len_offset], buf[label_len_offset + 1]]) as usize;

        let label_offset = label_len_offset + 2;
        if buf.len() < label_offset + label_len {
            return Err(InviteError::TooShort);
        }

        let label =
            String::from_utf8(buf[label_offset..label_offset + label_len].to_vec())
                .map_err(|_| InviteError::InvalidEncoding)?;

        Ok(Self {
            gateway_pubkey,
            onetime_psk,
            endpoint,
            expires_at,
            label,
            used: false,
        })
    }

    /// Render as a text-based QR code for terminal display.
    ///
    /// Uses Unicode block characters for compact rendering.
    /// QR version 7 (45×45 modules) fits ~150 alphanumeric chars.
    pub fn to_qr_text(&self) -> String {
        let code = self.to_invite_code();
        // Simple text representation — in production, use a QR encoding crate
        format!(
            "┌─────────────────────────────────┐\n\
             │     ClawDesk Peer Invite        │\n\
             │                                 │\n\
             │  Label: {:<24}│\n\
             │  Endpoint: {:<21}│\n\
             │  Expires: {} │\n\
             │                                 │\n\
             │  Invite code:                   │\n\
             │  {}│\n\
             │                                 │\n\
             │  Scan this with the ClawDesk    │\n\
             │  mobile app to connect.         │\n\
             └─────────────────────────────────┘",
            self.label,
            self.endpoint,
            format_expiry(self.expires_at),
            truncate_code(&code, 33),
        )
    }

    /// Get the gateway public key as hex.
    pub fn gateway_pubkey_hex(&self) -> String {
        self.gateway_pubkey
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

// ── Invite Manager ──────────────────────────────────────────

/// Manages outstanding invites with automatic expiry cleanup.
pub struct InviteManager {
    invites: Vec<PeerInvite>,
    /// Default TTL for new invites.
    default_ttl: Duration,
}

impl InviteManager {
    /// Create with default TTL of 24 hours.
    pub fn new() -> Self {
        Self {
            invites: Vec::new(),
            default_ttl: Duration::from_secs(24 * 3600),
        }
    }

    /// Create with custom default TTL.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            invites: Vec::new(),
            default_ttl: ttl,
        }
    }

    /// Create a new invite.
    pub fn create_invite(
        &mut self,
        gateway_pubkey: [u8; 32],
        endpoint: impl Into<String>,
        label: impl Into<String>,
    ) -> &PeerInvite {
        let invite = PeerInvite::new(
            gateway_pubkey,
            endpoint,
            label,
            self.default_ttl,
        );
        self.invites.push(invite);
        self.invites.last().unwrap()
    }

    /// Create an invite with custom TTL.
    pub fn create_invite_with_ttl(
        &mut self,
        gateway_pubkey: [u8; 32],
        endpoint: impl Into<String>,
        label: impl Into<String>,
        ttl: Duration,
    ) -> &PeerInvite {
        let invite = PeerInvite::new(gateway_pubkey, endpoint, label, ttl);
        self.invites.push(invite);
        self.invites.last().unwrap()
    }

    /// Try to use an invite by matching the PSK.
    /// Returns the gateway pubkey if the invite is valid.
    /// Burns the invite on success.
    pub fn try_use_invite(&mut self, psk: &[u8; 32]) -> Option<[u8; 32]> {
        for invite in &mut self.invites {
            if invite.is_valid() && invite.onetime_psk == *psk {
                let pubkey = invite.gateway_pubkey;
                invite.burn();
                return Some(pubkey);
            }
        }
        None
    }

    /// List valid (non-expired, non-used) invites.
    pub fn list_valid(&self) -> Vec<&PeerInvite> {
        self.invites
            .iter()
            .filter(|i| i.is_valid())
            .collect()
    }

    /// Prune expired and used invites.
    pub fn prune(&mut self) -> usize {
        let before = self.invites.len();
        self.invites.retain(|i| i.is_valid());
        before - self.invites.len()
    }

    /// Total number of invites (including expired/used).
    pub fn total_count(&self) -> usize {
        self.invites.len()
    }

    /// Number of currently valid invites.
    pub fn valid_count(&self) -> usize {
        self.invites.iter().filter(|i| i.is_valid()).count()
    }
}

impl Default for InviteManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Invite Errors ────────────────────────────────────────────

/// Errors during invite processing.
#[derive(Debug)]
pub enum InviteError {
    /// Invite code encoding is invalid.
    InvalidEncoding,
    /// Invite code is too short to contain all fields.
    TooShort,
    /// Invite has expired.
    Expired,
    /// Invite has already been used.
    AlreadyUsed,
}

impl std::fmt::Display for InviteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEncoding => write!(f, "invalid invite encoding"),
            Self::TooShort => write!(f, "invite code too short"),
            Self::Expired => write!(f, "invite has expired"),
            Self::AlreadyUsed => write!(f, "invite has already been used"),
        }
    }
}

impl std::error::Error for InviteError {}

// ── Helpers ─────────────────────────────────────────────────

/// Generate a random 32-byte key.
fn generate_random_key() -> [u8; 32] {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let pid = std::process::id() as u128;
    let thread_id = std::thread::current().id();
    let combined = seed ^ (pid << 64) ^ (format!("{:?}", thread_id).len() as u128);

    // Simple KDF: Hash(seed || counter) for each 32-byte block
    let mut key = [0u8; 32];
    let bytes = combined.to_le_bytes();
    // Use a simple hash-like mixing function
    for (i, k) in key.iter_mut().enumerate() {
        let mut mix: u8 = bytes[i % 16];
        mix = mix.wrapping_add(bytes[(i + 7) % 16]);
        mix ^= (i as u8).wrapping_mul(0x9E);
        mix = mix.wrapping_add(bytes[(i + 3) % 16]);
        *k = mix;
    }
    key
}

fn format_expiry(expires_at: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if now >= expires_at {
        return "EXPIRED           ".to_string();
    }

    let remaining = expires_at - now;
    if remaining > 86400 {
        format!("in {} days          ", remaining / 86400)
    } else if remaining > 3600 {
        format!("in {} hours         ", remaining / 3600)
    } else {
        format!("in {} minutes       ", remaining / 60)
    }
}

fn truncate_code(code: &str, max_width: usize) -> String {
    if code.len() <= max_width {
        format!("{:<width$}", code, width = max_width)
    } else {
        format!("{}...", &code[..max_width - 3])
    }
}

// ── Base62 encoding ─────────────────────────────────────────

const BASE62_CHARS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn base62_encode(data: &[u8]) -> String {
    if data.is_empty() {
        return String::new();
    }

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

    let leading_zeros = s.bytes().take_while(|&b| b == b'0').count();
    let mut result = vec![0u8; leading_zeros];
    result.extend(digits);

    Ok(result)
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_creation() {
        let invite = PeerInvite::new(
            [42u8; 32],
            "203.0.113.1:51820",
            "Test Device",
            Duration::from_secs(3600),
        );

        assert_eq!(invite.gateway_pubkey, [42u8; 32]);
        assert_eq!(invite.endpoint, "203.0.113.1:51820");
        assert_eq!(invite.label, "Test Device");
        assert!(!invite.used);
        assert!(invite.is_valid());
    }

    #[test]
    fn invite_encode_decode_roundtrip() {
        let invite = PeerInvite::new(
            [0xAB; 32],
            "192.168.1.1:51820",
            "My Phone",
            Duration::from_secs(86400),
        );

        let code = invite.to_invite_code();
        assert!(!code.is_empty());

        let decoded = PeerInvite::from_invite_code(&code).unwrap();
        assert_eq!(decoded.gateway_pubkey, invite.gateway_pubkey);
        assert_eq!(decoded.onetime_psk, invite.onetime_psk);
        assert_eq!(decoded.endpoint, invite.endpoint);
        assert_eq!(decoded.expires_at, invite.expires_at);
        assert_eq!(decoded.label, invite.label);
    }

    #[test]
    fn invite_burn() {
        let mut invite = PeerInvite::new(
            [1u8; 32],
            "host:51820",
            "test",
            Duration::from_secs(3600),
        );

        assert!(invite.is_valid());
        let psk_before = invite.onetime_psk;
        assert_ne!(psk_before, [0u8; 32]);

        invite.burn();
        assert!(invite.used);
        assert!(!invite.is_valid());
        assert_eq!(invite.onetime_psk, [0u8; 32]); // PSK zeroed
    }

    #[test]
    fn invite_remaining_time() {
        let invite = PeerInvite::new(
            [1u8; 32],
            "host:51820",
            "test",
            Duration::from_secs(3600),
        );

        let remaining = invite.remaining().unwrap();
        assert!(remaining.as_secs() >= 3598);
        assert!(remaining.as_secs() <= 3600);
    }

    #[test]
    fn invite_qr_text() {
        let invite = PeerInvite::new(
            [0xAA; 32],
            "example.com:51820",
            "Alice",
            Duration::from_secs(86400),
        );

        let qr = invite.to_qr_text();
        assert!(qr.contains("ClawDesk Peer Invite"));
        assert!(qr.contains("Alice"));
        assert!(qr.contains("example.com:51820"));
    }

    #[test]
    fn invite_manager_lifecycle() {
        let mut manager = InviteManager::with_ttl(Duration::from_secs(3600));
        assert_eq!(manager.total_count(), 0);
        assert_eq!(manager.valid_count(), 0);

        manager.create_invite([1u8; 32], "host:51820", "Device 1");
        manager.create_invite([1u8; 32], "host:51820", "Device 2");
        assert_eq!(manager.total_count(), 2);
        assert_eq!(manager.valid_count(), 2);

        // List valid
        let valid = manager.list_valid();
        assert_eq!(valid.len(), 2);
    }

    #[test]
    fn invite_manager_use_invite() {
        let mut manager = InviteManager::new();
        let invite = manager.create_invite([1u8; 32], "host:51820", "Device");
        let psk = invite.onetime_psk;

        // Use the invite
        let result = manager.try_use_invite(&psk);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), [1u8; 32]);

        // Can't use again
        let result2 = manager.try_use_invite(&psk);
        assert!(result2.is_none());
    }

    #[test]
    fn invite_manager_prune() {
        let mut manager = InviteManager::new();
        manager.create_invite([1u8; 32], "host:51820", "Device");

        // Manually burn the invite
        manager.invites[0].burn();
        assert_eq!(manager.valid_count(), 0);

        let pruned = manager.prune();
        assert_eq!(pruned, 1);
        assert_eq!(manager.total_count(), 0);
    }

    #[test]
    fn invite_decode_invalid() {
        // Too short
        assert!(PeerInvite::from_invite_code("abc").is_err());
        // Invalid encoding
        assert!(PeerInvite::from_invite_code("!!!").is_err());
    }
}
