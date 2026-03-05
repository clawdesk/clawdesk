//! Sandbox policy — lattice-based security model for tool execution.
//!
//! Isolation levels form a partial order (lattice):
//! ```text
//! None < PathScope < ProcessIsolation < FullSandbox
//! ```
//!
//! Each tool has a required minimum isolation level. The runtime provides
//! a maximum available level based on the platform. The effective level is:
//! ```text
//! effective(t) = max(req(t), policy_override(t))
//! ```
//! If effective(t) > avail(platform), the tool is BLOCKED.
//!
//! ## Resource Limits
//!
//! Memory: mem_limit = base_allocation + estimated_output × safety_factor
//! Timeout: Hierarchical timer wheel with O(1) start/cancel
//!
//! EWMA for output estimation:
//! ```text
//! EWMA_n = α × x_n + (1 - α) × EWMA_{n-1},  α = 0.1
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Canonical isolation level — re-exported from `clawdesk-types`.
///
/// ```text
/// None < PathScope < ProcessIsolation < FullSandbox
/// ```
pub use clawdesk_types::IsolationLevel;

/// Resource limits for sandboxed tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maximum CPU time in seconds.
    pub cpu_time_secs: u64,
    /// Maximum wall-clock time in seconds.
    pub wall_time_secs: u64,
    /// Maximum memory in bytes.
    pub memory_bytes: u64,
    /// Maximum number of open file descriptors.
    pub max_fds: u64,
    /// Maximum output size in bytes.
    pub max_output_bytes: u64,
    /// Maximum number of child processes.
    pub max_processes: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpu_time_secs: 30,
            wall_time_secs: 60,
            memory_bytes: 256 * 1024 * 1024, // 256 MiB
            max_fds: 256,
            max_output_bytes: 10 * 1024 * 1024, // 10 MiB
            max_processes: 10,
        }
    }
}

/// Per-tool sandbox policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSandboxPolicy {
    /// Required minimum isolation level.
    pub required_level: IsolationLevel,
    /// Resource limits for this tool.
    pub resource_limits: ResourceLimits,
    /// Whether network access is allowed.
    pub allow_network: bool,
    /// Whitelisted filesystem paths (beyond workspace).
    pub extra_paths: Vec<String>,
    /// Environment variables to pass through.
    pub env_passthrough: Vec<String>,
}

impl Default for ToolSandboxPolicy {
    fn default() -> Self {
        Self {
            required_level: IsolationLevel::PathScope,
            resource_limits: ResourceLimits::default(),
            allow_network: false,
            extra_paths: Vec::new(),
            env_passthrough: Vec::new(),
        }
    }
}

/// Platform sandbox capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformCapabilities {
    /// Maximum isolation level available on this platform.
    pub max_level: IsolationLevel,
    /// Whether macOS seatbelt (sandbox-exec) is available.
    pub has_seatbelt: bool,
    /// Whether Linux seccomp-bpf is available.
    pub has_seccomp: bool,
    /// Whether Linux namespaces (unshare) are available.
    pub has_namespaces: bool,
    /// Whether Docker is available for container sandbox.
    pub has_docker: bool,
    /// Whether setrlimit is available.
    pub has_rlimit: bool,
}

