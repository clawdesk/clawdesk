//! NAT traversal — STUN endpoint discovery and UDP hole-punching.
//!
//! Most consumer ClawDesk instances will be behind NAT. WireGuard handles
//! this natively via UDP, but peers need to discover each other's public
//! endpoint first.
//!
//! # Approach
//!
//! 1. **STUN endpoint discovery**: Send a STUN Binding Request to a public
//!    STUN server. The server responds with our public IP:port as seen from
//!    the internet. Total: 60 bytes on the wire, 1 round-trip (~50ms).
//!
//! 2. **NAT type classification**: Determines which hole-punching strategies
//!    will work. Full cone → always works. Symmetric → may need relay.
//!
//! 3. **Relay fallback**: For symmetric NAT (common on mobile carriers),
//!    use a lightweight relay that only forwards encrypted WireGuard packets.
//!    The relay cannot see plaintext. Bandwidth: ~5KB/s per active session.
//!
//! # Performance
//!
//! STUN Binding Request: 20 bytes. Response: ~40 bytes.
//! Discovery latency: ~50ms to nearby STUN server.
//! Zero overhead after discovery (direct peer-to-peer).

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;
use tracing::{info, warn};

// ── NAT Strategy ─────────────────────────────────────────────

/// NAT traversal strategies, ordered by preference.
///
/// The tunnel manager tries each strategy in order until one succeeds.
/// Direct is always preferred (no overhead). STUN punch adds ~50ms of
/// initial latency but zero steady-state overhead. Relay adds ~20ms
/// per packet but works through all NAT types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NatStrategy {
    /// Direct connection (both peers have public IPs or are on the same LAN).
    Direct,

    /// UDP hole-punching via STUN-discovered endpoints.
    StunPunch {
        stun_servers: Vec<String>,
    },

    /// Relay via TURN server (last resort, adds ~20ms latency).
    /// The relay only forwards encrypted WireGuard packets — it cannot
    /// see the plaintext.
    Relay {
        relay_url: String,
    },
}

impl Default for NatStrategy {
    fn default() -> Self {
        Self::StunPunch {
            stun_servers: vec![
                "stun.l.google.com:19302".to_string(),
                "stun1.l.google.com:19302".to_string(),
                "stun.cloudflare.com:3478".to_string(),
            ],
        }
    }
}

// ── NAT Type Classification ─────────────────────────────────

/// NAT type classification (simplified RFC 3489).
///
/// Determines which hole-punching strategies will work for this network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NatType {
    /// No NAT (public IP). Direct connection always works.
    OpenInternet,
    /// Full cone NAT. Any external host can send to the mapped port.
    /// Hole-punching always works.
    FullCone,
    /// Restricted cone NAT. Only hosts the internal sent to can reply.
    /// Hole-punching works after initial outbound packet.
    RestrictedCone,
    /// Port-restricted cone NAT. Reply must come from same IP:port.
    /// Hole-punching works with coordination.
    PortRestricted,
    /// Symmetric NAT. Different mapping per destination. Hole-punching
    /// may fail — relay is the reliable fallback.
    Symmetric,
    /// Could not determine NAT type (STUN failed or timed out).
    Unknown,
}

impl NatType {
    /// Whether UDP hole-punching is expected to work.
    pub fn hole_punch_likely(&self) -> bool {
        matches!(
            self,
            Self::OpenInternet | Self::FullCone | Self::RestrictedCone | Self::PortRestricted
        )
    }

    /// Whether a relay may be needed.
    pub fn may_need_relay(&self) -> bool {
        matches!(self, Self::Symmetric | Self::Unknown)
    }
}

