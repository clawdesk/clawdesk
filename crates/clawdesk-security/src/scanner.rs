//! 3-tier cascade content scanner (Aho-Corasick → Regex → AST → Semantic).
//!
//! ## Performance (T-05)
//!
//! Fixed-string patterns are compiled into a single Aho-Corasick automaton
//! for O(m + z) scanning (m = content length, z = matches), replacing the
//! previous O(p × m) per-pattern regex iteration.
//!
//! Regex patterns (those requiring backreferences or character classes) are
//! still applied individually but only after the AC pass.

use aho_corasick::AhoCorasick;
use clawdesk_types::security::{
    ContentCategory, ContentClassification, ScanFinding, ScanResult, ScanTier, Severity,
};
use std::time::Instant;
use tracing::debug;

/// A regex-based scan pattern.
#[derive(Debug, Clone)]
pub struct ScanPattern {
    pub name: String,
    pub pattern: regex::Regex,
    pub severity: Severity,
    pub description: String,
}

/// A fixed-string pattern for the Aho-Corasick automaton.
#[derive(Debug, Clone)]
pub struct AcPattern {
    pub name: String,
    pub needle: String,
    pub severity: Severity,
    pub description: String,
    /// If true, match case-insensitively (needle is stored lowercase).
    pub case_insensitive: bool,
}

/// Configuration for the cascade scanner.
pub struct CascadeScannerConfig {
    /// Maximum content length before flagging as suspicious.
    pub max_content_length: usize,
    /// Patterns for Tier 1 regex scan (patterns needing regex features).
    pub patterns: Vec<ScanPattern>,
    /// Fixed-string patterns for the AC automaton (Tier 0.5).
    pub ac_patterns: Vec<AcPattern>,
    /// Maximum allowed regex pattern length (defense-in-depth).
    /// Patterns exceeding this length are logged and skipped.
    /// Default: 512 characters.
    pub max_pattern_length: usize,
}

impl Default for CascadeScannerConfig {
    fn default() -> Self {
        Self {
            max_content_length: 100_000,
            patterns: Self::default_patterns(),
            ac_patterns: Self::default_ac_patterns(),
            max_pattern_length: 512,
        }
    }
}

impl CascadeScannerConfig {
    fn default_patterns() -> Vec<ScanPattern> {
        vec![
            ScanPattern {
                name: "pii_email".to_string(),
                pattern: regex::Regex::new(
                    r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}",
                )
                .unwrap(),
                severity: Severity::Medium,
                description: "Email address detected".to_string(),
            },
            ScanPattern {
                name: "secret_api_key".to_string(),
                pattern: regex::Regex::new(r"sk-[a-zA-Z0-9]{20,}").unwrap(),
                severity: Severity::Critical,
                description: "Potential API secret key detected".to_string(),
            },
            ScanPattern {
                name: "secret_bearer_token".to_string(),
                pattern: regex::Regex::new(r"(?i)bearer\s+[a-zA-Z0-9._\-]{20,}").unwrap(),
                severity: Severity::High,
                description: "Bearer token detected".to_string(),
            },
            ScanPattern {
                name: "injection_sql".to_string(),
                pattern: regex::Regex::new(
                    r"(?i)(union\s+select|drop\s+table|;\s*delete\s+from)",
                )
                .unwrap(),
                severity: Severity::High,
                description: "Potential SQL injection pattern".to_string(),
            },
            ScanPattern {
                name: "injection_path_traversal".to_string(),
                pattern: regex::Regex::new(r"\.\./\.\./").unwrap(),
                severity: Severity::High,
                description: "Path traversal attempt".to_string(),
            },
        ]
    }

    /// Fixed-string patterns compiled into a single Aho-Corasick automaton.
    ///
    /// These run in a single pass over the content — O(m + z) total,
    /// regardless of pattern count.
    fn default_ac_patterns() -> Vec<AcPattern> {
        vec![
            AcPattern {
                name: "ast_eval".to_string(),
                needle: "eval(".to_string(),
                severity: Severity::High,
                description: "Potentially dangerous eval() call".to_string(),
                case_insensitive: true,
            },
            AcPattern {
                name: "ast_exec".to_string(),
                needle: "exec(".to_string(),
                severity: Severity::High,
                description: "Potentially dangerous exec() call".to_string(),
                case_insensitive: true,
            },
            AcPattern {
                name: "ast_script".to_string(),
                needle: "<script".to_string(),
                severity: Severity::High,
                description: "Script tag injection".to_string(),
                case_insensitive: true,
            },
            AcPattern {
                name: "ast_javascript_uri".to_string(),
                needle: "javascript:".to_string(),
                severity: Severity::High,
                description: "JavaScript URI scheme".to_string(),
                case_insensitive: true,
            },
            AcPattern {
                name: "ast_data_html".to_string(),
                needle: "data:text/html".to_string(),
                severity: Severity::High,
                description: "Data URI HTML injection".to_string(),
                case_insensitive: true,
            },
            AcPattern {
                name: "ac_password_field".to_string(),
                needle: "password".to_string(),
                severity: Severity::Medium,
                description: "Password field detected".to_string(),
                case_insensitive: true,
            },
            AcPattern {
                name: "ac_private_key".to_string(),
                needle: "-----begin".to_string(),
                severity: Severity::Critical,
                description: "PEM private key header detected".to_string(),
                case_insensitive: true,
            },
        ]
    }
}