impl PlatformCapabilities {
    /// Detect capabilities for the current platform.
    pub fn detect() -> Self {
        let mut caps = Self {
            max_level: IsolationLevel::PathScope, // Always available
            has_seatbelt: false,
            has_seccomp: false,
            has_namespaces: false,
            has_docker: false,
            has_rlimit: false,
        };

        #[cfg(target_os = "macos")]
        {
            caps.has_seatbelt = Self::check_command("sandbox-exec");
            caps.has_rlimit = true; // POSIX
            if caps.has_seatbelt {
                caps.max_level = IsolationLevel::FullSandbox;
            } else {
                caps.max_level = IsolationLevel::ProcessIsolation;
            }
        }

        #[cfg(target_os = "linux")]
        {
            caps.has_seccomp = std::path::Path::new("/proc/sys/kernel/seccomp").exists();
            caps.has_namespaces = std::path::Path::new("/proc/self/ns").exists();
            caps.has_rlimit = true; // POSIX
            if caps.has_seccomp && caps.has_namespaces {
                caps.max_level = IsolationLevel::FullSandbox;
            } else if caps.has_seccomp || caps.has_namespaces {
                caps.max_level = IsolationLevel::ProcessIsolation;
            }
        }

        // Docker check (platform-independent)
        caps.has_docker = Self::check_command("docker");

        info!(
            max_level = %caps.max_level,
            seatbelt = caps.has_seatbelt,
            seccomp = caps.has_seccomp,
            namespaces = caps.has_namespaces,
            docker = caps.has_docker,
            "Platform sandbox capabilities detected"
        );

        caps
    }

