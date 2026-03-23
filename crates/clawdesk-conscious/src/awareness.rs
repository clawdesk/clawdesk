//! L0: Awareness Classifier — O(1) risk scoring for tool invocations.
//!
//! Maps each tool call to a continuous risk score and then to a graduated
//! consciousness level. The classifier uses:
//!
//! 1. **Base risk** — static per-tool score from `FxHashMap` (O(1) lookup)
//! 2. **Contextual risk** — argument-dependent analysis (e.g., file path sensitivity)
//! 3. **Sentinel boost** — dynamic escalation from anomaly detection
//!
//! The composite score `(base + contextual + sentinel).clamp(0.0, 1.0)` maps
//! to four consciousness levels via configurable thresholds.

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Graduated consciousness levels — each level engages progressively more
/// expensive gating mechanisms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ConsciousnessLevel {
    /// L0: Auto-execute. read_file, search, list_dir — no gate.
    Reflexive = 0,
    /// L1: Sentinel-monitored. file_write(known paths), git status.
    Preconscious = 1,
    /// L2: LLM self-review before execution. shell_exec, http_fetch.
    Deliberative = 2,
    /// L3: Human must approve. rm, deploy, email, subagent spawn.
    Critical = 3,
}

impl ConsciousnessLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reflexive => "reflexive",
            Self::Preconscious => "preconscious",
            Self::Deliberative => "deliberative",
            Self::Critical => "critical",
        }
    }
}

impl std::fmt::Display for ConsciousnessLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Composite risk score with breakdown for auditability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskScore {
    /// Static per-tool risk (from classification table).
    pub base: f64,
    /// Dynamic risk from argument analysis (path sensitivity, etc.).
    pub contextual: f64,
    /// Escalation boost from sentinel anomaly detection.
    pub sentinel_boost: f64,
    /// Final composite: `(base + contextual + sentinel_boost).clamp(0.0, 1.0)`.
    pub composite: f64,
}

impl RiskScore {
    pub fn compute(base: f64, contextual: f64, sentinel_boost: f64) -> Self {
        Self {
            base,
            contextual,
            sentinel_boost,
            composite: (base + contextual + sentinel_boost).clamp(0.0, 1.0),
        }
    }

    /// Map composite score to a consciousness level using given thresholds.
    pub fn to_level(&self, t: &LevelThresholds) -> ConsciousnessLevel {
        if self.composite >= t.critical {
            ConsciousnessLevel::Critical
        } else if self.composite >= t.deliberative {
            ConsciousnessLevel::Deliberative
        } else if self.composite >= t.preconscious {
            ConsciousnessLevel::Preconscious
        } else {
            ConsciousnessLevel::Reflexive
        }
    }
}

/// Configurable thresholds for mapping risk scores to consciousness levels.
///
/// Four presets are provided, corresponding to operational safety postures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelThresholds {
    /// Score above which → Preconscious (sentinel-monitored).
    pub preconscious: f64,
    /// Score above which → Deliberative (LLM self-review).
    pub deliberative: f64,
    /// Score above which → Critical (human must approve).
    pub critical: f64,
}

impl Default for LevelThresholds {
    fn default() -> Self {
        Self::balanced()
    }
}

impl LevelThresholds {
    /// Maximum caution — nearly everything requires human review.
    pub fn paranoid() -> Self {
        Self { preconscious: 0.05, deliberative: 0.15, critical: 0.3 }
    }
    /// Active human oversight — moderate gating.
    pub fn supervised() -> Self {
        Self { preconscious: 0.15, deliberative: 0.4, critical: 0.7 }
    }
    /// Standard operation — reasonable autonomy with safety nets.
    pub fn balanced() -> Self {
        Self { preconscious: 0.2, deliberative: 0.5, critical: 0.8 }
    }
    /// Maximum autonomy — only truly dangerous operations need human approval.
    pub fn autonomous() -> Self {
        Self { preconscious: 0.4, deliberative: 0.7, critical: 0.95 }
    }
}

/// Trait for tool-specific argument risk analysis.
///
/// Implementations inspect tool arguments and return a contextual risk delta.
/// For example, `file_write` to `/tmp/test.rs` = 0.0, but to `/etc/hosts` = 0.4.
pub trait ArgRiskAnalyzer: Send + Sync {
    fn analyze(&self, tool_name: &str, args: &serde_json::Value) -> f64;
}

/// Default argument risk analyzer — inspects common argument patterns.
pub struct DefaultArgAnalyzer;

impl ArgRiskAnalyzer for DefaultArgAnalyzer {
    fn analyze(&self, tool_name: &str, args: &serde_json::Value) -> f64 {
        let mut risk = 0.0;

        // Path-based escalation
        if let Some(path) = args.get("path").or(args.get("file_path")).and_then(|v| v.as_str()) {
            risk += path_risk(path);
        }

        // Command-based escalation for shell_exec
        if tool_name == "shell_exec" || tool_name == "shell_exec_background" {
            if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                risk += command_risk(cmd);
            }
        }

        // URL-based escalation for http_fetch
        if tool_name == "http_fetch" {
            if let Some(url) = args.get("url").and_then(|v| v.as_str()) {
                risk += url_risk(url);
            }
        }

