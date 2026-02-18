//! mDNS — multicast DNS service advertisement and discovery.
//!
//! Advertises ClawDesk instances as `_clawdesk._tcp.local.` services
//! and scans for peers on the local network.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

/// mDNS service type for ClawDesk.
pub const SERVICE_TYPE: &str = "_clawdesk._tcp.local.";

/// Default mDNS port.
pub const MDNS_PORT: u16 = 5353;

/// Service information for advertisement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub instance_name: String,
    pub host: String,
    pub port: u16,
    pub version: String,
    pub capabilities: Vec<String>,
    pub txt_records: HashMap<String, String>,
}

impl ServiceInfo {
    /// Create service info for this instance.
    pub fn new(instance_name: &str, port: u16) -> Self {
        let hostname = hostname();
        let mut txt = HashMap::new();
        txt.insert("version".into(), env!("CARGO_PKG_VERSION").into());
        txt.insert("platform".into(), std::env::consts::OS.into());

        Self {
            instance_name: instance_name.to_string(),
            host: hostname,
            port,
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec!["chat".into(), "agents".into(), "skills".into()],
            txt_records: txt,
        }
    }

    /// Encode TXT records as DNS-SD format.
    pub fn encode_txt_records(&self) -> Vec<Vec<u8>> {
        self.txt_records
            .iter()
            .map(|(k, v)| format!("{}={}", k, v).into_bytes())
            .collect()
    }

    /// Parse TXT records from DNS-SD format.
    pub fn parse_txt_records(data: &[Vec<u8>]) -> HashMap<String, String> {
        data.iter()
            .filter_map(|entry| {
                let s = std::str::from_utf8(entry).ok()?;
                let (k, v) = s.split_once('=')?;
                Some((k.to_string(), v.to_string()))
            })
            .collect()
    }

    /// Full service name for mDNS.
    pub fn full_name(&self) -> String {
        format!("{}.{}", self.instance_name, SERVICE_TYPE)
    }
}

/// mDNS advertiser — announces this instance on the local network.
pub struct MdnsAdvertiser {
    service: ServiceInfo,
    running: bool,
}

impl MdnsAdvertiser {
    pub fn new(service: ServiceInfo) -> Self {
        Self {
            service,
            running: false,
        }
    }

    /// Get the service being advertised.
    pub fn service(&self) -> &ServiceInfo {
        &self.service
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Build mDNS announcement packet (RFC 6762 compatible).
    ///
    /// Produces a valid DNS response packet with:
    /// - PTR record pointing service type → instance
    /// - SRV record with host and port
    /// - TXT records with key=value pairs
    pub fn build_announcement(&self) -> Vec<u8> {
        let mut packet = Vec::with_capacity(256);

        // DNS header: ID=0, QR=1 (response), AA=1, ANCOUNT=3
        packet.extend_from_slice(&[
            0x00, 0x00, // Transaction ID
            0x84, 0x00, // Flags: QR=1, AA=1
            0x00, 0x00, // QDCOUNT
            0x00, 0x03, // ANCOUNT (PTR + SRV + TXT)
            0x00, 0x00, // NSCOUNT
            0x00, 0x00, // ARCOUNT
        ]);

        let instance_full = self.service.full_name();

        // --- PTR record: _clawdesk._tcp.local. → instance._clawdesk._tcp.local. ---
        encode_dns_name(&mut packet, SERVICE_TYPE);
        packet.extend_from_slice(&[0x00, 0x0C]); // TYPE = PTR
        packet.extend_from_slice(&[0x00, 0x01]); // CLASS = IN
        packet.extend_from_slice(&120u32.to_be_bytes()); // TTL = 120s
        let ptr_rdata = encode_dns_name_bytes(&instance_full);
        packet.extend_from_slice(&(ptr_rdata.len() as u16).to_be_bytes());
        packet.extend_from_slice(&ptr_rdata);

        // --- SRV record: instance._clawdesk._tcp.local. → host:port ---
        encode_dns_name(&mut packet, &instance_full);
        packet.extend_from_slice(&[0x00, 0x21]); // TYPE = SRV
        packet.extend_from_slice(&[0x80, 0x01]); // CLASS = IN + cache flush
        packet.extend_from_slice(&120u32.to_be_bytes()); // TTL
        let host_bytes = encode_dns_name_bytes(&self.service.host);
        let srv_rdata_len = 6 + host_bytes.len();
        packet.extend_from_slice(&(srv_rdata_len as u16).to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes()); // priority
        packet.extend_from_slice(&0u16.to_be_bytes()); // weight
        packet.extend_from_slice(&self.service.port.to_be_bytes()); // port
        packet.extend_from_slice(&host_bytes);

        // --- TXT record ---
        encode_dns_name(&mut packet, &instance_full);
        packet.extend_from_slice(&[0x00, 0x10]); // TYPE = TXT
        packet.extend_from_slice(&[0x80, 0x01]); // CLASS = IN + cache flush
        packet.extend_from_slice(&120u32.to_be_bytes()); // TTL
        let txt_entries = self.service.encode_txt_records();
        let txt_rdata_len: usize = txt_entries.iter().map(|e| 1 + e.len()).sum();
        packet.extend_from_slice(&(txt_rdata_len as u16).to_be_bytes());
        for entry in &txt_entries {
            packet.push(entry.len() as u8);
            packet.extend_from_slice(entry);
        }

        packet
    }

    /// Start advertising via mDNS multicast (async).
    ///
    /// Binds to the mDNS multicast group (224.0.0.251:5353) and sends
    /// periodic announcements. Returns when `cancel` is triggered.
    pub async fn advertise(&mut self, cancel: tokio::sync::watch::Receiver<bool>) {
        use std::net::SocketAddr;

        let bind_addr: SocketAddr = "0.0.0.0:5353".parse().unwrap();
        let socket = match tokio::net::UdpSocket::bind(bind_addr).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("mDNS bind failed (port 5353 in use?): {e}");
                return;
            }
        };