    fn check_command(name: &str) -> bool {
        std::process::Command::new("which")
            .arg(name)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// Sandbox policy engine — decides isolation level per tool.
pub struct SandboxPolicyEngine {
    /// Default policy for tools without explicit policy.
    default_policy: ToolSandboxPolicy,
    /// Per-tool policy overrides.
    tool_policies: HashMap<String, ToolSandboxPolicy>,
    /// Platform capabilities.
    platform: PlatformCapabilities,
}

/// Decision from the policy engine.
#[derive(Debug, Clone)]
pub enum SandboxDecision {
    /// Tool is allowed at the specified isolation level.
    Allow {
        level: IsolationLevel,
        limits: ResourceLimits,
        allow_network: bool,
    },
    /// Tool is blocked because required isolation exceeds platform capability.
    Block {
        required: IsolationLevel,
        available: IsolationLevel,
        tool_name: String,
    },
}

impl SandboxPolicyEngine {
    /// Create a new policy engine with auto-detected platform capabilities.
    pub fn new() -> Self {
        Self {
            default_policy: ToolSandboxPolicy::default(),
            tool_policies: Self::builtin_policies(),
            platform: PlatformCapabilities::detect(),
        }
    }

    /// Create with explicit platform capabilities (for testing).
    pub fn with_platform(platform: PlatformCapabilities) -> Self {
        Self {
            default_policy: ToolSandboxPolicy::default(),
            tool_policies: Self::builtin_policies(),
            platform,
        }
    }

    /// Built-in policies for common tools.
    fn builtin_policies() -> HashMap<String, ToolSandboxPolicy> {
        let mut policies = HashMap::new();

        // bash/shell execution — requires full sandbox
        policies.insert(
            "bash".to_string(),
            ToolSandboxPolicy {
                required_level: IsolationLevel::FullSandbox,
                resource_limits: ResourceLimits {
                    cpu_time_secs: 30,
                    wall_time_secs: 120,
                    memory_bytes: 512 * 1024 * 1024,
                    max_output_bytes: 50 * 1024 * 1024,
                    ..Default::default()
                },
                allow_network: false,
                extra_paths: Vec::new(),
                env_passthrough: vec!["PATH".to_string(), "HOME".to_string()],
            },
        );

        // shell_exec — same as bash (Skills=Prompts architecture uses this tool name)
        policies.insert(
            "shell_exec".to_string(),
            ToolSandboxPolicy {
                required_level: IsolationLevel::FullSandbox,
                resource_limits: ResourceLimits {
                    cpu_time_secs: 30,
                    wall_time_secs: 120,
                    memory_bytes: 512 * 1024 * 1024,
                    max_output_bytes: 50 * 1024 * 1024,
                    ..Default::default()
                },
                allow_network: true,  // skills may need network (e.g., bear, memo sync)
                extra_paths: Vec::new(),
                env_passthrough: vec!["PATH".to_string(), "HOME".to_string()],
            },
        );

        // http_fetch — needs network, process isolation
        policies.insert(
            "http_fetch".to_string(),
            ToolSandboxPolicy {
                required_level: IsolationLevel::ProcessIsolation,
                resource_limits: ResourceLimits {
                    cpu_time_secs: 15,
                    wall_time_secs: 30,
                    memory_bytes: 128 * 1024 * 1024,
                    ..Default::default()
                },
                allow_network: true,
                extra_paths: Vec::new(),
                env_passthrough: Vec::new(),
            },
        );

        // web_search — needs network, process isolation
        policies.insert(
            "web_search".to_string(),
            ToolSandboxPolicy {
                required_level: IsolationLevel::ProcessIsolation,
                resource_limits: ResourceLimits {
                    cpu_time_secs: 15,
                    wall_time_secs: 30,
                    memory_bytes: 128 * 1024 * 1024,
                    ..Default::default()
                },
                allow_network: true,
                extra_paths: Vec::new(),
                env_passthrough: Vec::new(),
            },
        );

        // File read — path scope only
        policies.insert(
            "read_file".to_string(),
            ToolSandboxPolicy {
                required_level: IsolationLevel::PathScope,
                resource_limits: ResourceLimits {
                    cpu_time_secs: 5,
                    wall_time_secs: 10,
                    memory_bytes: 64 * 1024 * 1024,
                    ..Default::default()
                },
                allow_network: false,
                extra_paths: Vec::new(),
                env_passthrough: Vec::new(),
            },
        );

        // File write — path scope
        policies.insert(
            "write_file".to_string(),
            ToolSandboxPolicy {
                required_level: IsolationLevel::PathScope,
                resource_limits: ResourceLimits {
                    cpu_time_secs: 5,
                    wall_time_secs: 10,
                    memory_bytes: 64 * 1024 * 1024,
                    ..Default::default()
                },
                allow_network: false,
                extra_paths: Vec::new(),
                env_passthrough: Vec::new(),
            },
        );

        // Web fetch — needs network, process isolation
        policies.insert(
            "fetch".to_string(),
            ToolSandboxPolicy {
                required_level: IsolationLevel::ProcessIsolation,
                resource_limits: ResourceLimits {
                    cpu_time_secs: 15,
                    wall_time_secs: 30,
                    memory_bytes: 128 * 1024 * 1024,
                    ..Default::default()
                },
                allow_network: true,
                extra_paths: Vec::new(),
                env_passthrough: Vec::new(),
            },
        );

        policies
    }

    /// Decide the sandbox level for a tool.
    ///
    /// Lattice meet operation:
    /// effective = max(required, policy_override)
    /// if effective > available → BLOCK
    pub fn decide(&self, tool_name: &str) -> SandboxDecision {
        let policy = self
            .tool_policies
            .get(tool_name)
            .unwrap_or(&self.default_policy);

        let required = policy.required_level;

        if required > self.platform.max_level {
            debug!(
                tool = tool_name,
                required = %required,
                available = %self.platform.max_level,
                "Tool blocked: required isolation exceeds platform capability"
            );
            return SandboxDecision::Block {
                required,
                available: self.platform.max_level,
                tool_name: tool_name.to_string(),
            };
        }

        // Use the effective level (at least what's required)
        let effective = required;

        SandboxDecision::Allow {
            level: effective,
            limits: policy.resource_limits.clone(),
            allow_network: policy.allow_network,
        }
    }

    /// Set a custom policy for a tool.
    pub fn set_policy(&mut self, tool_name: impl Into<String>, policy: ToolSandboxPolicy) {
        self.tool_policies.insert(tool_name.into(), policy);
    }

    /// Get the platform capabilities.
    pub fn platform(&self) -> &PlatformCapabilities {
        &self.platform
    }
}

impl Default for SandboxPolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Exponentially weighted moving average for output size estimation.
///
/// EWMA_n = α × x_n + (1 - α) × EWMA_{n-1}
///
/// Used to estimate expected tool output size for memory limit calculation.
pub struct OutputSizeEstimator {
    /// Current EWMA value.
    ewma: f64,
    /// Smoothing factor (0 < α < 1).
    alpha: f64,
    /// Number of observations.
    observations: u64,
    /// Maximum observed value (for P95 approximation).
    max_observed: u64,
}

impl OutputSizeEstimator {
    pub fn new(alpha: f64) -> Self {
        Self {
            ewma: 0.0,
            alpha,
            observations: 0,
            max_observed: 0,
        }
    }

    /// Record an observation.
    pub fn observe(&mut self, size: u64) {
        if self.observations == 0 {
            self.ewma = size as f64;
        } else {
            self.ewma = self.alpha * size as f64 + (1.0 - self.alpha) * self.ewma;
        }
        self.observations += 1;
        self.max_observed = self.max_observed.max(size);
    }

    /// Get the EWMA estimate.
    pub fn estimate(&self) -> u64 {
        self.ewma as u64
    }

    /// Get estimated memory limit: base + EWMA × safety_factor.
    pub fn memory_limit(&self, base: u64, safety_factor: f64) -> u64 {
        base + (self.ewma * safety_factor) as u64
    }

    /// Number of observations.
    pub fn observations(&self) -> u64 {
        self.observations
    }
}

impl Default for OutputSizeEstimator {
    fn default() -> Self {
        Self::new(0.1)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// D4: Sandbox Hardening — Network ACL, Path Validation, Rate Limiting
// ═══════════════════════════════════════════════════════════════════════════

/// Network ACL rule for sandboxed tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkAclRule {
    /// Domain or CIDR pattern to match.
    pub pattern: String,
    /// Whether this is an allow or deny rule.
    pub action: NetworkAction,
    /// Optional port restriction.
    pub ports: Option<Vec<u16>>,
    /// Protocol restriction (tcp/udp/any).
    pub protocol: String,
}

/// Network ACL action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAction {
    Allow,
    Deny,
}

/// Network ACL — controls which hosts a sandboxed tool may connect to.
///
/// Rules are evaluated top-to-bottom. First matching rule wins.
/// Default policy is deny-all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkAcl {
    /// Ordered list of rules.
    pub rules: Vec<NetworkAclRule>,
    /// Default action when no rule matches.
    pub default_action: NetworkAction,
}

impl Default for NetworkAcl {
    fn default() -> Self {
        Self {
            rules: vec![
                // Allow known LLM API endpoints
                NetworkAclRule {
                    pattern: "api.anthropic.com".into(),
                    action: NetworkAction::Allow,
                    ports: Some(vec![443]),
                    protocol: "tcp".into(),
                },
                NetworkAclRule {
                    pattern: "api.openai.com".into(),
                    action: NetworkAction::Allow,
                    ports: Some(vec![443]),
                    protocol: "tcp".into(),
                },
                NetworkAclRule {
                    pattern: "generativelanguage.googleapis.com".into(),
                    action: NetworkAction::Allow,
                    ports: Some(vec![443]),
                    protocol: "tcp".into(),
                },
                // Block RFC1918 private addresses
                NetworkAclRule {
                    pattern: "10.0.0.0/8".into(),
                    action: NetworkAction::Deny,
                    ports: None,
                    protocol: "any".into(),
                },
                NetworkAclRule {
                    pattern: "172.16.0.0/12".into(),
                    action: NetworkAction::Deny,
                    ports: None,
                    protocol: "any".into(),
                },
                NetworkAclRule {
                    pattern: "192.168.0.0/16".into(),
                    action: NetworkAction::Deny,
                    ports: None,
                    protocol: "any".into(),
                },
                // Block link-local
                NetworkAclRule {
                    pattern: "169.254.0.0/16".into(),
                    action: NetworkAction::Deny,
                    ports: None,
                    protocol: "any".into(),
                },
                // Block localhost
                NetworkAclRule {
                    pattern: "127.0.0.0/8".into(),
                    action: NetworkAction::Deny,
                    ports: None,
                    protocol: "any".into(),
                },
            ],
            default_action: NetworkAction::Deny,
        }
    }
}

impl NetworkAcl {
    /// Check if a connection to the given host and port is allowed.
    ///
    /// Evaluates rules in order; first match wins.
    pub fn check(&self, host: &str, port: u16) -> NetworkAction {
        for rule in &self.rules {
            if Self::host_matches(&rule.pattern, host) {
                // Check port restriction
                if let Some(allowed_ports) = &rule.ports {
                    if !allowed_ports.contains(&port) {
                        continue; // Port doesn't match, try next rule
                    }
                }
                debug!(
                    host,
                    port,
                    action = ?rule.action,
                    "Network ACL rule matched"
                );
                return rule.action;
            }
        }
        self.default_action
    }