        risk.clamp(0.0, 0.5) // contextual risk caps at 0.5
    }
}

/// Assess risk from file path sensitivity.
fn path_risk(path: &str) -> f64 {
    let p = Path::new(path);
    let s = path.to_lowercase();

    // System directories → high risk
    if s.starts_with("/etc/") || s.starts_with("/usr/") || s.starts_with("/sys/")
        || s.starts_with("/boot/") || s.starts_with("/dev/")
        || s.starts_with("c:\\windows") || s.starts_with("c:\\program files")
    {
        return 0.4;
    }

    // Home directory dotfiles → medium risk
    if p.components().any(|c| {
        c.as_os_str().to_str().map(|s| s.starts_with('.')).unwrap_or(false)
    }) {
        return 0.15;
    }

    // Known sensitive files
    if s.contains("id_rsa") || s.contains("id_ed25519") || s.contains(".env")
        || s.contains("credentials") || s.contains("secret")
        || s.contains("password") || s.contains(".pem")
    {
        return 0.3;
    }

    0.0
}

/// Assess risk from shell command content.
fn command_risk(cmd: &str) -> f64 {
    let lower = cmd.to_lowercase();

    // Highly dangerous patterns
    if lower.contains("rm -rf") || lower.contains("mkfs")
        || lower.contains("dd if=") || lower.contains("> /dev/")
        || lower.contains(":(){ :|:& };:") // fork bomb
        || lower.contains("curl") && lower.contains("| sh")
        || lower.contains("wget") && lower.contains("| bash")
    {
        return 0.5;
    }

    // Moderately dangerous
    if lower.contains("sudo") || lower.contains("chmod 777")
        || lower.contains("kill -9") || lower.contains("pkill")
        || lower.contains("shutdown") || lower.contains("reboot")
    {
        return 0.3;
    }

    // Network operations
    if lower.contains("curl") || lower.contains("wget")
        || lower.contains("ssh") || lower.contains("scp")
    {
        return 0.15;
    }

    0.0
}

/// Assess risk from URL targets.
fn url_risk(url: &str) -> f64 {
    let lower = url.to_lowercase();

    // Internal/metadata endpoints → SSRF risk
    if lower.contains("169.254.169.254") || lower.contains("metadata.google")
        || lower.contains("localhost") || lower.contains("127.0.0.1")
        || lower.contains("[::1]") || lower.starts_with("file://")
    {
        return 0.4;
    }

    0.0
}

/// The Awareness Classifier — O(1) risk classification for tool invocations.
pub struct AwarenessClassifier {
    /// Base risk score per tool name. O(1) lookup via FxHashMap.
    base_risks: FxHashMap<String, f64>,
    /// Consciousness level thresholds.
    thresholds: LevelThresholds,
    /// Argument risk analyzer (pluggable).
    arg_analyzer: Box<dyn ArgRiskAnalyzer>,
    /// Per-tool level overrides (force a tool to a specific level).
    overrides: FxHashMap<String, ConsciousnessLevel>,
}

impl AwarenessClassifier {
    /// Create a classifier with default tool risk table and balanced thresholds.
    pub fn new() -> Self {
        Self::with_thresholds(LevelThresholds::balanced())
    }

    /// Create a classifier with custom thresholds.
    pub fn with_thresholds(thresholds: LevelThresholds) -> Self {
        Self {
            base_risks: default_risk_table(),
            thresholds,
            arg_analyzer: Box::new(DefaultArgAnalyzer),
            overrides: FxHashMap::default(),
        }
    }

    /// Set a custom argument risk analyzer.
    pub fn with_arg_analyzer(mut self, analyzer: Box<dyn ArgRiskAnalyzer>) -> Self {
        self.arg_analyzer = analyzer;
        self
    }

    /// Override a tool to always classify at a specific level.
    pub fn override_tool(&mut self, tool: impl Into<String>, level: ConsciousnessLevel) {
        self.overrides.insert(tool.into(), level);
    }

    /// Classify a tool invocation → (consciousness level, risk score).
    ///
    /// O(1) for base lookup + O(arg_analysis) for contextual.
    pub fn classify(
        &self,
        tool: &str,
        args: &serde_json::Value,
        sentinel_boost: f64,
    ) -> (ConsciousnessLevel, RiskScore) {
        // Check for forced override first
        if let Some(&forced) = self.overrides.get(tool) {
            return (forced, RiskScore::compute(1.0, 0.0, 0.0));
        }

        let base = self.base_risks.get(tool).copied().unwrap_or(0.6);
        let contextual = self.arg_analyzer.analyze(tool, args);
        let score = RiskScore::compute(base, contextual, sentinel_boost);
        (score.to_level(&self.thresholds), score)
    }

    /// Adjust base risk for a tool (L4 → L0 feedback loop).
    ///
    /// Called by the retrospective trace when human veto rates change:
    /// - High veto rate → increase risk
    /// - Low veto rate with sufficient samples → decrease risk
    pub fn adjust_base_risk(&mut self, tool: &str, delta: f64) {
        if let Some(r) = self.base_risks.get_mut(tool) {
            *r = (*r + delta).clamp(0.0, 1.0);
        }
    }

