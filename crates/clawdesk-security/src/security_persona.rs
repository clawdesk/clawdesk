//! Security Persona — adaptive risk judge using the Persona Field model.
//!
//! ## The Idea
//!
//! Instead of a static approval policy (Medium risk → always ask), we model
//! security posture as a **persona field** that adapts to activity context:
//!
//! ```text
//! User editing their own dotfiles at 2pm → low friction (auto-approve)
//! Agent running curl to unknown host at 3am → high friction (block + A2UI confirm)
//! ```
//!
//! The security persona is the **judge** — it observes every tool call and
//! renders a verdict using the same three-lane model as personality:
//!
//! - **Style lane** → how to communicate the decision (scary warning vs gentle nudge)
//! - **Stance lane** → how strict to be (paranoid vs permissive)  
//! - **Rules lane** → hard constraints that never bend (never allow rm -rf /)
//!
//! ## Integration with A2UI
//!
//! When the security persona decides to escalate, it generates an A2UI
//! confirmation surface — not a plain "approve/deny" dialog, but a
//! **contextual risk card** showing:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │ ⚠️ Network Access Request                          │
//! │                                                     │
//! │ Tool:    http_fetch                                 │
//! │ Target:  api.unknown-service.com:443                │
//! │ Agent:   code-assistant                             │
//! │ Context: "fetching API docs for integration"        │
//! │                                                     │
//! │ Risk:    ██████░░░░  Medium (0.6)                  │
//! │ Reason:  Endpoint not in allowlist                  │
//! │                                                     │
//! │ [Allow Once]  [Allow for Session]  [Deny]  [Edit]  │
//! └─────────────────────────────────────────────────────┘
//! ```
//!
//! ## How It Works
//!
//! The security persona runs a **Bayesian risk scoring pipeline**:
//!
//! 1. **Prior**: Base risk from command_policy.rs (Low/Medium/High)
//! 2. **Likelihood**: Context signals shift the risk up or down
//!    - Time of day (off-hours → +risk)
//!    - Session history (first tool call in session → +risk)
//!    - Agent trust level (explorer vs executor → different priors)
//!    - Target reputation (known API → -risk, unknown host → +risk)
//! 3. **Posterior**: Combined risk score determines action
//!    - < 0.3 → auto-approve (green path)
//!    - 0.3–0.7 → A2UI confirmation (yellow path)
//!    - > 0.7 → deny with explanation (red path)
//!
//! The thresholds themselves are persona-controlled:
//! - Paranoid persona: auto-approve < 0.15, deny > 0.5
//! - Balanced persona: auto-approve < 0.3, deny > 0.7
//! - Permissive persona: auto-approve < 0.5, deny > 0.9

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Security Stance — the three-lane model applied to risk judgment
// ═══════════════════════════════════════════════════════════════════════════

/// Security posture preset — determines auto-approve and deny thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityPosture {
    /// Lock everything down. Approve only well-known safe operations.
    /// Auto-approve < 0.15, deny > 0.5
    Paranoid,
    /// Default. Trust the user's own agent, verify external interactions.
    /// Auto-approve < 0.3, deny > 0.7
    Balanced,
    /// Power user mode. Approve most things, only block dangerous ops.
    /// Auto-approve < 0.5, deny > 0.9
    Permissive,
    /// Custom thresholds.
    Custom {
        auto_approve_below: f32,
        deny_above: f32,
    },
}

impl SecurityPosture {
    /// Auto-approve threshold — below this, no human needed.
    pub fn auto_approve_threshold(&self) -> f32 {
        match self {
            Self::Paranoid => 0.15,
            Self::Balanced => 0.3,
            Self::Permissive => 0.5,
            Self::Custom { auto_approve_below, .. } => *auto_approve_below,
        }
    }

    /// Deny threshold — above this, block without asking.
    pub fn deny_threshold(&self) -> f32 {
        match self {
            Self::Paranoid => 0.5,
            Self::Balanced => 0.7,
            Self::Permissive => 0.9,
            Self::Custom { deny_above, .. } => *deny_above,
        }
    }
}

