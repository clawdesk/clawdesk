//! Skill registry CLI — search, install, update, audit skills from local
//! or remote registries.
//!
//! ```text
//! clawdesk skill search "web scraping"
//! clawdesk skill install user/web-scraper@1.2.0
//! clawdesk skill audit
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Registry index types
// ---------------------------------------------------------------------------

/// A skill's entry in a registry index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillIndexEntry {
    /// Unique skill ID (e.g. "core/web-research").
    pub id: String,
    /// Human-readable name.
    pub display_name: String,
    /// Short description.
    pub description: String,
    /// Author or organisation.
    pub author: String,
    /// Available versions, latest first.
    pub versions: Vec<VersionEntry>,
    /// Tags / categories.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Download count (if known).
    pub downloads: Option<u64>,
}

/// A single version of a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionEntry {
    pub version: String,
    /// SHA-256 of the tarball.
    pub sha256: String,
    /// Ed25519 signature (hex-encoded).
    pub signature: Option<String>,
    /// Minimum ClawDesk version required.
    pub min_clawdesk_version: Option<String>,
}

// ---------------------------------------------------------------------------
// Local install manifest
// ---------------------------------------------------------------------------

/// Tracks locally installed skills.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstalledSkills {
    /// Map from skill-id → installed metadata.
    pub skills: HashMap<String, InstalledSkillMeta>,
}

/// Metadata for an installed skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkillMeta {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub signature_verified: bool,
    pub installed_at: String,
    pub install_path: PathBuf,
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// Search result item.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub latest_version: String,
    pub downloads: Option<u64>,
    pub relevance: f64,
}

/// Simple local search: match query against id, display_name, description, tags.
pub fn search_local_index(index: &[SkillIndexEntry], query: &str) -> Vec<SearchResult> {
    let query_lower = query.to_lowercase();
    let terms: Vec<&str> = query_lower.split_whitespace().collect();

    let mut results: Vec<SearchResult> = index
        .iter()
        .filter_map(|entry| {
            let haystack = format!(
                "{} {} {} {}",
                entry.id,
                entry.display_name,
                entry.description,
                entry.tags.join(" ")
            )
            .to_lowercase();

            let matched = terms.iter().filter(|t| haystack.contains(**t)).count();
            if matched == 0 {
                return None;
            }

            let relevance = matched as f64 / terms.len() as f64;
            let latest_version = entry
                .versions
                .first()
                .map(|v| v.version.clone())
                .unwrap_or_default();

            Some(SearchResult {
                id: entry.id.clone(),
                display_name: entry.display_name.clone(),
                description: entry.description.clone(),
                latest_version,
                downloads: entry.downloads,
                relevance,
            })
        })
        .collect();

    results.sort_by(|a, b| b.relevance.partial_cmp(&a.relevance).unwrap_or(std::cmp::Ordering::Equal));
    results
}

// ---------------------------------------------------------------------------
// Install / update logic
// ---------------------------------------------------------------------------

/// A resolved install request.
#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub skill_id: String,
    pub version: Option<String>,
}

/// Outcome of an install attempt.
#[derive(Debug, Clone)]
pub struct InstallResult {
    pub skill_id: String,
    pub version: String,
    pub was_upgrade: bool,
    pub warnings: Vec<String>,
}

/// Parse an install reference: "author/skill@version" or "author/skill".
pub fn parse_install_ref(reference: &str) -> InstallRequest {
    if let Some((id, version)) = reference.rsplit_once('@') {
        InstallRequest {
            skill_id: id.to_string(),
            version: Some(version.to_string()),
        }
    } else {
        InstallRequest {
            skill_id: reference.to_string(),
            version: None,
        }
    }
}