    /// Get current thresholds.
    pub fn thresholds(&self) -> &LevelThresholds {
        &self.thresholds
    }

    /// Set new thresholds (e.g., from config change).
    pub fn set_thresholds(&mut self, thresholds: LevelThresholds) {
        self.thresholds = thresholds;
    }
}

impl Default for AwarenessClassifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Default risk table — maps tool names to base risk scores.
///
/// Risk scores encode the inherent danger of a tool independent of arguments.
/// The argument analyzer adds contextual risk on top.
fn default_risk_table() -> FxHashMap<String, f64> {
    let mut m = FxHashMap::default();

    // L0: Reflexive — read-only, no side effects
    for tool in &[
        "file_read", "search", "list_dir", "bg_status", "file_search",
        "grep_search", "semantic_search", "read_notebook", "get_errors",
        "file_list", "grep", "workspace_search", "workspace_grep",
        "memory_search", "agents_list", "discover_agents", "ask_human",
    ] {
        m.insert(tool.to_string(), 0.0);
    }

    // L1: Preconscious — limited writes, reversible
    for tool in &[
        "file_write", "file_edit", "git_status", "git_diff", "git_log",
        "git_add", "git_stash", "http_fetch",
        "web_search", "memory_store", "memory_forget",
        "send_notification", "mcp_connect",
    ] {
        m.insert(tool.to_string(), 0.25);
    }

    // L2: Deliberative — significant side effects, semi-reversible
    for tool in &[
        "shell_exec", "shell_exec_background", "git_commit", "git_checkout",
        "git_branch", "git_merge", "bg_kill", "message_send",
        "browser_navigate", "browser_click",
        "mcp_call", "compose_pipeline", "sessions_send",
        "durable_task", "process_start",
    ] {
        m.insert(tool.to_string(), 0.55);
    }

    // L3: Critical — irreversible or high-impact
    for tool in &[
        "git_push", "deploy", "email_send", "delete_file", "rm",
        "subagent_spawn", "database_exec", "credential_store",
        "dynamic_spawn", "spawn_subagent", "cron_create",
    ] {
        m.insert(tool.to_string(), 0.85);
    }

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflexive_tools_are_level_zero() {
        let c = AwarenessClassifier::new();
        let (level, score) = c.classify("file_read", &serde_json::json!({}), 0.0);
        assert_eq!(level, ConsciousnessLevel::Reflexive);
        assert!(score.composite < 0.01);
    }

    #[test]
    fn shell_exec_is_deliberative() {
        let c = AwarenessClassifier::new();
        let (level, _) = c.classify("shell_exec", &serde_json::json!({"command": "ls"}), 0.0);
        assert_eq!(level, ConsciousnessLevel::Deliberative);
    }

    #[test]
    fn dangerous_command_escalates_shell_exec() {
        let c = AwarenessClassifier::new();
        let (level, _) = c.classify(
            "shell_exec",
            &serde_json::json!({"command": "rm -rf /"}),
            0.0,
        );
        assert_eq!(level, ConsciousnessLevel::Critical);
    }

    #[test]
    fn sentinel_boost_escalates_level() {
        let c = AwarenessClassifier::new();
        let (level, _) = c.classify("file_write", &serde_json::json!({}), 0.6);
        // base=0.25 + sentinel=0.6 = 0.85 → Critical
        assert_eq!(level, ConsciousnessLevel::Critical);
    }

    #[test]
    fn override_forces_level() {
        let mut c = AwarenessClassifier::new();
        c.override_tool("file_read", ConsciousnessLevel::Critical);
        let (level, _) = c.classify("file_read", &serde_json::json!({}), 0.0);
        assert_eq!(level, ConsciousnessLevel::Critical);
    }

    #[test]
    fn feedback_adjusts_base_risk() {
        let mut c = AwarenessClassifier::new();
        let (l1, _) = c.classify("file_write", &serde_json::json!({}), 0.0);
        assert_eq!(l1, ConsciousnessLevel::Preconscious);

        // Feedback: users keep vetoing file_write → increase risk
        c.adjust_base_risk("file_write", 0.4);
        let (l2, _) = c.classify("file_write", &serde_json::json!({}), 0.0);
        assert_eq!(l2, ConsciousnessLevel::Deliberative);
    }

    #[test]
    fn system_path_escalates_file_write() {
        let c = AwarenessClassifier::new();
        let (level, _) = c.classify(
            "file_write",
            &serde_json::json!({"path": "/etc/passwd"}),
            0.0,
        );
        // base=0.25 + contextual=0.4 = 0.65 → Deliberative
        assert_eq!(level, ConsciousnessLevel::Deliberative);
    }

    #[test]
    fn unknown_tool_defaults_to_deliberative() {
        let c = AwarenessClassifier::new();
        let (level, score) = c.classify("unknown_tool", &serde_json::json!({}), 0.0);
        assert_eq!(level, ConsciousnessLevel::Deliberative);
        assert!((score.base - 0.6).abs() < f64::EPSILON);
    }
}