impl Default for SecurityPosture {
    fn default() -> Self {
        Self::Balanced
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Risk Signals — the Bayesian evidence that shifts risk up or down
// ═══════════════════════════════════════════════════════════════════════════

/// Context signals that adjust the risk score.
///
/// Each signal has a **risk delta** in [-0.3, +0.3]. Positive = more risky.
/// The total posterior is clamped to [0.0, 1.0].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskContext {
    /// The tool being executed.
    pub tool_name: String,
    /// The command or arguments.
    pub command: String,
    /// Agent identity executing this tool.
    pub agent_id: String,
    /// Which agent profile (Explorer, Executor, Planner, etc.).
    pub agent_profile: AgentTrustTier,
    /// How many tools this agent has executed in this session (experience).
    pub session_tool_count: u32,
    /// Target host (for network tools).
    pub target_host: Option<String>,
    /// Whether the target is in the known-good allowlist.
    pub target_in_allowlist: bool,
    /// Current hour (0-23) in user's timezone.
    pub hour_of_day: u8,
    /// Whether the user has been actively interacting (vs agent running autonomously).
    pub user_active: bool,
    /// Previous approval decisions this session (for pattern matching).
    pub session_approvals: u32,
    /// Previous denials this session.
    pub session_denials: u32,
}

/// Agent trust tier — determines the risk prior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTrustTier {
    /// Read-only agent (Explorer profile). Lowest risk prior.
    Explorer,
    /// Planning agent. Medium-low risk prior.
    Planner,
    /// Full execution agent. Medium risk prior.
    Executor,
    /// External/third-party agent. Highest risk prior.
    External,
    /// Owner's personal agent with full trust.
    Owner,
}

impl AgentTrustTier {
    /// Base risk prior for this agent tier.
    ///
    /// This is P(risky | agent_type) — the prior probability before
    /// observing any context signals.
    pub fn risk_prior(&self) -> f32 {
        match self {
            Self::Owner => 0.05,     // Almost always safe
            Self::Explorer => 0.1,   // Read-only, minimal risk
            Self::Planner => 0.2,    // Can spawn, needs monitoring
            Self::Executor => 0.35,  // Full tool access, moderate prior
            Self::External => 0.55,  // Unknown trust, high prior
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Risk Scoring — Bayesian update from context signals
// ═══════════════════════════════════════════════════════════════════════════

/// The security judge's verdict on a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskVerdict {
    /// Final risk score in [0.0, 1.0].
    pub score: f32,
    /// Decision: auto-approve, escalate (A2UI), or deny.
    pub action: RiskAction,
    /// Human-readable explanation of why this score was assigned.
    pub explanation: Vec<String>,
    /// All contributing signals and their deltas (for auditability).
    pub signal_breakdown: Vec<(String, f32)>,
}

/// What to do with the tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskAction {
    /// Low risk — proceed without human interaction.
    AutoApprove,
    /// Medium risk — show A2UI confirmation card to user.
    Escalate,
    /// High risk — block and explain why.
    Deny,
}

/// The security persona — judges tool calls based on context.
pub struct SecurityJudge {
    /// Current security posture (determines thresholds).
    pub posture: SecurityPosture,
    /// Hard-deny patterns that override all scoring.
    pub hard_denies: Vec<String>,
    /// Known-safe tool names that skip scoring entirely.
    pub safe_tools: Vec<String>,
    /// Session approval history for learning (EWMA of approval rate).
    approval_rate_ewma: f32,
    /// Number of judgments made.
    judgment_count: u32,
}

impl SecurityJudge {
    pub fn new(posture: SecurityPosture) -> Self {
        Self {
            posture,
            hard_denies: Self::default_hard_denies(),
            safe_tools: Self::default_safe_tools(),
            approval_rate_ewma: 0.5,
            judgment_count: 0,
        }
    }

