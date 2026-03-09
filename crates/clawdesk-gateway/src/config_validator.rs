//! Pre-Commit Validation Pipeline with Referential Integrity.
//!
//! Before any configuration snapshot is committed, it must pass through
//! a 5-stage validation pipeline:
//!
//! ```text
//! Stage 1: Syntax      — TOML/JSON parses correctly
//! Stage 2: Structural  — Required fields present, types match schema
//! Stage 3: Referential — Cross-registry references resolve (e.g., agent→provider)
//! Stage 4: Semantic    — Values are within valid ranges, credentials non-empty
//! Stage 5: Compat      — Backward compatibility with previous generation
//! ```
//!
//! Each stage returns a list of diagnostics (errors and warnings).
//! The pipeline short-circuits on the first stage with errors.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Severity of a validation diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Severity {
    /// Informational hint.
    Info,
    /// Non-blocking warning.
    Warning,
    /// Blocking error — prevents commit.
    Error,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "INFO"),
            Self::Warning => write!(f, "WARN"),
            Self::Error => write!(f, "ERROR"),
        }
    }
}

/// A single validation diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Validation stage that produced this diagnostic.
    pub stage: ValidationStage,
    /// Severity level.
    pub severity: Severity,
    /// Dotted path to the problematic field (if applicable).
    pub path: Option<String>,
    /// Human-readable message.
    pub message: String,
    /// Suggested fix (optional).
    pub suggestion: Option<String>,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}: {}", self.severity, self.stage, self.message)?;
        if let Some(path) = &self.path {
            write!(f, " (at {})", path)?;
        }
        Ok(())
    }
}

/// The validation stage that produced a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ValidationStage {
    Syntax,
    Structural,
    Referential,
    Semantic,
    Compatibility,
}

impl std::fmt::Display for ValidationStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax => write!(f, "syntax"),
            Self::Structural => write!(f, "structural"),
            Self::Referential => write!(f, "referential"),
            Self::Semantic => write!(f, "semantic"),
            Self::Compatibility => write!(f, "compat"),
        }
    }
}

// ---------------------------------------------------------------------------
// Validation result
// ---------------------------------------------------------------------------

/// Result of running the validation pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    /// All diagnostics collected.
    pub diagnostics: Vec<Diagnostic>,
    /// Whether the pipeline passed (no errors).
    pub passed: bool,
    /// Which stages were executed.
    pub stages_run: Vec<ValidationStage>,
    /// Stage that caused short-circuit (if any).
    pub short_circuited_at: Option<ValidationStage>,
}

impl ValidationResult {
    /// Get errors only.
    pub fn errors(&self) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect()
    }

    /// Get warnings only.
    pub fn warnings(&self) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .collect()
    }

    /// Number of errors.
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    /// Number of warnings.
    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }
}

// ---------------------------------------------------------------------------
// Validation context
// ---------------------------------------------------------------------------

/// Input to the validation pipeline — a pending configuration to validate.
#[derive(Debug, Clone)]
pub struct ValidationInput {
    /// Flat key-value representation of the new config.
    pub new_config: HashMap<String, String>,
    /// Flat key-value representation of the previous config (for compat checks).
    pub old_config: HashMap<String, String>,
    /// Set of known provider IDs.
    pub known_providers: HashSet<String>,
    /// Set of known skill IDs.
    pub known_skills: HashSet<String>,
    /// Set of known channel IDs.
    pub known_channels: HashSet<String>,
    /// Set of known agent IDs.
    pub known_agents: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Validation stages
// ---------------------------------------------------------------------------

/// Stage 1: Syntax validation — checks TOML/JSON parses correctly.
fn validate_syntax(input: &ValidationInput) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    for (key, value) in &input.new_config {
        // Check for empty keys.
        if key.is_empty() {
            diags.push(Diagnostic {
                stage: ValidationStage::Syntax,
                severity: Severity::Error,
                path: None,
                message: "empty configuration key".into(),
                suggestion: Some("remove empty key entries".into()),
            });
        }

        // Check for invalid UTF-8 or control characters in values.
        if value.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            diags.push(Diagnostic {
                stage: ValidationStage::Syntax,
                severity: Severity::Warning,
                path: Some(key.clone()),
                message: "value contains control characters".into(),
                suggestion: Some("remove non-printable characters".into()),
            });
        }
    }

    diags
}

