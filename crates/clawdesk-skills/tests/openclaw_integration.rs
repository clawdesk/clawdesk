//! Integration test — batch-load actual OpenClaw skills and generate triage report.
//!
//! This test exercises the OpenClaw adapter against the real skill files
//! in the repository to verify compatibility and produce a triage report.

use clawdesk_skills::openclaw_adapter::{
    self, AdaptationTier, AdapterConfig, BatchAdaptResult,
    parse_skill_md, adapt_skill, resolve_metadata,
};
use std::path::Path;

/// Known OpenClaw skills directory in the workspace.
const OPENCLAW_SKILLS_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn workspace_openclaw_skills_dir() -> std::path::PathBuf {
    // Navigate from clawdesk/crates/clawdesk-skills/ up to llamabot/openclaw/skills/
    Path::new(OPENCLAW_SKILLS_DIR)
        .join("../../..")
        .join("openclaw/skills")
        .canonicalize()
        .expect("openclaw/skills directory should exist in workspace")
}

#[tokio::test]
async fn batch_load_all_openclaw_skills() {
    let skills_dir = workspace_openclaw_skills_dir();
    if !skills_dir.exists() {
        eprintln!("Skipping: OpenClaw skills dir not found at {}", skills_dir.display());
        return;
    }

    let config = AdapterConfig::default();
    let result = openclaw_adapter::load_all_openclaw_skills(&skills_dir, &config).await;

    // Print the triage report for visibility.
    let report = result.triage_report();
    println!("\n{}", report);

    // ── Assertions ──

    // We expect at least 40 skills to load successfully.
    assert!(
        result.total >= 40,
        "expected ≥40 skills, got {}",
        result.total
    );

    // Errors should be minimal (some skills may have unusual formats).
    assert!(
        result.errors.len() <= 5,
        "too many errors ({}): {:?}",
        result.errors.len(),
        result.errors
    );

    // At least some skills should be Direct tier (zero-modification).
    assert!(
        result.direct > 0,
        "expected at least 1 Direct-tier skill, got 0"
    );

    // Distribution sanity checks.
    let direct_pct = result.direct as f64 / result.total as f64;
    assert!(
        direct_pct >= 0.40,
        "expected ≥40% Direct-tier, got {:.0}%",
        direct_pct * 100.0
    );

    // Every adapted skill should have a valid ID and non-empty prompt.
    for adapted in &result.skills {
        assert!(
            !adapted.skill.manifest.id.name().is_empty(),
            "skill has empty name"
        );
        assert!(
            !adapted.skill.prompt_fragment.is_empty(),
            "skill {} has empty prompt",
            adapted.skill.manifest.id
        );
        assert!(
            adapted.skill.manifest.content_hash.is_some(),
            "skill {} has no content hash",
            adapted.skill.manifest.id
        );
        assert!(
            adapted.skill.manifest.estimated_tokens > 0,
            "skill {} has 0 estimated tokens",
            adapted.skill.manifest.id
        );
    }

    // Print summary counts.
    println!("Direct:       {} ({:.0}%)", result.direct, direct_pct * 100.0);
    println!(
        "Context-patch: {} ({:.0}%)",
        result.context_patch,
        result.context_patch as f64 / result.total as f64 * 100.0
    );
    println!(
        "Needs-rewrite: {} ({:.0}%)",
        result.needs_rewrite,
        result.needs_rewrite as f64 / result.total as f64 * 100.0
    );
    if !result.errors.is_empty() {
        println!("Errors:       {}", result.errors.len());
        for e in &result.errors {
            println!("  - {}", e);
        }
    }
}

#[tokio::test]
async fn individual_skill_parsing_consistency() {
    let skills_dir = workspace_openclaw_skills_dir();
    if !skills_dir.exists() {
        return;
    }

    let config = AdapterConfig::default();
    let entries: Vec<_> = std::fs::read_dir(&skills_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();

    for entry in entries {
        let path = entry.path();
        let skill_md = path.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&skill_md).unwrap();
        let result = parse_skill_md(&content);

        match result {
            Ok((fm, body)) => {
                // If frontmatter parses, we should be able to adapt.
                if fm.name.is_some() && fm.description.is_some() {
                    let adapted = adapt_skill(&fm, &body, &config);
                    assert!(
                        adapted.is_ok(),
                        "skill at {} parsed but failed adapt: {:?}",
                        path.display(),
                        adapted.err()
                    );

                    let adapted = adapted.unwrap();
                    // Content hash should be deterministic.
                    let adapted2 = adapt_skill(&fm, &body, &config).unwrap();
                    assert_eq!(
                        adapted.skill.manifest.content_hash,
                        adapted2.skill.manifest.content_hash,
                        "content hash not deterministic for {}",
                        path.display()
                    );
                }
            }
            Err(e) => {
                // Log but don't fail — some files may have novel formats.
                eprintln!("WARN: {} — {}", path.display(), e);
            }
        }
    }
}