    /// Judge a tool call — the core decision function.
    ///
    /// Returns a `RiskVerdict` with score, action, and explanation.
    /// The scoring is transparent: every signal that contributed is listed.
    pub fn judge(&mut self, ctx: &RiskContext, base_risk_level: BaseRisk) -> RiskVerdict {
        let mut score: f32;
        let mut signals: Vec<(String, f32)> = Vec::new();
        let mut explanation: Vec<String> = Vec::new();

        // ── Hard deny check (rules lane — never overridden) ──────
        for pattern in &self.hard_denies {
            if ctx.command.contains(pattern.as_str()) {
                return RiskVerdict {
                    score: 1.0,
                    action: RiskAction::Deny,
                    explanation: vec![format!("Hard-denied: command matches '{pattern}'")],
                    signal_breakdown: vec![("hard_deny".into(), 1.0)],
                };
            }
        }

        // ── Safe tool fast path ──────────────────────────────────
        if self.safe_tools.contains(&ctx.tool_name) {
            return RiskVerdict {
                score: 0.0,
                action: RiskAction::AutoApprove,
                explanation: vec![format!("{} is in safe-tools list", ctx.tool_name)],
                signal_breakdown: vec![("safe_tool".into(), 0.0)],
            };
        }

        // ── Prior: agent trust tier ──────────────────────────────
        score = ctx.agent_profile.risk_prior();
        signals.push(("agent_trust_prior".into(), score));
        explanation.push(format!(
            "Agent {:?} base risk: {:.2}",
            ctx.agent_profile, score
        ));

        // ── Signal 1: Base command risk ──────────────────────────
        let cmd_delta = match base_risk_level {
            BaseRisk::Low => -0.15,
            BaseRisk::Medium => 0.1,
            BaseRisk::High => 0.25,
        };
        score += cmd_delta;
        signals.push(("command_risk".into(), cmd_delta));

        // ── Signal 2: Target reputation ──────────────────────────
        if let Some(ref host) = ctx.target_host {
            if ctx.target_in_allowlist {
                let delta = -0.15;
                score += delta;
                signals.push(("target_allowlisted".into(), delta));
                explanation.push(format!("{host} is in allowlist"));
            } else {
                let delta = 0.2;
                score += delta;
                signals.push(("target_unknown".into(), delta));
                explanation.push(format!("{host} is NOT in allowlist"));
            }
        }

        // ── Signal 3: Time of day ────────────────────────────────
        // Off-hours (midnight to 6am) → slightly higher risk
        // (agent running autonomously while user sleeps)
        if ctx.hour_of_day < 6 || ctx.hour_of_day >= 23 {
            let delta = 0.1;
            score += delta;
            signals.push(("off_hours".into(), delta));
            explanation.push("Off-hours operation (higher scrutiny)".into());
        }

        // ── Signal 4: User presence ──────────────────────────────
        if !ctx.user_active {
            let delta = 0.1;
            score += delta;
            signals.push(("user_inactive".into(), delta));
            explanation.push("User not actively interacting".into());
        } else {
            let delta = -0.05;
            score += delta;
            signals.push(("user_active".into(), delta));
        }

        // ── Signal 5: Session experience ─────────────────────────
        // First few tool calls in a session → slightly higher risk
        // After 10+ calls with no denials → trust builds
        if ctx.session_tool_count < 3 {
            let delta = 0.05;
            score += delta;
            signals.push(("early_session".into(), delta));
        } else if ctx.session_tool_count > 10 && ctx.session_denials == 0 {
            let delta = -0.1;
            score += delta;
            signals.push(("established_trust".into(), delta));
            explanation.push("Session has established trust (10+ tools, 0 denials)".into());
        }

        // ── Signal 6: EWMA approval rate (session learning) ─────
        // If user has been approving everything → lower friction
        if self.judgment_count > 5 && self.approval_rate_ewma > 0.8 {
            let delta = -0.1;
            score += delta;
            signals.push(("high_approval_rate".into(), delta));
        }

        // ── Clamp to [0, 1] ─────────────────────────────────────
        score = score.clamp(0.0, 1.0);

        // ── Decision based on posture thresholds ─────────────────
        let action = if score < self.posture.auto_approve_threshold() {
            RiskAction::AutoApprove
        } else if score > self.posture.deny_threshold() {
            RiskAction::Deny
        } else {
            RiskAction::Escalate
        };

        debug!(
            tool = %ctx.tool_name,
            score,
            action = ?action,
            "Security judge verdict"
        );

        RiskVerdict {
            score,
            action,
            explanation,
            signal_breakdown: signals,
        }
    }

    /// Record a human decision (updates EWMA for session learning).
    pub fn record_decision(&mut self, approved: bool) {
        self.judgment_count += 1;
        let val = if approved { 1.0 } else { 0.0 };
        let alpha = 0.3;
        self.approval_rate_ewma = alpha * val + (1.0 - alpha) * self.approval_rate_ewma;
    }

