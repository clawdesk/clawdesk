//! A1: Federated Skill Registry with Content-Addressable Storage (CAS).
//!
//! Extends the core `SkillRegistry` with:
//! - **Content-addressable storage** — skills keyed by SHA-256 of their content
//! - **Multi-source federation** — merge skills from local, embedded, and remote registries
//! - **Version resolution** — when multiple sources provide the same skill, latest wins
//! - **Provenance tracking** — every skill tracks its source registry
//!
//! ## CAS Model
//!
//! Each skill's content (prompt + parameters + dependencies) is hashed to produce
//! a content address. This enables:
//! - **Deduplication** across registries (same content → same hash)
//! - **Integrity verification** (hash mismatch → tampering detected)
//! - **Cache invalidation** (content changed → new hash → cache miss)
//!
//! ## Federation Protocol
//!
//! ```text
//! [Local FS]  ──┐
//! [Embedded]  ──┼──→ FederatedRegistry ──→ unified SkillRegistry
//! [Remote API]──┘
//! ```
//!
//! Each source registers skills independently. The federated registry
//! resolves conflicts by version comparison (semver), with local overrides.

use crate::definition::{Skill, SkillId, SkillSource};
use crate::registry::SkillRegistry;
use crate::verification::{SkillVerifier, VerificationResult, TrustLevel};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Content-Addressable Storage
// ═══════════════════════════════════════════════════════════════════════════

/// A content address (SHA-256 hash of skill content).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentAddress(pub String);

impl ContentAddress {
    /// Compute the content address for a skill.
    ///
    /// Hash inputs: skill ID + version + prompt fragment + parameter schema.
    /// This ensures that any change to the skill's "semantic content"
    /// produces a different address.
    pub fn compute(skill: &Skill) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(skill.manifest.id.as_str().as_bytes());
        hasher.update(skill.manifest.version.as_bytes());
        hasher.update(skill.prompt_fragment.as_bytes());

        // Include parameter names for schema changes
        for param in &skill.manifest.parameters {
            hasher.update(param.name.as_bytes());
        }

        // Include dependency IDs
        for dep in &skill.manifest.dependencies {
            hasher.update(dep.as_str().as_bytes());
        }

        Self(hex::encode(hasher.finalize()))
    }

    /// Short form for display (first 12 hex chars).
    pub fn short(&self) -> &str {
        &self.0[..self.0.len().min(12)]
    }
}

impl std::fmt::Display for ContentAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cas:{}", self.short())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Federation source
// ═══════════════════════════════════════════════════════════════════════════

/// Priority levels for federation sources.
///
/// Higher priority sources override lower ones when the same skill ID
/// exists in multiple registries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SourcePriority {
    /// Lowest: remote community registries.
    Remote = 0,
    /// Middle: embedded skills shipped with the binary.
    Embedded = 1,
    /// Highest: local user-installed skills.
    Local = 2,
}

impl std::fmt::Display for SourcePriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Remote => write!(f, "remote"),
            Self::Embedded => write!(f, "embedded"),
            Self::Local => write!(f, "local"),
        }
    }
}

/// A federation source — one registry feeding into the federated view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationSource {
    /// Unique identifier for this source.
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// Priority for conflict resolution.
    pub priority: SourcePriority,
    /// Base URL for remote sources (None for local/embedded).
    pub url: Option<String>,
    /// Whether this source is currently enabled.
    pub enabled: bool,
    /// When this source was last synced.
    pub last_sync: Option<chrono::DateTime<chrono::Utc>>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Federated skill entry
// ═══════════════════════════════════════════════════════════════════════════

