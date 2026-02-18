//! Skill scaffolding — template generation, validation, and dry-run testing.
//!
//! Provides the core logic behind `clawdesk skill create`, `clawdesk skill lint`,
//! and `clawdesk skill test --dry-run`.
//!
//! ## Design
//!
//! Skill scaffolding generates a complete skill directory from interactive inputs
//! or CLI flags. The output is a `skill.toml` manifest + `prompt.md` file that
//! pass schema validation immediately.

pub use clawdesk_types::estimate_tokens;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::definition::{Skill, SkillId, SkillTrigger, SkillParameter};
use crate::verification::TrustLevel;

// ---------------------------------------------------------------------------
// Scaffold configuration
// ---------------------------------------------------------------------------

/// Input for the skill scaffolding wizard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillScaffoldInput {
    /// Skill identifier (kebab-case, e.g. "ui-design-reviewer").
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Short description (one line).
    pub description: String,
    /// Trigger phrases / keywords.
    pub triggers: Vec<String>,
    /// Tools this skill uses.
    pub tools: Vec<String>,
    /// Skill parameters with types and descriptions.
    pub parameters: Vec<ScaffoldParam>,
    /// Author name.
    pub author: String,
    /// Version (semver).
    pub version: String,
    /// Dependencies (other skill IDs).
    pub dependencies: Vec<String>,
    /// Tags for search/categorization.
    pub tags: Vec<String>,
}

/// A parameter definition for scaffolding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldParam {
    pub name: String,
    pub param_type: String, // "string", "number", "boolean"
    pub description: String,
    pub required: bool,
    pub default_value: Option<String>,
}

