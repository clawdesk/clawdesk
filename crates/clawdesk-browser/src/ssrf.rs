//! SSRF Protection — URL validation, IP blocklist, DNS rebinding defense.
//!
//! Defense layers:
//! 1. Protocol allowlist (http/https only)
//! 2. Port restriction (80, 443, 8080, 8443 default)
//! 3. DNS resolution → IP blocklist (RFC 1918, link-local, loopback, metadata)
//! 4. DNS pinning — resolve once, connect to pinned IP (closes TOCTOU gap)
//! 5. Redirect chain validation — re-validate every redirect target
//! 6. Response size + timeout limits

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

/// Validate a URL against SSRF attacks before browser navigation.
///
/// Returns `Ok(())` if URL is safe to navigate, `Err` with reason if blocked.
pub fn check_ssrf(url: &str, config: &super::manager::BrowserConfig) -> Result<(), String> {
    // Parse URL manually without the `url` crate — we only need scheme, host, port
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("invalid URL: missing scheme in '{}'", url))?;

    // Layer 1: Protocol allowlist
    match scheme {
        "http" | "https" => {}
        _ => return Err(format!("blocked scheme '{}' — only http/https allowed", scheme)),
    }

    // Extract host and port from the rest
    // Strip path: "host:port/path..." → "host:port"
    let authority = rest.split('/').next().unwrap_or(rest);
    // Strip userinfo: "user:pass@host" → "host"
    let host_port = if let Some((_userinfo, hp)) = authority.rsplit_once('@') {
        hp
    } else {
        authority
    };

    // Split host and port, handling IPv6 bracket notation [::1]:port
    let (host, port) = if host_port.starts_with('[') {
        // IPv6: [::1]:port or [::1]
        if let Some(bracket_end) = host_port.find(']') {
            let h = &host_port[1..bracket_end];
            let p = host_port.get(bracket_end + 1..).and_then(|s| {
                s.strip_prefix(':')
                    .and_then(|p| p.parse::<u16>().ok())
            });
            (h, p)
        } else {
            return Err("invalid IPv6 address in URL".to_string());
        }
    } else if let Some((h, p)) = host_port.rsplit_once(':') {
        // Only treat as port if it actually parses as a number
        if let Ok(port) = p.parse::<u16>() {
            (h, Some(port))
        } else {
            (host_port, None)
        }
    } else {
        (host_port, None)
    };

    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });

    if host.is_empty() {
        return Err("URL has no host".to_string());
    }

    // Layer 2: Port restriction
    if !config.allowed_ports.contains(&port) {
        return Err(format!(
            "blocked port {} — allowed: {:?}",
            port, config.allowed_ports
        ));
    }

    // Allow explicitly allowlisted private hosts
    if config.ssrf_allow_private.iter().any(|h| h == host) {
        return Ok(());
    }

    // Layer 3: DNS resolution → IP blocklist
    let addr_str = format!("{}:{}", host, port);
    let addrs: Vec<_> = addr_str
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution failed for '{}': {}", host, e))?
        .collect();

    if addrs.is_empty() {
        return Err(format!(
            "DNS resolution returned no addresses for '{}'",
            host
        ));
    }

    for addr in &addrs {
        if is_blocked_ip(&addr.ip()) {
            return Err(format!(
                "blocked: '{}' resolves to private/reserved IP {}",
                host,
                addr.ip()
            ));
        }
    }

    Ok(())
}

fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

fn is_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    ip.is_loopback()                                                    // 127.0.0.0/8
    || ip.is_private()                                                  // 10/8, 172.16/12, 192.168/16
    || ip.is_link_local()                                               // 169.254.0.0/16
    || ip.is_broadcast()                                                // 255.255.255.255
    || ip.is_unspecified()                                              // 0.0.0.0
    || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xC0) == 64)        // 100.64.0.0/10 (CGN)
    || *ip == Ipv4Addr::new(169, 254, 169, 254)                        // AWS/GCP/Azure metadata
}

