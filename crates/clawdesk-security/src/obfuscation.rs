//! # Obfuscation Detection for Execution Approvals
//!
//! Detects when a shell command uses encoding, piping, or variable
//! expansion to hide its true intent from the approval system.
//!
//! This is a Layer 4 (Judgment Engineering) enhancement. The existing
//! `CascadeScanner` catches known-bad patterns, but doesn't detect
//! *intentional obscuring* of otherwise-blocked commands.
//!
//! Examples caught:
//! - `echo "cm0gLXJmIC8=" | base64 -d | sh`  (base64-encoded `rm -rf /`)
//! - `eval $(printf '\x72\x6d')` (hex-encoded `rm`)
//! - `$'\x72\x6d' -rf /` (ANSI-C quoting)
//! - `r""m -rf /` (zero-width chars or empty-string injection)

use serde::{Deserialize, Serialize};

/// Result of obfuscation analysis.
#[derive(Debug, Clone, Serialize)]
pub struct ObfuscationReport {
    pub is_obfuscated: bool,
    pub confidence: f64,
    pub signals: Vec<ObfuscationSignal>,
    pub recommendation: ObfuscationAction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObfuscationSignal {
    pub kind: SignalKind,
    pub detail: String,
    pub severity: u8, // 1-10
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalKind {
    Base64Pipe,
    HexEscape,
    AnsiCQuoting,
    EvalExpansion,
    VariableSubstitution,
    BacktickExec,
    ZeroWidthChars,
    UnicodeHomoglyph,
    ExcessiveEscaping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ObfuscationAction {
    Allow,
    RequireApproval,
    Deny,
}

/// Analyze a command for obfuscation patterns.
pub fn analyze_obfuscation(command: &str) -> ObfuscationReport {
    let mut signals = Vec::new();

    // ── Base64 piped to shell ──
    if command.contains("base64") && (command.contains("| sh") || command.contains("| bash") || command.contains("| zsh")) {
        signals.push(ObfuscationSignal {
            kind: SignalKind::Base64Pipe,
            detail: "Base64-encoded content piped to shell interpreter".into(),
            severity: 9,
        });
    }

    // ── Hex escapes ──
    let hex_count = command.matches("\\x").count();
    if hex_count >= 3 {
        signals.push(ObfuscationSignal {
            kind: SignalKind::HexEscape,
            detail: format!("{} hex escape sequences detected", hex_count),
            severity: 7,
        });
    }

    // ── ANSI-C quoting ($'...') ──
    if command.contains("$'\\x") || command.contains("$'\\0") {
        signals.push(ObfuscationSignal {
            kind: SignalKind::AnsiCQuoting,
            detail: "ANSI-C quoting with escape sequences".into(),
            severity: 8,
        });
    }

    // ── eval with dynamic expansion ──
    if command.contains("eval ") && (command.contains("$(") || command.contains("`")) {
        signals.push(ObfuscationSignal {
            kind: SignalKind::EvalExpansion,
            detail: "eval with command substitution".into(),
            severity: 8,
        });
    }

    // ── Variable substitution hiding command names ──
    // e.g., $cmd where cmd=rm
    let var_count = command.matches("${").count() + command.matches("$(" ).count();
    if var_count >= 3 {
        signals.push(ObfuscationSignal {
            kind: SignalKind::VariableSubstitution,
            detail: format!("{} variable substitutions", var_count),
            severity: 5,
        });
    }

    // ── Backtick execution ──
    let backtick_count = command.matches('`').count();
    if backtick_count >= 2 {
        signals.push(ObfuscationSignal {
            kind: SignalKind::BacktickExec,
            detail: "Backtick command substitution".into(),
            severity: 6,
        });
    }

    // ── Zero-width characters ──
    let zwc = ['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}', '\u{00AD}'];
    let zw_count: usize = zwc.iter().map(|c| command.matches(*c).count()).sum();
    if zw_count > 0 {
        signals.push(ObfuscationSignal {
            kind: SignalKind::ZeroWidthChars,
            detail: format!("{} zero-width characters hiding command structure", zw_count),
            severity: 10,
        });
    }

    // ── Excessive escaping ──
    let backslash_count = command.matches('\\').count();
    let cmd_len = command.len();
    if cmd_len > 10 && backslash_count as f64 / cmd_len as f64 > 0.15 {
        signals.push(ObfuscationSignal {
            kind: SignalKind::ExcessiveEscaping,
            detail: format!("{}% of command is escape characters", (backslash_count * 100) / cmd_len),
            severity: 6,
        });
    }

    // Compute overall confidence and recommendation
    let max_severity = signals.iter().map(|s| s.severity).max().unwrap_or(0);
    let total_severity: u32 = signals.iter().map(|s| s.severity as u32).sum();
    let confidence = if signals.is_empty() {
        0.0
    } else {
        (total_severity as f64 / (signals.len() as f64 * 10.0)).min(1.0)
    };

    let recommendation = if max_severity >= 9 {
        ObfuscationAction::Deny
    } else if max_severity >= 6 || signals.len() >= 3 {
        ObfuscationAction::RequireApproval
    } else {
        ObfuscationAction::Allow
    };

    ObfuscationReport {
        is_obfuscated: !signals.is_empty(),
        confidence,
        signals,
        recommendation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_command() {
        let r = analyze_obfuscation("ls -la /tmp");
        assert!(!r.is_obfuscated);
        assert_eq!(r.recommendation, ObfuscationAction::Allow);
    }

    #[test]
    fn test_base64_pipe() {
        let r = analyze_obfuscation("echo 'cm0gLXJmIC8=' | base64 -d | sh");
        assert!(r.is_obfuscated);
        assert_eq!(r.recommendation, ObfuscationAction::Deny);
        assert!(r.signals.iter().any(|s| s.kind == SignalKind::Base64Pipe));
    }

    #[test]
    fn test_hex_escape() {
        let r = analyze_obfuscation("printf '\\x72\\x6d\\x20\\x2d\\x72\\x66' | sh");
        assert!(r.signals.iter().any(|s| s.kind == SignalKind::HexEscape));
    }

    #[test]
    fn test_eval_expansion() {
        let r = analyze_obfuscation("eval $(echo 'rm -rf ~')");
        assert!(r.signals.iter().any(|s| s.kind == SignalKind::EvalExpansion));
    }

    #[test]
    fn test_zero_width_chars() {
        let cmd = format!("r\u{200B}m -rf /");
        let r = analyze_obfuscation(&cmd);
        assert!(r.signals.iter().any(|s| s.kind == SignalKind::ZeroWidthChars));
        assert_eq!(r.recommendation, ObfuscationAction::Deny);
    }
}
