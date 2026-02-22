//! Skill eligibility evaluation engine.
//!
//! ## Eligibility Engine (P1)
//!
//! Ports OpenClaw's `shouldIncludeSkill()` runtime requirements checker.
//! Determines whether a skill should be loaded based on:
//!
//! 1. **Explicit disable**: skill disabled in config
//! 2. **OS filter**: skill's `metadata.openclaw.os` must match runtime platform
//! 3. **Binary requirements**: `requires.bins` — all must be on PATH
//! 4. **Any-binary requirements**: `requires.anyBins` — at least one on PATH
//! 5. **Environment variables**: `requires.env` — must be set
//! 6. **Config paths**: `requires.config` — must be truthy
//! 7. **Always-on override**: `metadata.openclaw.always` bypasses all checks
//!
//! The engine runs at load time AND is re-evaluatable for hot-reload.

use crate::openclaw_adapter::OpenClawMetadata;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tracing::debug;

/// Result of eligibility evaluation for a single skill.
#[derive(Debug, Clone)]
pub struct EligibilityResult {
    /// Whether the skill is eligible for loading.
    pub eligible: bool,
    /// Reasons why the skill is ineligible (empty if eligible).
    pub reasons: Vec<IneligibilityReason>,
    /// Whether the skill was marked as always-on (bypasses checks).
    pub always_on: bool,
}

/// Reason a skill is ineligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IneligibilityReason {
    /// Explicitly disabled in config.
    Disabled,
    /// OS platform doesn't match.
    OsMismatch {
        required: Vec<String>,
        current: String,
    },
    /// Required binary not found on PATH.
    MissingBinary(String),
    /// None of the required alternative binaries found.
    MissingAnyBinary(Vec<String>),
    /// Required environment variable not set.
    MissingEnvVar(String),
    /// Required config path not set or falsy.
    MissingConfig(String),
    /// Not in the allowed skills list.
    NotInAllowList,
}

impl std::fmt::Display for IneligibilityReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IneligibilityReason::Disabled => write!(f, "skill is disabled"),
            IneligibilityReason::OsMismatch { required, current } => {
                write!(
                    f,
                    "OS mismatch: requires {}, running {}",
                    required.join("|"),
                    current
                )
            }
            IneligibilityReason::MissingBinary(bin) => {
                write!(f, "required binary '{}' not found on PATH", bin)
            }
            IneligibilityReason::MissingAnyBinary(bins) => {
                write!(
                    f,
                    "none of the required binaries found: {}",
                    bins.join(", ")
                )
            }
            IneligibilityReason::MissingEnvVar(var) => {
                write!(f, "required env var '{}' not set", var)
            }
            IneligibilityReason::MissingConfig(path) => {
                write!(f, "required config path '{}' not set", path)
            }
            IneligibilityReason::NotInAllowList => {
                write!(f, "skill not in the allowed skills list")
            }
        }
    }
}

/// Configuration for the eligibility engine.
#[derive(Debug, Clone, Default)]
pub struct EligibilityConfig {
    /// Explicitly disabled skill names.
    pub disabled_skills: HashSet<String>,
    /// If set, only these skills are allowed (allowlist mode).
    pub allowed_skills: Option<HashSet<String>>,
    /// Config values available for `requires.config` checks.
    /// Maps config path (e.g., "channels.slack") to whether it's truthy.
    pub config_values: HashMap<String, bool>,
    /// Additional environment variables to inject (beyond OS env).
    /// Used for config-to-env mapping.
    pub injected_env: HashMap<String, String>,
}

/// Skill eligibility evaluation engine.
pub struct EligibilityEngine {
    config: EligibilityConfig,
    /// Cached current OS identifier.
    current_os: String,
    /// Cached PATH binary lookup results.
    binary_cache: HashMap<String, bool>,
}

impl EligibilityEngine {
    /// Create a new eligibility engine.
    pub fn new(config: EligibilityConfig) -> Self {
        let current_os = normalize_os(std::env::consts::OS);
        Self {
            config,
            current_os,
            binary_cache: HashMap::new(),
        }
    }