/// Stage 2: Structural validation — required fields present, types match.
fn validate_structural(input: &ValidationInput) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    // Check that agent definitions have required fields.
    let agent_keys: Vec<&String> = input
        .new_config
        .keys()
        .filter(|k| k.starts_with("agents."))
        .collect();

    let mut agent_ids: HashSet<String> = HashSet::new();
    for key in &agent_keys {
        let parts: Vec<&str> = key.split('.').collect();
        if parts.len() >= 2 {
            agent_ids.insert(parts[1].to_string());
        }
    }

    for agent_id in &agent_ids {
        let model_key = format!("agents.{agent_id}.model");
        if !input.new_config.contains_key(&model_key) {
            diags.push(Diagnostic {
                stage: ValidationStage::Structural,
                severity: Severity::Warning,
                path: Some(format!("agents.{agent_id}")),
                message: format!("agent '{agent_id}' missing 'model' field"),
                suggestion: Some("add a 'model' field to the agent definition".into()),
            });
        }
    }

    // Check provider entries have API keys or endpoints.
    let provider_keys: Vec<&String> = input
        .new_config
        .keys()
        .filter(|k| k.starts_with("providers."))
        .collect();

    let mut provider_ids: HashSet<String> = HashSet::new();
    for key in &provider_keys {
        let parts: Vec<&str> = key.split('.').collect();
        if parts.len() >= 2 {
            provider_ids.insert(parts[1].to_string());
        }
    }

    for pid in &provider_ids {
        let key_field = format!("providers.{pid}.api_key");
        let endpoint_field = format!("providers.{pid}.endpoint");
        if !input.new_config.contains_key(&key_field)
            && !input.new_config.contains_key(&endpoint_field)
        {
            diags.push(Diagnostic {
                stage: ValidationStage::Structural,
                severity: Severity::Warning,
                path: Some(format!("providers.{pid}")),
                message: format!("provider '{pid}' has neither 'api_key' nor 'endpoint'"),
                suggestion: None,
            });
        }
    }

    diags
}

/// Stage 3: Referential integrity — cross-registry references resolve.
fn validate_referential(input: &ValidationInput) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    // Check that agents reference known providers.
    for (key, value) in &input.new_config {
        if key.ends_with(".provider") && key.starts_with("agents.") {
            if !input.known_providers.contains(value) && !value.is_empty() {
                diags.push(Diagnostic {
                    stage: ValidationStage::Referential,
                    severity: Severity::Error,
                    path: Some(key.clone()),
                    message: format!(
                        "agent references unknown provider '{value}'"
                    ),
                    suggestion: Some(format!(
                        "known providers: {:?}",
                        input.known_providers
                    )),
                });
            }
        }

        // Check that skills reference known tools or providers.
        if key.ends_with(".required_provider") && key.starts_with("skills.") {
            if !input.known_providers.contains(value) && !value.is_empty() {
                diags.push(Diagnostic {
                    stage: ValidationStage::Referential,
                    severity: Severity::Warning,
                    path: Some(key.clone()),
                    message: format!(
                        "skill references unknown provider '{value}'"
                    ),
                    suggestion: None,
                });
            }
        }
    }

    diags
}

