//! Compile-time embedded OpenClaw skills via `include_dir!`.
//!
//! Bakes all `openclaw-skills/*/SKILL.md` files (and any `references/*.md`)
//! into the binary's `.rodata` section. At startup, iterates the embedded
//! directory tree, calls `parse_skill_md()` + `adapt_skill()` (both pure —
//! no filesystem needed), and returns ready-to-register skills.
//!
//! ## Binary size impact
//!
//!   52 skills × ~8KB avg = ~416KB in `.rodata`
//!   Fits in L3 cache. Negligible vs a Tauri app bundle (~50MB+).
//!
//! ## Performance
//!
//!   All parsing happens once at startup. No async, no I/O.
//!   52 skills parse in <5ms on any modern CPU.

use include_dir::{include_dir, Dir};
use tracing::{debug, info, warn};

use crate::definition::{Skill, SkillSource};
use crate::openclaw_adapter::{adapt_skill, parse_skill_md, AdapterConfig};
use crate::registry::SkillRegistry;

/// The entire `openclaw-skills/` directory, embedded at compile time.
/// Each subdirectory contains a `SKILL.md` and optionally `references/*.md`.
static EMBEDDED_SKILLS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/openclaw-skills");

/// Result of loading embedded skills.
#[derive(Debug, Default)]
pub struct EmbedLoadResult {
    pub loaded: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Load all embedded OpenClaw skills and register them.
///
/// Called once at startup from `load_bundled_skills()`.
/// Entirely synchronous — no filesystem, no async, no I/O.
pub fn load_embedded_openclaw_skills(registry: &mut SkillRegistry) -> EmbedLoadResult {
    let config = AdapterConfig::default();
    let mut result = EmbedLoadResult::default();

    for skill_dir in EMBEDDED_SKILLS.dirs() {
        let dir_name = skill_dir.path().display().to_string();

        // Find SKILL.md in this subdirectory
        let skill_md = match skill_dir.get_file(skill_dir.path().join("SKILL.md")) {
            Some(f) => f,
            None => {
                debug!(dir = %dir_name, "no SKILL.md found, skipping");
                result.skipped += 1;
                continue;
            }
        };

        // Read SKILL.md content as UTF-8
        let content = match skill_md.contents_utf8() {
            Some(s) => s,
            None => {
                warn!(dir = %dir_name, "SKILL.md is not valid UTF-8");
                result.errors.push(format!("{}: invalid UTF-8", dir_name));
                continue;
            }
        };

        // Parse frontmatter + body (pure function, no I/O)
        let (frontmatter, mut body) = match parse_skill_md(content) {
            Ok(parsed) => parsed,
            Err(e) => {
                warn!(dir = %dir_name, error = %e, "failed to parse SKILL.md");
                result.errors.push(format!("{}: {}", dir_name, e));
                continue;
            }
        };

        // Append any references/*.md files into the body
        let refs_subdir = skill_dir.path().join("references");
        if let Some(refs_dir) = skill_dir.get_dir(&refs_subdir) {
            let mut ref_files: Vec<(&str, &str)> = Vec::new();
            for ref_file in refs_dir.files() {
                if let Some(ext) = ref_file.path().extension() {
                    if ext == "md" {
                        if let Some(ref_content) = ref_file.contents_utf8() {
                            let fname = ref_file
                                .path()
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown.md");
                            ref_files.push((fname, ref_content));
                        }
                    }
                }
            }
            // Sort for deterministic ordering across platforms
            ref_files.sort_by_key(|(name, _)| *name);
            for (fname, ref_content) in ref_files {
                body.push_str(&format!(
                    "\n\n---\n## Reference: {}\n\n{}",
                    fname, ref_content
                ));
            }
        }

        // Adapt to ClawDesk Skill (pure function, no I/O)
        match adapt_skill(&frontmatter, &body, &config) {
            Ok(adapted) => {
                debug!(
                    skill = %adapted.skill.manifest.id,
                    tier = %adapted.tier,
                    tokens = adapted.skill.manifest.estimated_tokens,
                    "embedded OpenClaw skill loaded"
                );
                registry.register(adapted.skill, SkillSource::Builtin);
                result.loaded += 1;
            }
            Err(e) => {
                warn!(dir = %dir_name, error = %e, "failed to adapt skill");
                result.errors.push(format!("{}: adapt failed: {}", dir_name, e));
            }
        }
    }

    info!(
        loaded = result.loaded,
        skipped = result.skipped,
        errors = result.errors.len(),
        "embedded OpenClaw skills loaded"
    );

    result
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_dir_has_skills() {
        // The include_dir! should have captured 52+ subdirectories
        let count = EMBEDDED_SKILLS.dirs().count();
        assert!(
            count >= 50,
            "expected 50+ embedded skill dirs, got {}",
            count
        );
    }

    #[test]
    fn all_skills_have_skill_md() {
        let mut with_md = 0;
        let mut without_md = 0;

        for dir in EMBEDDED_SKILLS.dirs() {
            let skill_md_path = dir.path().join("SKILL.md");
            if dir.get_file(&skill_md_path).is_some() {
                with_md += 1;
            } else {
                without_md += 1;
            }
        }

        assert!(
            with_md >= 50,
            "expected 50+ dirs with SKILL.md, got {}",
            with_md
        );
        assert_eq!(without_md, 0, "every skill dir should have SKILL.md");
    }

    #[test]
    fn load_all_embedded_skills() {
        let mut registry = SkillRegistry::new();
        let result = load_embedded_openclaw_skills(&mut registry);

        assert!(
            result.loaded >= 45,
            "expected 45+ loaded, got {} (errors: {:?})",
            result.loaded,
            result.errors
        );
        assert!(
            result.errors.len() <= 5,
            "too many errors ({}): {:?}",
            result.errors.len(),
            result.errors
        );
    }

    #[test]
    fn weather_skill_loads() {
        let mut registry = SkillRegistry::new();
        let _result = load_embedded_openclaw_skills(&mut registry);

        // Weather should be one of the loaded skills
        let weather = registry.get(&crate::definition::SkillId::new("openclaw", "weather"));
        assert!(weather.is_some(), "weather skill should be loaded");
    }

    #[test]
    fn skills_with_references_include_content() {
        // himalaya, 1password, model-usage have references/
        let mut registry = SkillRegistry::new();
        let _result = load_embedded_openclaw_skills(&mut registry);

        let himalaya = registry.get(&crate::definition::SkillId::new("openclaw", "himalaya"));
        if let Some(entry) = himalaya {
            // The prompt fragment should contain reference material
            assert!(
                entry.skill.prompt_fragment.contains("Reference:"),
                "himalaya should have reference content appended"
            );
        }
    }
}