    /// Simple host matching (exact or suffix for domain patterns).
    fn host_matches(pattern: &str, host: &str) -> bool {
        if pattern == host {
            return true;
        }
        // CIDR notation — for now just match the prefix portion
        if pattern.contains('/') {
            let prefix = pattern.split('/').next().unwrap_or("");
            // Simple check: if host starts with the network prefix
            if host.starts_with(prefix.trim_end_matches(".0")) {
                return true;
            }
        }
        false
    }
}

/// Path validator — ensures filesystem access stays within workspace bounds.
#[derive(Debug, Clone)]
pub struct PathValidator {
    /// Workspace root (all access must be within this directory).
    workspace_root: String,
    /// Additional allowed paths outside the workspace.
    allowed_paths: Vec<String>,
    /// Patterns that are always blocked.
    blocked_patterns: Vec<String>,
}

impl PathValidator {
    /// Create a new path validator for the given workspace root.
    pub fn new(workspace_root: impl Into<String>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            allowed_paths: Vec::new(),
            blocked_patterns: vec![
                "..".into(),                  // Path traversal
                "~".into(),                   // Home directory escape
                "/etc/shadow".into(),         // Sensitive system files
                "/etc/passwd".into(),
                ".ssh".into(),
                ".gnupg".into(),
                ".aws/credentials".into(),
                ".env".into(),                // Env files with secrets
                "id_rsa".into(),
                "id_ed25519".into(),
            ],
        }
    }

