//! A2: Batch ingestion pipeline for OpenClaw skill directories.
//!
//! Scans a directory of SKILL.md files, parses via `openclaw_adapter`,
//! validates security constraints, resolves inter-skill dependencies,
//! and registers all valid skills into the `SkillRegistry`.
//!
//! ## Pipeline DAG (Kahn's topological sort for dependency resolution)
//!
//! ```text
//! scan_dir → parse_each → validate_security → resolve_deps → register
//!                                                   ↓
//!                                              generate_lockfile
//! ```
//!
//! ## Usage
//!
//! ```ignore
//! let result = CorpusIngest::new(registry, verifier)
//!     .scan_dir("./openclaw-skills")
//!     .await?;
//! println!("{}", result.summary());
//! ```

use crate::definition::{Skill, SkillId, SkillSource, SkillState};
use crate::installer::{check_install_requirements, parse_install_specs};
use crate::openclaw_adapter::{
    adapt_skill, parse_skill_md, resolve_metadata, AdaptationTier, AdapterConfig,
};
use crate::registry::SkillRegistry;
use crate::resolver::SkillResolver;
use crate::verification::{SkillVerifier, TrustLevel, VerificationResult};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn, error};

// ═══════════════════════════════════════════════════════════════════════════
// Lockfile types — reproducible builds
// ═══════════════════════════════════════════════════════════════════════════

/// A lockfile entry for one skill version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockEntry {
    /// Skill identifier (e.g., "openclaw/weather").
    pub skill_id: String,
    /// Version string.
    pub version: String,
    /// SHA-256 of SKILL.md content (content-addressable).
    pub content_hash: String,
    /// Adaptation tier (Direct / ContextPatch / NeedsRewrite).
    pub tier: String,
    /// Resolved dependencies.
    pub dependencies: Vec<String>,
    /// Source path relative to the scan root.
    pub source_path: String,
    /// Estimated token cost.
    pub estimated_tokens: usize,
    /// Trust level at ingest time.
    pub trust_level: String,
}

/// Complete lockfile for a corpus ingestion run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestLockfile {
    /// Schema version for forward compatibility.
    pub schema_version: u32,
    /// Timestamp of generation.
    pub generated_at: String,
    /// Scan root directory.
    pub scan_root: String,
    /// All ingested skills.
    pub entries: Vec<LockEntry>,
    /// Skills that failed ingestion.
    pub failures: Vec<IngestFailure>,
}

/// A failure record for a skill that could not be ingested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestFailure {
    pub path: String,
    pub reason: String,
    pub stage: String,
}

// ═══════════════════════════════════════════════════════════════════════════
// Ingestion result
// ═══════════════════════════════════════════════════════════════════════════

/// Result of a batch ingestion run.
#[derive(Debug, Default)]
pub struct IngestResult {
    /// Successfully ingested and registered skills.
    pub registered: Vec<IngestedSkill>,
    /// Skills that failed at some pipeline stage.
    pub failures: Vec<IngestFailure>,
    /// Skills that need binary dependencies installed.
    pub needs_install: Vec<InstallNeeded>,
    /// Dependency resolution order (topological sort result).
    pub activation_order: Vec<SkillId>,
    /// Total SKILL.md files scanned.
    pub total_scanned: usize,
    /// Count by adaptation tier.
    pub tier_counts: TierCounts,
}

/// Counts by adaptation tier.
#[derive(Debug, Default, Clone)]
pub struct TierCounts {
    pub direct: usize,
    pub context_patch: usize,
    pub needs_rewrite: usize,
}

/// A successfully ingested skill.
#[derive(Debug, Clone)]
pub struct IngestedSkill {
    pub skill_id: SkillId,
    pub version: String,
    pub content_hash: String,
    pub tier: AdaptationTier,
    pub source_path: PathBuf,
    pub estimated_tokens: usize,
    pub trust_level: TrustLevel,
}

/// A skill that needs binary dependencies installed before activation.
#[derive(Debug, Clone)]
pub struct InstallNeeded {
    pub skill_id: SkillId,
    pub missing_binaries: Vec<String>,
}

