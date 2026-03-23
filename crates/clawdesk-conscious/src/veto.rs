//! L3: Veto Gate — human approval with richer decision set.
//!
//! Extends the existing `ApprovalGate` with:
//! - `Modify` — approve with changed arguments
//! - Timeout-to-deny (safe default)
//! - Session-scoped decisions
//! - Audit trail integration

use serde::{Deserialize, Serialize};

/// Human veto decision — richer than binary allow/deny.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VetoDecision {
    /// Allow this single invocation.
    Allow,
    /// Allow this tool for the rest of the session.
    AllowForSession,
    /// Deny this single invocation.
    Deny,
    /// Deny this tool for the rest of the session.
    DenyForSession,
    /// Approve but with modified arguments.
    Modify { modified_args: String },
    /// Approval timed out → treated as Deny (safe default).
    Timeout,
}

impl VetoDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow | Self::AllowForSession | Self::Modify { .. })
    }

    pub fn is_session_scoped(&self) -> bool {
        matches!(self, Self::AllowForSession | Self::DenyForSession)
    }
}

/// Configuration for the veto layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VetoConfig {
    /// Seconds before timeout → automatic deny.
    pub timeout_seconds: u64,
    /// Whether the "Modify" option is available.
    pub allow_modification: bool,
}

impl Default for VetoConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 30,
            allow_modification: true,
        }
    }
}

/// Trait for veto gate implementations (CLI, TUI, Tauri GUI, webhook).
///
/// This extends the existing `ApprovalGate` concept with consciousness-aware
/// context. Implementations show the user the tool name, arguments, risk score,
/// consciousness level, and sentinel explanations.
#[async_trait::async_trait]
pub trait VetoGate: Send + Sync + 'static {
    /// Request human decision on a tool invocation.
    ///
    /// The implementation should present:
    /// - Tool name and arguments
    /// - Risk score and consciousness level
    /// - Sentinel explanation (why this needs approval)
    /// - Options: Allow / Allow for Session / Deny / Modify
    ///
    /// Must respect `timeout_seconds` — if no decision within timeout,
    /// return `Timeout` (which maps to Deny).
    async fn request_veto(
        &self,
        tool: &str,
        args: &serde_json::Value,
        risk_score: f64,
        level: &str,
        explanation: &str,
        config: &VetoConfig,
    ) -> VetoDecision;
}

/// CLI veto gate — interactive terminal approval.
///
/// Displays tool info and waits for user input with timeout.
pub struct CliVetoGate;

#[async_trait::async_trait]
impl VetoGate for CliVetoGate {
    async fn request_veto(
        &self,
        tool: &str,
        args: &serde_json::Value,
        risk_score: f64,
        level: &str,
        explanation: &str,
        config: &VetoConfig,
    ) -> VetoDecision {
        use std::io::{self, Write};

        let args_preview = {
            let s = args.to_string();
            if s.len() > 200 {
                format!("{}...", &s[..200])
            } else {
                s
            }
        };

        eprintln!();
        eprintln!("╔══════════════════════════════════════════════════════════╗");
        eprintln!("║  CONSCIOUS GATEWAY — Tool Approval Required            ║");
        eprintln!("╠══════════════════════════════════════════════════════════╣");
        eprintln!("║  Tool:  {:<48} ║", tool);
        eprintln!("║  Level: {:<48} ║", level);
        eprintln!("║  Risk:  {:<48.2} ║", risk_score);
        eprintln!("║  Args:  {:<48} ║", &args_preview[..args_preview.len().min(48)]);
        if !explanation.is_empty() {
            eprintln!("║  Why:   {:<48} ║", &explanation[..explanation.len().min(48)]);
        }
        eprintln!("╠══════════════════════════════════════════════════════════╣");
        eprintln!("║  [a] Allow  [s] Allow for session  [d] Deny            ║");
        if config.allow_modification {
            eprintln!("║  [m] Modify args                                       ║");
        }
        eprintln!("║  Timeout in {}s → deny                                ║",
            config.timeout_seconds);
        eprintln!("╚══════════════════════════════════════════════════════════╝");
        eprint!("  Decision: ");
        let _ = io::stderr().flush();

        // Read with timeout
        let decision = tokio::time::timeout(
            std::time::Duration::from_secs(config.timeout_seconds),
            tokio::task::spawn_blocking(|| {
                let mut input = String::new();
                io::stdin().read_line(&mut input).ok();
                input.trim().to_lowercase()
            }),
        ).await;

        match decision {
            Ok(Ok(input)) => match input.as_str() {
                "a" | "allow" | "y" | "yes" => VetoDecision::Allow,
                "s" | "session" => VetoDecision::AllowForSession,
                "d" | "deny" | "n" | "no" => VetoDecision::Deny,
                "ds" => VetoDecision::DenyForSession,
                "m" | "modify" if config.allow_modification => {
                    eprint!("  Modified args (JSON): ");
                    let _ = io::stderr().flush();
                    let mut modified = String::new();
                    match io::stdin().read_line(&mut modified) {
                        Ok(_) => VetoDecision::Modify {
                            modified_args: modified.trim().to_string(),
                        },
                        Err(_) => VetoDecision::Deny,
                    }
                }
                _ => VetoDecision::Deny,
            },
            _ => {
                eprintln!("  ⏰ Timeout — denied by default.");
                VetoDecision::Timeout
            }
        }
    }
}

/// Auto-approve gate — for fully autonomous operation (autonomous preset).
pub struct AutoApproveGate;

#[async_trait::async_trait]
impl VetoGate for AutoApproveGate {
    async fn request_veto(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        _risk_score: f64,
        _level: &str,
        _explanation: &str,
        _config: &VetoConfig,
    ) -> VetoDecision {
        VetoDecision::Allow
    }
}

/// Auto-deny gate — for maximum safety (paranoid preset testing).
pub struct AutoDenyGate;

#[async_trait::async_trait]
impl VetoGate for AutoDenyGate {
    async fn request_veto(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        _risk_score: f64,
        _level: &str,
        _explanation: &str,
        _config: &VetoConfig,
    ) -> VetoDecision {
        VetoDecision::Deny
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn veto_decision_is_allowed() {
        assert!(VetoDecision::Allow.is_allowed());
        assert!(VetoDecision::AllowForSession.is_allowed());
        assert!(VetoDecision::Modify { modified_args: "{}".into() }.is_allowed());
        assert!(!VetoDecision::Deny.is_allowed());
        assert!(!VetoDecision::DenyForSession.is_allowed());
        assert!(!VetoDecision::Timeout.is_allowed());
    }

    #[tokio::test]
    async fn auto_approve_always_allows() {
        let gate = AutoApproveGate;
        let decision = gate.request_veto(
            "rm", &serde_json::json!({}), 0.9, "critical", "test",
            &VetoConfig::default(),
        ).await;
        assert_eq!(decision, VetoDecision::Allow);
    }

    #[tokio::test]
    async fn auto_deny_always_denies() {
        let gate = AutoDenyGate;
        let decision = gate.request_veto(
            "file_read", &serde_json::json!({}), 0.0, "reflexive", "",
            &VetoConfig::default(),
        ).await;
        assert_eq!(decision, VetoDecision::Deny);
    }
}
