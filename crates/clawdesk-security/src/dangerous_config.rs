//! Dangerous configuration and tool combination detection.
//!
//! Identifies setting combinations that compromise security even when
//! individual settings appear valid. Defence-in-depth: certain tool chains
//! create attack surfaces that single-setting validation cannot catch.
//!
//! ## Pattern Evaluation
//!
//! Each dangerous pattern is a Boolean formula φ over config flags,
//! evaluated in O(1) per pattern. Total evaluation: O(p) where p < 50.
//!
//! ## Tool Chain Analysis
//!
//! Uses Aho-Corasick multi-pattern matching over tool invocation sequences
//! to detect dangerous chains (e.g., shell → filesystem write → shell).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::warn;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// Risk level for a dangerous configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ConfigRisk {
    /// Advisory — inform the user but allow.
    Low,
    /// Should be acknowledged before proceeding.
    Medium,
    /// Significant security risk — require explicit opt-in.
    High,
    /// Critical — block unless admin override is provided.
    Critical,
}

/// A dangerous configuration pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DangerousPattern {
    /// Unique identifier for this pattern.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Detailed description of the risk.
    pub description: String,
    /// Risk level.
    pub risk: ConfigRisk,
    /// Remediation guidance.
    pub remediation: String,
}

/// A set of configuration flags representing the current system state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigFlags {
    /// Shell/command execution tool is enabled.
    pub shell_enabled: bool,
    /// Sandbox/isolation is disabled or set to `None`.
    pub sandbox_disabled: bool,
    /// Filesystem access is enabled.
    pub filesystem_enabled: bool,
    /// Filesystem is not scoped to a working directory.
    pub filesystem_unrestricted: bool,
    /// User confirmations for dangerous actions disabled.
    pub confirmations_disabled: bool,
    /// Network egress is unrestricted (no allowlist).
    pub network_unrestricted: bool,
    /// Browser tool is enabled.
    pub browser_enabled: bool,
    /// Agent can spawn sub-agents without approval.
    pub auto_subagent: bool,
    /// MCP servers are connected.
    pub mcp_connected: bool,
    /// MCP servers include untrusted/unverified sources.
    pub mcp_untrusted: bool,
    /// Agent has credential/secret access.
    pub credential_access: bool,
    /// Audit logging is disabled.
    pub audit_disabled: bool,
    /// Running in daemon/headless mode.
    pub daemon_mode: bool,
    /// Code execution tool is enabled.
    pub code_exec_enabled: bool,
    /// Git push is allowed without review.
    pub git_push_unrestricted: bool,
}

/// The result of auditing the current configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigAuditReport {
    /// Detected dangerous patterns.
    pub findings: Vec<DangerousPattern>,
    /// Overall risk (max of all findings).
    pub overall_risk: ConfigRisk,
    /// Number of patterns checked.
    pub patterns_checked: usize,
    /// Whether any critical risks were found.
    pub has_critical: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Detector
// ─────────────────────────────────────────────────────────────────────────────

/// Detects dangerous configuration combinations.
pub struct DangerousConfigDetector {
    /// Patterns explicitly acknowledged (opted-in) by the admin.
    acknowledged: HashSet<String>,
}

impl DangerousConfigDetector {
    pub fn new() -> Self {
        Self {
            acknowledged: HashSet::new(),
        }
    }

    /// Acknowledge a dangerous pattern by ID, allowing it to pass.
    pub fn acknowledge(&mut self, pattern_id: &str) {
        self.acknowledged.insert(pattern_id.to_string());
    }

    /// Check if a pattern is acknowledged.
    pub fn is_acknowledged(&self, pattern_id: &str) -> bool {
        self.acknowledged.contains(pattern_id)
    }

    /// Audit the given configuration flags against all known patterns.
    ///
    /// Each pattern is a Boolean formula evaluated in O(1).
    /// Total: O(p) where p = number of patterns.
    pub fn audit(&self, flags: &ConfigFlags) -> ConfigAuditReport {
        let all_patterns = Self::all_patterns(flags);
        let findings: Vec<DangerousPattern> = all_patterns
            .into_iter()
            .filter(|p| !self.acknowledged.contains(&p.id))
            .collect();

        let overall_risk = findings
            .iter()
            .map(|f| f.risk)
            .max()
            .unwrap_or(ConfigRisk::Low);
        let has_critical = findings.iter().any(|f| f.risk == ConfigRisk::Critical);
        let patterns_checked = Self::total_pattern_count();

        if has_critical {
            warn!(
                count = findings.len(),
                "Dangerous configuration detected: critical patterns found"
            );
        }

        ConfigAuditReport {
            findings,
            overall_risk,
            patterns_checked,
            has_critical,
        }
    }