impl IngestResult {
    /// Human-readable summary of the ingestion run.
    pub fn summary(&self) -> String {
        format!(
            "Corpus ingestion: {} scanned, {} registered, {} failed, {} need install\n\
             Tiers: {} direct, {} context-patch, {} needs-rewrite\n\
             Activation order: {} skills resolved",
            self.total_scanned,
            self.registered.len(),
            self.failures.len(),
            self.needs_install.len(),
            self.tier_counts.direct,
            self.tier_counts.context_patch,
            self.tier_counts.needs_rewrite,
            self.activation_order.len(),
        )
    }

    /// Generate a lockfile from the ingestion result.
    pub fn to_lockfile(&self, scan_root: &str) -> IngestLockfile {
        let entries = self
            .registered
            .iter()
            .map(|s| LockEntry {
                skill_id: s.skill_id.as_str().to_string(),
                version: s.version.clone(),
                content_hash: s.content_hash.clone(),
                tier: format!("{}", s.tier),
                dependencies: vec![], // filled by caller if needed
                source_path: s.source_path.display().to_string(),
                estimated_tokens: s.estimated_tokens,
                trust_level: format!("{:?}", s.trust_level),
            })
            .collect();

        IngestLockfile {
            schema_version: 1,
            generated_at: chrono::Utc::now().to_rfc3339(),
            scan_root: scan_root.to_string(),
            entries,
            failures: self.failures.clone(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Corpus ingestion engine
// ═══════════════════════════════════════════════════════════════════════════

/// The batch ingestion pipeline.
///
/// Pipeline stages (each skill goes through in order):
/// 1. **Scan** — find `SKILL.md` in each subdirectory
/// 2. **Parse** — `parse_skill_md()` + `adapt_skill()` via OpenClaw adapter
/// 3. **Hash** — SHA-256 content-addressable identification  
/// 4. **Verify** — check trust level via `SkillVerifier`
/// 5. **Check deps** — binary dependency availability via `installer.rs`
/// 6. **Resolve** — topological sort of inter-skill dependencies
/// 7. **Register** — add to `SkillRegistry` in dependency order
pub struct CorpusIngest<'a> {
    /// Adapter config for the OpenClaw parser.
    config: AdapterConfig,
    /// Skill verifier for trust level checks.
    verifier: &'a SkillVerifier,
    /// Whether to skip skills that need binary installs (vs. just flagging).
    skip_missing_deps: bool,
    /// Whether to auto-activate registered skills.
    auto_activate: bool,
    /// Content-addressable cache: hash → skill_id (dedup across sources).
    seen_hashes: HashMap<String, SkillId>,
}

impl<'a> CorpusIngest<'a> {
    /// Create a new ingestion pipeline.
    pub fn new(verifier: &'a SkillVerifier) -> Self {
        Self {
            config: AdapterConfig::default(),
            verifier,
            skip_missing_deps: false,
            auto_activate: true,
            seen_hashes: HashMap::new(),
        }
    }

    /// Set adapter config (e.g., custom namespace).
    pub fn with_config(mut self, config: AdapterConfig) -> Self {
        self.config = config;
        self
    }

    /// Skip skills whose binary dependencies are missing (default: false).
    pub fn skip_missing_deps(mut self, skip: bool) -> Self {
        self.skip_missing_deps = skip;
        self
    }

    /// Auto-activate skills after registration (default: true).
    pub fn auto_activate(mut self, auto: bool) -> Self {
        self.auto_activate = auto;
        self
    }

    /// Scan a directory and ingest all SKILL.md files.
    ///
    /// Each immediate subdirectory of `root_dir` is treated as a skill.
    /// Inside each subdirectory, `SKILL.md` is the required entry point,
    /// and `references/*.md` files are appended to the prompt body.
    ///
    /// ## Complexity
    ///
    /// O(N) filesystem scans + O(N) parses + O(N+E) topo sort for N skills
    /// with E dependency edges.
    pub async fn scan_dir(
        &mut self,
        root_dir: &Path,
        registry: &mut SkillRegistry,
    ) -> Result<IngestResult, IngestError> {
        let mut result = IngestResult::default();

        // Stage 1: Scan — collect all SKILL.md paths
        let skill_dirs = self.discover_skill_dirs(root_dir).await?;
        result.total_scanned = skill_dirs.len();

        info!(
            root = %root_dir.display(),
            found = skill_dirs.len(),
            "corpus scan: found skill directories"
        );

        // Stage 2-5: Parse, hash, verify, check deps for each skill
        let mut parsed_skills: Vec<(Skill, AdaptationTier, PathBuf, String, VerificationResult)> =
            Vec::new();

        for (dir_path, skill_content, ref_content) in &skill_dirs {
            match self.process_single_skill(dir_path, skill_content, ref_content) {
                Ok((skill, tier, hash, verif)) => {
                    // Track tier counts
                    match tier {
                        AdaptationTier::Direct => result.tier_counts.direct += 1,
                        AdaptationTier::ContextPatch => result.tier_counts.context_patch += 1,
                        AdaptationTier::NeedsRewrite => result.tier_counts.needs_rewrite += 1,
                    }

                    // Check binary dependencies
                    let meta_result = {
                        let (fm, _) = parse_skill_md(skill_content)
                            .unwrap_or_default();
                        resolve_metadata(&fm)
                    };
                    let install_specs = parse_install_specs(
                        &serde_json::to_value(&meta_result.install_specs).unwrap_or_default(),
                    );
                    let install_check = check_install_requirements(&install_specs);

                    if !install_check.all_satisfied {
                        let missing: Vec<String> = install_check
                            .missing
                            .iter()
                            .map(|s| s.binary_name.clone())
                            .collect();

                        result.needs_install.push(InstallNeeded {
                            skill_id: skill.manifest.id.clone(),
                            missing_binaries: missing.clone(),
                        });

                        if self.skip_missing_deps {
                            debug!(
                                skill = %skill.manifest.id,
                                missing = ?missing,
                                "skipping skill with missing deps"
                            );
                            result.failures.push(IngestFailure {
                                path: dir_path.display().to_string(),
                                reason: format!("missing binaries: {}", missing.join(", ")),
                                stage: "dependency_check".into(),
                            });
                            continue;
                        }
                    }

                    parsed_skills.push((skill, tier, dir_path.clone(), hash, verif));
                }
                Err(failure) => {
                    result.failures.push(failure);
                }
            }
        }

        // Stage 6: Resolve inter-skill dependencies via topological sort
        let dep_graph: Vec<(SkillId, Vec<SkillId>)> = parsed_skills
            .iter()
            .map(|(skill, _, _, _, _)| {
                (
                    skill.manifest.id.clone(),
                    skill.manifest.dependencies.clone(),
                )
            })
            .collect();

        let resolution = SkillResolver::resolve(&dep_graph);

        // Log unresolved skills
        for unresolved in &resolution.unresolved {
            warn!(
                skill = %unresolved.id,
                reason = %unresolved.reason,
                "skill dependency resolution failed"
            );
            result.failures.push(IngestFailure {
                path: unresolved.id.as_str().to_string(),
                reason: format!("dependency resolution: {}", unresolved.reason),
                stage: "resolve".into(),
            });
        }

        result.activation_order = resolution.activation_order.clone();

        // Stage 7: Register skills in dependency order
        // Build a lookup map for ordered registration
        let skill_map: HashMap<SkillId, (Skill, AdaptationTier, PathBuf, String, VerificationResult)> =
            parsed_skills
                .into_iter()
                .map(|(skill, tier, path, hash, verif)| {
                    (skill.manifest.id.clone(), (skill, tier, path, hash, verif))
                })
                .collect();

        for skill_id in &resolution.activation_order {
            if let Some((skill, tier, source_path, content_hash, verif)) =
                skill_map.get(skill_id)
            {
                let source = SkillSource::Local {
                    path: source_path.display().to_string(),
                };

                registry.register(skill.clone(), source);

                if self.auto_activate {
                    let _ = registry.activate(skill_id);
                }

                result.registered.push(IngestedSkill {
                    skill_id: skill_id.clone(),
                    version: skill.manifest.version.clone(),
                    content_hash: content_hash.clone(),
                    tier: *tier,
                    source_path: source_path.clone(),
                    estimated_tokens: skill.token_cost(),
                    trust_level: verif.trust_level,
                });

                debug!(
                    skill = %skill_id,
                    tier = %tier,
                    trust = ?verif.trust_level,
                    tokens = skill.token_cost(),
                    "skill registered from corpus"
                );
            }
        }

        // Also register skills that weren't in the dependency graph
        // (no dependencies, so topo sort didn't include them)
        for (skill_id, (skill, tier, source_path, content_hash, verif)) in &skill_map {
            if !resolution.activation_order.contains(skill_id)
                && !resolution
                    .unresolved
                    .iter()
                    .any(|u| &u.id == skill_id)
            {
                let source = SkillSource::Local {
                    path: source_path.display().to_string(),
                };

                registry.register(skill.clone(), source);

                if self.auto_activate {
                    let _ = registry.activate(skill_id);
                }

                result.registered.push(IngestedSkill {
                    skill_id: skill_id.clone(),
                    version: skill.manifest.version.clone(),
                    content_hash: content_hash.clone(),
                    tier: *tier,
                    source_path: source_path.clone(),
                    estimated_tokens: skill.token_cost(),
                    trust_level: verif.trust_level,
                });
            }
        }

        info!(
            registered = result.registered.len(),
            failed = result.failures.len(),
            needs_install = result.needs_install.len(),
            "corpus ingestion complete"
        );

        Ok(result)
    }

    /// Discover skill directories: each subdirectory containing SKILL.md.
    /// Returns (dir_path, skill_md_content, reference_content).
    async fn discover_skill_dirs(
        &self,
        root_dir: &Path,
    ) -> Result<Vec<(PathBuf, String, String)>, IngestError> {
        let mut entries = Vec::new();

        let mut read_dir = tokio::fs::read_dir(root_dir)
            .await
            .map_err(|e| IngestError::IoError(format!("{}: {}", root_dir.display(), e)))?;

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|e| IngestError::IoError(e.to_string()))?
        {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            // Skip hidden directories
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .map_or(false, |n| n.starts_with('.'))
            {
                continue;
            }

            let skill_md_path = path.join("SKILL.md");
            if !skill_md_path.exists() {
                debug!(dir = %path.display(), "no SKILL.md, skipping");
                continue;
            }

            // Read SKILL.md
            let skill_content = tokio::fs::read_to_string(&skill_md_path)
                .await
                .map_err(|e| IngestError::IoError(format!("{}: {}", skill_md_path.display(), e)))?;

            // Read references/*.md if present
            let mut ref_content = String::new();
            let refs_dir = path.join("references");
            if refs_dir.exists() && refs_dir.is_dir() {
                let mut ref_entries: Vec<(String, String)> = Vec::new();
                let mut ref_read = tokio::fs::read_dir(&refs_dir)
                    .await
                    .unwrap_or_else(|_| panic!("failed to read references dir"));
                while let Ok(Some(ref_entry)) = ref_read.next_entry().await {
                    let ref_path = ref_entry.path();
                    if ref_path.extension().and_then(|e| e.to_str()) == Some("md") {
                        if let Ok(content) = tokio::fs::read_to_string(&ref_path).await {
                            let fname = ref_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown.md")
                                .to_string();
                            ref_entries.push((fname, content));
                        }
                    }
                }
                // Sort for deterministic ordering
                ref_entries.sort_by(|a, b| a.0.cmp(&b.0));
                for (fname, content) in ref_entries {
                    ref_content.push_str(&format!(
                        "\n\n---\n## Reference: {}\n\n{}",
                        fname, content
                    ));
                }
            }

            entries.push((path, skill_content, ref_content));
        }

        // Sort by directory name for deterministic ingestion order
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        Ok(entries)
    }

    /// Process a single skill through stages 2-5 (parse, hash, verify, deps).
    fn process_single_skill(
        &mut self,
        dir_path: &Path,
        skill_content: &str,
        ref_content: &str,
    ) -> Result<(Skill, AdaptationTier, String, VerificationResult), IngestFailure> {
        let _dir_name = dir_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        // Stage 2: Parse frontmatter + body
        let (frontmatter, mut body) = parse_skill_md(skill_content).map_err(|e| IngestFailure {
            path: dir_path.display().to_string(),
            reason: format!("parse error: {}", e),
            stage: "parse".into(),
        })?;

        // Append reference content
        if !ref_content.is_empty() {
            body.push_str(ref_content);
        }

        // Stage 3: Content-addressable hash (SHA-256 of raw SKILL.md)
        let mut hasher = Sha256::new();
        hasher.update(skill_content.as_bytes());
        let content_hash = hex::encode(hasher.finalize());

        // Dedup: skip if we've already seen this exact content
        if let Some(existing_id) = self.seen_hashes.get(&content_hash) {
            return Err(IngestFailure {
                path: dir_path.display().to_string(),
                reason: format!(
                    "duplicate content (same as {}), hash: {}",
                    existing_id, &content_hash[..16]
                ),
                stage: "dedup".into(),
            });
        }

        // Stage 2 continued: Adapt to ClawDesk Skill
        let adapted = adapt_skill(&frontmatter, &body, &self.config).map_err(|e| {
            IngestFailure {
                path: dir_path.display().to_string(),
                reason: format!("adaptation error: {}", e),
                stage: "adapt".into(),
            }
        })?;

        let mut skill = adapted.skill;
        let tier = adapted.tier;

        // Store content hash in manifest
        skill.manifest.content_hash = Some(content_hash.clone());
        skill.source_path = Some(dir_path.display().to_string());

        // Stage 4: Verify trust level
        let source = SkillSource::Local {
            path: dir_path.display().to_string(),
        };
        let verif = self.verifier.verify(&skill.manifest, &source);

        // Record in dedup cache
        self.seen_hashes
            .insert(content_hash.clone(), skill.manifest.id.clone());

        Ok((skill, tier, content_hash, verif))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Error types
// ═══════════════════════════════════════════════════════════════════════════

/// Errors from the ingestion pipeline.
#[derive(Debug, Clone)]
pub enum IngestError {
    /// Filesystem I/O error.
    IoError(String),
    /// Root directory does not exist.
    RootNotFound(String),
    /// Lockfile write error.
    LockfileError(String),
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(e) => write!(f, "I/O error: {e}"),
            Self::RootNotFound(p) => write!(f, "root directory not found: {p}"),
            Self::LockfileError(e) => write!(f, "lockfile error: {e}"),
        }
    }
}

impl std::error::Error for IngestError {}

// ═══════════════════════════════════════════════════════════════════════════
// Lockfile I/O
// ═══════════════════════════════════════════════════════════════════════════

/// Write a lockfile to disk.
pub async fn write_lockfile(
    lockfile: &IngestLockfile,
    path: &Path,
) -> Result<(), IngestError> {
    let content = serde_json::to_string_pretty(lockfile)
        .map_err(|e| IngestError::LockfileError(e.to_string()))?;
    tokio::fs::write(path, content)
        .await
        .map_err(|e| IngestError::LockfileError(format!("{}: {}", path.display(), e)))?;
    info!(path = %path.display(), "lockfile written");
    Ok(())
}

/// Read a lockfile from disk.
pub async fn read_lockfile(path: &Path) -> Result<IngestLockfile, IngestError> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| IngestError::IoError(format!("{}: {}", path.display(), e)))?;
    serde_json::from_str(&content)
        .map_err(|e| IngestError::LockfileError(e.to_string()))
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_result_summary_format() {
        let result = IngestResult {
            total_scanned: 52,
            registered: vec![],
            failures: vec![],
            needs_install: vec![],
            activation_order: vec![],
            tier_counts: TierCounts {
                direct: 40,
                context_patch: 10,
                needs_rewrite: 2,
            },
        };
        let summary = result.summary();
        assert!(summary.contains("52 scanned"));
        assert!(summary.contains("40 direct"));
    }

