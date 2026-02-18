//! Skill loader — discovers and loads skills from the filesystem.
//!
//! Scans `~/.clawdesk/skills/` for skill directories containing a
//! `skill.toml` manifest and `prompt.md` file.
//!
//! Directory structure:
//! ```text
//! ~/.clawdesk/skills/
//! ├── core-web-search/
//! │   ├── skill.toml        # Manifest
//! │   └── prompt.md         # Prompt fragment (Markdown)
//! ├── community-code-review/
//! │   ├── skill.toml
//! │   └── prompt.md
//! ```

use crate::definition::{
    Skill, SkillId, SkillManifest, SkillSource,
};
use crate::registry::SkillRegistry;
use crate::verification::{SkillVerifier, TrustLevel, VerificationResult};
use sha2::{Sha256, Digest};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Result of a `load_fresh` operation — used for hot-reload.
#[derive(Debug)]
pub struct LoadResult {
    /// Fresh registry containing all loaded skills.
    pub registry: SkillRegistry,
    /// Number of skills successfully loaded.
    pub loaded: usize,
    /// Error messages for skills that failed to load.
    pub errors: Vec<String>,
}

/// Skill filesystem loader.
pub struct SkillLoader {
    /// Root directory to scan for skills.
    skills_dir: PathBuf,
    /// Cryptographic signature verifier.
    verifier: SkillVerifier,
}

impl SkillLoader {
    pub fn new(skills_dir: PathBuf) -> Self {
        Self {
            skills_dir,
            verifier: SkillVerifier::development(),
        }
    }

    /// Create a loader with a custom verifier.
    pub fn with_verifier(skills_dir: PathBuf, verifier: SkillVerifier) -> Self {
        Self {
            skills_dir,
            verifier,
        }
    }

    /// Default skills directory: `~/.clawdesk/skills/`.
    pub fn default_dir() -> PathBuf {
        dirs_or_home().join(".clawdesk").join("skills")
    }

    /// Scan the skills directory and load all valid skills.
    ///
    /// Returns the number of successfully loaded skills.
    pub async fn load_all(&self, registry: &mut SkillRegistry) -> usize {
        if !self.skills_dir.exists() {
            info!(dir = %self.skills_dir.display(), "skills directory does not exist, skipping");
            return 0;
        }

        let mut loaded = 0;
        let entries = match std::fs::read_dir(&self.skills_dir) {
            Ok(e) => e,
            Err(e) => {
                warn!(dir = %self.skills_dir.display(), error = %e, "failed to read skills directory");
                return 0;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            match self.load_skill(&path).await {
                Ok(skill) => {
                    let source = SkillSource::Local {
                        path: path.to_string_lossy().to_string(),
                    };
                    registry.register(skill, source);
                    loaded += 1;
                }
                Err(e) => {
                    warn!(dir = %path.display(), error = %e, "failed to load skill");
                }
            }
        }

        info!(count = loaded, dir = %self.skills_dir.display(), "loaded skills from filesystem");
        loaded
    }

    /// Load all skills into a **fresh** registry for atomic swap.
    ///
    /// This is the hot-reload entry point:
    /// 1. Creates a new `SkillRegistry`
    /// 2. Scans the skills directory
    /// 3. Optionally auto-activates all loaded skills
    /// 4. Returns the complete `LoadResult` for ArcSwap
    pub async fn load_fresh(&self, auto_activate: bool) -> LoadResult {
        let mut registry = SkillRegistry::new();
        let mut errors = Vec::new();

        if !self.skills_dir.exists() {
            info!(dir = %self.skills_dir.display(), "skills directory does not exist");
            return LoadResult { registry, loaded: 0, errors };
        }

        let entries = match std::fs::read_dir(&self.skills_dir) {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("failed to read skills dir '{}': {}", self.skills_dir.display(), e));
                return LoadResult { registry, loaded: 0, errors };
            }
        };

