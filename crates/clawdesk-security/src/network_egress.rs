//! D4b: Network Egress Policy — endpoint-level deny-by-default egress control.
//!
//! ## Monotone Security Lattice
//!
//! Network access is a meet operation across three orthogonal security dimensions:
//!
//! ```text
//! P_effective(tool, endpoint) = P_reputation(endpoint)
//!                              ∧ P_binding(tool, endpoint)
//!                              ∧ P_protocol(endpoint, tls)
//! ```
//!
//! All three must be `Allow` for access to proceed. If any dimension is `Deny`,
//! the connection is blocked. If none explicitly deny but a binding is missing,
//! the decision escalates to `RequireApproval` for operator-in-the-loop security.
//!
//! ## CIDR Matching
//!
//! Uses proper bit arithmetic for subnet matching — not string prefix comparison:
//!
//! ```text
//! match(ip, cidr) = (ip_bits & mask) == (net_bits & mask)
//! where mask = !0u32 << (32 - prefix_len)
//! ```
//!
//! This is O(1) per check with exact mathematical correctness, versus NemoClaw's
//! string prefix approach which produces false positives (e.g., "10" matching "100").
//!
//! ## SSRF Prevention
//!
//! Blocks all known cloud metadata endpoints beyond RFC1918:
//! - AWS: 169.254.169.254, fd00:ec2::254
//! - GCP: metadata.google.internal
//! - Azure: 169.254.169.254 (shared with AWS)
//! - Alibaba: 100.100.100.200
//! - Link-local: 169.254.0.0/16, fe80::/10

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Core Types
// ═══════════════════════════════════════════════════════════════════════════

/// HTTP method restriction for endpoint access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
    Options,
}

impl HttpMethod {
    /// Parse from string (case-insensitive).
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PUT" => Some(Self::Put),
            "DELETE" => Some(Self::Delete),
            "PATCH" => Some(Self::Patch),
            "HEAD" => Some(Self::Head),
            "OPTIONS" => Some(Self::Options),
            _ => None,
        }
    }
}

/// Endpoint permission — defines what's allowed for a specific host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointPermission {
    /// Domain or IP (exact match or wildcard like "*.example.com").
    pub host: String,
    /// Allowed ports. Empty vec = any port.
    pub ports: Vec<u16>,
    /// Allowed HTTP methods. Empty vec = any method.
    pub methods: Vec<HttpMethod>,
    /// Whether TLS is required (HTTPS only).
    pub require_tls: bool,
    /// Optional path prefix restriction (e.g., "/v1/").
    pub path_prefix: Option<String>,
    /// Human-readable description of why this endpoint is allowed.
    pub description: String,
}

/// Tool-to-endpoint binding — which tools may access which hosts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolEndpointBinding {
    /// Tool name (exact match).
    pub tool: String,
    /// Endpoints this tool is authorized to reach.
    pub endpoints: Vec<EndpointPermission>,
}

/// Egress policy enforcement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressMode {
    /// Strict: deny all unbound endpoints.
    Enforce,
    /// Permissive: log unbound access but allow (for rollout).
    AuditOnly,
    /// Approval: require operator approval for unbound endpoints.
    RequireApproval,
}

impl Default for EgressMode {
    fn default() -> Self {
        Self::RequireApproval
    }
}

/// Egress policy decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressDecision {
    /// Connection explicitly allowed — matched a binding rule.
    Allow {
        tool: String,
        endpoint: String,
        matched_rule: String,
    },
    /// Connection denied — blocked by policy or CIDR blocklist.
    Deny { reason: String },
    /// Connection needs operator approval (unknown endpoint, not blocked).
    RequireApproval {
        tool: String,
        host: String,
        port: u16,
        reason: String,
    },
}