    /// Evaluate whether a skill is eligible based on its metadata.
    pub fn evaluate(&mut self, skill_name: &str, meta: &OpenClawMetadata) -> EligibilityResult {
        let mut reasons = Vec::new();

        // Check 7: Always-on override — bypasses all other checks
        if meta.always {
            debug!(skill = %skill_name, "always-on skill — bypassing eligibility checks");
            return EligibilityResult {
                eligible: true,
                reasons: vec![],
                always_on: true,
            };
        }

        // Check 1: Explicit disable
        if self.config.disabled_skills.contains(skill_name) {
            reasons.push(IneligibilityReason::Disabled);
        }

        // Check 1b: Allowlist
        if let Some(ref allowed) = self.config.allowed_skills {
            if !allowed.contains(skill_name) {
                reasons.push(IneligibilityReason::NotInAllowList);
            }
        }

        // Check 2: OS filter
        if !meta.os_filters.is_empty() {
            let matches = meta
                .os_filters
                .iter()
                .any(|os| normalize_os(os) == self.current_os);
            if !matches {
                reasons.push(IneligibilityReason::OsMismatch {
                    required: meta.os_filters.clone(),
                    current: self.current_os.clone(),
                });
            }
        }

        // Check 3: Required binaries (all must be on PATH)
        for bin in &meta.required_bins {
            if !self.check_binary(bin) {
                reasons.push(IneligibilityReason::MissingBinary(bin.clone()));
            }
        }

        // Check 4: Any-binary (at least one must be on PATH)
        if !meta.any_bins.is_empty() {
            let any_found = meta.any_bins.iter().any(|bin| self.check_binary(bin));
            if !any_found {
                reasons.push(IneligibilityReason::MissingAnyBinary(
                    meta.any_bins.clone(),
                ));
            }
        }

        // Check 5: Required environment variables
        for var in &meta.required_env {
            let has_env = std::env::var(var).is_ok()
                || self.config.injected_env.contains_key(var);
            if !has_env {
                reasons.push(IneligibilityReason::MissingEnvVar(var.clone()));
            }
        }

        // Check 6: Required config paths
        for path in &meta.required_config {
            let has_config = self
                .config
                .config_values
                .get(path)
                .copied()
                .unwrap_or(false);
            if !has_config {
                reasons.push(IneligibilityReason::MissingConfig(path.clone()));
            }
        }

        let eligible = reasons.is_empty();
        debug!(
            skill = %skill_name,
            eligible,
            reasons = reasons.len(),
            "eligibility evaluation"
        );

        EligibilityResult {
            eligible,
            reasons,
            always_on: false,
        }
    }

    /// Re-evaluate a single skill (for hot-reload after user installs a binary).
    pub fn reevaluate(&mut self, skill_name: &str, meta: &OpenClawMetadata) -> EligibilityResult {
        // Clear binary cache to pick up newly installed tools
        self.binary_cache.clear();
        self.evaluate(skill_name, meta)
    }

    /// Clear the binary PATH cache (call after PATH changes).
    pub fn clear_cache(&mut self) {
        self.binary_cache.clear();
    }

    /// Check if a binary is available on PATH (with caching).
    fn check_binary(&mut self, name: &str) -> bool {
        if let Some(&cached) = self.binary_cache.get(name) {
            return cached;
        }

        let found = find_on_path(name);
        self.binary_cache.insert(name.to_string(), found);
        found
    }
}

/// Normalize OS identifier to match OpenClaw conventions.
///
/// OpenClaw uses `"darwin"` for macOS, `"linux"` for Linux, `"windows"` for Windows.
/// Rust's `std::env::consts::OS` already uses these names, but we normalize
/// to lowercase for case-insensitive comparison.
fn normalize_os(os: &str) -> String {
    os.to_lowercase()
}

