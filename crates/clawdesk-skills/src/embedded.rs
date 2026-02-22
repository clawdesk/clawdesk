//! Embedded OpenClaw SKILL.md files — compiled into the binary.
//!
//! ## SKILL.md Filesystem Shipping & Embedding
//!
//! Ships ~52 production-quality skills in the ClawDesk binary via `include_str!`
//! at compile time. Zero runtime filesystem dependency for embedded skills.
//!
//! The embedded skills serve as the lowest-priority layer. Disk-based skills
//! from `~/.clawdesk/skills/` override embedded ones by name.
//!
//! ## Architecture
//!
//! ```text
//! build.rs → scans skills-source/*/SKILL.md → generates embedded_skills.rs
//!   → include_str!() embeds content into .rodata section
//!   → load_embedded_skills() parses at runtime into SkillRegistry
//! ```
//!
//! Total embedded size: ~185 KB (fits in L3 cache on any modern CPU).

use crate::definition::{Skill, SkillSource};
use crate::openclaw_adapter::{self, AdapterConfig};
use crate::registry::SkillRegistry;
use tracing::{debug, info, warn};

// Include the auto-generated embedded skills array.
include!(concat!(env!("OUT_DIR"), "/embedded_skills.rs"));

/// Load all embedded OpenClaw skills into a registry.
///
/// Parses each embedded SKILL.md using the OpenClaw adapter and registers
/// the resulting skills as `SkillSource::Builtin`.
///
/// Skills that fail to parse are logged as warnings and skipped.
///
/// # Returns
/// A tuple of (skills loaded, errors encountered).
pub fn load_embedded_skills(registry: &mut SkillRegistry) -> (usize, Vec<String>) {
    let skills = embedded_skills();
    let config = AdapterConfig::default();
    let mut loaded = 0;
    let mut errors = Vec::new();

    for (name, content) in skills {
        match parse_and_register(name, content, &config, registry) {
            Ok(()) => {
                loaded += 1;
            }
            Err(e) => {
                warn!(skill = %name, error = %e, "failed to parse embedded skill");
                errors.push(format!("{}: {}", name, e));
            }
        }
    }

    info!(
        loaded,
        total = skills.len(),
        errors = errors.len(),
        "loaded embedded OpenClaw skills"
    );

    (loaded, errors)
}

/// Parse a single embedded SKILL.md and register it.
fn parse_and_register(
    name: &str,
    content: &str,
    config: &AdapterConfig,
    registry: &mut SkillRegistry,
) -> Result<(), String> {
    let (frontmatter, body) =
        openclaw_adapter::parse_skill_md(content).map_err(|e| format!("parse: {}", e))?;

    let adapted =
        openclaw_adapter::adapt_skill(&frontmatter, &body, config).map_err(|e| format!("adapt: {}", e))?;

    let id = adapted.skill.manifest.id.clone();
    registry.register(adapted.skill, SkillSource::Builtin);

    debug!(skill = %id, "registered embedded skill");
    Ok(())
}

/// Get the number of skills available for embedding.
pub fn embedded_skill_count() -> usize {
    embedded_skills().len()
}

/// Get the names of all embedded skills.
pub fn embedded_skill_names() -> Vec<&'static str> {
    embedded_skills().iter().map(|(name, _)| *name).collect()
}

/// Get the raw content of an embedded skill by name.
pub fn get_embedded_skill_content(name: &str) -> Option<&'static str> {
    embedded_skills()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, content)| *content)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_skills_are_available() {
        let count = embedded_skill_count();
        // Should have 40+ skills if the skills-source link exists
        // In CI without the link, this will be 0 — that's fine
        if count > 0 {
            assert!(count >= 40, "expected 40+ embedded skills, got {}", count);
        }
    }

    #[test]
    fn embedded_skills_parse_successfully() {
        let skills = embedded_skills();
        if skills.is_empty() {
            return; // No skills linked — skip
        }

        let config = AdapterConfig::default();
        let mut successes = 0;
        let mut failures = 0;

        for (name, content) in skills {
            match openclaw_adapter::parse_skill_md(content) {
                Ok((fm, body)) => {
                    match openclaw_adapter::adapt_skill(&fm, &body, &config) {
                        Ok(_) => successes += 1,
                        Err(e) => {
                            eprintln!("adapt failed for {}: {}", name, e);
                            failures += 1;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("parse failed for {}: {}", name, e);
                    failures += 1;
                }
            }
        }

        // At least 90% should parse successfully
        let total = successes + failures;
        let success_rate = successes as f64 / total as f64;
        assert!(
            success_rate >= 0.9,
            "expected 90%+ parse success, got {:.0}% ({}/{})",
            success_rate * 100.0,
            successes,
            total
        );
    }

    #[test]
    fn load_embedded_into_registry() {
        let mut registry = SkillRegistry::new();
        let (loaded, errors) = load_embedded_skills(&mut registry);

        if embedded_skill_count() > 0 {
            assert!(loaded >= 40, "expected 40+ loaded, got {}", loaded);
            assert!(errors.len() <= 5, "too many errors: {:?}", errors);
        }
    }

    #[test]
    fn get_embedded_by_name() {
        if embedded_skill_count() == 0 {
            return;
        }
        let content = get_embedded_skill_content("weather");
        assert!(content.is_some(), "weather skill should be embedded");
        assert!(
            content.unwrap().contains("weather") || content.unwrap().contains("Weather"),
            "weather content should mention weather"
        );
    }

    #[test]
    fn embedded_skill_names_list() {
        let names = embedded_skill_names();
        if names.is_empty() {
            return;
        }
        assert!(names.contains(&"weather"));
        assert!(names.contains(&"github"));
    }
}