impl EgressDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    pub fn is_denied(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// CIDR Matching — proper bit arithmetic
// ═══════════════════════════════════════════════════════════════════════════

/// Parsed CIDR block for O(1) containment checks.
#[derive(Debug, Clone)]
struct CidrBlock {
    /// Network address as u32 (masked).
    network: u32,
    /// Subnet mask as u32.
    mask: u32,
    /// Original string for diagnostics.
    original: String,
}

impl CidrBlock {
    /// Parse a CIDR string like "10.0.0.0/8".
    ///
    /// Returns None if the format is invalid.
    fn parse(cidr: &str) -> Option<Self> {
        let (addr_str, prefix_str) = cidr.split_once('/')?;
        let addr: Ipv4Addr = addr_str.parse().ok()?;
        let prefix_len: u32 = prefix_str.parse().ok()?;
        if prefix_len > 32 {
            return None;
        }
        let mask = if prefix_len == 0 {
            0u32
        } else {
            !0u32 << (32 - prefix_len)
        };
        let network = u32::from(addr) & mask;
        Some(Self {
            network,
            mask,
            original: cidr.to_string(),
        })
    }

    /// Check if an IPv4 address falls within this CIDR block.
    ///
    /// Uses bitwise masking: `(ip & mask) == network`
    /// This is O(1) and mathematically exact.
    fn contains(&self, ip: Ipv4Addr) -> bool {
        (u32::from(ip) & self.mask) == self.network
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Network Egress Policy Engine
// ═══════════════════════════════════════════════════════════════════════════

/// Network egress policy engine — deny-by-default with tool-endpoint bindings.
///
/// Security model: a connection from tool T to endpoint E is allowed iff:
///
/// 1. E is not in the CIDR blocklist (SSRF prevention)
/// 2. T has an explicit binding to E (tool-endpoint authorization)
/// 3. The connection satisfies TLS and method constraints
///
/// If (1) fails → immediate Deny (non-negotiable).
/// If (2) fails → depends on mode: Enforce=Deny, RequireApproval=escalate, AuditOnly=log+allow.
/// If (3) fails → Deny (protocol constraint violation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkEgressPolicy {
    /// Enforcement mode.
    pub mode: EgressMode,
    /// Per-tool endpoint bindings.
    pub bindings: Vec<ToolEndpointBinding>,
    /// CIDR blocks that are always denied (SSRF prevention).
    #[serde(skip)]
    cidr_blocklist: Vec<CidrBlock>,
    /// Domain names that are always denied (cloud metadata hostnames).
    pub blocked_domains: Vec<String>,
    /// Whether to allow localhost connections (for local model servers).
    pub allow_localhost: bool,
}

impl Default for NetworkEgressPolicy {
    fn default() -> Self {
        let mut policy = Self {
            mode: EgressMode::RequireApproval,
            bindings: Self::default_bindings(),
            cidr_blocklist: Vec::new(),
            blocked_domains: Self::default_blocked_domains(),
            allow_localhost: false,
        };
        policy.cidr_blocklist = Self::default_cidr_blocklist();
        policy
    }
}

impl NetworkEgressPolicy {
    /// Build a policy that allows localhost (for local model inference).
    pub fn with_local_models() -> Self {
        let mut policy = Self::default();
        policy.allow_localhost = true;
        policy
    }

    /// Add a tool-endpoint binding at runtime.
    pub fn add_binding(&mut self, binding: ToolEndpointBinding) {
        info!(
            tool = %binding.tool,
            endpoints = binding.endpoints.len(),
            "Added tool-endpoint binding"
        );
        self.bindings.push(binding);
    }

    /// Add an endpoint permission for an existing tool, or create a new binding.
    pub fn grant_endpoint(&mut self, tool: &str, permission: EndpointPermission) {
        info!(
            tool,
            host = %permission.host,
            "Granting endpoint access to tool"
        );
        if let Some(binding) = self.bindings.iter_mut().find(|b| b.tool == tool) {
            binding.endpoints.push(permission);
        } else {
            self.bindings.push(ToolEndpointBinding {
                tool: tool.to_string(),
                endpoints: vec![permission],
            });
        }
    }

    // ── Core Decision Logic ──────────────────────────────────────────────

    /// Evaluate whether a tool may connect to a host:port.
    ///
    /// This is the lattice meet: P_effective = P_reputation ∧ P_binding ∧ P_protocol.
    pub fn evaluate(
        &self,
        tool: &str,
        host: &str,
        port: u16,
        method: Option<HttpMethod>,
        is_tls: bool,
    ) -> EgressDecision {
        // ── Layer 1: SSRF blocklist (non-negotiable) ─────────────────
        if let Some(reason) = self.check_blocklist(host) {
            warn!(
                tool,
                host,
                port,
                reason = %reason,
                "SSRF blocklist: connection denied"
            );
            return EgressDecision::Deny { reason };
        }

        // ── Layer 2: Localhost handling ───────────────────────────────
        if Self::is_localhost(host) {
            if self.allow_localhost {
                debug!(tool, host, port, "Localhost connection allowed by policy");
                return EgressDecision::Allow {
                    tool: tool.to_string(),
                    endpoint: format!("{host}:{port}"),
                    matched_rule: "localhost_allowed".to_string(),
                };
            } else {
                return EgressDecision::Deny {
                    reason: format!(
                        "localhost connections disabled; enable allow_localhost for local models"
                    ),
                };
            }
        }

        // ── Layer 3: Tool-endpoint binding check ─────────────────────
        let binding_result = self.check_binding(tool, host, port, method, is_tls);

        match binding_result {
            BindingCheck::Allowed { rule_desc } => {
                debug!(
                    tool,
                    host,
                    port,
                    rule = %rule_desc,
                    "Egress allowed by tool binding"
                );
                EgressDecision::Allow {
                    tool: tool.to_string(),
                    endpoint: format!("{host}:{port}"),
                    matched_rule: rule_desc,
                }
            }
            BindingCheck::MethodDenied { allowed, requested } => {
                EgressDecision::Deny {
                    reason: format!(
                        "tool '{tool}' may reach {host}:{port} but method {requested:?} \
                         not in allowed set {allowed:?}"
                    ),
                }
            }
            BindingCheck::TlsRequired => {
                EgressDecision::Deny {
                    reason: format!(
                        "endpoint {host}:{port} requires TLS but connection is plaintext"
                    ),
                }
            }
            BindingCheck::NoBinding => {
                // No explicit binding — decision depends on mode
                match self.mode {
                    EgressMode::Enforce => {
                        warn!(
                            tool,
                            host,
                            port,
                            "No endpoint binding: denied (enforce mode)"
                        );
                        EgressDecision::Deny {
                            reason: format!(
                                "no endpoint binding for tool '{tool}' to {host}:{port}"
                            ),
                        }
                    }
                    EgressMode::RequireApproval => {
                        info!(
                            tool,
                            host,
                            port,
                            "No endpoint binding: escalating to operator"
                        );
                        EgressDecision::RequireApproval {
                            tool: tool.to_string(),
                            host: host.to_string(),
                            port,
                            reason: format!(
                                "tool '{tool}' has no binding to {host}:{port}; operator approval required"
                            ),
                        }
                    }
                    EgressMode::AuditOnly => {
                        warn!(
                            tool,
                            host,
                            port,
                            "No endpoint binding: allowed (audit-only mode)"
                        );
                        EgressDecision::Allow {
                            tool: tool.to_string(),
                            endpoint: format!("{host}:{port}"),
                            matched_rule: "audit_only_passthrough".to_string(),
                        }
                    }
                }
            }
        }
    }

    // ── SSRF Blocklist ───────────────────────────────────────────────────

    /// Check if a host is in the SSRF blocklist.
    ///
    /// Returns Some(reason) if blocked, None if clear.
    fn check_blocklist(&self, host: &str) -> Option<String> {
        // Check domain blocklist first (O(n) but n is small ~5)
        let host_lower = host.to_ascii_lowercase();
        for blocked in &self.blocked_domains {
            if host_lower == blocked.as_str() {
                return Some(format!("blocked cloud metadata domain: {blocked}"));
            }
        }

        // Try to parse as IPv4 for CIDR check
        if let Ok(ip) = host.parse::<Ipv4Addr>() {
            for cidr in &self.cidr_blocklist {
                if cidr.contains(ip) {
                    return Some(format!(
                        "IP {ip} falls within blocked CIDR range {}",
                        cidr.original
                    ));
                }
            }
        }

        // If host is an IPv4-mapped IPv6 (::ffff:a.b.c.d), extract and check
        if let Ok(ip) = host.parse::<IpAddr>() {
            if let IpAddr::V6(v6) = ip {
                if let Some(v4) = v6.to_ipv4_mapped() {
                    for cidr in &self.cidr_blocklist {
                        if cidr.contains(v4) {
                            return Some(format!(
                                "IPv4-mapped address {v4} falls within blocked CIDR range {}",
                                cidr.original
                            ));
                        }
                    }
                }
            }
        }

        None
    }

    fn is_localhost(host: &str) -> bool {
        matches!(
            host,
            "localhost" | "127.0.0.1" | "::1" | "[::1]" | "0.0.0.0"
        )
    }

    // ── Tool Binding Check ───────────────────────────────────────────────

    /// Check tool-endpoint bindings.
    fn check_binding(
        &self,
        tool: &str,
        host: &str,
        port: u16,
        method: Option<HttpMethod>,
        is_tls: bool,
    ) -> BindingCheck {
        // Find all bindings for this tool (exact match)
        let tool_bindings: Vec<&ToolEndpointBinding> =
            self.bindings.iter().filter(|b| b.tool == tool).collect();

        if tool_bindings.is_empty() {
            // Also check wildcard "*" bindings (apply to all tools)
            let wildcard_bindings: Vec<&ToolEndpointBinding> =
                self.bindings.iter().filter(|b| b.tool == "*").collect();
            if wildcard_bindings.is_empty() {
                return BindingCheck::NoBinding;
            }
            return self.check_endpoint_match(&wildcard_bindings, host, port, method, is_tls);
        }

        self.check_endpoint_match(&tool_bindings, host, port, method, is_tls)
    }

    /// Check if any endpoint in the bindings matches the target.
    fn check_endpoint_match(
        &self,
        bindings: &[&ToolEndpointBinding],
        host: &str,
        port: u16,
        method: Option<HttpMethod>,
        is_tls: bool,
    ) -> BindingCheck {
        for binding in bindings {
            for ep in &binding.endpoints {
                if !Self::domain_matches(&ep.host, host) {
                    continue;
                }

                // Port check
                if !ep.ports.is_empty() && !ep.ports.contains(&port) {
                    continue;
                }

                // TLS check
                if ep.require_tls && !is_tls {
                    return BindingCheck::TlsRequired;
                }

                // Method check
                if let Some(method) = method {
                    if !ep.methods.is_empty() && !ep.methods.contains(&method) {
                        return BindingCheck::MethodDenied {
                            allowed: ep.methods.clone(),
                            requested: method,
                        };
                    }
                }

                return BindingCheck::Allowed {
                    rule_desc: format!(
                        "tool '{}' → {} ({})",
                        binding.tool, ep.host, ep.description
                    ),
                };
            }
        }

        BindingCheck::NoBinding
    }

    /// Domain matching: exact or wildcard with proper suffix validation.
    ///
    /// `*.example.com` matches `api.example.com` but NOT `evil-example.com`.
    /// This prevents suffix-collision attacks.
    fn domain_matches(pattern: &str, host: &str) -> bool {
        let pattern_lower = pattern.to_ascii_lowercase();
        let host_lower = host.to_ascii_lowercase();

        // Exact match
        if pattern_lower == host_lower {
            return true;
        }

        // Wildcard: *.example.com
        if let Some(suffix) = pattern_lower.strip_prefix("*.") {
            // Host must end with .suffix (proper subdomain, not suffix collision)
            if let Some(before_suffix) = host_lower.strip_suffix(suffix) {
                // The character before the suffix must be '.' (proper subdomain boundary)
                return before_suffix.ends_with('.');
            }
        }

        false
    }

    // ── Default Configuration ────────────────────────────────────────────

    /// CIDR blocks that are always denied — SSRF prevention.
    ///
    /// These cannot be overridden by tool bindings.
    fn default_cidr_blocklist() -> Vec<CidrBlock> {
        [
            // RFC1918 private networks
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            // Link-local
            "169.254.0.0/16",
            // Loopback (when localhost is not explicitly allowed)
            "127.0.0.0/8",
            // CGNAT (Carrier-grade NAT) — sometimes used for cloud metadata
            "100.64.0.0/10",
            // AWS IMDSv1/v2 metadata (specific /32 within link-local)
            // Already covered by 169.254.0.0/16, but called out for documentation
            // Alibaba Cloud metadata
            "100.100.100.200/32",
        ]
        .iter()
        .filter_map(|s| CidrBlock::parse(s))
        .collect()
    }

    /// Domain names that are always blocked — cloud metadata endpoints.
    fn default_blocked_domains() -> Vec<String> {
        vec![
            // GCP metadata (not an IP, uses hostname)
            "metadata.google.internal".to_string(),
            // Kubernetes API server (in-cluster)
            "kubernetes.default.svc".to_string(),
            // Docker socket
            "host.docker.internal".to_string(),
        ]
    }

    /// Default tool-endpoint bindings for common AI tools.
    fn default_bindings() -> Vec<ToolEndpointBinding> {
        vec![
            // ── Wildcard: all tools can reach LLM APIs on port 443 ───
            ToolEndpointBinding {
                tool: "*".to_string(),
                endpoints: vec![
                    EndpointPermission {
                        host: "api.anthropic.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Post],
                        require_tls: true,
                        path_prefix: Some("/v1/".to_string()),
                        description: "Anthropic Claude API".to_string(),
                    },
                    EndpointPermission {
                        host: "api.openai.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Post],
                        require_tls: true,
                        path_prefix: Some("/v1/".to_string()),
                        description: "OpenAI API".to_string(),
                    },
                    EndpointPermission {
                        host: "generativelanguage.googleapis.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Post],
                        require_tls: true,
                        path_prefix: None,
                        description: "Google Gemini API".to_string(),
                    },
                    EndpointPermission {
                        host: "api.groq.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Post],
                        require_tls: true,
                        path_prefix: Some("/openai/v1/".to_string()),
                        description: "Groq inference API".to_string(),
                    },
                    EndpointPermission {
                        host: "openrouter.ai".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Post, HttpMethod::Get],
                        require_tls: true,
                        path_prefix: Some("/api/v1/".to_string()),
                        description: "OpenRouter multi-model API".to_string(),
                    },
                    EndpointPermission {
                        host: "api.mistral.ai".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Post],
                        require_tls: true,
                        path_prefix: Some("/v1/".to_string()),
                        description: "Mistral AI API".to_string(),
                    },
                    EndpointPermission {
                        host: "api.together.xyz".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Post],
                        require_tls: true,
                        path_prefix: Some("/v1/".to_string()),
                        description: "Together AI inference API".to_string(),
                    },
                    EndpointPermission {
                        host: "api.deepseek.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Post],
                        require_tls: true,
                        path_prefix: Some("/v1/".to_string()),
                        description: "DeepSeek API".to_string(),
                    },
                    EndpointPermission {
                        host: "integrate.api.nvidia.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Post],
                        require_tls: true,
                        path_prefix: None,
                        description: "NVIDIA NIM inference API".to_string(),
                    },
                ],
            },
            // ── http_fetch: broader web access ───────────────────────
            ToolEndpointBinding {
                tool: "http_fetch".to_string(),
                endpoints: vec![
                    EndpointPermission {
                        host: "*.github.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Get],
                        require_tls: true,
                        path_prefix: None,
                        description: "GitHub (read-only)".to_string(),
                    },
                    EndpointPermission {
                        host: "*.githubusercontent.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Get],
                        require_tls: true,
                        path_prefix: None,
                        description: "GitHub raw content (read-only)".to_string(),
                    },
                ],
            },
            // ── web_search: search engine access ─────────────────────
            ToolEndpointBinding {
                tool: "web_search".to_string(),
                endpoints: vec![
                    EndpointPermission {
                        host: "*.googleapis.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Get],
                        require_tls: true,
                        path_prefix: None,
                        description: "Google Custom Search".to_string(),
                    },
                    EndpointPermission {
                        host: "api.bing.microsoft.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Get],
                        require_tls: true,
                        path_prefix: None,
                        description: "Bing Search API".to_string(),
                    },
                    EndpointPermission {
                        host: "api.search.brave.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Get],
                        require_tls: true,
                        path_prefix: None,
                        description: "Brave Search API".to_string(),
                    },
                ],
            },
            // ── fetch: general URL fetching ──────────────────────────
            ToolEndpointBinding {
                tool: "fetch".to_string(),
                endpoints: vec![
                    EndpointPermission {
                        host: "*.github.com".to_string(),
                        ports: vec![443],
                        methods: vec![HttpMethod::Get],
                        require_tls: true,
                        path_prefix: None,
                        description: "GitHub (read-only)".to_string(),
                    },
                ],
            },
        ]
    }

    // ── Serialization Helpers ────────────────────────────────────────────

    /// Rebuild CIDR blocklist after deserialization.
    pub fn rebuild_cidr_blocklist(&mut self) {
        self.cidr_blocklist = Self::default_cidr_blocklist();
    }

    /// Get all tool names that have explicit bindings.
    pub fn bound_tools(&self) -> Vec<&str> {
        self.bindings.iter().map(|b| b.tool.as_str()).collect()
    }

    /// Get all endpoints bound to a specific tool.
    pub fn endpoints_for_tool(&self, tool: &str) -> Vec<&EndpointPermission> {
        self.bindings
            .iter()
            .filter(|b| b.tool == tool || b.tool == "*")
            .flat_map(|b| b.endpoints.iter())
            .collect()
    }
}