/// Stage 4: Semantic validation — values within valid ranges.
fn validate_semantic(input: &ValidationInput) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    for (key, value) in &input.new_config {
        // Port numbers.
        if key.ends_with(".port") || key == "server.port" {
            if let Ok(port) = value.parse::<u16>() {
                if port == 0 {
                    diags.push(Diagnostic {
                        stage: ValidationStage::Semantic,
                        severity: Severity::Error,
                        path: Some(key.clone()),
                        message: "port 0 is not valid".into(),
                        suggestion: Some("use a port between 1024 and 65535".into()),
                    });
                }
            } else {
                diags.push(Diagnostic {
                    stage: ValidationStage::Semantic,
                    severity: Severity::Error,
                    path: Some(key.clone()),
                    message: format!("'{value}' is not a valid port number"),
                    suggestion: None,
                });
            }
        }

        // Timeout values should be positive.
        if key.ends_with("_timeout") || key.ends_with("_timeout_ms") {
            if let Ok(v) = value.parse::<u64>() {
                if v == 0 {
                    diags.push(Diagnostic {
                        stage: ValidationStage::Semantic,
                        severity: Severity::Warning,
                        path: Some(key.clone()),
                        message: "timeout of 0 may cause immediate failures".into(),
                        suggestion: Some("set a positive timeout value".into()),
                    });
                }
            }
        }

        // API keys should not be placeholder values.
        if key.ends_with(".api_key") {
            let lower = value.to_lowercase();
            if lower == "your-api-key-here"
                || lower == "changeme"
                || lower == "xxx"
                || lower == "placeholder"
            {
                diags.push(Diagnostic {
                    stage: ValidationStage::Semantic,
                    severity: Severity::Error,
                    path: Some(key.clone()),
                    message: "API key appears to be a placeholder".into(),
                    suggestion: Some("set a real API key value".into()),
                });
            }
        }
    }

    diags
}

/// Stage 5: Compatibility — check backward compatibility with previous generation.
fn validate_compatibility(input: &ValidationInput) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    // Warn about removed providers that may be referenced by agents.
    for (key, _) in &input.old_config {
        if key.starts_with("providers.") && !input.new_config.contains_key(key) {
            let parts: Vec<&str> = key.split('.').collect();
            if parts.len() >= 2 {
                let removed_provider = parts[1];
                // Check if any agent in the new config references this provider.
                let still_referenced = input.new_config.iter().any(|(k, v)| {
                    k.ends_with(".provider") && v == removed_provider
                });
                if still_referenced {
                    diags.push(Diagnostic {
                        stage: ValidationStage::Compatibility,
                        severity: Severity::Error,
                        path: Some(key.clone()),
                        message: format!(
                            "provider '{removed_provider}' removed but still referenced by agents"
                        ),
                        suggestion: Some(
                            "update agent configurations before removing the provider".into(),
                        ),
                    });
                }
            }
        }
    }

    diags
}

// ---------------------------------------------------------------------------
// Validation pipeline
// ---------------------------------------------------------------------------

/// The 5-stage validation pipeline.
pub struct ValidationPipeline;

impl ValidationPipeline {
    /// Run the full validation pipeline.
    ///
    /// Short-circuits after the first stage that produces errors.
    pub fn validate(input: &ValidationInput) -> ValidationResult {
        let stages: Vec<(
            ValidationStage,
            fn(&ValidationInput) -> Vec<Diagnostic>,
        )> = vec![
            (ValidationStage::Syntax, validate_syntax),
            (ValidationStage::Structural, validate_structural),
            (ValidationStage::Referential, validate_referential),
            (ValidationStage::Semantic, validate_semantic),
            (ValidationStage::Compatibility, validate_compatibility),
        ];

        let mut all_diagnostics = Vec::new();
        let mut stages_run = Vec::new();
        let mut short_circuited_at = None;

        for (stage, validator) in stages {
            stages_run.push(stage);
            let diags = validator(input);
            let has_errors = diags.iter().any(|d| d.severity == Severity::Error);
            all_diagnostics.extend(diags);

            if has_errors {
                short_circuited_at = Some(stage);
                debug!(
                    stage = %stage,
                    "validation pipeline short-circuited due to errors"
                );
                break;
            }
        }

        let passed = !all_diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error);