/// Check if a binary exists on PATH.
///
/// Uses a simple PATH search instead of the `which` crate to avoid
/// adding a dependency. Falls back to `which` semantics.
fn find_on_path(name: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        for dir in path_var.split(separator) {
            let candidate = PathBuf::from(dir).join(name);
            if candidate.exists() {
                return true;
            }
            // On Windows, also check with .exe extension
            #[cfg(windows)]
            {
                let exe_candidate = PathBuf::from(dir).join(format!("{}.exe", name));
                if exe_candidate.exists() {
                    return true;
                }
            }
        }
    }
    false
}

// ═══════════════════════════════════════════════════════════════════════════
// Remote Bin Probing
// ═══════════════════════════════════════════════════════════════════════════

/// Result of probing a binary on the system.
#[derive(Debug, Clone)]
pub struct BinProbeResult {
    /// Binary name.
    pub name: String,
    /// Whether the binary was found.
    pub found: bool,
    /// Full path to the binary (if found).
    pub path: Option<PathBuf>,
    /// Version string (if binary was found and --version works).
    pub version: Option<String>,
}

/// Probe a binary: check if it exists on PATH and get its version.
pub fn probe_binary(name: &str) -> BinProbeResult {
    let path = find_binary_path(name);
    let found = path.is_some();
    let version = if found {
        get_binary_version(name)
    } else {
        None
    };

    BinProbeResult {
        name: name.to_string(),
        found,
        path,
        version,
    }
}

/// Find the full path of a binary on PATH.
fn find_binary_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    let separator = if cfg!(windows) { ';' } else { ':' };
    for dir in path_var.split(separator) {
        let candidate = PathBuf::from(dir).join(name);
        if candidate.exists() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe_candidate = PathBuf::from(dir).join(format!("{}.exe", name));
            if exe_candidate.exists() {
                return Some(exe_candidate);
            }
        }
    }
    None
}

/// Try to get the version of a binary by running `binary --version`.
fn get_binary_version(name: &str) -> Option<String> {
    use std::process::Command;

    let output = Command::new(name)
        .arg("--version")
        .output()
        .ok()?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Take just the first line
        stdout.lines().next().map(|l| l.trim().to_string())
    } else {
        None
    }
}

/// Probe all binaries required by a skill's metadata.
pub fn probe_skill_requirements(
    meta: &OpenClawMetadata,
) -> Vec<BinProbeResult> {
    let mut results = Vec::new();

    for bin in &meta.required_bins {
        results.push(probe_binary(bin));
    }

    for bin in &meta.any_bins {
        results.push(probe_binary(bin));
    }

    results
}