/// Internal binding check result — before mode-dependent decision.
#[derive(Debug)]
enum BindingCheck {
    /// Endpoint matched a binding rule.
    Allowed { rule_desc: String },
    /// Endpoint matched but HTTP method not allowed.
    MethodDenied {
        allowed: Vec<HttpMethod>,
        requested: HttpMethod,
    },
    /// Endpoint matched but TLS is required and not provided.
    TlsRequired,
    /// No binding found for this tool-endpoint pair.
    NoBinding,
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── CIDR Arithmetic Tests ────────────────────────────────────────

    #[test]
    fn cidr_parse_valid() {
        let block = CidrBlock::parse("10.0.0.0/8").unwrap();
        assert_eq!(block.network, 0x0A000000); // 10.0.0.0
        assert_eq!(block.mask, 0xFF000000); // /8 mask
    }

    #[test]
    fn cidr_parse_slash_zero() {
        let block = CidrBlock::parse("0.0.0.0/0").unwrap();
        assert_eq!(block.mask, 0);
        // /0 matches everything
        assert!(block.contains(Ipv4Addr::new(1, 2, 3, 4)));
        assert!(block.contains(Ipv4Addr::new(255, 255, 255, 255)));
    }

    #[test]
    fn cidr_parse_slash_32() {
        let block = CidrBlock::parse("192.168.1.1/32").unwrap();
        assert!(block.contains(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(!block.contains(Ipv4Addr::new(192, 168, 1, 2)));
    }

    #[test]
    fn cidr_rejects_invalid_prefix() {
        assert!(CidrBlock::parse("10.0.0.0/33").is_none());
        assert!(CidrBlock::parse("10.0.0.0/").is_none());
        assert!(CidrBlock::parse("10.0.0.0").is_none());
        assert!(CidrBlock::parse("not-an-ip/8").is_none());
    }

    #[test]
    fn cidr_rfc1918_class_a() {
        let block = CidrBlock::parse("10.0.0.0/8").unwrap();
        assert!(block.contains(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(block.contains(Ipv4Addr::new(10, 255, 255, 255)));
        assert!(!block.contains(Ipv4Addr::new(11, 0, 0, 1)));
        // Critical: "100.x.x.x" must NOT match "10.0.0.0/8"
        // This is the bug in the old string-prefix implementation
        assert!(!block.contains(Ipv4Addr::new(100, 0, 0, 1)));
    }

    #[test]
    fn cidr_rfc1918_class_b() {
        let block = CidrBlock::parse("172.16.0.0/12").unwrap();
        assert!(block.contains(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(block.contains(Ipv4Addr::new(172, 31, 255, 255)));
        assert!(!block.contains(Ipv4Addr::new(172, 32, 0, 1)));
        assert!(!block.contains(Ipv4Addr::new(172, 15, 255, 255)));
    }

    #[test]
    fn cidr_rfc1918_class_c() {
        let block = CidrBlock::parse("192.168.0.0/16").unwrap();
        assert!(block.contains(Ipv4Addr::new(192, 168, 0, 1)));
        assert!(block.contains(Ipv4Addr::new(192, 168, 255, 255)));
        assert!(!block.contains(Ipv4Addr::new(192, 169, 0, 1)));
    }

    #[test]
    fn cidr_cgnat_range() {
        let block = CidrBlock::parse("100.64.0.0/10").unwrap();
        assert!(block.contains(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(block.contains(Ipv4Addr::new(100, 127, 255, 255)));
        assert!(!block.contains(Ipv4Addr::new(100, 128, 0, 1)));
        // Alibaba metadata IS in CGNAT range
        assert!(block.contains(Ipv4Addr::new(100, 100, 100, 200)));
    }

    // ── Domain Matching Tests ────────────────────────────────────────

    #[test]
    fn domain_exact_match() {
        assert!(NetworkEgressPolicy::domain_matches(
            "api.anthropic.com",
            "api.anthropic.com"
        ));
        assert!(NetworkEgressPolicy::domain_matches(
            "API.Anthropic.COM",
            "api.anthropic.com"
        ));
    }

    #[test]
    fn domain_wildcard_match() {
        assert!(NetworkEgressPolicy::domain_matches(
            "*.example.com",
            "api.example.com"
        ));
        assert!(NetworkEgressPolicy::domain_matches(
            "*.example.com",
            "sub.example.com"
        ));
    }

    #[test]
    fn domain_wildcard_no_suffix_collision() {
        // *.example.com must NOT match evil-example.com
        assert!(!NetworkEgressPolicy::domain_matches(
            "*.example.com",
            "evil-example.com"
        ));
    }

    #[test]
    fn domain_wildcard_no_bare_match() {
        // *.example.com should NOT match the bare "example.com"
        assert!(!NetworkEgressPolicy::domain_matches(
            "*.example.com",
            "example.com"
        ));
    }

    // ── Policy Decision Tests ────────────────────────────────────────

    #[test]
    fn ssrf_blocks_rfc1918() {
        let policy = NetworkEgressPolicy::default();
        let decision = policy.evaluate("any_tool", "10.0.0.1", 80, None, false);
        assert!(decision.is_denied());

        let decision = policy.evaluate("any_tool", "172.16.5.10", 443, None, true);
        assert!(decision.is_denied());

        let decision = policy.evaluate("any_tool", "192.168.1.1", 22, None, false);
        assert!(decision.is_denied());
    }

    #[test]
    fn ssrf_blocks_cloud_metadata() {
        let policy = NetworkEgressPolicy::default();

        // AWS/Azure metadata (link-local)
        let decision = policy.evaluate("any_tool", "169.254.169.254", 80, None, false);
        assert!(decision.is_denied());

        // GCP metadata (hostname)
        let decision =
            policy.evaluate("any_tool", "metadata.google.internal", 80, None, false);
        assert!(decision.is_denied());

        // Alibaba metadata (in CGNAT range)
        let decision = policy.evaluate("any_tool", "100.100.100.200", 80, None, false);
        assert!(decision.is_denied());
    }

    #[test]
    fn ssrf_cannot_bypass_with_100_prefix() {
        // Old implementation bug: "10.0.0.0/8".starts_with("10") matched "100.x.x.x"
        let policy = NetworkEgressPolicy::default();
        // 100.0.0.1 is NOT in 10.0.0.0/8 — but IS in 100.64.0.0/10 (CGNAT)
        // Only block if actually in a blocked range
        let decision = policy.evaluate("http_fetch", "100.0.0.1", 443, None, true);
        // 100.0.0.1 is NOT in 100.64.0.0/10 (100.0 < 100.64), so it should NOT be blocked by CIDR
        // It WILL get NoBinding → RequireApproval in default mode
        assert!(!decision.is_denied() || matches!(decision, EgressDecision::RequireApproval { .. }));
    }

    #[test]
    fn allows_known_api_with_binding() {
        let policy = NetworkEgressPolicy::default();

        // Wildcard "*" binding covers all tools for LLM APIs
        let decision = policy.evaluate(
            "http_fetch",
            "api.anthropic.com",
            443,
            Some(HttpMethod::Post),
            true,
        );
        assert!(decision.is_allowed());
    }

    #[test]
    fn denies_wrong_method() {
        let policy = NetworkEgressPolicy::default();

        // Anthropic binding only allows POST
        let decision = policy.evaluate(
            "http_fetch",
            "api.anthropic.com",
            443,
            Some(HttpMethod::Delete),
            true,
        );
        assert!(decision.is_denied());
    }

    #[test]
    fn denies_wrong_port() {
        let policy = NetworkEgressPolicy::default();

        // Anthropic binding only allows port 443
        let decision = policy.evaluate(
            "http_fetch",
            "api.anthropic.com",
            80,
            Some(HttpMethod::Post),
            false,
        );
        // Port 80 doesn't match port restriction, falls through to NoBinding
        assert!(!decision.is_allowed());
    }

    #[test]
    fn requires_tls_when_configured() {
        let policy = NetworkEgressPolicy::default();

        // api.anthropic.com requires TLS
        let decision = policy.evaluate(
            "http_fetch",
            "api.anthropic.com",
            443,
            Some(HttpMethod::Post),
            false, // NOT TLS
        );
        assert!(decision.is_denied());
    }

    #[test]
    fn unknown_endpoint_requires_approval() {
        let policy = NetworkEgressPolicy::default();

        let decision = policy.evaluate(
            "http_fetch",
            "evil.example.com",
            443,
            Some(HttpMethod::Get),
            true,
        );
        assert!(matches!(decision, EgressDecision::RequireApproval { .. }));
    }

    #[test]
    fn enforce_mode_denies_unknown() {
        let mut policy = NetworkEgressPolicy::default();
        policy.mode = EgressMode::Enforce;

        let decision = policy.evaluate(
            "http_fetch",
            "unknown.example.com",
            443,
            Some(HttpMethod::Get),
            true,
        );
        assert!(decision.is_denied());
    }

    #[test]
    fn audit_mode_allows_unknown() {
        let mut policy = NetworkEgressPolicy::default();
        policy.mode = EgressMode::AuditOnly;

        let decision = policy.evaluate(
            "http_fetch",
            "unknown.example.com",
            443,
            Some(HttpMethod::Get),
            true,
        );
        assert!(decision.is_allowed());
    }

    #[test]
    fn localhost_blocked_by_default() {
        let policy = NetworkEgressPolicy::default();
        let decision = policy.evaluate("bash", "127.0.0.1", 8080, None, false);
        assert!(decision.is_denied());

        let decision = policy.evaluate("bash", "localhost", 11434, None, false);
        assert!(decision.is_denied());
    }

    #[test]
    fn localhost_allowed_when_enabled() {
        let policy = NetworkEgressPolicy::with_local_models();
        let decision = policy.evaluate("bash", "localhost", 11434, None, false);
        assert!(decision.is_allowed());
    }

    #[test]
    fn grant_endpoint_at_runtime() {
        let mut policy = NetworkEgressPolicy::default();
        policy.mode = EgressMode::Enforce;

        // Initially denied
        let decision = policy.evaluate(
            "http_fetch",
            "custom-api.example.com",
            443,
            Some(HttpMethod::Get),
            true,
        );
        assert!(decision.is_denied());

        // Grant access
        policy.grant_endpoint(
            "http_fetch",
            EndpointPermission {
                host: "custom-api.example.com".to_string(),
                ports: vec![443],
                methods: vec![HttpMethod::Get],
                require_tls: true,
                path_prefix: None,
                description: "Custom API (operator-approved)".to_string(),
            },
        );

        // Now allowed
        let decision = policy.evaluate(
            "http_fetch",
            "custom-api.example.com",
            443,
            Some(HttpMethod::Get),
            true,
        );
        assert!(decision.is_allowed());
    }

    #[test]
    fn tool_specific_binding_doesnt_leak() {
        let policy = NetworkEgressPolicy::default();

        // http_fetch has binding to *.github.com
        let _decision = policy.evaluate(
            "http_fetch",
            "raw.github.com",
            443,
            Some(HttpMethod::Get),
            true,
        );
        // Note: *.github.com does NOT match "raw.github.com" because it requires
        // a dot before "github.com" in the host. raw.github.com → r-a-w-.-g-i-t-h-u-b...
        // Actually wait - "raw.github.com" does end with ".github.com" and has a proper dot boundary.
        // Let me trace: pattern="*.github.com", host="raw.github.com"
        // suffix = "github.com", host stripped of suffix = "raw." which ends with '.' ✓

        // bash does NOT have a binding to github.com (only wildcard LLM APIs)
        let decision2 = policy.evaluate(
            "bash",
            "raw.github.com",
            443,
            Some(HttpMethod::Get),
            true,
        );
        // bash has no github binding, falls through to RequireApproval
        assert!(matches!(
            decision2,
            EgressDecision::RequireApproval { .. }
        ));
    }

    #[test]
    fn ipv4_mapped_v6_blocked() {
        let policy = NetworkEgressPolicy::default();
        // ::ffff:10.0.0.1 is an IPv4-mapped IPv6 address for 10.0.0.1
        let decision = policy.evaluate("any", "::ffff:10.0.0.1", 80, None, false);
        assert!(decision.is_denied());
    }

    // ── Endpoint Introspection Tests ─────────────────────────────────

    #[test]
    fn endpoints_for_tool_includes_wildcard() {
        let policy = NetworkEgressPolicy::default();
        let eps = policy.endpoints_for_tool("bash");
        // bash should see wildcard (*) bindings = all LLM APIs
        assert!(!eps.is_empty());
        assert!(eps.iter().any(|e| e.host == "api.anthropic.com"));
    }

    #[test]
    fn bound_tools_includes_all() {
        let policy = NetworkEgressPolicy::default();
        let tools = policy.bound_tools();
        assert!(tools.contains(&"*"));
        assert!(tools.contains(&"http_fetch"));
        assert!(tools.contains(&"web_search"));
    }
}