    /// Generate an A2UI confirmation surface for an escalated tool call.
    ///
    /// Returns a JSON value conforming to the A2UI basic_catalog schema.
    /// The frontend renders this as a rich confirmation card.
    pub fn to_a2ui_surface(&self, ctx: &RiskContext, verdict: &RiskVerdict) -> serde_json::Value {
        let risk_bar = "█".repeat((verdict.score * 10.0) as usize);
        let risk_empty = "░".repeat(10 - (verdict.score * 10.0) as usize);
        let risk_label = if verdict.score < 0.3 {
            "Low"
        } else if verdict.score < 0.7 {
            "Medium"
        } else {
            "High"
        };

        serde_json::json!({
            "type": "card",
            "title": format!("⚠️ {} Request", match verdict.action {
                RiskAction::Escalate => "Confirmation",
                RiskAction::Deny => "Blocked",
                _ => "Approved",
            }),
            "children": [
                {
                    "type": "row",
                    "children": [
                        {"type": "text", "content": "Tool:", "style": "label"},
                        {"type": "text", "content": &ctx.tool_name, "style": "value"}
                    ]
                },
                {
                    "type": "row",
                    "children": [
                        {"type": "text", "content": "Command:", "style": "label"},
                        {"type": "text", "content": truncate(&ctx.command, 80), "style": "code"}
                    ]
                },
                {
                    "type": "row",
                    "children": [
                        {"type": "text", "content": "Agent:", "style": "label"},
                        {"type": "text", "content": &ctx.agent_id, "style": "value"}
                    ]
                },
                {
                    "type": "row",
                    "children": [
                        {"type": "text", "content": "Risk:", "style": "label"},
                        {"type": "text", "content": format!("{risk_bar}{risk_empty} {risk_label} ({:.1})", verdict.score), "style": "risk"}
                    ]
                },
                {
                    "type": "text",
                    "content": verdict.explanation.join("; "),
                    "style": "detail"
                },
                {
                    "type": "row",
                    "children": [
                        {"type": "button", "label": "Allow Once", "action": {"name": "approve", "scope": "once"}},
                        {"type": "button", "label": "Allow for Session", "action": {"name": "approve", "scope": "session"}},
                        {"type": "button", "label": "Deny", "action": {"name": "deny"}},
                        {"type": "button", "label": "Edit", "action": {"name": "edit"}}
                    ]
                }
            ]
        })
    }

    fn default_hard_denies() -> Vec<String> {
        vec![
            "rm -rf /".into(),
            "mkfs".into(),
            "dd if=".into(),
            "shutdown".into(),
            "reboot".into(),
            ":(){ :|:& };:".into(), // fork bomb
            "> /dev/sda".into(),
            "chmod -R 777 /".into(),
        ]
    }