fn is_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    ip.is_loopback()                                                    // ::1
    || ip.is_unspecified()                                              // ::
    || (ip.segments()[0] & 0xFE00) == 0xFC00                           // ULA: fc00::/7
    || (ip.segments()[0] & 0xFFC0) == 0xFE80                           // Link-local: fe80::/10
    || ip.to_ipv4_mapped().map(|v4| is_blocked_ipv4(&v4)).unwrap_or(false) // IPv4-mapped
}

// ═══════════════════════════════════════════════════════════════════════════
// DNS Pinning — resolve once, validate, return pinned address
// ═══════════════════════════════════════════════════════════════════════════

/// Result of SSRF-safe DNS resolution with pinned IP address.
///
/// Use `pinned_addr` directly for connection to close the TOCTOU gap:
/// the IP was validated at resolution time and cannot change via DNS rebinding.
#[derive(Debug, Clone)]
pub struct PinnedResolution {
    /// The original hostname that was resolved.
    pub hostname: String,
    /// The validated, pinned socket address to connect to.
    pub pinned_addr: SocketAddr,
    /// The port from the original URL.
    pub port: u16,
}

/// Resolve and pin: resolves a hostname, validates all IPs against blocklists,
/// and returns a pinned address for connection.
///
/// This closes the DNS rebinding TOCTOU vulnerability: the resolved IP is
/// checked and then returned for direct use. No second DNS resolution occurs.
///
/// # Errors
///
/// Returns an error if:
/// - DNS resolution fails
/// - All resolved IPs are blocked (private, loopback, metadata, etc.)
pub fn resolve_and_pin(
    host: &str,
    port: u16,
    config: &super::manager::BrowserConfig,
) -> Result<PinnedResolution, String> {
    // Allow explicitly allowlisted private hosts
    if config.ssrf_allow_private.iter().any(|h| h == host) {
        let addr_str = format!("{}:{}", host, port);
        let addr = addr_str
            .to_socket_addrs()
            .map_err(|e| format!("DNS resolution failed for '{}': {}", host, e))?
            .next()
            .ok_or_else(|| format!("DNS returned no addresses for '{}'", host))?;
        return Ok(PinnedResolution {
            hostname: host.to_string(),
            pinned_addr: addr,
            port,
        });
    }

    let addr_str = format!("{}:{}", host, port);
    let addrs: Vec<SocketAddr> = addr_str
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution failed for '{}': {}", host, e))?
        .collect();

    if addrs.is_empty() {
        return Err(format!("DNS returned no addresses for '{}'", host));
    }

    // Find the first non-blocked address
    for addr in &addrs {
        if !is_blocked_ip(&addr.ip()) {
            return Ok(PinnedResolution {
                hostname: host.to_string(),
                pinned_addr: *addr,
                port,
            });
        }
    }

    Err(format!(
        "all resolved IPs for '{}' are blocked: {:?}",
        host,
        addrs.iter().map(|a| a.ip()).collect::<Vec<_>>()
    ))
}

/// Validate a redirect target URL against SSRF rules.
///
/// Called on each redirect hop to prevent redirect-based SSRF bypass:
/// `public.evil.com → 302 → http://169.254.169.254/latest/meta-data/`
///
/// # Arguments
///
/// - `url`: The redirect target URL
/// - `config`: Browser configuration with port allowlist
/// - `current_depth`: Current redirect chain depth
/// - `max_redirects`: Maximum allowed redirect depth (recommended: 3)
///
/// # Returns
///
/// `Ok(PinnedResolution)` if the redirect target is safe, `Err` otherwise.
pub fn validate_redirect(
    url: &str,
    config: &super::manager::BrowserConfig,
    current_depth: usize,
    max_redirects: usize,
) -> Result<PinnedResolution, String> {
    if current_depth >= max_redirects {
        return Err(format!(
            "redirect chain too deep ({} hops, max {})",
            current_depth, max_redirects
        ));
    }

    // Re-validate the entire URL just like check_ssrf
    check_ssrf(url, config)?;

    // Extract host and port for pinning
    let (_scheme, rest) = url
        .split_once("://")
        .ok_or("invalid redirect URL")?;
    let scheme = url.split("://").next().unwrap_or("http");

    let authority = rest.split('/').next().unwrap_or(rest);
    let host_port = if let Some((_userinfo, hp)) = authority.rsplit_once('@') {
        hp
    } else {
        authority
    };

    let (host, port) = if host_port.starts_with('[') {
        if let Some(bracket_end) = host_port.find(']') {
            let h = &host_port[1..bracket_end];
            let p = host_port.get(bracket_end + 1..).and_then(|s| {
                s.strip_prefix(':').and_then(|p| p.parse::<u16>().ok())
            });
            (h, p)
        } else {
            return Err("invalid IPv6 in redirect".to_string());
        }
    } else if let Some((h, p)) = host_port.rsplit_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            (h, Some(port))
        } else {
            (host_port, None)
        }
    } else {
        (host_port, None)
    };

    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    resolve_and_pin(host, port, config)
}