/// Simulate an install from an index + request.
/// Returns `Err` if the skill or version is not found.
pub fn resolve_install(
    index: &[SkillIndexEntry],
    req: &InstallRequest,
    installed: &InstalledSkills,
) -> Result<InstallResult, String> {
    let entry = index
        .iter()
        .find(|e| e.id == req.skill_id)
        .ok_or_else(|| format!("Skill '{}' not found in registry", req.skill_id))?;

    let version_entry = if let Some(ref v) = req.version {
        entry
            .versions
            .iter()
            .find(|ve| ve.version == *v)
            .ok_or_else(|| format!("Version '{v}' not found for skill '{}'", req.skill_id))?
    } else {
        entry
            .versions
            .first()
            .ok_or_else(|| format!("No versions available for skill '{}'", req.skill_id))?
    };

    let was_upgrade = installed.skills.contains_key(&req.skill_id);

    let mut warnings = Vec::new();
    if version_entry.signature.is_none() {
        warnings.push("Skill package is unsigned — install at your own risk".into());
    }

    Ok(InstallResult {
        skill_id: req.skill_id.clone(),
        version: version_entry.version.clone(),
        was_upgrade,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

/// Audit result for a single installed skill.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub skill_id: String,
    pub installed_version: String,
    pub latest_version: Option<String>,
    pub is_outdated: bool,
    pub signature_ok: bool,
    pub integrity_ok: bool,
}

/// Audit all installed skills against the index.
pub fn audit_installed(
    installed: &InstalledSkills,
    index: &[SkillIndexEntry],
) -> Vec<AuditEntry> {
    installed
        .skills
        .values()
        .map(|meta| {
            let registry_entry = index.iter().find(|e| e.id == meta.id);

            let latest_version = registry_entry
                .and_then(|e| e.versions.first())
                .map(|v| v.version.clone());

            let is_outdated = latest_version
                .as_ref()
                .map(|lv| lv != &meta.version)
                .unwrap_or(false);

            AuditEntry {
                skill_id: meta.id.clone(),
                installed_version: meta.version.clone(),
                latest_version,
                is_outdated,
                signature_ok: meta.signature_verified,
                integrity_ok: true, // placeholder, would re-hash in production
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Merkle tree integrity
// ---------------------------------------------------------------------------

/// Compute a simple Merkle root from a list of SHA-256 hashes (hex strings).
///
/// `hashes` are leaf-level hex strings. They are sorted, then combined
/// pairwise by concatenation + SHA-256. An odd node is promoted unpaired.
pub fn merkle_root(hashes: &[String]) -> String {
    if hashes.is_empty() {
        return "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(); // SHA-256 of empty
    }
    if hashes.len() == 1 {
        return hashes[0].clone();
    }

    let mut level: Vec<String> = hashes.to_vec();
    level.sort();

    while level.len() > 1 {
        let mut next = Vec::new();
        let mut i = 0;
        while i < level.len() {
            if i + 1 < level.len() {
                let combined = format!("{}{}", level[i], level[i + 1]);
                next.push(sha256_hex(combined.as_bytes()));
                i += 2;
            } else {
                next.push(level[i].clone());
                i += 1;
            }
        }
        level = next;
    }

    level.into_iter().next().unwrap_or_default()
}

/// SHA-256 hex digest.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(data);
    hex::encode(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_index() -> Vec<SkillIndexEntry> {
        vec![
            SkillIndexEntry {
                id: "core/web-research".into(),
                display_name: "Web Research".into(),
                description: "Search and summarise web pages".into(),
                author: "clawdesk".into(),
                versions: vec![
                    VersionEntry {
                        version: "1.1.0".into(),
                        sha256: "aabbcc".into(),
                        signature: Some("sig1".into()),
                        min_clawdesk_version: None,
                    },
                    VersionEntry {
                        version: "1.0.0".into(),
                        sha256: "ddeeff".into(),
                        signature: Some("sig0".into()),
                        min_clawdesk_version: None,
                    },
                ],
                tags: vec!["web".into(), "research".into()],
                downloads: Some(1200),
            },
            SkillIndexEntry {
                id: "community/code-review".into(),
                display_name: "Code Review".into(),
                description: "Automated code review agent".into(),
                author: "alice".into(),
                versions: vec![VersionEntry {
                    version: "0.5.0".into(),
                    sha256: "112233".into(),
                    signature: None,
                    min_clawdesk_version: None,
                }],
                tags: vec!["code".into(), "review".into()],
                downloads: Some(300),
            },
        ]
    }

    #[test]
    fn test_search_local_index() {
        let results = search_local_index(&sample_index(), "web research");
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "core/web-research");
    }

    #[test]
    fn test_search_no_match() {
        let results = search_local_index(&sample_index(), "quantum physics");
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_install_ref_with_version() {
        let req = parse_install_ref("core/web-research@1.1.0");
        assert_eq!(req.skill_id, "core/web-research");
        assert_eq!(req.version.as_deref(), Some("1.1.0"));
    }

    #[test]
    fn test_parse_install_ref_without_version() {
        let req = parse_install_ref("core/web-research");
        assert_eq!(req.skill_id, "core/web-research");
        assert!(req.version.is_none());
    }

    #[test]
    fn test_resolve_install_latest() {
        let req = InstallRequest {
            skill_id: "core/web-research".into(),
            version: None,
        };
        let result = resolve_install(&sample_index(), &req, &InstalledSkills::default()).unwrap();
        assert_eq!(result.version, "1.1.0");
        assert!(!result.was_upgrade);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_resolve_install_unsigned_warning() {
        let req = InstallRequest {
            skill_id: "community/code-review".into(),
            version: None,
        };
        let result = resolve_install(&sample_index(), &req, &InstalledSkills::default()).unwrap();
        assert_eq!(result.version, "0.5.0");
        assert!(!result.warnings.is_empty());
    }

    #[test]
    fn test_resolve_install_not_found() {
        let req = InstallRequest {
            skill_id: "nonexistent/skill".into(),
            version: None,
        };
        let result = resolve_install(&sample_index(), &req, &InstalledSkills::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_audit_installed() {
        let mut installed = InstalledSkills::default();
        installed.skills.insert(
            "core/web-research".into(),
            InstalledSkillMeta {
                id: "core/web-research".into(),
                version: "1.0.0".into(),
                sha256: "ddeeff".into(),
                signature_verified: true,
                installed_at: "2025-01-01T00:00:00Z".into(),
                install_path: PathBuf::from("/skills/web-research"),
            },
        );

        let entries = audit_installed(&installed, &sample_index());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_outdated);
        assert!(entries[0].signature_ok);
    }

    #[test]
    fn test_merkle_root_single() {
        let result = merkle_root(&["abc123".to_string()]);
        assert_eq!(result, "abc123");
    }

    #[test]
    fn test_merkle_root_multiple() {
        let hashes = vec!["aaa".to_string(), "bbb".to_string(), "ccc".to_string()];
        let root = merkle_root(&hashes);
        assert!(!root.is_empty());
        // Deterministic: same input → same root
        assert_eq!(root, merkle_root(&hashes));
    }

    #[test]
    fn test_merkle_root_empty() {
        let root = merkle_root(&[]);
        assert_eq!(root, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