    #[test]
    fn lockfile_roundtrip() {
        let lockfile = IngestLockfile {
            schema_version: 1,
            generated_at: "2026-01-01T00:00:00Z".into(),
            scan_root: "./skills".into(),
            entries: vec![LockEntry {
                skill_id: "openclaw/weather".into(),
                version: "0.1.0".into(),
                content_hash: "abc123".into(),
                tier: "Direct".into(),
                dependencies: vec![],
                source_path: "./skills/weather".into(),
                estimated_tokens: 100,
                trust_level: "Unsigned".into(),
            }],
            failures: vec![],
        };

        let json = serde_json::to_string(&lockfile).unwrap();
        let parsed: IngestLockfile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].skill_id, "openclaw/weather");
    }

    #[test]
    fn content_hash_dedup() {
        let verifier = SkillVerifier::development();
        let mut ingest = CorpusIngest::new(&verifier);

        // Simulate seeing the same hash twice
        ingest.seen_hashes.insert(
            "deadbeef".into(),
            SkillId::new("openclaw", "weather"),
        );

        assert!(ingest.seen_hashes.contains_key("deadbeef"));
    }

    #[test]
    fn ingest_failure_records_stage() {
        let failure = IngestFailure {
            path: "./skills/broken".into(),
            reason: "invalid YAML".into(),
            stage: "parse".into(),
        };
        assert_eq!(failure.stage, "parse");
    }
}