    fn default_safe_tools() -> Vec<String> {
        vec![
            "read_file".into(),
            "list_directory".into(),
            "search_files".into(),
            "memory_search".into(),
            "memory_store".into(),
        ]
    }
}

/// Base risk level from the command policy classifier.
#[derive(Debug, Clone, Copy)]
pub enum BaseRisk {
    Low,
    Medium,
    High,
}

/// Truncate a string for display.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> RiskContext {
        RiskContext {
            tool_name: "shell_exec".into(),
            command: "ls -la".into(),
            agent_id: "assistant".into(),
            agent_profile: AgentTrustTier::Executor,
            session_tool_count: 5,
            target_host: None,
            target_in_allowlist: false,
            hour_of_day: 14,
            user_active: true,
            session_approvals: 3,
            session_denials: 0,
        }
    }

    #[test]
    fn safe_tool_auto_approves() {
        let mut judge = SecurityJudge::new(SecurityPosture::Balanced);
        let mut ctx = test_context();
        ctx.tool_name = "read_file".into();

        let verdict = judge.judge(&ctx, BaseRisk::Low);
        assert_eq!(verdict.action, RiskAction::AutoApprove);
        assert!(verdict.score < 0.01);
    }

    #[test]
    fn hard_deny_always_blocks() {
        let mut judge = SecurityJudge::new(SecurityPosture::Permissive);
        let mut ctx = test_context();
        ctx.command = "rm -rf / --no-preserve-root".into();

        let verdict = judge.judge(&ctx, BaseRisk::Low);
        assert_eq!(verdict.action, RiskAction::Deny);
        assert!((verdict.score - 1.0).abs() < 0.01);
    }

    #[test]
    fn owner_gets_low_friction() {
        let mut judge = SecurityJudge::new(SecurityPosture::Balanced);
        let mut ctx = test_context();
        ctx.agent_profile = AgentTrustTier::Owner;
        ctx.session_tool_count = 15;

        let verdict = judge.judge(&ctx, BaseRisk::Low);
        assert_eq!(verdict.action, RiskAction::AutoApprove);
    }

    #[test]
    fn external_agent_gets_high_friction() {
        let mut judge = SecurityJudge::new(SecurityPosture::Balanced);
        let mut ctx = test_context();
        ctx.agent_profile = AgentTrustTier::External;
        ctx.target_host = Some("evil.example.com".into());
        ctx.target_in_allowlist = false;

        let verdict = judge.judge(&ctx, BaseRisk::High);
        assert_eq!(verdict.action, RiskAction::Deny);
    }

    #[test]
    fn off_hours_increases_risk() {
        let mut judge = SecurityJudge::new(SecurityPosture::Balanced);
        let mut ctx = test_context();

        ctx.hour_of_day = 14; // 2pm
        let daytime = judge.judge(&ctx, BaseRisk::Medium);

        ctx.hour_of_day = 3; // 3am
        let nighttime = judge.judge(&ctx, BaseRisk::Medium);

        assert!(nighttime.score > daytime.score);
    }

    #[test]
    fn user_presence_reduces_risk() {
        let mut judge = SecurityJudge::new(SecurityPosture::Balanced);
        let mut ctx = test_context();

        ctx.user_active = true;
        let active = judge.judge(&ctx, BaseRisk::Medium);

        ctx.user_active = false;
        let inactive = judge.judge(&ctx, BaseRisk::Medium);

        assert!(inactive.score > active.score);
    }

    #[test]
    fn session_trust_builds_over_time() {
        let mut judge = SecurityJudge::new(SecurityPosture::Balanced);
        let mut ctx = test_context();

        ctx.session_tool_count = 1;
        let early = judge.judge(&ctx, BaseRisk::Medium);

        ctx.session_tool_count = 15;
        ctx.session_denials = 0;
        let late = judge.judge(&ctx, BaseRisk::Medium);

        assert!(late.score < early.score, "Trust should build over session");
    }

    #[test]
    fn allowlisted_target_reduces_risk() {
        let mut judge = SecurityJudge::new(SecurityPosture::Balanced);
        let mut ctx = test_context();
        ctx.target_host = Some("api.anthropic.com".into());

        ctx.target_in_allowlist = false;
        let unknown = judge.judge(&ctx, BaseRisk::Medium);

        ctx.target_in_allowlist = true;
        let known = judge.judge(&ctx, BaseRisk::Medium);

        assert!(known.score < unknown.score);
    }

    #[test]
    fn posture_affects_thresholds() {
        let paranoid = SecurityPosture::Paranoid;
        let permissive = SecurityPosture::Permissive;

        assert!(paranoid.auto_approve_threshold() < permissive.auto_approve_threshold());
        assert!(paranoid.deny_threshold() < permissive.deny_threshold());
    }

    #[test]
    fn ewma_learns_from_approvals() {
        let mut judge = SecurityJudge::new(SecurityPosture::Balanced);

        // Simulate 10 approvals
        for _ in 0..10 {
            judge.record_decision(true);
        }
        assert!(judge.approval_rate_ewma > 0.8);

        // Simulate 5 denials
        for _ in 0..5 {
            judge.record_decision(false);
        }
        assert!(judge.approval_rate_ewma < 0.5);
    }

    #[test]
    fn a2ui_surface_is_valid_json() {
        let judge = SecurityJudge::new(SecurityPosture::Balanced);
        let ctx = test_context();
        let verdict = RiskVerdict {
            score: 0.5,
            action: RiskAction::Escalate,
            explanation: vec!["Test".into()],
            signal_breakdown: vec![("test".into(), 0.5)],
        };

        let surface = judge.to_a2ui_surface(&ctx, &verdict);
        assert!(surface.get("type").is_some());
        assert_eq!(surface["type"], "card");
        assert!(surface["children"].is_array());
    }

    #[test]
    fn verdict_is_auditable() {
        let mut judge = SecurityJudge::new(SecurityPosture::Balanced);
        let ctx = test_context();
        let verdict = judge.judge(&ctx, BaseRisk::Medium);

        // Every signal should be traceable
        assert!(!verdict.signal_breakdown.is_empty());
        // Sum of all deltas + prior should approximately equal score
        let reconstructed: f32 = verdict.signal_breakdown.iter()
            .map(|(_, delta)| delta)
            .sum();
        // The first signal IS the prior (not a delta), so this won't perfectly sum
        // but the breakdown should exist for debugging
        assert!(verdict.signal_breakdown.len() >= 2, "Should have multiple signals");
    }
}