/// A skill stored in the CAS with provenance metadata.
#[derive(Debug, Clone)]
pub struct FederatedSkillEntry {
    /// The skill itself.
    pub skill: Skill,
    /// Content address (SHA-256).
    pub content_address: ContentAddress,
    /// Which source provided this skill.
    pub source_id: String,
    /// Source priority at registration time.
    pub priority: SourcePriority,
    /// Verification result from the trust chain.
    pub verification: VerificationResult,
    /// When this entry was registered.
    pub registered_at: chrono::DateTime<chrono::Utc>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Federated Registry
// ═══════════════════════════════════════════════════════════════════════════

/// A federated skill registry that merges skills from multiple sources.
///
/// The federation layer sits above the core `SkillRegistry` and provides:
/// - Content-addressable deduplication
/// - Multi-source conflict resolution (priority-based)
/// - Provenance tracking
///
/// The underlying `SkillRegistry` is updated to reflect the merged view.
pub struct FederatedRegistry {
    /// Registered federation sources.
    sources: Vec<FederationSource>,
    /// CAS: content_address → skill entry.
    cas: HashMap<ContentAddress, FederatedSkillEntry>,
    /// Skill ID → best content address (after conflict resolution).
    resolved: HashMap<SkillId, ContentAddress>,
    /// Skill ID → all available versions from all sources.
    all_versions: HashMap<SkillId, Vec<ContentAddress>>,
}

impl FederatedRegistry {
    /// Create an empty federated registry.
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            cas: HashMap::new(),
            resolved: HashMap::new(),
            all_versions: HashMap::new(),
        }
    }

    /// Register a federation source.
    pub fn add_source(&mut self, source: FederationSource) {
        info!(
            source_id = %source.id,
            priority = %source.priority,
            "federation source registered"
        );
        self.sources.push(source);
    }

    /// List all registered sources.
    pub fn sources(&self) -> &[FederationSource] {
        &self.sources
    }

    /// Register a skill from a specific source.
    ///
    /// Computes the content address, checks for duplicates,
    /// and resolves conflicts with existing entries for the same skill ID.
    pub fn register_from_source(
        &mut self,
        skill: Skill,
        source_id: &str,
        verifier: &SkillVerifier,
    ) -> RegisterResult {
        let skill_id = skill.manifest.id.clone();
        let content_address = ContentAddress::compute(&skill);

        // Check CAS for exact duplicate
        if self.cas.contains_key(&content_address) {
            return RegisterResult::Duplicate {
                skill_id,
                content_address,
            };
        }

        // Find the source
        let source = self.sources.iter().find(|s| s.id == source_id);
        let priority = source.map_or(SourcePriority::Remote, |s| s.priority);

        // Verify
        let source_ref = SkillSource::Local {
            path: source_id.to_string(),
        };
        let verification = verifier.verify(&skill.manifest, &source_ref);

        let entry = FederatedSkillEntry {
            skill,
            content_address: content_address.clone(),
            source_id: source_id.to_string(),
            priority,
            verification,
            registered_at: chrono::Utc::now(),
        };

        // Store in CAS
        self.cas.insert(content_address.clone(), entry);

        // Track all versions
        self.all_versions
            .entry(skill_id.clone())
            .or_insert_with(Vec::new)
            .push(content_address.clone());

        // Conflict resolution: highest priority wins, then latest version
        let should_update = match self.resolved.get(&skill_id) {
            None => true,
            Some(existing_addr) => {
                if let Some(existing) = self.cas.get(existing_addr) {
                    priority > existing.priority
                        || (priority == existing.priority
                            && self.cas.get(&content_address)
                                .map_or(false, |new| {
                                    new.skill.manifest.version > existing.skill.manifest.version
                                }))
                } else {
                    true
                }
            }
        };

        if should_update {
            let was_override = self.resolved.contains_key(&skill_id);
            self.resolved.insert(skill_id.clone(), content_address.clone());
            if was_override {
                debug!(
                    skill = %skill_id,
                    source = %source_id,
                    cas = %content_address,
                    "federated override: higher priority source"
                );
                RegisterResult::Override {
                    skill_id,
                    content_address,
                }
            } else {
                RegisterResult::Registered {
                    skill_id,
                    content_address,
                }
            }
        } else {
            RegisterResult::LowerPriority {
                skill_id,
                content_address,
            }
        }
    }

    /// Apply the resolved federation view to a core SkillRegistry.
    ///
    /// This replaces the registry contents with the federated view.
    pub fn apply_to_registry(&self, registry: &mut SkillRegistry) {
        let mut applied = 0;

        for (skill_id, content_addr) in &self.resolved {
            if let Some(entry) = self.cas.get(content_addr) {
                let source = SkillSource::Local {
                    path: format!("federation:{}", entry.source_id),
                };
                registry.register(entry.skill.clone(), source);
                applied += 1;
            }
        }

        info!(
            applied,
            sources = self.sources.len(),
            cas_entries = self.cas.len(),
            "federated view applied to registry"
        );
    }

    /// Get a skill by its content address.
    pub fn get_by_cas(&self, address: &ContentAddress) -> Option<&FederatedSkillEntry> {
        self.cas.get(address)
    }

    /// Get the resolved skill entry for a skill ID.
    pub fn get_resolved(&self, skill_id: &SkillId) -> Option<&FederatedSkillEntry> {
        self.resolved
            .get(skill_id)
            .and_then(|addr| self.cas.get(addr))
    }

    /// List all versions of a skill from all sources.
    pub fn all_versions_of(&self, skill_id: &SkillId) -> Vec<&FederatedSkillEntry> {
        self.all_versions
            .get(skill_id)
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(|addr| self.cas.get(addr))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Total number of unique skills (after dedup).
    pub fn unique_skill_count(&self) -> usize {
        self.resolved.len()
    }

    /// Check if a skill ID is resolved (has a winning entry).
    pub fn is_resolved(&self, skill_id: &SkillId) -> bool {
        self.resolved.contains_key(skill_id)
    }

    /// Iterate over all resolved skills: (SkillId, FederatedSkillEntry).
    pub fn resolved_skills(&self) -> Vec<(&SkillId, &FederatedSkillEntry)> {
        self.resolved
            .iter()
            .filter_map(|(id, addr)| {
                self.cas.get(addr).map(|entry| (id, entry))
            })
            .collect()
    }

    /// Total number of CAS entries (before dedup).
    pub fn cas_entry_count(&self) -> usize {
        self.cas.len()
    }

    /// Summary statistics.
    pub fn stats(&self) -> FederationStats {
        let mut by_source: HashMap<String, usize> = HashMap::new();
        for entry in self.cas.values() {
            *by_source.entry(entry.source_id.clone()).or_default() += 1;
        }

        FederationStats {
            total_sources: self.sources.len(),
            total_cas_entries: self.cas.len(),
            unique_skills: self.resolved.len(),
            by_source,
        }
    }
}