/// SSRF-safe request limits for agent tool HTTP calls.
#[derive(Debug, Clone)]
pub struct SsrfLimits {
    /// Maximum response body size in bytes (default: 10MB).
    pub max_response_bytes: usize,
    /// DNS resolution timeout (default: 2s).
    pub dns_timeout_ms: u64,
    /// TCP connect timeout (default: 5s).
    pub connect_timeout_ms: u64,
    /// Total transfer timeout (default: 30s).
    pub transfer_timeout_ms: u64,
    /// Maximum redirect chain depth (default: 3).
    pub max_redirects: usize,
}

impl Default for SsrfLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: 10 * 1024 * 1024, // 10MB
            dns_timeout_ms: 2_000,
            connect_timeout_ms: 5_000,
            transfer_timeout_ms: 30_000,
            max_redirects: 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::BrowserConfig;

    fn default_config() -> BrowserConfig {
        BrowserConfig::default()
    }

    #[test]
    fn test_valid_https_url() {
        let config = default_config();
        // We can't resolve example.com in unit tests reliably, but
        // we can test the URL parsing and scheme/port checks.
        let result = check_ssrf("https://example.com", &config);
        // May fail on DNS in CI, but scheme/port pass
        assert!(result.is_ok() || result.as_ref().unwrap_err().contains("DNS"));
    }

    #[test]
    fn test_blocked_scheme_ftp() {
        let config = default_config();
        let result = check_ssrf("ftp://evil.com/file", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked scheme 'ftp'"));
    }

    #[test]
    fn test_blocked_scheme_file() {
        let config = default_config();
        let result = check_ssrf("file:///etc/passwd", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked scheme 'file'"));
    }

    #[test]
    fn test_blocked_scheme_javascript() {
        let config = default_config();
        let result = check_ssrf("javascript:alert(1)", &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_blocked_port() {
        let config = default_config();
        let result = check_ssrf("http://example.com:22/", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked port 22"));
    }

    #[test]
    fn test_allowed_port_8080() {
        let config = default_config();
        let result = check_ssrf("http://example.com:8080/", &config);
        // Port is allowed; may fail on DNS
        assert!(result.is_ok() || result.as_ref().unwrap_err().contains("DNS"));
    }

    #[test]
    fn test_localhost_blocked() {
        let config = default_config();
        let result = check_ssrf("http://localhost/admin", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("private/reserved IP"));
    }

    #[test]
    fn test_loopback_ip_blocked() {
        let config = default_config();
        let result = check_ssrf("http://127.0.0.1/admin", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("private/reserved IP"));
    }

    #[test]
    fn test_private_ip_10_blocked() {
        let config = default_config();
        let result = check_ssrf("http://10.0.0.1/", &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_private_ip_192_blocked() {
        let config = default_config();
        let result = check_ssrf("http://192.168.1.1/", &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_metadata_ip_blocked() {
        let config = default_config();
        let result = check_ssrf("http://169.254.169.254/latest/meta-data/", &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_allowlisted_private_host() {
        let mut config = default_config();
        config.ssrf_allow_private.push("internal-wiki.local".to_string());
        // Will fail DNS in test but shouldn't fail on allowlist logic
        let result = check_ssrf("http://internal-wiki.local/", &config);
        // Since we allowlist it, the ssrf_allow_private check passes — DNS may still fail
        assert!(result.is_ok() || result.as_ref().unwrap_err().contains("DNS"));
    }

    #[test]
    fn test_missing_scheme() {
        let config = default_config();
        let result = check_ssrf("example.com", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing scheme"));
    }

    #[test]
    fn test_ipv4_blocklist_helpers() {
        assert!(is_blocked_ipv4(&Ipv4Addr::LOCALHOST));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(192, 168, 1, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(169, 254, 169, 254)));
        assert!(is_blocked_ipv4(&Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_blocked_ipv4(&Ipv4Addr::UNSPECIFIED));
        // Public IP should not be blocked
        assert!(!is_blocked_ipv4(&Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_blocked_ipv4(&Ipv4Addr::new(93, 184, 216, 34)));
    }

    #[test]
    fn test_ipv6_blocklist_helpers() {
        assert!(is_blocked_ipv6(&Ipv6Addr::LOCALHOST));
        assert!(is_blocked_ipv6(&Ipv6Addr::UNSPECIFIED));
        // ULA fc00::/7
        assert!(is_blocked_ipv6(&Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1)));
        assert!(is_blocked_ipv6(&Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)));
        // Link-local fe80::/10
        assert!(is_blocked_ipv6(&Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)));
        // Public should not be blocked
        assert!(!is_blocked_ipv6(&Ipv6Addr::new(
            0x2607, 0xf8b0, 0x4004, 0x800, 0, 0, 0, 0x200e
        )));
    }

    // ── T9: DNS pinning tests ───────────────────────────────────────────

    #[test]
    fn test_resolve_and_pin_loopback_blocked() {
        let config = default_config();
        let result = resolve_and_pin("127.0.0.1", 80, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("blocked"));
    }

    #[test]
    fn test_resolve_and_pin_metadata_ip_blocked() {
        let config = default_config();
        let result = resolve_and_pin("169.254.169.254", 80, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_and_pin_private_10_blocked() {
        let config = default_config();
        let result = resolve_and_pin("10.0.0.1", 443, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_and_pin_allowlisted_private() {
        let mut config = default_config();
        config.ssrf_allow_private.push("127.0.0.1".to_string());
        let result = resolve_and_pin("127.0.0.1", 80, &config);
        assert!(result.is_ok());
        let pinned = result.unwrap();
        assert_eq!(pinned.hostname, "127.0.0.1");
        assert_eq!(pinned.port, 80);
    }

    #[test]
    fn test_resolve_and_pin_invalid_host() {
        let config = default_config();
        let result = resolve_and_pin("this-host-does-not-exist-12345.invalid", 80, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("DNS"));
    }

    #[test]
    fn test_validate_redirect_chain_depth() {
        let config = default_config();
        let result = validate_redirect("http://example.com", &config, 3, 3);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("redirect chain too deep"));
    }

    #[test]
    fn test_validate_redirect_to_metadata_blocked() {
        let config = default_config();
        let result = validate_redirect(
            "http://169.254.169.254/latest/meta-data/",
            &config,
            0,
            3,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_redirect_to_private_blocked() {
        let config = default_config();
        let result = validate_redirect("http://10.0.0.1/internal", &config, 0, 3);
        assert!(result.is_err());
    }

    #[test]
    fn test_ssrf_limits_default() {
        let limits = SsrfLimits::default();
        assert_eq!(limits.max_response_bytes, 10 * 1024 * 1024);
        assert_eq!(limits.dns_timeout_ms, 2_000);
        assert_eq!(limits.connect_timeout_ms, 5_000);
        assert_eq!(limits.transfer_timeout_ms, 30_000);
        assert_eq!(limits.max_redirects, 3);
    }
}