/// Formatted eligibility check report for CLI display.
pub fn format_eligibility_report(
    skill_name: &str,
    result: &EligibilityResult,
    probes: &[BinProbeResult],
) -> String {
    let mut report = String::new();
    report.push_str(&format!("Eligibility: {}\n", skill_name));
    report.push_str(&format!(
        "  Status: {}\n",
        if result.eligible { "✓ Eligible" } else { "✗ Ineligible" }
    ));

    if result.always_on {
        report.push_str("  Mode: always-on (bypasses all checks)\n");
    }

    if !result.reasons.is_empty() {
        report.push_str("  Issues:\n");
        for reason in &result.reasons {
            report.push_str(&format!("    • {}\n", reason));
        }
    }

    if !probes.is_empty() {
        report.push_str("  Binary probes:\n");
        for probe in probes {
            let status = if probe.found { "✓" } else { "✗" };
            let version = probe
                .version
                .as_deref()
                .unwrap_or("unknown version");
            let path = probe
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not found".to_string());
            report.push_str(&format!(
                "    {} {} — {} ({})\n",
                status, probe.name, version, path
            ));
        }
    }

    report
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_meta() -> OpenClawMetadata {
        OpenClawMetadata::default()
    }

    #[test]
    fn no_requirements_is_eligible() {
        let mut engine = EligibilityEngine::new(EligibilityConfig::default());
        let result = engine.evaluate("test-skill", &empty_meta());
        assert!(result.eligible);
        assert!(result.reasons.is_empty());
        assert!(!result.always_on);
    }

    #[test]
    fn always_on_bypasses_all_checks() {
        let mut config = EligibilityConfig::default();
        config.disabled_skills.insert("oracle".to_string());

        let mut engine = EligibilityEngine::new(config);
        let meta = OpenClawMetadata {
            always: true,
            ..Default::default()
        };
        let result = engine.evaluate("oracle", &meta);
        assert!(result.eligible);
        assert!(result.always_on);
    }

    #[test]
    fn disabled_skill_is_ineligible() {
        let mut config = EligibilityConfig::default();
        config.disabled_skills.insert("bad-skill".to_string());

        let mut engine = EligibilityEngine::new(config);
        let result = engine.evaluate("bad-skill", &empty_meta());
        assert!(!result.eligible);
        assert!(result
            .reasons
            .contains(&IneligibilityReason::Disabled));
    }

    #[test]
    fn os_filter_mismatch() {
        let mut engine = EligibilityEngine::new(EligibilityConfig::default());
        let meta = OpenClawMetadata {
            os_filters: vec!["not-a-real-os".to_string()],
            ..Default::default()
        };
        let result = engine.evaluate("os-skill", &meta);
        assert!(!result.eligible);
        assert!(result.reasons.iter().any(|r| matches!(r, IneligibilityReason::OsMismatch { .. })));
    }

    #[test]
    fn os_filter_matches_current() {
        let mut engine = EligibilityEngine::new(EligibilityConfig::default());
        let current = std::env::consts::OS.to_string();
        let meta = OpenClawMetadata {
            os_filters: vec![current],
            ..Default::default()
        };
        let result = engine.evaluate("os-skill", &meta);
        assert!(result.eligible);
    }

    #[test]
    fn missing_binary_is_ineligible() {
        let mut engine = EligibilityEngine::new(EligibilityConfig::default());
        let meta = OpenClawMetadata {
            required_bins: vec!["nonexistent_binary_12345".to_string()],
            ..Default::default()
        };
        let result = engine.evaluate("bin-skill", &meta);
        assert!(!result.eligible);
        assert!(result.reasons.iter().any(|r| matches!(r,
            IneligibilityReason::MissingBinary(b) if b == "nonexistent_binary_12345"
        )));
    }

    #[test]
    fn common_binary_is_eligible() {
        // `ls` or `cat` should be on PATH on macOS/Linux
        let mut engine = EligibilityEngine::new(EligibilityConfig::default());
        let meta = OpenClawMetadata {
            required_bins: vec!["ls".to_string()],
            ..Default::default()
        };
        let result = engine.evaluate("ls-skill", &meta);
        assert!(result.eligible);
    }

    #[test]
    fn any_bins_one_present() {
        let mut engine = EligibilityEngine::new(EligibilityConfig::default());
        let meta = OpenClawMetadata {
            any_bins: vec![
                "nonexistent_12345".to_string(),
                "ls".to_string(), // should be found
                "also_nonexistent".to_string(),
            ],
            ..Default::default()
        };
        let result = engine.evaluate("any-bin-skill", &meta);
        assert!(result.eligible);
    }

    #[test]
    fn any_bins_none_present() {
        let mut engine = EligibilityEngine::new(EligibilityConfig::default());
        let meta = OpenClawMetadata {
            any_bins: vec!["no_exist_a".to_string(), "no_exist_b".to_string()],
            ..Default::default()
        };
        let result = engine.evaluate("any-bin-skill", &meta);
        assert!(!result.eligible);
        assert!(result.reasons.iter().any(|r| matches!(r, IneligibilityReason::MissingAnyBinary(_))));
    }

    #[test]
    fn missing_env_var_is_ineligible() {
        let mut engine = EligibilityEngine::new(EligibilityConfig::default());
        let meta = OpenClawMetadata {
            required_env: vec!["TOTALLY_FAKE_ENV_VAR_12345".to_string()],
            ..Default::default()
        };
        let result = engine.evaluate("env-skill", &meta);
        assert!(!result.eligible);
        assert!(result.reasons.iter().any(|r| matches!(r,
            IneligibilityReason::MissingEnvVar(v) if v == "TOTALLY_FAKE_ENV_VAR_12345"
        )));
    }

    #[test]
    fn injected_env_satisfies_requirement() {
        let mut config = EligibilityConfig::default();
        config
            .injected_env
            .insert("MY_API_KEY".to_string(), "secret".to_string());

        let mut engine = EligibilityEngine::new(config);
        let meta = OpenClawMetadata {
            required_env: vec!["MY_API_KEY".to_string()],
            ..Default::default()
        };
        let result = engine.evaluate("api-skill", &meta);
        assert!(result.eligible);
    }

    #[test]
    fn missing_config_is_ineligible() {
        let mut engine = EligibilityEngine::new(EligibilityConfig::default());
        let meta = OpenClawMetadata {
            required_config: vec!["channels.slack".to_string()],
            ..Default::default()
        };
        let result = engine.evaluate("config-skill", &meta);
        assert!(!result.eligible);
        assert!(result.reasons.iter().any(|r| matches!(r,
            IneligibilityReason::MissingConfig(p) if p == "channels.slack"
        )));
    }

    #[test]
    fn config_value_satisfies_requirement() {
        let mut config = EligibilityConfig::default();
        config
            .config_values
            .insert("channels.slack".to_string(), true);

        let mut engine = EligibilityEngine::new(config);
        let meta = OpenClawMetadata {
            required_config: vec!["channels.slack".to_string()],
            ..Default::default()
        };
        let result = engine.evaluate("config-skill", &meta);
        assert!(result.eligible);
    }

    #[test]
    fn allowlist_filters_skills() {
        let mut config = EligibilityConfig::default();
        config.allowed_skills = Some(["weather", "github"].iter().map(|s| s.to_string()).collect());

        let mut engine = EligibilityEngine::new(config);

        let r1 = engine.evaluate("weather", &empty_meta());
        assert!(r1.eligible);

        let r2 = engine.evaluate("himalaya", &empty_meta());
        assert!(!r2.eligible);
        assert!(r2.reasons.contains(&IneligibilityReason::NotInAllowList));
    }

    #[test]
    fn binary_cache_works() {
        let mut engine = EligibilityEngine::new(EligibilityConfig::default());

        // First check — populates cache
        let found1 = engine.check_binary("ls");
        // Second check — hits cache
        let found2 = engine.check_binary("ls");
        assert_eq!(found1, found2);

        // Clear cache
        engine.clear_cache();
        assert!(engine.binary_cache.is_empty());
    }

    #[test]
    fn ineligibility_reason_display() {
        let r = IneligibilityReason::MissingBinary("himalaya".to_string());
        assert!(r.to_string().contains("himalaya"));
        assert!(r.to_string().contains("PATH"));
    }

    #[test]
    fn probe_known_binary() {
        let result = probe_binary("ls");
        assert!(result.found);
        assert!(result.path.is_some());
    }

    #[test]
    fn probe_unknown_binary() {
        let result = probe_binary("nonexistent_binary_xyz_999");
        assert!(!result.found);
        assert!(result.path.is_none());
        assert!(result.version.is_none());
    }

    #[test]
    fn format_report_eligible() {
        let result = EligibilityResult {
            eligible: true,
            reasons: vec![],
            always_on: false,
        };
        let report = format_eligibility_report("test-skill", &result, &[]);
        assert!(report.contains("✓ Eligible"));
    }

    #[test]
    fn format_report_with_probes() {
        let result = EligibilityResult {
            eligible: true,
            reasons: vec![],
            always_on: false,
        };
        let probes = vec![
            BinProbeResult {
                name: "jq".to_string(),
                found: true,
                path: Some(PathBuf::from("/usr/bin/jq")),
                version: Some("jq-1.7".to_string()),
            },
            BinProbeResult {
                name: "missing".to_string(),
                found: false,
                path: None,
                version: None,
            },
        ];
        let report = format_eligibility_report("test-skill", &result, &probes);
        assert!(report.contains("✓ jq"));
        assert!(report.contains("✗ missing"));
    }
}