impl std::fmt::Display for NatType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenInternet => write!(f, "open internet"),
            Self::FullCone => write!(f, "full cone NAT"),
            Self::RestrictedCone => write!(f, "restricted cone NAT"),
            Self::PortRestricted => write!(f, "port-restricted cone NAT"),
            Self::Symmetric => write!(f, "symmetric NAT"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

// ── STUN Protocol ────────────────────────────────────────────

/// Magic cookie as defined in RFC 5389.
const STUN_MAGIC_COOKIE: u32 = 0x2112A442;

/// STUN message types.
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_RESPONSE: u16 = 0x0101;

/// STUN attribute types.
const STUN_ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const STUN_ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Build a minimal STUN Binding Request (20 bytes).
///
/// Format:
/// - 2 bytes: message type (0x0001 = Binding Request)
/// - 2 bytes: message length (0 for basic request)
/// - 4 bytes: magic cookie (0x2112A442)
/// - 12 bytes: transaction ID (random)
fn build_stun_request(txn_id: &[u8; 12]) -> [u8; 20] {
    let mut buf = [0u8; 20];
    buf[0..2].copy_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    buf[2..4].copy_from_slice(&0u16.to_be_bytes()); // length = 0
    buf[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    buf[8..20].copy_from_slice(txn_id);
    buf
}

/// Parse a STUN Binding Response to extract the XOR-MAPPED-ADDRESS.
///
/// Returns `Some((ip, port))` if successful, `None` if the response
/// is malformed or doesn't contain the expected attribute.
fn parse_stun_response(
    response: &[u8],
    txn_id: &[u8; 12],
) -> Option<(IpAddr, u16)> {
    if response.len() < 20 {
        return None;
    }

    // Check message type
    let msg_type = u16::from_be_bytes([response[0], response[1]]);
    if msg_type != STUN_BINDING_RESPONSE {
        return None;
    }

    // Check magic cookie
    let cookie = u32::from_be_bytes([response[4], response[5], response[6], response[7]]);
    if cookie != STUN_MAGIC_COOKIE {
        return None;
    }

    // Check transaction ID
    if &response[8..20] != txn_id {
        return None;
    }

    let msg_len = u16::from_be_bytes([response[2], response[3]]) as usize;
    if response.len() < 20 + msg_len {
        return None;
    }

    // Parse attributes
    let mut offset = 20;
    while offset + 4 <= 20 + msg_len {
        let attr_type = u16::from_be_bytes([response[offset], response[offset + 1]]);
        let attr_len =
            u16::from_be_bytes([response[offset + 2], response[offset + 3]]) as usize;

        if offset + 4 + attr_len > response.len() {
            break;
        }

        let attr_data = &response[offset + 4..offset + 4 + attr_len];

        if attr_type == STUN_ATTR_XOR_MAPPED_ADDRESS && attr_len >= 8 {
            return parse_xor_mapped_address(attr_data, txn_id);
        }

        if attr_type == STUN_ATTR_MAPPED_ADDRESS && attr_len >= 8 {
            return parse_mapped_address(attr_data);
        }

        // Advance to next attribute (4-byte aligned)
        offset += 4 + ((attr_len + 3) & !3);
    }

    None
}

/// Parse XOR-MAPPED-ADDRESS attribute.
fn parse_xor_mapped_address(data: &[u8], _txn_id: &[u8; 12]) -> Option<(IpAddr, u16)> {
    if data.len() < 8 {
        return None;
    }

    let family = data[1];
    let xor_port = u16::from_be_bytes([data[2], data[3]]);
    let port = xor_port ^ (STUN_MAGIC_COOKIE >> 16) as u16;

    match family {
        0x01 => {
            // IPv4
            let xor_ip = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            let ip = xor_ip ^ STUN_MAGIC_COOKIE;
            let octets = ip.to_be_bytes();
            Some((IpAddr::V4(std::net::Ipv4Addr::new(
                octets[0], octets[1], octets[2], octets[3],
            )), port))
        }
        0x02 => {
            // IPv6 — need 16 bytes for address
            if data.len() < 20 {
                return None;
            }
            // XOR with magic cookie + transaction ID
            let mut ip_bytes = [0u8; 16];
            let cookie_bytes = STUN_MAGIC_COOKIE.to_be_bytes();
            for i in 0..4 {
                ip_bytes[i] = data[4 + i] ^ cookie_bytes[i];
            }
            for i in 0..12 {
                ip_bytes[4 + i] = data[8 + i] ^ _txn_id[i];
            }
            let ip = std::net::Ipv6Addr::from(ip_bytes);
            Some((IpAddr::V6(ip), port))
        }
        _ => None,
    }
}

/// Parse MAPPED-ADDRESS attribute (non-XOR, RFC 3489 compat).
fn parse_mapped_address(data: &[u8]) -> Option<(IpAddr, u16)> {
    if data.len() < 8 {
        return None;
    }

    let family = data[1];
    let port = u16::from_be_bytes([data[2], data[3]]);

    match family {
        0x01 => {
            // IPv4
            let ip = std::net::Ipv4Addr::new(data[4], data[5], data[6], data[7]);
            Some((IpAddr::V4(ip), port))
        }
        _ => None,
    }
}

// ── Endpoint Discovery ──────────────────────────────────────

/// Result of endpoint discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredEndpoint {
    /// Our public IP as seen from the STUN server.
    pub public_ip: IpAddr,
    /// Our public port as seen from the STUN server.
    pub public_port: u16,
    /// The STUN server that responded.
    pub stun_server: String,
    /// Detected NAT type.
    pub nat_type: NatType,
    /// Round-trip time to the STUN server.
    pub rtt_ms: u64,
}

/// Discover our public endpoint via STUN.
///
/// Sends a STUN Binding Request to the specified server and parses
/// the response to determine our public IP:port and NAT type.
///
/// # Performance
///
/// - Request: 20 bytes
/// - Response: ~40 bytes
/// - Total wire: 60 bytes
/// - Latency: ~50ms to nearby STUN server
pub async fn discover_endpoint(
    stun_server: &str,
    local_socket: &UdpSocket,
    timeout: Duration,
) -> Result<DiscoveredEndpoint, NatError> {
    // Generate random transaction ID
    let mut txn_id = [0u8; 12];
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    for (i, byte) in txn_id.iter_mut().enumerate() {
        *byte = ((seed >> (i * 8)) & 0xFF) as u8;
    }

    let request = build_stun_request(&txn_id);

    // Resolve STUN server address
    let server_addr: SocketAddr = tokio::net::lookup_host(stun_server)
        .await
        .map_err(|e| NatError::DnsResolution(e.to_string()))?
        .next()
        .ok_or(NatError::DnsResolution("no addresses found".into()))?;

    let start = std::time::Instant::now();

    // Send STUN request
    local_socket
        .send_to(&request, server_addr)
        .await
        .map_err(|e| NatError::SendFailed(e.to_string()))?;

    // Wait for response with timeout
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(timeout, local_socket.recv(&mut buf))
        .await
        .map_err(|_| NatError::Timeout)?
        .map_err(|e| NatError::RecvFailed(e.to_string()))?;

    let rtt = start.elapsed();

    let (ip, port) = parse_stun_response(&buf[..n], &txn_id)
        .ok_or(NatError::InvalidResponse)?;

    let local_addr = local_socket.local_addr().map_err(|e| NatError::RecvFailed(e.to_string()))?;

    // Simple NAT type inference
    let nat_type = if ip == local_addr.ip() && port == local_addr.port() {
        NatType::OpenInternet
    } else if port == local_addr.port() {
        NatType::FullCone // Same port, different IP → full cone likely
    } else {
        NatType::PortRestricted // Different port → at least port-restricted
    };

    info!(
        public_ip = %ip,
        public_port = port,
        nat_type = %nat_type,
        rtt_ms = rtt.as_millis(),
        stun_server = stun_server,
        "endpoint discovered"
    );

    Ok(DiscoveredEndpoint {
        public_ip: ip,
        public_port: port,
        stun_server: stun_server.to_string(),
        nat_type,
        rtt_ms: rtt.as_millis() as u64,
    })
}

/// Try multiple STUN servers until one responds.
///
/// Returns the first successful result.
pub async fn discover_endpoint_multi(
    stun_servers: &[String],
    local_socket: &UdpSocket,
    timeout: Duration,
) -> Result<DiscoveredEndpoint, NatError> {
    for server in stun_servers {
        match discover_endpoint(server, local_socket, timeout).await {
            Ok(endpoint) => return Ok(endpoint),
            Err(e) => {
                warn!(stun_server = %server, error = %e, "STUN server failed, trying next");
            }
        }
    }
    Err(NatError::AllServersFailed)
}

// ── NAT Errors ──────────────────────────────────────────────

/// Errors during NAT traversal.
#[derive(Debug)]
pub enum NatError {
    /// DNS resolution failed.
    DnsResolution(String),
    /// Failed to send STUN request.
    SendFailed(String),
    /// Failed to receive STUN response.
    RecvFailed(String),
    /// STUN server did not respond within timeout.
    Timeout,
    /// STUN response was malformed.
    InvalidResponse,
    /// All STUN servers failed.
    AllServersFailed,
}

impl std::fmt::Display for NatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DnsResolution(msg) => write!(f, "DNS resolution failed: {}", msg),
            Self::SendFailed(msg) => write!(f, "STUN send failed: {}", msg),
            Self::RecvFailed(msg) => write!(f, "STUN recv failed: {}", msg),
            Self::Timeout => write!(f, "STUN server timed out"),
            Self::InvalidResponse => write!(f, "invalid STUN response"),
            Self::AllServersFailed => write!(f, "all STUN servers failed"),
        }
    }
}