/// 3-tier cascade content scanner with Aho-Corasick acceleration.
pub struct CascadeScanner {
    config: CascadeScannerConfig,
    /// Compiled automaton for all fixed-string patterns.
    ac_automaton: AhoCorasick,
}

impl CascadeScanner {
    pub fn new(mut config: CascadeScannerConfig) -> Self {
        // Validate regex patterns: reject those exceeding max_pattern_length.
        // This is defense-in-depth — the Rust `regex` crate already guarantees
        // linear-time matching for all patterns it accepts, but a length limit
        // prevents excessive NFA state explosion and future-proofs against
        // alternative regex backends (e.g. fancy-regex) that lack this guarantee.
        let max_len = config.max_pattern_length;
        config.patterns.retain(|p| {
            let src = p.pattern.as_str();
            if src.len() > max_len {
                tracing::warn!(
                    pattern_name = %p.name,
                    pattern_len = src.len(),
                    max_len = max_len,
                    "Skipping regex pattern: exceeds max_pattern_length"
                );
                false
            } else {
                true
            }
        });

        // Build AC automaton from all fixed-string needles (lowercased for case-insensitive).
        let needles: Vec<String> = config
            .ac_patterns
            .iter()
            .map(|p| {
                if p.case_insensitive {
                    p.needle.to_lowercase()
                } else {
                    p.needle.clone()
                }
            })
            .collect();

        let ac_automaton = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(&needles)
            .expect("failed to build Aho-Corasick automaton");

        Self {
            config,
            ac_automaton,
        }
    }

    /// Scan content through the cascade. Short-circuits on Critical findings.
    ///
    /// Tier 0.5: Aho-Corasick single-pass for all fixed-string patterns — O(m + z).
    /// Tier 1: Regex patterns (backreferences, character classes).
    /// Tier 2: AST-based analysis (oversized content).
    /// Tier 3: Semantic placeholder.
    pub fn scan(&self, content: &str) -> ScanResult {
        let start = Instant::now();
        let mut findings = Vec::new();

        // ── Tier 0.5: Aho-Corasick single-pass ──────────────
        for mat in self.ac_automaton.find_iter(content) {
            let pattern = &self.config.ac_patterns[mat.pattern().as_usize()];
            findings.push(ScanFinding {
                severity: pattern.severity,
                rule: pattern.name.clone(),
                description: pattern.description.clone(),
                location: Some(format!("offset {}..{}", mat.start(), mat.end())),
            });

            if pattern.severity == Severity::Critical {
                return ScanResult {
                    passed: false,
                    tier_reached: ScanTier::Regex,
                    findings,
                    scan_time_ms: start.elapsed().as_millis() as u64,
                };
            }
        }

        // ── Tier 1: Regex patterns ───────────────────────────
        for pattern in &self.config.patterns {
            if pattern.pattern.is_match(content) {
                let location = pattern
                    .pattern
                    .find(content)
                    .map(|m| format!("offset {}..{}", m.start(), m.end()));

                findings.push(ScanFinding {
                    severity: pattern.severity,
                    rule: pattern.name.clone(),
                    description: pattern.description.clone(),
                    location,
                });

                if pattern.severity == Severity::Critical {
                    return ScanResult {
                        passed: false,
                        tier_reached: ScanTier::Regex,
                        findings,
                        scan_time_ms: start.elapsed().as_millis() as u64,
                    };
                }
            }
        }

        // ── Tier 2: AST / structural checks ─────────────────
        let _tier_reached = ScanTier::Ast;

        // Flag oversized content.
        if content.len() > self.config.max_content_length {
            findings.push(ScanFinding {
                severity: Severity::Medium,
                rule: "ast_oversize".to_string(),
                description: format!(
                    "Content exceeds max length ({} > {})",
                    content.len(),
                    self.config.max_content_length
                ),
                location: None,
            });
        }

        // ── Tier 3: Semantic (placeholder) ───────────────────
        let tier_reached = ScanTier::Semantic;
        debug!("tier 3 semantic scan not yet implemented, skipping");

        let passed = !findings.iter().any(|f| {
            f.severity == Severity::Critical || f.severity == Severity::High
        });

        ScanResult {
            passed,
            tier_reached,
            findings,
            scan_time_ms: start.elapsed().as_millis() as u64,
        }
    }