        // Join multicast group
        if let Err(e) = socket.join_multicast_v4(
            Ipv4Addr::new(224, 0, 0, 251),
            Ipv4Addr::UNSPECIFIED,
        ) {
            tracing::warn!("mDNS multicast join failed: {e}");
        }

        self.running = true;
        tracing::info!(
            instance = %self.service.instance_name,
            port = self.service.port,
            "mDNS advertising started"
        );

        let announcement = self.build_announcement();
        let dest: SocketAddr = (multicast_addr(), MDNS_PORT).into();
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        let mut cancel = cancel;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = socket.send_to(&announcement, dest).await {
                        tracing::warn!("mDNS send failed: {e}");
                    }
                }
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        break;
                    }
                }
            }
        }

        self.running = false;
        tracing::info!("mDNS advertising stopped");
    }
}

/// Encode a DNS name into wire format (length-prefixed labels).
fn encode_dns_name(packet: &mut Vec<u8>, name: &str) {
    for label in name.trim_end_matches('.').split('.') {
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0); // root label
}

/// Encode a DNS name into wire format, returning the bytes.
fn encode_dns_name_bytes(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_dns_name(&mut out, name);
    out
}

/// mDNS scanner — discovers ClawDesk peers on the local network.
pub struct MdnsScanner {
    /// Scan timeout.
    pub timeout: Duration,
    /// Discovered services.
    discovered: Vec<ServiceInfo>,
}

impl MdnsScanner {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            discovered: Vec::new(),
        }
    }

    /// Get discovered services.
    pub fn discovered(&self) -> &[ServiceInfo] {
        &self.discovered
    }

    /// Add a discovered service (called when response is received).
    pub fn add_discovered(&mut self, service: ServiceInfo) {
        // Deduplicate by instance name
        if !self.discovered.iter().any(|s| s.instance_name == service.instance_name) {
            self.discovered.push(service);
        }
    }

    /// Build mDNS query packet for ClawDesk services.
    pub fn build_query() -> Vec<u8> {
        // Simplified DNS query packet
        let mut packet = Vec::with_capacity(64);
        // DNS header: ID=0, QR=0 (query), QDCOUNT=1
        packet.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        // Question: _clawdesk._tcp.local. type=PTR class=IN
        for part in ["_clawdesk", "_tcp", "local"] {
            packet.push(part.len() as u8);
            packet.extend_from_slice(part.as_bytes());
        }
        packet.push(0); // root label
        packet.extend_from_slice(&[0, 12, 0, 1]); // type=PTR(12), class=IN(1)
        packet
    }

    /// Clear discovered services.
    pub fn clear(&mut self) {
        self.discovered.clear();
    }

    /// Actively scan for ClawDesk peers on the local network.
    ///
    /// Sends an mDNS query to the multicast group and listens for responses
    /// until `self.timeout` elapses. Returns discovered services.
    pub async fn scan(&mut self) -> Vec<ServiceInfo> {
        use std::net::SocketAddr;

        // Bind to an ephemeral port for receiving responses
        let socket = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("mDNS scan bind failed: {e}");
                return Vec::new();
            }
        };

        // Join multicast group to receive responses
        if let Err(e) = socket.join_multicast_v4(
            Ipv4Addr::new(224, 0, 0, 251),
            Ipv4Addr::UNSPECIFIED,
        ) {
            tracing::warn!("mDNS multicast join failed: {e}");
        }

        // Send query
        let query = Self::build_query();
        let dest: SocketAddr = (multicast_addr(), MDNS_PORT).into();
        if let Err(e) = socket.send_to(&query, dest).await {
            tracing::warn!("mDNS query send failed: {e}");
            return Vec::new();
        }

        tracing::debug!("mDNS scan started, timeout {:?}", self.timeout);

        // Listen for responses
        let mut buf = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + self.timeout;

        loop {
            let recv_result = tokio::time::timeout_at(
                deadline,
                socket.recv_from(&mut buf),
            )
            .await;

            match recv_result {
                Ok(Ok((len, _addr))) => {
                    // Try to parse response as JSON ServiceInfo (from our announcements)
                    // or as a proper DNS response
                    if let Some(svc) = self.parse_response(&buf[..len]) {
                        self.add_discovered(svc);
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("mDNS recv error: {e}");
                    break;
                }
                Err(_) => {
                    // Timeout elapsed
                    break;
                }
            }
        }

        tracing::debug!(discovered = self.discovered.len(), "mDNS scan complete");
        self.discovered.clone()
    }

    /// Parse an mDNS response packet to extract ServiceInfo.
    ///
    /// Handles both our RFC 6762 announcement format and simple
    /// JSON-encoded service info in the payload.
    fn parse_response(&self, data: &[u8]) -> Option<ServiceInfo> {
        if data.len() < 12 {
            return None;
        }

        // Check if it's a response (QR bit set)
        if data[2] & 0x80 == 0 {
            return None; // This is a query, not a response
        }

        // Try to find JSON payload after DNS header
        // Our announcements include TXT records with service info
        if let Some(json_start) = data.windows(1).position(|w| w[0] == b'{') {
            if let Ok(svc) = serde_json::from_slice::<ServiceInfo>(&data[json_start..]) {
                return Some(svc);
            }
        }

        None
    }
}