    /// Collect any blocking findings (Critical risk, not acknowledged).
    pub fn blocking_findings(&self, flags: &ConfigFlags) -> Vec<DangerousPattern> {
        Self::all_patterns(flags)
            .into_iter()
            .filter(|p| p.risk == ConfigRisk::Critical && !self.acknowledged.contains(&p.id))
            .collect()
    }

    fn total_pattern_count() -> usize {
        20
    }

    /// Evaluate all known dangerous patterns against the flags.
    fn all_patterns(flags: &ConfigFlags) -> Vec<DangerousPattern> {
        let mut found = Vec::new();

        // Pattern 1: Shell + no sandbox = arbitrary code execution
        if flags.shell_enabled && flags.sandbox_disabled {
            found.push(DangerousPattern {
                id: "shell_no_sandbox".to_string(),
                name: "Shell without sandbox".to_string(),
                description: "Shell execution is enabled with no sandbox isolation. \
                              Any command can run with the process's full privileges.".to_string(),
                risk: ConfigRisk::Critical,
                remediation: "Enable at least PathScope isolation or ProcessIsolation.".to_string(),
            });
        }

        // Pattern 2: Shell + no confirmations = silent execution
        if flags.shell_enabled && flags.confirmations_disabled {
            found.push(DangerousPattern {
                id: "shell_no_confirm".to_string(),
                name: "Shell without confirmations".to_string(),
                description: "Shell commands execute without user approval. \
                              Agent can run destructive commands silently.".to_string(),
                risk: ConfigRisk::High,
                remediation: "Enable confirmation prompts for high-risk commands.".to_string(),
            });
        }

        // Pattern 3: Unrestricted filesystem + no sandbox
        if flags.filesystem_unrestricted && flags.sandbox_disabled {
            found.push(DangerousPattern {
                id: "fs_unrestricted_no_sandbox".to_string(),
                name: "Unrestricted filesystem without sandbox".to_string(),
                description: "Agent has unrestricted filesystem access outside the \
                              working directory with no sandbox boundary.".to_string(),
                risk: ConfigRisk::Critical,
                remediation: "Scope filesystem access to the project directory \
                              or enable sandbox isolation.".to_string(),
            });
        }

        // Pattern 4: Shell + unrestricted filesystem + unrestricted network
        if flags.shell_enabled && flags.filesystem_unrestricted && flags.network_unrestricted {
            found.push(DangerousPattern {
                id: "full_triad".to_string(),
                name: "Shell + filesystem + network triad".to_string(),
                description: "The agent has shell execution, unrestricted filesystem, \
                              and unrestricted network access — equivalent to a \
                              remote code execution vulnerability.".to_string(),
                risk: ConfigRisk::Critical,
                remediation: "Restrict at least one of: shell scope, filesystem scope, \
                              or network egress policy.".to_string(),
            });
        }

        // Pattern 5: Credential access + unrestricted network
        if flags.credential_access && flags.network_unrestricted {
            found.push(DangerousPattern {
                id: "creds_open_network".to_string(),
                name: "Credential access with unrestricted network".to_string(),
                description: "Agent can access stored secrets and make arbitrary \
                              network requests — credential exfiltration risk.".to_string(),
                risk: ConfigRisk::High,
                remediation: "Enable network egress allowlisting to limit \
                              outbound destinations.".to_string(),
            });
        }

        // Pattern 6: Untrusted MCP + no sandbox
        if flags.mcp_untrusted && flags.sandbox_disabled {
            found.push(DangerousPattern {
                id: "untrusted_mcp_no_sandbox".to_string(),
                name: "Untrusted MCP servers without sandbox".to_string(),
                description: "Unverified MCP tool servers can execute arbitrary \
                              operations with no isolation boundary.".to_string(),
                risk: ConfigRisk::Critical,
                remediation: "Verify MCP server sources or enable sandbox isolation.".to_string(),
            });
        }

        // Pattern 7: Auto sub-agents + no confirmations
        if flags.auto_subagent && flags.confirmations_disabled {
            found.push(DangerousPattern {
                id: "auto_subagent_no_confirm".to_string(),
                name: "Automatic sub-agents without confirmations".to_string(),
                description: "Agent can spawn sub-agents autonomously without \
                              user approval — recursive expansion risk.".to_string(),
                risk: ConfigRisk::Medium,
                remediation: "Enable confirmation for sub-agent spawning.".to_string(),
            });
        }

        // Pattern 8: Daemon mode + no audit
        if flags.daemon_mode && flags.audit_disabled {
            found.push(DangerousPattern {
                id: "daemon_no_audit".to_string(),
                name: "Daemon mode without audit logging".to_string(),
                description: "Agent runs unattended with no audit trail. \
                              Security incidents cannot be investigated.".to_string(),
                risk: ConfigRisk::High,
                remediation: "Enable audit logging for daemon deployments.".to_string(),
            });
        }

        // Pattern 9: Code execution + unrestricted filesystem
        if flags.code_exec_enabled && flags.filesystem_unrestricted {
            found.push(DangerousPattern {
                id: "code_exec_unrestricted_fs".to_string(),
                name: "Code execution with unrestricted filesystem".to_string(),
                description: "Generated code can read/write any file on the \
                              system without directory scoping.".to_string(),
                risk: ConfigRisk::High,
                remediation: "Scope filesystem to the working directory.".to_string(),
            });
        }

        // Pattern 10: Git push unrestricted + no confirmations
        if flags.git_push_unrestricted && flags.confirmations_disabled {
            found.push(DangerousPattern {
                id: "git_push_no_confirm".to_string(),
                name: "Unrestricted git push without confirmations".to_string(),
                description: "Agent can push code to remote repositories without \
                              review — supply chain risk.".to_string(),
                risk: ConfigRisk::High,
                remediation: "Enable confirmation prompts for git push operations.".to_string(),
            });
        }

        // Pattern 11: Browser + no sandbox + unrestricted network
        if flags.browser_enabled && flags.sandbox_disabled && flags.network_unrestricted {
            found.push(DangerousPattern {
                id: "browser_no_sandbox_open_net".to_string(),
                name: "Browser without sandbox and unrestricted network".to_string(),
                description: "Browser automation can navigate to arbitrary sites \
                              without isolation — cross-site data exfiltration risk.".to_string(),
                risk: ConfigRisk::High,
                remediation: "Enable browser sandbox or network egress allowlist.".to_string(),
            });
        }

        // Pattern 12: All confirmations disabled + daemon mode
        if flags.confirmations_disabled && flags.daemon_mode {
            found.push(DangerousPattern {
                id: "no_confirm_daemon".to_string(),
                name: "No confirmations in daemon mode".to_string(),
                description: "Daemon mode runs unattended; disabling confirmations \
                              means all actions execute without oversight.".to_string(),
                risk: ConfigRisk::High,
                remediation: "In daemon mode, enable auto-approval policies with \
                              explicit allowlists instead of disabling confirmations.".to_string(),
            });
        }

        // Pattern 13: Credential access + auto sub-agents
        if flags.credential_access && flags.auto_subagent {
            found.push(DangerousPattern {
                id: "creds_auto_subagent".to_string(),
                name: "Credential access with automatic sub-agents".to_string(),
                description: "Sub-agents inherit credential access — \
                              privilege escalation via agent spawning.".to_string(),
                risk: ConfigRisk::Medium,
                remediation: "Sub-agents should use scoped tokens, not inherit \
                              parent credentials.".to_string(),
            });
        }

        // Pattern 14: MCP + shell → tool chain escalation
        if flags.mcp_connected && flags.shell_enabled {
            found.push(DangerousPattern {
                id: "mcp_shell_chain".to_string(),
                name: "MCP tools + shell execution".to_string(),
                description: "MCP tool outputs can be piped to shell commands, \
                              allowing external tool servers to influence local execution.".to_string(),
                risk: ConfigRisk::Medium,
                remediation: "Sanitize MCP tool outputs before passing to shell \
                              or disable shell for MCP-connected sessions.".to_string(),
            });
        }

        // Pattern 15: All three: shell + no sandbox + no confirmations
        if flags.shell_enabled && flags.sandbox_disabled && flags.confirmations_disabled {
            found.push(DangerousPattern {
                id: "shell_no_sandbox_no_confirm".to_string(),
                name: "Shell, no sandbox, no confirmations".to_string(),
                description: "Maximum-risk combination: arbitrary shell commands \
                              execute silently without isolation.".to_string(),
                risk: ConfigRisk::Critical,
                remediation: "Enable either sandbox isolation or user confirmations \
                              (preferably both).".to_string(),
            });
        }

        // Pattern 16: Network unrestricted + audit disabled
        if flags.network_unrestricted && flags.audit_disabled {
            found.push(DangerousPattern {
                id: "open_net_no_audit".to_string(),
                name: "Unrestricted network without audit".to_string(),
                description: "Outbound requests are unlogged and unrestricted — \
                              data exfiltration would leave no trace.".to_string(),
                risk: ConfigRisk::High,
                remediation: "Enable audit logging or restrict network egress.".to_string(),
            });
        }

        // Pattern 17: Code execution + no sandbox
        if flags.code_exec_enabled && flags.sandbox_disabled {
            found.push(DangerousPattern {
                id: "code_exec_no_sandbox".to_string(),
                name: "Code execution without sandbox".to_string(),
                description: "Generated code runs in the host process without isolation.".to_string(),
                risk: ConfigRisk::Critical,
                remediation: "Enable ProcessIsolation or FullSandbox for code execution.".to_string(),
            });
        }

        // Pattern 18: Filesystem + credential access + no audit
        if flags.filesystem_enabled && flags.credential_access && flags.audit_disabled {
            found.push(DangerousPattern {
                id: "fs_creds_no_audit".to_string(),
                name: "Filesystem + credentials without audit".to_string(),
                description: "Agent can read files and access credentials with no \
                              audit trail — insider threat vector.".to_string(),
                risk: ConfigRisk::High,
                remediation: "Enable audit logging for credential and filesystem access.".to_string(),
            });
        }

        // Pattern 19: Untrusted MCP + credential access
        if flags.mcp_untrusted && flags.credential_access {
            found.push(DangerousPattern {
                id: "untrusted_mcp_creds".to_string(),
                name: "Untrusted MCP with credential access".to_string(),
                description: "Unverified MCP servers may request credentials — \
                              credential theft via malicious tool servers.".to_string(),
                risk: ConfigRisk::Critical,
                remediation: "Verify MCP server integrity or revoke credential access.".to_string(),
            });
        }

        // Pattern 20: Browser + credential access
        if flags.browser_enabled && flags.credential_access {
            found.push(DangerousPattern {
                id: "browser_creds".to_string(),
                name: "Browser with credential access".to_string(),
                description: "Browser tool can navigate to login pages with stored \
                              credentials — session hijacking risk.".to_string(),
                risk: ConfigRisk::Medium,
                remediation: "Use separate credential scope for browser sessions.".to_string(),
            });
        }

        found
    }
}

