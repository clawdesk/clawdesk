//! Security Health Dashboard — Visual Trust Surface
//!
//! Surfaces existing security infrastructure as a computed score (0–100)
//! with per-component drill-down. The score is a weighted sum based on
//! CVSS v3.1 base score impact weights.
//!
//! ## Scoring Formula
//!
//! `S = Σᵢ wᵢ × sᵢ` where `sᵢ ∈ {0, 1}` is pass/fail status of check `i`.
//!
//! Weights (by CVSS impact):
//! - Credential encryption: 25 (Confidentiality:High)
//! - Sandbox enforcement: 20 (Integrity:High)
//! - Skill signatures: 20 (Supply Chain)
//! - Port exposure: 15 (Attack Vector:Network)
//! - Data-at-rest encryption: 10
//! - Audit trail: 10

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// Individual security check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityCheck {
    /// Unique check identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Detailed description of what this checks
    pub description: String,
    /// CVSS-based weight (contributes to total score)
    pub weight: u32,
    /// Whether the check passes
    pub passed: bool,
    /// Severity if failed
    pub severity: CheckSeverity,
    /// Human-readable status message
    pub status_message: String,
    /// Remediation guidance if failed
    pub remediation: Option<String>,
    /// Category for grouping
    pub category: CheckCategory,
}

/// Severity levels aligned with CVSS v3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

/// Check categories for dashboard grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckCategory {
    Credentials,
    Sandbox,
    SkillSecurity,
    Network,
    DataProtection,
    AuditCompliance,
}

/// Complete security health report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityHealthReport {
    /// Overall security score (0–100)
    pub score: u32,
    /// Score grade (A+ through F)
    pub grade: String,
    /// Individual check results
    pub checks: Vec<SecurityCheck>,
    /// Number of checks that passed
    pub passed_count: usize,
    /// Total number of checks
    pub total_count: usize,
    /// When this report was generated
    pub generated_at: u64,
    /// Summary of most critical issues
    pub critical_issues: Vec<String>,
}

impl SecurityHealthReport {
    /// Compute score from individual checks.
    ///
    /// `S = Σᵢ wᵢ × sᵢ` — monotone and decomposable.
    /// Time complexity: O(k) where k ≈ 6 checks.
    pub fn compute(checks: Vec<SecurityCheck>) -> Self {
        let total_weight: u32 = checks.iter().map(|c| c.weight).sum();
        let earned_weight: u32 = checks.iter()
            .filter(|c| c.passed)
            .map(|c| c.weight)
            .sum();

        let score = if total_weight > 0 {
            (earned_weight as f64 / total_weight as f64 * 100.0) as u32
        } else {
            0
        };

        let grade = match score {
            95..=100 => "A+",
            90..=94 => "A",
            85..=89 => "A-",
            80..=84 => "B+",
            75..=79 => "B",
            70..=74 => "B-",
            60..=69 => "C",
            50..=59 => "D",
            _ => "F",
        }.to_string();

        let passed_count = checks.iter().filter(|c| c.passed).count();
        let total_count = checks.len();

        let critical_issues: Vec<String> = checks.iter()
            .filter(|c| !c.passed && matches!(c.severity, CheckSeverity::Critical | CheckSeverity::High))
            .map(|c| c.status_message.clone())
            .collect();

        let generated_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            score,
            grade,
            checks,
            passed_count,
            total_count,
            generated_at,
            critical_issues,
        }
    }
}

/// Security health evaluator — runs all checks against current system state.
pub struct SecurityHealthEvaluator;

impl SecurityHealthEvaluator {
    /// Run all security checks and produce a health report.
    ///
    /// Each check queries cached state — O(1) per check, O(k) total.
    pub fn evaluate(state: &SecurityState) -> SecurityHealthReport {
        let checks = vec![
            Self::check_credential_encryption(state),
            Self::check_sandbox_enforcement(state),
            Self::check_skill_signatures(state),
            Self::check_port_exposure(state),
            Self::check_data_at_rest_encryption(state),
            Self::check_audit_trail(state),
        ];

        SecurityHealthReport::compute(checks)
    }