impl Default for FederatedRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of registering a skill in the federated registry.
#[derive(Debug)]
pub enum RegisterResult {
    /// New skill registered successfully.
    Registered {
        skill_id: SkillId,
        content_address: ContentAddress,
    },
    /// Skill overrode an existing entry (higher priority).
    Override {
        skill_id: SkillId,
        content_address: ContentAddress,
    },
    /// Exact duplicate content (same hash).
    Duplicate {
        skill_id: SkillId,
        content_address: ContentAddress,
    },
    /// Not used because a higher-priority source already provides this skill.
    LowerPriority {
        skill_id: SkillId,
        content_address: ContentAddress,
    },
}

/// Federation statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationStats {
    pub total_sources: usize,
    pub total_cas_entries: usize,
    pub unique_skills: usize,
    pub by_source: HashMap<String, usize>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::SkillManifest;

    fn test_skill(namespace: &str, name: &str, version: &str, prompt: &str) -> Skill {
        Skill {
            manifest: SkillManifest {
                id: SkillId::new(namespace, name),
                display_name: name.to_string(),
                version: version.to_string(),
                description: format!("Test skill: {}", name),
                author: None,
                dependencies: vec![],
                required_tools: vec![],
                parameters: vec![],
                triggers: vec![],
                estimated_tokens: 0,
                priority_weight: 1.0,
                tags: vec![],
                signature: None,
                publisher_key: None,
                content_hash: None,
                schema_version: 1,
            },
            prompt_fragment: prompt.to_string(),
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: None,
        }
    }

    #[test]
    fn content_address_deterministic() {
        let skill = test_skill("test", "hello", "1.0.0", "Say hello");
        let addr1 = ContentAddress::compute(&skill);
        let addr2 = ContentAddress::compute(&skill);
        assert_eq!(addr1, addr2);
    }

    #[test]
    fn content_address_changes_on_prompt_change() {
        let skill1 = test_skill("test", "hello", "1.0.0", "Say hello");
        let skill2 = test_skill("test", "hello", "1.0.0", "Say goodbye");
        assert_ne!(ContentAddress::compute(&skill1), ContentAddress::compute(&skill2));
    }

    #[test]
    fn register_and_resolve() {
        let verifier = SkillVerifier::development();
        let mut fed = FederatedRegistry::new();

        fed.add_source(FederationSource {
            id: "local".into(),
            label: "Local".into(),
            priority: SourcePriority::Local,
            url: None,
            enabled: true,
            last_sync: None,
        });

        let skill = test_skill("test", "hello", "1.0.0", "Say hello");
        let skill_id = skill.manifest.id.clone();

        let result = fed.register_from_source(skill, "local", &verifier);
        assert!(matches!(result, RegisterResult::Registered { .. }));

        let resolved = fed.get_resolved(&skill_id);
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().source_id, "local");
    }

    #[test]
    fn duplicate_detection() {
        let verifier = SkillVerifier::development();
        let mut fed = FederatedRegistry::new();

        let skill = test_skill("test", "hello", "1.0.0", "Say hello");

        fed.register_from_source(skill.clone(), "source1", &verifier);
        let result = fed.register_from_source(skill, "source2", &verifier);
        assert!(matches!(result, RegisterResult::Duplicate { .. }));
    }

    #[test]
    fn priority_override() {
        let verifier = SkillVerifier::development();
        let mut fed = FederatedRegistry::new();

        fed.add_source(FederationSource {
            id: "remote".into(),
            label: "Remote".into(),
            priority: SourcePriority::Remote,
            url: Some("https://example.com".into()),
            enabled: true,
            last_sync: None,
        });
        fed.add_source(FederationSource {
            id: "local".into(),
            label: "Local".into(),
            priority: SourcePriority::Local,
            url: None,
            enabled: true,
            last_sync: None,
        });

        // Register from remote first
        let skill_remote = test_skill("test", "hello", "1.0.0", "Remote version");
        fed.register_from_source(skill_remote, "remote", &verifier);

        // Register from local — should override
        let skill_local = test_skill("test", "hello", "1.0.0", "Local version");
        let result = fed.register_from_source(skill_local, "local", &verifier);
        assert!(matches!(result, RegisterResult::Override { .. }));

        let skill_id = SkillId::new("test", "hello");
        let resolved = fed.get_resolved(&skill_id).unwrap();
        assert_eq!(resolved.source_id, "local");
    }

    #[test]
    fn stats_tracking() {
        let verifier = SkillVerifier::development();
        let mut fed = FederatedRegistry::new();

        fed.add_source(FederationSource {
            id: "embedded".into(),
            label: "Embedded".into(),
            priority: SourcePriority::Embedded,
            url: None,
            enabled: true,
            last_sync: None,
        });

        fed.register_from_source(
            test_skill("test", "a", "1.0.0", "Skill A"),
            "embedded",
            &verifier,
        );
        fed.register_from_source(
            test_skill("test", "b", "1.0.0", "Skill B"),
            "embedded",
            &verifier,
        );

        let stats = fed.stats();
        assert_eq!(stats.total_sources, 1);
        assert_eq!(stats.unique_skills, 2);
        assert_eq!(stats.total_cas_entries, 2);
    }

    #[test]
    fn all_versions() {
        let verifier = SkillVerifier::development();
        let mut fed = FederatedRegistry::new();

        // Same skill ID, different content from different sources
        let skill_v1 = test_skill("test", "hello", "1.0.0", "V1 prompt");
        let skill_v2 = test_skill("test", "hello", "2.0.0", "V2 prompt");

        fed.register_from_source(skill_v1, "source1", &verifier);
        fed.register_from_source(skill_v2, "source2", &verifier);

        let skill_id = SkillId::new("test", "hello");
        let versions = fed.all_versions_of(&skill_id);
        assert_eq!(versions.len(), 2);
    }
}