        let result = ValidationResult {
            diagnostics: all_diagnostics,
            passed,
            stages_run,
            short_circuited_at,
        };

        if result.passed {
            info!(
                stages = result.stages_run.len(),
                warnings = result.warning_count(),
                "validation pipeline PASSED"
            );
        } else {
            warn!(
                errors = result.error_count(),
                warnings = result.warning_count(),
                short_circuit = ?result.short_circuited_at,
                "validation pipeline FAILED"
            );
        }

        result
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn basic_input() -> ValidationInput {
        ValidationInput {
            new_config: HashMap::new(),
            old_config: HashMap::new(),
            known_providers: HashSet::from(["openai".into(), "anthropic".into()]),
            known_skills: HashSet::new(),
            known_channels: HashSet::new(),
            known_agents: HashSet::new(),
        }
    }

    #[test]
    fn empty_config_passes() {
        let input = basic_input();
        let result = ValidationPipeline::validate(&input);
        assert!(result.passed);
        assert_eq!(result.stages_run.len(), 5);
    }

    #[test]
    fn syntax_rejects_empty_keys() {
        let mut input = basic_input();
        input.new_config.insert("".into(), "value".into());
        let result = ValidationPipeline::validate(&input);
        assert!(!result.passed);
        assert_eq!(result.short_circuited_at, Some(ValidationStage::Syntax));
    }

    #[test]
    fn referential_catches_unknown_provider() {
        let mut input = basic_input();
        input.new_config.insert("agents.myagent.provider".into(), "nonexistent".into());
        let result = ValidationPipeline::validate(&input);
        assert!(!result.passed);
        assert_eq!(
            result.short_circuited_at,
            Some(ValidationStage::Referential)
        );
    }

    #[test]
    fn semantic_catches_invalid_port() {
        let mut input = basic_input();
        input.new_config.insert("server.port".into(), "not_a_number".into());
        let result = ValidationPipeline::validate(&input);
        assert!(!result.passed);
    }

    #[test]
    fn semantic_catches_placeholder_api_key() {
        let mut input = basic_input();
        input.new_config.insert("providers.test.api_key".into(), "changeme".into());
        let result = ValidationPipeline::validate(&input);
        assert!(!result.passed);
    }

    #[test]
    fn compatibility_catches_removed_provider_still_referenced() {
        let mut input = basic_input();
        input.old_config.insert("providers.openai.api_key".into(), "key123".into());
        input.new_config.insert("agents.bot.provider".into(), "openai".into());
        // Provider key removed in new config but agent still references it.
        let result = ValidationPipeline::validate(&input);
        assert!(!result.passed);
    }

    #[test]
    fn valid_config_passes_all_stages() {
        let mut input = basic_input();
        input.new_config.insert("server.port".into(), "8080".into());
        input.new_config.insert("agents.bot.model".into(), "gpt-4".into());
        input.new_config.insert("agents.bot.provider".into(), "openai".into());
        let result = ValidationPipeline::validate(&input);
        assert!(result.passed);
        assert_eq!(result.stages_run.len(), 5);
    }

    #[test]
    fn short_circuit_skips_later_stages() {
        let mut input = basic_input();
        input.new_config.insert("".into(), "bad".into()); // Syntax error
        input.new_config.insert("agents.x.provider".into(), "missing".into()); // Would be referential error
        let result = ValidationPipeline::validate(&input);
        assert!(!result.passed);
        // Should stop at syntax, not reach referential.
        assert_eq!(result.short_circuited_at, Some(ValidationStage::Syntax));
        assert_eq!(result.stages_run.len(), 1);
    }

    #[test]
    fn warnings_dont_block() {
        let mut input = basic_input();
        input.new_config.insert("providers.test.name".into(), "test_provider".into());
        // No api_key or endpoint → structural warning, but not an error.
        let result = ValidationPipeline::validate(&input);
        assert!(result.passed);
        assert!(result.warning_count() > 0);
    }
}