    /// Check 1: Credential encryption status (AES-GCM envelope).
    /// Weight: 25 (Confidentiality:High per CVSS v3.1)
    fn check_credential_encryption(state: &SecurityState) -> SecurityCheck {
        SecurityCheck {
            id: "cred_encryption".into(),
            name: "Credential Encryption".into(),
            description: "All stored credentials are encrypted with AES-256-GCM authenticated encryption".into(),
            weight: 25,
            passed: state.credentials_encrypted,
            severity: CheckSeverity::Critical,
            status_message: if state.credentials_encrypted {
                format!("{} credentials encrypted with AES-256-GCM", state.credential_count)
            } else {
                "Credentials stored without encryption".into()
            },
            remediation: if state.credentials_encrypted {
                None
            } else {
                Some("Enable OS keychain backend or encrypted file vault".into())
            },
            category: CheckCategory::Credentials,
        }
    }

    /// Check 2: Sandbox enforcement status per skill.
    /// Weight: 20 (Integrity:High)
    fn check_sandbox_enforcement(state: &SecurityState) -> SecurityCheck {
        let all_sandboxed = state.skills_sandboxed == state.total_skills && state.total_skills > 0;
        SecurityCheck {
            id: "sandbox_enforcement".into(),
            name: "Sandbox Enforcement".into(),
            description: "All skills execute within sandboxed environment with capability-gated permissions".into(),
            weight: 20,
            passed: all_sandboxed || state.sandbox_default_empty,
            severity: CheckSeverity::Critical,
            status_message: if state.sandbox_default_empty {
                format!("Sandbox-by-default active. {}/{} skills sandboxed", state.skills_sandboxed, state.total_skills)
            } else {
                "Sandbox default is permissive — skills may execute unsandboxed".into()
            },
            remediation: if all_sandboxed || state.sandbox_default_empty {
                None
            } else {
                Some("Set default_grant to CapabilitySet::EMPTY in sandbox dispatcher".into())
            },
            category: CheckCategory::Sandbox,
        }
    }

    /// Check 3: Skill signature verification results.
    /// Weight: 20 (Supply Chain)
    fn check_skill_signatures(state: &SecurityState) -> SecurityCheck {
        let all_verified = state.skills_verified == state.total_skills;
        SecurityCheck {
            id: "skill_signatures".into(),
            name: "Skill Signatures".into(),
            description: "All installed skills have valid Ed25519 digital signatures".into(),
            weight: 20,
            passed: all_verified || state.total_skills == 0,
            severity: CheckSeverity::High,
            status_message: if all_verified {
                format!("{}/{} skills have verified signatures", state.skills_verified, state.total_skills)
            } else {
                format!("{}/{} skills are unverified", state.total_skills - state.skills_verified, state.total_skills)
            },
            remediation: if all_verified || state.total_skills == 0 {
                None
            } else {
                Some("Review and verify or remove unverified skills".into())
            },
            category: CheckCategory::SkillSecurity,
        }
    }

    /// Check 4: Exposed port scan (0 ports = green).
    /// Weight: 15 (Attack Vector:Network)
    fn check_port_exposure(state: &SecurityState) -> SecurityCheck {
        let safe = state.exposed_ports == 0;
        SecurityCheck {
            id: "port_exposure".into(),
            name: "Network Exposure".into(),
            description: "No unnecessary ports are exposed to the network".into(),
            weight: 15,
            passed: safe,
            severity: CheckSeverity::High,
            status_message: if safe {
                "No externally exposed ports detected".into()
            } else {
                format!("{} port(s) exposed to non-localhost", state.exposed_ports)
            },
            remediation: if safe {
                None
            } else {
                Some("Bind services to 127.0.0.1 or configure firewall rules".into())
            },
            category: CheckCategory::Network,
        }
    }

    /// Check 5: Data-at-rest encryption status via SochDB.
    /// Weight: 10
    fn check_data_at_rest_encryption(state: &SecurityState) -> SecurityCheck {
        SecurityCheck {
            id: "data_encryption".into(),
            name: "Data-at-Rest Encryption".into(),
            description: "SochDB data store is encrypted at rest".into(),
            weight: 10,
            passed: state.data_encrypted_at_rest,
            severity: CheckSeverity::Medium,
            status_message: if state.data_encrypted_at_rest {
                "SochDB encrypted at rest".into()
            } else {
                "Data stored without at-rest encryption".into()
            },
            remediation: if state.data_encrypted_at_rest {
                None
            } else {
                Some("Enable SochDB encryption in configuration".into())
            },
            category: CheckCategory::DataProtection,
        }
    }