        let mut loaded = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            match self.load_skill(&path).await {
                Ok(skill) => {
                    let source = SkillSource::Local {
                        path: path.to_string_lossy().to_string(),
                    };
                    let id = skill.manifest.id.clone();
                    registry.register(skill, source);
                    if auto_activate {
                        let _ = registry.activate(&id);
                    }
                    loaded += 1;
                }
                Err(e) => {
                    errors.push(format!("{}: {}", path.display(), e));
                }
            }
        }

        info!(count = loaded, dir = %self.skills_dir.display(), "loaded skills (hot-reload)");
        LoadResult { registry, loaded, errors }
    }

    /// Load a single skill from a directory.
    async fn load_skill(&self, dir: &Path) -> Result<Skill, String> {
        let manifest_path = dir.join("skill.toml");
        let prompt_path = dir.join("prompt.md");

        // Read manifest
        let manifest_str = tokio::fs::read_to_string(&manifest_path)
            .await
            .map_err(|e| format!("failed to read {}: {}", manifest_path.display(), e))?;

        let manifest: SkillManifest = toml_parse(&manifest_str)
            .map_err(|e| format!("failed to parse {}: {}", manifest_path.display(), e))?;

        // ── T-01: Cryptographic signature verification gate ──
        let source = SkillSource::Local {
            path: dir.to_string_lossy().to_string(),
        };
        let verification = self.verifier.verify_and_gate(&manifest, &source)
            .map_err(|e| format!("signature verification failed for {}: {}", manifest.id, e))?;

        if verification.trust_level < TrustLevel::SignedTrusted {
            debug!(
                skill = %manifest.id,
                trust = %verification.trust_level,
                "skill loaded with reduced trust"
            );
        }

        // Read prompt fragment
        let prompt_fragment = if prompt_path.exists() {
            tokio::fs::read_to_string(&prompt_path)
                .await
                .map_err(|e| format!("failed to read {}: {}", prompt_path.display(), e))?
        } else {
            // Allow inline prompt in manifest description as fallback
            manifest.description.clone()
        };

        // ── T-06: Content-addressed identity ─────────────────
        // Compute SHA-256 of the prompt fragment for deduplication and
        // integrity verification. This binds the content to the manifest.
        let mut hasher = Sha256::new();
        hasher.update(prompt_fragment.as_bytes());
        let content_hash = hex::encode(hasher.finalize());
        // Assign content hash to manifest.
        let mut manifest = manifest;
        manifest.content_hash = Some(content_hash);

        debug!(
            skill = %manifest.id,
            tokens = manifest.estimated_tokens,
            trust = %verification.trust_level,
            content_hash = manifest.content_hash.as_deref().unwrap_or("none"),
            "loaded skill from disk"
        );

        Ok(Skill {
            manifest,
            prompt_fragment,
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: Some(dir.to_string_lossy().to_string()),
        })
    }

    /// Install a skill from a tarball or URL.
    pub async fn install(
        &self,
        id: &SkillId,
        source: &str,
        registry: &mut SkillRegistry,
    ) -> Result<(), String> {
        // Create the skill directory
        let skill_dir = self.skills_dir.join(id.as_str().replace('/', "-"));
        tokio::fs::create_dir_all(&skill_dir)
            .await
            .map_err(|e| format!("failed to create skill dir: {}", e))?;

        // For now, treat source as a local path to copy from
        // Future: support HTTP URLs, tar.bz2 archives, git repos
        if Path::new(source).exists() {
            copy_dir_recursive(Path::new(source), &skill_dir)
                .await
                .map_err(|e| format!("failed to copy skill: {}", e))?;
        } else {
            return Err(format!(
                "remote skill installation not yet implemented (source: {})",
                source
            ));
        }

        // Load the installed skill
        let skill = self.load_skill(&skill_dir).await?;
        registry.register(
            skill,
            SkillSource::Local {
                path: skill_dir.to_string_lossy().to_string(),
            },
        );

        info!(skill = %id, "skill installed successfully");
        Ok(())
    }

    /// Uninstall a skill by removing its directory.
    pub async fn uninstall(
        &self,
        id: &SkillId,
        registry: &mut SkillRegistry,
    ) -> Result<(), String> {
        registry.remove(id);
        let skill_dir = self.skills_dir.join(id.as_str().replace('/', "-"));
        if skill_dir.exists() {
            tokio::fs::remove_dir_all(&skill_dir)
                .await
                .map_err(|e| format!("failed to remove skill dir: {}", e))?;
        }
        info!(skill = %id, "skill uninstalled");
        Ok(())
    }
}

/// Parse a TOML manifest string into a `SkillManifest`.
fn toml_parse(s: &str) -> Result<SkillManifest, String> {
    toml::from_str(s).map_err(|e| format!("TOML parse error: {}", e))
}

/// Get the user's home directory, or fall back to current dir.
fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Recursively copy a directory.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    tokio::fs::create_dir_all(dst).await?;
    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            Box::pin(copy_dir_recursive(&src_path, &dst_path)).await?;
        } else {
            tokio::fs::copy(&src_path, &dst_path).await?;
        }
    }
    Ok(())
}