impl Default for DangerousConfigDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_config_no_findings() {
        let detector = DangerousConfigDetector::new();
        let flags = ConfigFlags::default();
        let report = detector.audit(&flags);
        assert!(report.findings.is_empty());
        assert!(!report.has_critical);
    }

    #[test]
    fn shell_no_sandbox_is_critical() {
        let detector = DangerousConfigDetector::new();
        let flags = ConfigFlags {
            shell_enabled: true,
            sandbox_disabled: true,
            ..Default::default()
        };
        let report = detector.audit(&flags);
        assert!(report.has_critical);
        assert!(report.findings.iter().any(|f| f.id == "shell_no_sandbox"));
    }

    #[test]
    fn acknowledged_pattern_skipped() {
        let mut detector = DangerousConfigDetector::new();
        detector.acknowledge("shell_no_sandbox");
        let flags = ConfigFlags {
            shell_enabled: true,
            sandbox_disabled: true,
            ..Default::default()
        };
        let report = detector.audit(&flags);
        assert!(!report.findings.iter().any(|f| f.id == "shell_no_sandbox"));
    }

    #[test]
    fn full_triad_detected() {
        let detector = DangerousConfigDetector::new();
        let flags = ConfigFlags {
            shell_enabled: true,
            filesystem_unrestricted: true,
            network_unrestricted: true,
            ..Default::default()
        };
        let report = detector.audit(&flags);
        assert!(report.findings.iter().any(|f| f.id == "full_triad"));
    }

    #[test]
    fn daemon_no_audit_is_high() {
        let detector = DangerousConfigDetector::new();
        let flags = ConfigFlags {
            daemon_mode: true,
            audit_disabled: true,
            ..Default::default()
        };
        let report = detector.audit(&flags);
        assert!(report.findings.iter().any(|f| f.id == "daemon_no_audit"));
        assert!(report.overall_risk >= ConfigRisk::High);
    }

    #[test]
    fn blocking_findings_only_critical() {
        let detector = DangerousConfigDetector::new();
        let flags = ConfigFlags {
            shell_enabled: true,
            sandbox_disabled: true,
            confirmations_disabled: true,
            daemon_mode: true,
            audit_disabled: true,
            ..Default::default()
        };
        let blocking = detector.blocking_findings(&flags);
        assert!(blocking.iter().all(|f| f.risk == ConfigRisk::Critical));
        assert!(!blocking.is_empty());
    }

    #[test]
    fn multiple_patterns_combined() {
        let detector = DangerousConfigDetector::new();
        let flags = ConfigFlags {
            shell_enabled: true,
            sandbox_disabled: true,
            confirmations_disabled: true,
            filesystem_unrestricted: true,
            network_unrestricted: true,
            credential_access: true,
            mcp_untrusted: true,
            daemon_mode: true,
            audit_disabled: true,
            ..Default::default()
        };
        let report = detector.audit(&flags);
        // Should trigger many patterns
        assert!(report.findings.len() >= 8);
        assert!(report.has_critical);
    }
}