    /// Add an allowed path outside the workspace.
    pub fn allow_path(&mut self, path: impl Into<String>) {
        self.allowed_paths.push(path.into());
    }

    /// Validate that a file path is safe to access.
    pub fn validate(&self, path: &str) -> PathValidation {
        // Check blocked patterns
        for pattern in &self.blocked_patterns {
            if path.contains(pattern.as_str()) {
                warn!(path, pattern, "Path blocked by pattern");
                return PathValidation::Blocked {
                    path: path.to_string(),
                    reason: format!("contains blocked pattern: {pattern}"),
                };
            }
        }

        // Check if within workspace
        if path.starts_with(&self.workspace_root) {
            return PathValidation::Allowed;
        }

        // Check extra allowed paths
        for allowed in &self.allowed_paths {
            if path.starts_with(allowed.as_str()) {
                return PathValidation::Allowed;
            }
        }

        PathValidation::Blocked {
            path: path.to_string(),
            reason: "path is outside workspace and allowed paths".into(),
        }
    }
}

/// Result of path validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathValidation {
    /// Access is allowed.
    Allowed,
    /// Access is blocked.
    Blocked { path: String, reason: String },
}

/// Tool invocation rate limiter.
///
/// Uses a simple token bucket algorithm:
/// - Each tool gets `capacity` tokens.
/// - Each invocation costs 1 token.
/// - Tokens refill at `refill_rate` per second.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Maximum invocations per tool before throttling.
    pub capacity: u32,
    /// Number of tokens restored per second.
    pub refill_rate: f64,
    /// Per-tool overrides (tool name → capacity).
    pub overrides: HashMap<String, u32>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        let mut overrides = HashMap::new();
        // bash gets stricter limits
        overrides.insert("bash".into(), 5);
        // read_file is more permissive
        overrides.insert("read_file".into(), 100);

        Self {
            capacity: 30,
            refill_rate: 1.0,
            overrides,
        }
    }
}