impl Default for SkillScaffoldInput {
    fn default() -> Self {
        Self {
            id: "my-skill".to_string(),
            display_name: "My Skill".to_string(),
            description: "A custom ClawDesk skill".to_string(),
            triggers: vec!["trigger-word".to_string()],
            tools: Vec::new(),
            parameters: Vec::new(),
            author: "ClawDesk User".to_string(),
            version: "0.1.0".to_string(),
            dependencies: Vec::new(),
            tags: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Generated output
// ---------------------------------------------------------------------------

/// Result of skill scaffolding.
#[derive(Debug, Clone)]
pub struct ScaffoldOutput {
    /// Path to the generated skill directory.
    pub skill_dir: PathBuf,
    /// Content of skill.toml.
    pub manifest_toml: String,
    /// Content of prompt.md.
    pub prompt_md: String,
    /// Validation warnings (non-fatal).
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Scaffold engine
// ---------------------------------------------------------------------------

/// Generate a complete skill scaffold from input.
pub fn generate_scaffold(input: &SkillScaffoldInput, base_dir: &Path) -> ScaffoldOutput {
    let skill_dir = base_dir.join(&input.id);
    let mut warnings = Vec::new();

    // Validate ID format
    if !is_valid_skill_id(&input.id) {
        warnings.push(format!(
            "Skill ID '{}' should be kebab-case (lowercase, hyphens only)",
            input.id
        ));
    }

    // Generate skill.toml
    let manifest_toml = generate_manifest_toml(input);

    // Generate prompt.md
    let prompt_md = generate_prompt_md(input);

    // Warn about empty triggers
    if input.triggers.is_empty() {
        warnings.push("No triggers defined — skill will only activate via explicit invocation".into());
    }

    // Warn about large prompt
    let estimated_tokens = estimate_tokens(&prompt_md);
    if estimated_tokens > 2000 {
        warnings.push(format!(
            "Prompt is ~{estimated_tokens} tokens — consider splitting into sub-skills"
        ));
    }

    ScaffoldOutput {
        skill_dir,
        manifest_toml,
        prompt_md,
        warnings,
    }
}

/// Generate the skill.toml manifest content.
fn generate_manifest_toml(input: &SkillScaffoldInput) -> String {
    let mut lines = Vec::new();

    lines.push("[skill]".to_string());
    lines.push(format!("id = \"{}\"", input.id));
    lines.push(format!("name = \"{}\"", input.display_name));
    lines.push(format!("description = \"{}\"", input.description));
    lines.push(format!("author = \"{}\"", input.author));
    lines.push(format!("version = \"{}\"", input.version));
    lines.push(String::new());

    if !input.tags.is_empty() {
        let tags: Vec<String> = input.tags.iter().map(|t| format!("\"{}\"", t)).collect();
        lines.push(format!("tags = [{}]", tags.join(", ")));
        lines.push(String::new());
    }

    // Triggers
    if !input.triggers.is_empty() {
        lines.push("[skill.triggers]".to_string());
        let triggers: Vec<String> = input.triggers.iter().map(|t| format!("\"{}\"", t)).collect();
        lines.push(format!("keywords = [{}]", triggers.join(", ")));
        lines.push(String::new());
    }

    // Tools
    if !input.tools.is_empty() {
        lines.push("[skill.tools]".to_string());
        let tools: Vec<String> = input.tools.iter().map(|t| format!("\"{}\"", t)).collect();
        lines.push(format!("require = [{}]", tools.join(", ")));
        lines.push(String::new());
    }

    // Dependencies
    if !input.dependencies.is_empty() {
        lines.push("[skill.dependencies]".to_string());
        for dep in &input.dependencies {
            lines.push(format!("{} = \"*\"", dep));
        }
        lines.push(String::new());
    }

    // Parameters
    for param in &input.parameters {
        lines.push(format!("[[skill.parameters]]"));
        lines.push(format!("name = \"{}\"", param.name));
        lines.push(format!("type = \"{}\"", param.param_type));
        lines.push(format!("description = \"{}\"", param.description));
        lines.push(format!("required = {}", param.required));
        if let Some(ref default) = param.default_value {
            lines.push(format!("default = \"{}\"", default));
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Generate the prompt.md template.
fn generate_prompt_md(input: &SkillScaffoldInput) -> String {
    let mut lines = Vec::new();

    lines.push(format!("# {}", input.display_name));
    lines.push(String::new());
    lines.push(format!("## Purpose"));
    lines.push(String::new());
    lines.push(format!("{}", input.description));
    lines.push(String::new());
    lines.push("## Instructions".to_string());
    lines.push(String::new());
    lines.push("When this skill is activated, follow these guidelines:".to_string());
    lines.push(String::new());
    lines.push("1. Analyze the user's request carefully".to_string());
    lines.push("2. Apply your expertise to provide a thorough response".to_string());
    lines.push("3. Structure your output clearly with headings and sections".to_string());
    lines.push(String::new());

    if !input.parameters.is_empty() {
        lines.push("## Parameters".to_string());
        lines.push(String::new());
        for param in &input.parameters {
            let req = if param.required { "(required)" } else { "(optional)" };
            lines.push(format!("- **{}** {}: {}", param.name, req, param.description));
        }
        lines.push(String::new());
    }

    if !input.tools.is_empty() {
        lines.push("## Available Tools".to_string());
        lines.push(String::new());
        for tool in &input.tools {
            lines.push(format!("- `{}` — Use when needed for this task", tool));
        }
        lines.push(String::new());
    }

    lines.push("## Output Format".to_string());
    lines.push(String::new());
    lines.push("Provide your response in a clear, structured format.".to_string());
    lines.push(String::new());

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Validation (lint)
// ---------------------------------------------------------------------------

/// A lint diagnostic for a skill.
#[derive(Debug, Clone)]
pub struct LintDiagnostic {
    pub skill_id: String,
    pub severity: LintSeverity,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<usize>,
}

/// Severity of a lint diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintSeverity {
    Error,
    Warning,
    Info,
}

/// Lint a skill manifest and prompt.
pub fn lint_skill(manifest_toml: &str, prompt_md: Option<&str>, skill_id: &str) -> Vec<LintDiagnostic> {
    let mut diagnostics = Vec::new();

    // Try parse TOML
    let parsed: Result<toml::Value, _> = manifest_toml.parse();
    match parsed {
        Err(e) => {
            diagnostics.push(LintDiagnostic {
                skill_id: skill_id.to_string(),
                severity: LintSeverity::Error,
                message: format!("Invalid TOML: {e}"),
                file: Some("skill.toml".into()),
                line: None,
            });
            return diagnostics; // Can't continue
        }
        Ok(ref val) => {
            // Check required [skill] section
            if val.get("skill").is_none() {
                diagnostics.push(LintDiagnostic {
                    skill_id: skill_id.to_string(),
                    severity: LintSeverity::Error,
                    message: "Missing [skill] section".into(),
                    file: Some("skill.toml".into()),
                    line: None,
                });
            }

            // Check required fields
            if let Some(skill) = val.get("skill") {
                for field in &["id", "name", "description", "version"] {
                    if skill.get(field).is_none() {
                        diagnostics.push(LintDiagnostic {
                            skill_id: skill_id.to_string(),
                            severity: LintSeverity::Error,
                            message: format!("Missing required field: skill.{field}"),
                            file: Some("skill.toml".into()),
                            line: None,
                        });
                    }
                }

                // Warn if no author
                if skill.get("author").is_none() {
                    diagnostics.push(LintDiagnostic {
                        skill_id: skill_id.to_string(),
                        severity: LintSeverity::Warning,
                        message: "No author specified".into(),
                        file: Some("skill.toml".into()),
                        line: None,
                    });
                }

                // Warn if no triggers
                if skill.get("triggers").is_none() {
                    diagnostics.push(LintDiagnostic {
                        skill_id: skill_id.to_string(),
                        severity: LintSeverity::Warning,
                        message: "No triggers defined — skill will only activate via explicit invocation".into(),
                        file: Some("skill.toml".into()),
                        line: None,
                    });
                }
            }
        }
    }

    // Lint prompt
    if let Some(prompt) = prompt_md {
        let tokens = estimate_tokens(prompt);
        if tokens > 4000 {
            diagnostics.push(LintDiagnostic {
                skill_id: skill_id.to_string(),
                severity: LintSeverity::Warning,
                message: format!("Prompt is ~{tokens} tokens — may consume excessive context budget"),
                file: Some("prompt.md".into()),
                line: None,
            });
        }
        if prompt.trim().is_empty() {
            diagnostics.push(LintDiagnostic {
                skill_id: skill_id.to_string(),
                severity: LintSeverity::Error,
                message: "Prompt is empty".into(),
                file: Some("prompt.md".into()),
                line: None,
            });
        }
    }

    diagnostics
}

// ---------------------------------------------------------------------------
// Dry-run testing
// ---------------------------------------------------------------------------

/// Result of a skill dry-run test.
#[derive(Debug, Clone)]
pub struct DryRunResult {
    /// The assembled prompt including the skill's prompt fragment.
    pub assembled_prompt: String,
    /// Estimated token count for the full prompt.
    pub estimated_tokens: usize,
    /// Tools that would be bound.
    pub bound_tools: Vec<String>,
    /// Parameters detected in the input.
    pub detected_parameters: HashMap<String, String>,
    /// Warnings about the test run.
    pub warnings: Vec<String>,
    /// Skill trigger score (0.0 to 1.0).
    pub trigger_score: f64,
}

/// Run a dry-run test of a skill against sample input.
pub fn dry_run_skill(manifest_toml: &str, prompt_md: &str, test_input: &str) -> DryRunResult {
    let mut warnings = Vec::new();
    let mut bound_tools = Vec::new();

    // Parse manifest to extract tool requirements
    if let Ok(val) = manifest_toml.parse::<toml::Value>() {
        if let Some(skill) = val.get("skill") {
            if let Some(tools) = skill.get("tools").and_then(|t| t.get("require")) {
                if let Some(arr) = tools.as_array() {
                    for tool in arr {
                        if let Some(s) = tool.as_str() {
                            bound_tools.push(s.to_string());
                        }
                    }
                }
            }
        }
    }

    // Assemble the full prompt
    let assembled = format!(
        "--- SKILL PROMPT ---\n{prompt_md}\n--- END SKILL PROMPT ---\n\nUser: {test_input}"
    );

    let estimated_tokens = estimate_tokens(&assembled);

    // Calculate trigger score
    let trigger_score = calculate_trigger_score(manifest_toml, test_input);

    if trigger_score < 0.3 {
        warnings.push(format!(
            "Low trigger score ({trigger_score:.2}) — this input may not activate the skill"
        ));
    }

    if estimated_tokens > 8000 {
        warnings.push(format!(
            "Estimated {estimated_tokens} tokens — this may exceed budget for some models"
        ));
    }

    DryRunResult {
        assembled_prompt: assembled,
        estimated_tokens,
        bound_tools,
        detected_parameters: HashMap::new(),
        warnings,
        trigger_score,
    }
}

/// Calculate trigger match score between a skill manifest and input text.
fn calculate_trigger_score(manifest_toml: &str, input: &str) -> f64 {
    let input_lower = input.to_lowercase();
    let mut score = 0.0;
    let mut total_triggers = 0;

    if let Ok(val) = manifest_toml.parse::<toml::Value>() {
        if let Some(skill) = val.get("skill") {
            if let Some(triggers) = skill.get("triggers").and_then(|t| t.get("keywords")) {
                if let Some(arr) = triggers.as_array() {
                    total_triggers = arr.len();
                    for trigger in arr {
                        if let Some(keyword) = trigger.as_str() {
                            if input_lower.contains(&keyword.to_lowercase()) {
                                score += 1.0;
                            }
                        }
                    }
                }
            }
        }
    }

    if total_triggers > 0 {
        score / total_triggers as f64
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Validate that a skill ID is kebab-case.
pub fn is_valid_skill_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit())
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
}

// Token estimation consolidated in clawdesk_types::tokenizer::estimate_tokens
// Re-exported here for backward compatibility with clawdesk-cli.
// (The `pub use` above makes `clawdesk_skills::scaffold::estimate_tokens` work.)

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_valid_skill_ids() {
        assert!(is_valid_skill_id("my-skill"));
        assert!(is_valid_skill_id("ui-design-reviewer"));
        assert!(is_valid_skill_id("skill123"));
        assert!(!is_valid_skill_id(""));
        assert!(!is_valid_skill_id("-starts-hyphen"));
        assert!(!is_valid_skill_id("ends-hyphen-"));
        assert!(!is_valid_skill_id("double--hyphen"));
        assert!(!is_valid_skill_id("UpperCase"));
    }

    #[test]
    fn test_estimate_tokens() {
        // LUT-based estimator: "hello" = 5 alnum / 4.2 ≈ 1.19 → ceil = 2
        assert_eq!(estimate_tokens("hello"), 2);
        // 400 alnum chars / 4.2 = 95.2 → ceil = 96
        assert_eq!(estimate_tokens(&"a".repeat(400)), 96);
        // Empty string → 0
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_generate_scaffold() {
        let input = SkillScaffoldInput {
            id: "test-skill".into(),
            display_name: "Test Skill".into(),
            description: "A test skill for unit testing".into(),
            triggers: vec!["test".into(), "check".into()],
            tools: vec!["browser".into()],
            parameters: vec![ScaffoldParam {
                name: "target".into(),
                param_type: "string".into(),
                description: "The target to test".into(),
                required: true,
                default_value: None,
            }],
            author: "Test Author".into(),
            version: "1.0.0".into(),
            dependencies: Vec::new(),
            tags: vec!["testing".into()],
        };

        let output = generate_scaffold(&input, Path::new("/tmp/skills"));
        assert!(output.manifest_toml.contains("id = \"test-skill\""));
        assert!(output.manifest_toml.contains("keywords = [\"test\", \"check\"]"));
        assert!(output.manifest_toml.contains("require = [\"browser\"]"));
        assert!(output.prompt_md.contains("# Test Skill"));
        assert!(output.prompt_md.contains("**target**"));
        assert!(output.warnings.is_empty());
    }

    #[test]
    fn test_lint_valid_manifest() {
        let toml = r#"
[skill]
id = "test"
name = "Test"
description = "A test"
version = "1.0.0"
author = "Me"

[skill.triggers]
keywords = ["test"]
"#;
        let diags = lint_skill(toml, Some("# Test prompt"), "test");
        assert!(diags.iter().all(|d| d.severity != LintSeverity::Error));
    }

    #[test]
    fn test_lint_missing_fields() {
        let toml = "[skill]\nid = \"test\"";
        let diags = lint_skill(toml, None, "test");
        let errors: Vec<_> = diags.iter().filter(|d| d.severity == LintSeverity::Error).collect();
        assert!(!errors.is_empty()); // Missing name, description, version
    }

    #[test]
    fn test_lint_invalid_toml() {
        let diags = lint_skill("not valid toml {{", None, "bad");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, LintSeverity::Error);
    }

    #[test]
    fn test_dry_run_with_trigger_match() {
        let toml = r#"
[skill]
id = "weather"
name = "Weather"
description = "Weather info"
version = "1.0.0"
[skill.triggers]
keywords = ["weather", "forecast"]
[skill.tools]
require = ["web_search"]
"#;
        let prompt = "# Weather\nProvide weather information.";
        let result = dry_run_skill(toml, prompt, "What's the weather in London?");

        assert!(result.trigger_score > 0.0);
        assert!(result.bound_tools.contains(&"web_search".to_string()));
        assert!(result.assembled_prompt.contains("Weather"));
        assert!(result.assembled_prompt.contains("London"));
    }

    #[test]
    fn test_dry_run_no_trigger_match() {
        let toml = r#"
[skill]
id = "weather"
name = "Weather"
description = "Weather info"
version = "1.0.0"
[skill.triggers]
keywords = ["weather"]
"#;
        let result = dry_run_skill(toml, "# Weather", "Tell me a joke");
        assert_eq!(result.trigger_score, 0.0);
        assert!(!result.warnings.is_empty());
    }

    #[test]
    fn test_scaffold_output_paths() {
        let input = SkillScaffoldInput::default();
        let output = generate_scaffold(&input, Path::new("/home/user/.clawdesk/skills"));
        assert_eq!(output.skill_dir, PathBuf::from("/home/user/.clawdesk/skills/my-skill"));
    }
}