impl std::error::Error for NatError {}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_stun_request_format() {
        let txn_id = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let req = build_stun_request(&txn_id);

        assert_eq!(req.len(), 20);
        // Message type: Binding Request (0x0001)
        assert_eq!(req[0], 0x00);
        assert_eq!(req[1], 0x01);
        // Message length: 0
        assert_eq!(req[2], 0x00);
        assert_eq!(req[3], 0x00);
        // Magic cookie: 0x2112A442
        assert_eq!(req[4], 0x21);
        assert_eq!(req[5], 0x12);
        assert_eq!(req[6], 0xA4);
        assert_eq!(req[7], 0x42);
        // Transaction ID
        assert_eq!(&req[8..20], &txn_id);
    }

    #[test]
    fn parse_stun_response_xor_mapped() {
        let txn_id = [0; 12];

        // Build a synthetic STUN response with XOR-MAPPED-ADDRESS
        let mut resp = vec![0u8; 32];
        // Message type: Binding Response (0x0101)
        resp[0] = 0x01;
        resp[1] = 0x01;
        // Message length: 12 bytes
        resp[2] = 0x00;
        resp[3] = 0x0C;
        // Magic cookie
        resp[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        // Transaction ID
        resp[8..20].copy_from_slice(&txn_id);

        // XOR-MAPPED-ADDRESS attribute
        // Type: 0x0020
        resp[20] = 0x00;
        resp[21] = 0x20;
        // Length: 8
        resp[22] = 0x00;
        resp[23] = 0x08;
        // Reserved + Family (IPv4 = 0x01)
        resp[24] = 0x00;
        resp[25] = 0x01;

        // XOR port: 5000 XOR (magic >> 16) = 5000 XOR 0x2112 = 0x1706 XOR 0x2112
        let port: u16 = 5000;
        let xor_port = port ^ (STUN_MAGIC_COOKIE >> 16) as u16;
        resp[26..28].copy_from_slice(&xor_port.to_be_bytes());

        // XOR IP: 203.0.113.1 = 0xCB007101 XOR 0x2112A442
        let ip_u32: u32 = u32::from_be_bytes([203, 0, 113, 1]);
        let xor_ip = ip_u32 ^ STUN_MAGIC_COOKIE;
        resp[28..32].copy_from_slice(&xor_ip.to_be_bytes());

        let result = parse_stun_response(&resp, &txn_id);
        assert!(result.is_some());
        let (ip, parsed_port) = result.unwrap();
        assert_eq!(parsed_port, 5000);
        assert_eq!(ip, IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 1)));
    }

    #[test]
    fn parse_stun_response_too_short() {
        let txn_id = [0; 12];
        let result = parse_stun_response(&[0; 10], &txn_id);
        assert!(result.is_none());
    }

    #[test]
    fn parse_stun_response_wrong_type() {
        let txn_id = [0; 12];
        let mut resp = [0u8; 20];
        resp[0] = 0x01;
        resp[1] = 0x11; // Error response
        let result = parse_stun_response(&resp, &txn_id);
        assert!(result.is_none());
    }

    #[test]
    fn nat_type_hole_punch() {
        assert!(NatType::OpenInternet.hole_punch_likely());
        assert!(NatType::FullCone.hole_punch_likely());
        assert!(NatType::RestrictedCone.hole_punch_likely());
        assert!(NatType::PortRestricted.hole_punch_likely());
        assert!(!NatType::Symmetric.hole_punch_likely());
        assert!(!NatType::Unknown.hole_punch_likely());
    }

    #[test]
    fn nat_type_relay_needed() {
        assert!(!NatType::OpenInternet.may_need_relay());
        assert!(!NatType::FullCone.may_need_relay());
        assert!(NatType::Symmetric.may_need_relay());
        assert!(NatType::Unknown.may_need_relay());
    }

    #[test]
    fn nat_strategy_default() {
        let strategy = NatStrategy::default();
        match strategy {
            NatStrategy::StunPunch { stun_servers } => {
                assert!(!stun_servers.is_empty());
                assert!(stun_servers[0].contains("stun"));
            }
            _ => panic!("default should be StunPunch"),
        }
    }
}