impl RateLimitConfig {
    /// Get the capacity for a specific tool.
    pub fn capacity_for(&self, tool_name: &str) -> u32 {
        self.overrides
            .get(tool_name)
            .copied()
            .unwrap_or(self.capacity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_isolation_level_ordering() {
        assert!(IsolationLevel::None < IsolationLevel::PathScope);
        assert!(IsolationLevel::PathScope < IsolationLevel::ProcessIsolation);
        assert!(IsolationLevel::ProcessIsolation < IsolationLevel::FullSandbox);
    }

    #[test]
    fn test_isolation_satisfies() {
        assert!(IsolationLevel::FullSandbox.satisfies(IsolationLevel::PathScope));
        assert!(!IsolationLevel::PathScope.satisfies(IsolationLevel::FullSandbox));
        assert!(IsolationLevel::PathScope.satisfies(IsolationLevel::PathScope));
    }

    #[test]
    fn test_policy_engine_allows_read_file() {
        let platform = PlatformCapabilities {
            max_level: IsolationLevel::FullSandbox,
            has_seatbelt: true,
            has_seccomp: false,
            has_namespaces: false,
            has_docker: false,
            has_rlimit: true,
        };
        let engine = SandboxPolicyEngine::with_platform(platform);

        match engine.decide("read_file") {
            SandboxDecision::Allow { level, .. } => {
                assert_eq!(level, IsolationLevel::PathScope);
            }
            SandboxDecision::Block { .. } => panic!("read_file should be allowed"),
        }
    }

    #[test]
    fn test_policy_engine_blocks_bash_on_weak_platform() {
        let platform = PlatformCapabilities {
            max_level: IsolationLevel::PathScope,
            has_seatbelt: false,
            has_seccomp: false,
            has_namespaces: false,
            has_docker: false,
            has_rlimit: false,
        };
        let engine = SandboxPolicyEngine::with_platform(platform);

        match engine.decide("bash") {
            SandboxDecision::Block { required, available, .. } => {
                assert_eq!(required, IsolationLevel::FullSandbox);
                assert_eq!(available, IsolationLevel::PathScope);
            }
            SandboxDecision::Allow { .. } => panic!("bash should be blocked on weak platform"),
        }
    }

    #[test]
    fn test_policy_engine_unknown_tool_uses_default() {
        let platform = PlatformCapabilities {
            max_level: IsolationLevel::FullSandbox,
            has_seatbelt: true,
            has_seccomp: false,
            has_namespaces: false,
            has_docker: false,
            has_rlimit: true,
        };
        let engine = SandboxPolicyEngine::with_platform(platform);

        match engine.decide("my_custom_tool") {
            SandboxDecision::Allow { level, .. } => {
                assert_eq!(level, IsolationLevel::PathScope); // default
            }
            SandboxDecision::Block { .. } => panic!("unknown tool should use default"),
        }
    }

    #[test]
    fn test_output_size_estimator() {
        let mut est = OutputSizeEstimator::new(0.1);

        est.observe(1000);
        assert_eq!(est.estimate(), 1000);

        est.observe(2000);
        // EWMA = 0.1 * 2000 + 0.9 * 1000 = 1100
        assert_eq!(est.estimate(), 1100);

        est.observe(1500);
        // EWMA = 0.1 * 1500 + 0.9 * 1100 = 1140
        assert_eq!(est.estimate(), 1140);

        assert_eq!(est.observations(), 3);
    }

    #[test]
    fn test_memory_limit_calculation() {
        let mut est = OutputSizeEstimator::new(0.1);
        est.observe(10_000);

        let limit = est.memory_limit(1_000_000, 2.0);
        assert_eq!(limit, 1_000_000 + 20_000); // base + ewma * safety
    }

    #[test]
    fn test_custom_policy() {
        let platform = PlatformCapabilities {
            max_level: IsolationLevel::FullSandbox,
            has_seatbelt: true,
            has_seccomp: false,
            has_namespaces: false,
            has_docker: false,
            has_rlimit: true,
        };
        let mut engine = SandboxPolicyEngine::with_platform(platform);

        engine.set_policy(
            "my_tool",
            ToolSandboxPolicy {
                required_level: IsolationLevel::ProcessIsolation,
                allow_network: true,
                ..Default::default()
            },
        );

        match engine.decide("my_tool") {
            SandboxDecision::Allow {
                level,
                allow_network,
                ..
            } => {
                assert_eq!(level, IsolationLevel::ProcessIsolation);
                assert!(allow_network);
            }
            _ => panic!("custom tool should be allowed"),
        }
    }

    // ── D4: Sandbox Hardening tests ──

    #[test]
    fn test_network_acl_allows_known_api() {
        let acl = NetworkAcl::default();
        assert_eq!(acl.check("api.anthropic.com", 443), NetworkAction::Allow);
        assert_eq!(acl.check("api.openai.com", 443), NetworkAction::Allow);
    }

    #[test]
    fn test_network_acl_blocks_wrong_port() {
        let acl = NetworkAcl::default();
        // Port 80 is not in the allowed ports for anthropic
        assert_eq!(acl.check("api.anthropic.com", 80), NetworkAction::Deny);
    }

    #[test]
    fn test_network_acl_blocks_unknown_host() {
        let acl = NetworkAcl::default();
        assert_eq!(acl.check("evil.example.com", 443), NetworkAction::Deny);
    }

    #[test]
    fn test_network_acl_blocks_localhost() {
        let acl = NetworkAcl::default();
        assert_eq!(acl.check("127.0.0.1", 8080), NetworkAction::Deny);
    }

    #[test]
    fn test_path_validator_allows_workspace() {
        let v = PathValidator::new("/home/user/project");
        assert_eq!(
            v.validate("/home/user/project/src/main.rs"),
            PathValidation::Allowed
        );
    }

    #[test]
    fn test_path_validator_blocks_outside() {
        let v = PathValidator::new("/home/user/project");
        assert!(matches!(
            v.validate("/etc/passwd"),
            PathValidation::Blocked { .. }
        ));
    }

    #[test]
    fn test_path_validator_blocks_traversal() {
        let v = PathValidator::new("/home/user/project");
        assert!(matches!(
            v.validate("/home/user/project/../../../etc/shadow"),
            PathValidation::Blocked { .. }
        ));
    }

    #[test]
    fn test_path_validator_blocks_ssh() {
        let v = PathValidator::new("/home/user/project");
        assert!(matches!(
            v.validate("/home/user/.ssh/id_rsa"),
            PathValidation::Blocked { .. }
        ));
    }

    #[test]
    fn test_path_validator_allows_extra_path() {
        let mut v = PathValidator::new("/home/user/project");
        v.allow_path("/usr/share/dict");
        assert_eq!(
            v.validate("/usr/share/dict/words"),
            PathValidation::Allowed
        );
    }

    #[test]
    fn test_rate_limit_default_capacity() {
        let config = RateLimitConfig::default();
        assert_eq!(config.capacity_for("unknown_tool"), 30);
    }

    #[test]
    fn test_rate_limit_override_capacity() {
        let config = RateLimitConfig::default();
        assert_eq!(config.capacity_for("bash"), 5);
        assert_eq!(config.capacity_for("read_file"), 100);
    }
}
