//! CLI commands for skill authoring: `clawdesk skill create`, `clawdesk skill lint`,
//! `clawdesk skill test`.
//!
//! Provides the CLI surface for the scaffold engine in `clawdesk-skills::scaffold`.

use clawdesk_skills::scaffold::{
    SkillScaffoldInput, ScaffoldParam, generate_scaffold, lint_skill, dry_run_skill,
    LintSeverity, is_valid_skill_id, estimate_tokens,
};
use std::path::{Path, PathBuf};

/// Run the skill creation wizard (non-interactive mode for programmatic use).
pub fn cmd_skill_create(
    id: &str,
    display_name: Option<&str>,
    description: Option<&str>,
    triggers: Vec<String>,
    tools: Vec<String>,
    author: Option<&str>,
    base_dir: &Path,
) -> Result<PathBuf, String> {
    if !is_valid_skill_id(id) {
        return Err(format!(
            "Invalid skill ID '{id}'. Must be kebab-case (lowercase letters, digits, hyphens)."
        ));
    }

    let input = SkillScaffoldInput {
        id: id.to_string(),
        display_name: display_name.unwrap_or(id).to_string(),
        description: description.unwrap_or("A custom ClawDesk skill").to_string(),
        triggers,
        tools,
        parameters: Vec::new(),
        author: author.unwrap_or("ClawDesk User").to_string(),
        version: "0.1.0".to_string(),
        dependencies: Vec::new(),
        tags: Vec::new(),
    };

    let output = generate_scaffold(&input, base_dir);

    // Print warnings
    for warning in &output.warnings {
        eprintln!("  ⚠ {warning}");
    }

    Ok(output.skill_dir)
}

/// Run skill linting across all skills in a directory.
pub fn cmd_skill_lint(skills_dir: &Path) -> Result<LintReport, String> {
    let mut report = LintReport {
        skills_checked: 0,
        errors: 0,
        warnings: 0,
        diagnostics: Vec::new(),
    };

    if !skills_dir.exists() {
        return Err(format!("Skills directory does not exist: {}", skills_dir.display()));
    }

    // Scan for skill directories
    let entries = std::fs::read_dir(skills_dir)
        .map_err(|e| format!("Failed to read skills directory: {e}"))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let skill_id = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let manifest_path = path.join("skill.toml");
        let prompt_path = path.join("prompt.md");

        if !manifest_path.exists() {
            report.diagnostics.push(LintEntry {
                skill_id: skill_id.clone(),
                severity: "error".into(),
                message: "Missing skill.toml".into(),
            });
            report.errors += 1;
            report.skills_checked += 1;
            continue;
        }

        let manifest = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("Failed to read {}: {e}", manifest_path.display()))?;

        let prompt = if prompt_path.exists() {
            Some(std::fs::read_to_string(&prompt_path)
                .map_err(|e| format!("Failed to read {}: {e}", prompt_path.display()))?)
        } else {
            None
        };

        let diags = lint_skill(&manifest, prompt.as_deref(), &skill_id);

        for diag in diags {
            match diag.severity {
                LintSeverity::Error => report.errors += 1,
                LintSeverity::Warning => report.warnings += 1,
                LintSeverity::Info => {}
            }
            report.diagnostics.push(LintEntry {
                skill_id: diag.skill_id,
                severity: format!("{:?}", diag.severity).to_lowercase(),
                message: diag.message,
            });
        }

        report.skills_checked += 1;
    }

    Ok(report)
}

/// Run a dry-run test of a skill.
pub fn cmd_skill_test(skill_dir: &Path, input: &str) -> Result<TestReport, String> {
    let manifest_path = skill_dir.join("skill.toml");
    let prompt_path = skill_dir.join("prompt.md");

    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("Failed to read skill.toml: {e}"))?;
    let prompt = std::fs::read_to_string(&prompt_path)
        .unwrap_or_else(|_| String::new());

    let result = dry_run_skill(&manifest, &prompt, input);

    Ok(TestReport {
        trigger_score: result.trigger_score,
        estimated_tokens: result.estimated_tokens,
        bound_tools: result.bound_tools,
        warnings: result.warnings,
        prompt_preview: if result.assembled_prompt.len() > 500 {
            format!("{}...", &result.assembled_prompt[..500])
        } else {
            result.assembled_prompt
        },
    })
}

/// Summary of a lint run.
#[derive(Debug, Clone)]
pub struct LintReport {
    pub skills_checked: usize,
    pub errors: usize,
    pub warnings: usize,
    pub diagnostics: Vec<LintEntry>,
}

/// A single lint entry.
#[derive(Debug, Clone)]
pub struct LintEntry {
    pub skill_id: String,
    pub severity: String,
    pub message: String,
}

/// Result of a dry-run skill test.
#[derive(Debug, Clone)]
pub struct TestReport {
    pub trigger_score: f64,
    pub estimated_tokens: usize,
    pub bound_tools: Vec<String>,
    pub warnings: Vec<String>,
    pub prompt_preview: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_cmd_skill_create_invalid_id() {
        let result = cmd_skill_create(
            "Invalid-ID", None, None, vec![], vec![], None,
            Path::new("/tmp"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_cmd_skill_create_valid() {
        let result = cmd_skill_create(
            "my-skill",
            Some("My Skill"),
            Some("A test skill"),
            vec!["test".into()],
            vec!["browser".into()],
            Some("Author"),
            Path::new("/tmp"),
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("/tmp/my-skill"));
    }

    #[test]
    fn test_cmd_skill_lint_nonexistent() {
        let result = cmd_skill_lint(Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }
}