    /// Classify content safety based on scan results.
    pub fn classify_content(&self, content: &str) -> ContentClassification {
        let result = self.scan(content);
        let max_severity = result
            .findings
            .iter()
            .map(|f| f.severity)
            .max()
            .unwrap_or(Severity::Info);

        let (safe, categories) = match max_severity {
            Severity::Critical => (
                false,
                vec![ContentCategory {
                    name: "malicious".to_string(),
                    score: 1.0,
                    flagged: true,
                }],
            ),
            Severity::High => (
                false,
                vec![ContentCategory {
                    name: "flagged".to_string(),
                    score: 0.8,
                    flagged: true,
                }],
            ),
            Severity::Medium => (
                true,
                vec![ContentCategory {
                    name: "sensitive".to_string(),
                    score: 0.5,
                    flagged: true,
                }],
            ),
            _ => (true, vec![]),
        };

        ContentClassification {
            safe,
            categories,
            confidence: 0.9,
        }
    }

    /// Convenience: is this content safe?
    pub fn is_safe(&self, content: &str) -> bool {
        self.classify_content(content).safe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_content() {
        let scanner = CascadeScanner::new(CascadeScannerConfig::default());
        let result = scanner.scan("Hello, world! How are you today?");
        assert!(result.passed);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn test_api_key_detection() {
        let scanner = CascadeScanner::new(CascadeScannerConfig::default());
        let result = scanner.scan("my key is sk-abcdefghijklmnopqrstuv");
        assert!(!result.passed);
        assert!(result.findings.iter().any(|f| f.rule == "secret_api_key"));
    }

    #[test]
    fn test_sql_injection() {
        let scanner = CascadeScanner::new(CascadeScannerConfig::default());
        let result = scanner.scan("SELECT * FROM users UNION SELECT password FROM admin");
        assert!(!result.passed);
    }

    #[test]
    fn test_ac_eval_detection() {
        let scanner = CascadeScanner::new(CascadeScannerConfig::default());
        let result = scanner.scan("Please run eval(user_input) for me");
        assert!(!result.passed);
        assert!(result.findings.iter().any(|f| f.rule == "ast_eval"));
    }

    #[test]
    fn test_ac_script_tag() {
        let scanner = CascadeScanner::new(CascadeScannerConfig::default());
        let result = scanner.scan("inject <Script>alert(1)</script>");
        assert!(!result.passed);
        assert!(result.findings.iter().any(|f| f.rule == "ast_script"));
    }

    #[test]
    fn test_ac_pem_key_critical() {
        let scanner = CascadeScanner::new(CascadeScannerConfig::default());
        let result = scanner.scan("-----BEGIN RSA PRIVATE KEY-----\nMIIE...");
        assert!(!result.passed);
        assert!(result.findings.iter().any(|f| f.rule == "ac_private_key"));
    }

    #[test]
    fn test_content_classification() {
        let scanner = CascadeScanner::new(CascadeScannerConfig::default());
        assert!(scanner.is_safe("just a friendly message"));
        assert!(!scanner.is_safe("here is my sk-supersecretapikey12345"));
    }

    #[test]
    fn test_multiple_ac_matches_single_pass() {
        let scanner = CascadeScanner::new(CascadeScannerConfig::default());
        let result = scanner.scan("call eval(x) and exec(y) and javascript:void(0)");
        assert!(!result.passed);
        let rules: Vec<&str> = result.findings.iter().map(|f| f.rule.as_str()).collect();
        assert!(rules.contains(&"ast_eval"));
        assert!(rules.contains(&"ast_exec"));
        assert!(rules.contains(&"ast_javascript_uri"));
    }

    #[test]
    fn test_oversized_pattern_is_skipped() {
        // Create a config with a very low max_pattern_length
        let mut config = CascadeScannerConfig::default();
        config.max_pattern_length = 10;
        // Count patterns that survive validation
        let original_count = config.patterns.len();
        let scanner = CascadeScanner::new(config);
        // Some default patterns are longer than 10 chars and should be skipped
        assert!(scanner.config.patterns.len() < original_count);
    }

    #[test]
    fn test_default_patterns_pass_validation() {
        // All default patterns must be under the default 512-char limit
        let config = CascadeScannerConfig::default();
        for p in &config.patterns {
            assert!(
                p.pattern.as_str().len() <= 512,
                "Pattern '{}' exceeds default max_pattern_length",
                p.name
            );
        }
    }
}