    /// Check 6: Agent permission audit trail.
    /// Weight: 10
    fn check_audit_trail(state: &SecurityState) -> SecurityCheck {
        SecurityCheck {
            id: "audit_trail".into(),
            name: "Audit Trail".into(),
            description: "Hash-chained audit log is active and verified".into(),
            weight: 10,
            passed: state.audit_trail_active && state.audit_chain_valid,
            severity: CheckSeverity::Medium,
            status_message: if state.audit_trail_active && state.audit_chain_valid {
                format!("Audit trail active with {} entries, chain verified", state.audit_entry_count)
            } else if state.audit_trail_active {
                "Audit trail active but chain integrity compromised".into()
            } else {
                "Audit trail not active".into()
            },
            remediation: if state.audit_trail_active && state.audit_chain_valid {
                None
            } else {
                Some("Enable audit logging and verify chain integrity".into())
            },
            category: CheckCategory::AuditCompliance,
        }
    }
}

/// System security state snapshot — collected from various subsystems.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityState {
    /// Whether credentials are encrypted (vault backend check)
    pub credentials_encrypted: bool,
    /// Number of stored credentials
    pub credential_count: usize,
    /// Whether sandbox default grant is EMPTY
    pub sandbox_default_empty: bool,
    /// Number of skills running in sandbox
    pub skills_sandboxed: usize,
    /// Total number of installed skills
    pub total_skills: usize,
    /// Number of skills with verified signatures
    pub skills_verified: usize,
    /// Number of ports exposed to non-localhost
    pub exposed_ports: usize,
    /// Whether data is encrypted at rest
    pub data_encrypted_at_rest: bool,
    /// Whether audit trail is active
    pub audit_trail_active: bool,
    /// Whether audit hash chain is valid
    pub audit_chain_valid: bool,
    /// Number of audit log entries
    pub audit_entry_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_score() {
        let state = SecurityState {
            credentials_encrypted: true,
            credential_count: 5,
            sandbox_default_empty: true,
            skills_sandboxed: 10,
            total_skills: 10,
            skills_verified: 10,
            exposed_ports: 0,
            data_encrypted_at_rest: true,
            audit_trail_active: true,
            audit_chain_valid: true,
            audit_entry_count: 1000,
        };
        let report = SecurityHealthEvaluator::evaluate(&state);
        assert_eq!(report.score, 100);
        assert_eq!(report.grade, "A+");
        assert!(report.critical_issues.is_empty());
    }

    #[test]
    fn minimal_state_score() {
        let state = SecurityState::default();
        let report = SecurityHealthEvaluator::evaluate(&state);
        // With defaults: credentials_encrypted=false (-25), sandbox_default_empty=false (-20),
        // but total_skills=0 so skill_signatures passes (+20),
        // exposed_ports=0 passes (+15), data_encrypted=false (-10),
        // audit_trail_active=false (-10).
        // Expected: 20 + 15 = 35 out of 100
        assert_eq!(report.score, 35);
        assert!(!report.critical_issues.is_empty()); // credential + sandbox fail
    }

    #[test]
    fn partial_score() {
        let state = SecurityState {
            credentials_encrypted: true,   // +25
            sandbox_default_empty: true,    // +20
            skills_verified: 0,
            total_skills: 5,
            exposed_ports: 1,
            data_encrypted_at_rest: true,   // +10
            audit_trail_active: true,
            audit_chain_valid: true,        // +10
            ..Default::default()
        };
        let report = SecurityHealthEvaluator::evaluate(&state);
        // 25 + 20 + 10 + 10 = 65 out of 100
        assert_eq!(report.score, 65);
        assert_eq!(report.grade, "C");
    }

    #[test]
    fn critical_issues_reported() {
        let state = SecurityState {
            credentials_encrypted: false, // Critical
            sandbox_default_empty: false, // Critical
            ..Default::default()
        };
        let report = SecurityHealthEvaluator::evaluate(&state);
        assert!(!report.critical_issues.is_empty());
    }
}