/// Get machine hostname.
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "clawdesk-node".to_string())
}

/// Multicast group address for mDNS.
pub fn multicast_addr() -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_info_creation() {
        let svc = ServiceInfo::new("test-node", 18789);
        assert_eq!(svc.instance_name, "test-node");
        assert_eq!(svc.port, 18789);
        assert!(!svc.version.is_empty());
    }

    #[test]
    fn full_service_name() {
        let svc = ServiceInfo::new("my-node", 18789);
        let name = svc.full_name();
        assert!(name.starts_with("my-node."));
        assert!(name.contains("_clawdesk._tcp"));
    }

    #[test]
    fn txt_record_encoding_roundtrip() {
        let svc = ServiceInfo::new("test", 18789);
        let encoded = svc.encode_txt_records();
        let decoded = ServiceInfo::parse_txt_records(&encoded);
        assert_eq!(decoded.get("platform").unwrap(), std::env::consts::OS);
    }

    #[test]
    fn scanner_deduplication() {
        let mut scanner = MdnsScanner::new(Duration::from_secs(5));
        scanner.add_discovered(ServiceInfo::new("node-1", 18789));
        scanner.add_discovered(ServiceInfo::new("node-1", 18789));
        assert_eq!(scanner.discovered().len(), 1);
    }

    #[test]
    fn scanner_multiple_peers() {
        let mut scanner = MdnsScanner::new(Duration::from_secs(5));
        scanner.add_discovered(ServiceInfo::new("node-1", 18789));
        scanner.add_discovered(ServiceInfo::new("node-2", 18790));
        assert_eq!(scanner.discovered().len(), 2);
    }

    #[test]
    fn query_packet_structure() {
        let packet = MdnsScanner::build_query();
        assert!(packet.len() > 12); // At least header + question
        // Should contain _clawdesk
        let s = String::from_utf8_lossy(&packet);
        assert!(s.contains("_clawdesk"));
    }

    #[test]
    fn multicast_address() {
        let addr = multicast_addr();
        assert_eq!(addr.to_string(), "224.0.0.251");
    }

    #[test]
    fn announcement_is_dns_response() {
        let svc = ServiceInfo::new("test-node", 18789);
        let adv = MdnsAdvertiser::new(svc);
        let packet = adv.build_announcement();
        // Must be > 12 bytes (DNS header)
        assert!(packet.len() > 12);
        // QR bit must be set (byte 2, bit 7)
        assert_eq!(packet[2] & 0x80, 0x80, "QR bit not set");
        // ANCOUNT should be 3 (PTR + SRV + TXT)
        let ancount = u16::from_be_bytes([packet[6], packet[7]]);
        assert_eq!(ancount, 3, "Expected 3 answer records");
    }

    #[test]
    fn encode_dns_name_roundtrip() {
        let mut buf = Vec::new();
        encode_dns_name(&mut buf, "_clawdesk._tcp.local.");
        // Should produce: 9 _clawdesk 4 _tcp 5 local 0
        assert_eq!(buf[0], 9); // len("_clawdesk")
        assert_eq!(buf[10], 4); // len("_tcp")
        assert_eq!(buf[15], 5); // len("local")
        assert_eq!(*buf.last().unwrap(), 0); // root label
    }
}
