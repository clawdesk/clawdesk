//! Skill Pack Distribution — CAS-addressed pack federation and sync.
//!
//! Extends the existing `StoreBackend` and `FederatedRegistry` with
//! pack-level operations: content-addressed pack storage, multi-tier
//! resolution, and Merkle-diff synchronization for pack catalogs.
//!
//! ## Resolution Order
//!
//! 1. **Embedded** — bundled packs compiled into the binary
//! 2. **Local** — user-installed packs in `~/.clawdesk/packs/`
//! 3. **Peer** — packs discovered via A2A peer federation
//! 4. **Registry** — official pack registry at `store.clawdesk.dev`
//!
//! ## CAS Addressing
//!
//! Each pack is addressed by `SHA-256(toml_content)`, allowing
//! deduplication and integrity verification across federation tiers.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

// ─── Pack Source Tier ───────────────────────────────────────────────────────

/// Resolution tier for pack discovery (checked in priority order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PackSourceTier {
    /// Compiled into the binary — always available, zero latency.
    Embedded = 0,
    /// User-installed on local filesystem.
    Local = 1,
    /// Discovered from a peer agent via A2A federation.
    Peer = 2,
    /// Downloaded from the official pack registry.
    Registry = 3,
}

impl PackSourceTier {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Embedded => "embedded",
            Self::Local => "local",
            Self::Peer => "peer",
            Self::Registry => "registry",
        }
    }
}

impl std::fmt::Display for PackSourceTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Content Address ────────────────────────────────────────────────────────

/// Content address for a skill pack (SHA-256 of canonical TOML).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackContentAddress(pub String);

impl PackContentAddress {
    /// Compute content address from TOML bytes.
    pub fn from_bytes(content: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(content);
        let hash = hasher.finalize();
        Self(hex::encode(hash))
    }

    /// Compute from a string.
    pub fn from_str(content: &str) -> Self {
        Self::from_bytes(content.as_bytes())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackContentAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ─── Pack Catalog Entry ─────────────────────────────────────────────────────

/// A pack entry in the distributed catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackCatalogEntry {
    /// Pack ID (tier/name format from pack.rs).
    pub pack_id: String,
    /// Display name.
    pub display_name: String,
    /// Short description.
    pub description: String,
    /// Content address (SHA-256 hash of TOML).
    pub content_address: PackContentAddress,
    /// Version string.
    pub version: String,
    /// Source tier where this pack was resolved from.
    pub source_tier: PackSourceTier,
    /// Author or organization.
    pub author: String,
    /// Whether this pack is verified/signed.
    pub verified: bool,
    /// Size in bytes of the pack TOML.
    pub size_bytes: usize,
    /// Number of skills in the pack.
    pub skill_count: usize,
    /// Tags for search/discovery.
    pub tags: Vec<String>,
}

// ─── Pack Resolver ──────────────────────────────────────────────────────────

/// Multi-tier pack resolver — finds packs across all source tiers.
pub struct PackResolver {
    /// Embedded packs (compiled in).
    embedded: HashMap<String, PackCatalogEntry>,
    /// Local packs (from filesystem).
    local: HashMap<String, PackCatalogEntry>,
    /// Peer packs (from A2A federation).
    peer: HashMap<String, PackCatalogEntry>,
    /// Registry packs (from remote catalog).
    registry: HashMap<String, PackCatalogEntry>,
    /// Content-addressed store for deduplication.
    cas_store: HashMap<PackContentAddress, String>,
}

impl PackResolver {
    pub fn new() -> Self {
        Self {
            embedded: HashMap::new(),
            local: HashMap::new(),
            peer: HashMap::new(),
            registry: HashMap::new(),
            cas_store: HashMap::new(),
        }
    }

    /// Register a pack at a specific tier.
    pub fn register(
        &mut self,
        entry: PackCatalogEntry,
        toml_content: &str,
    ) {
        let cas = PackContentAddress::from_str(toml_content);
        self.cas_store.insert(cas, toml_content.to_string());

        let tier_map = match entry.source_tier {
            PackSourceTier::Embedded => &mut self.embedded,
            PackSourceTier::Local => &mut self.local,
            PackSourceTier::Peer => &mut self.peer,
            PackSourceTier::Registry => &mut self.registry,
        };
        tier_map.insert(entry.pack_id.clone(), entry);
    }

    /// Resolve a pack by ID, checking tiers in priority order.
    ///
    /// Returns the entry from the highest-priority tier that has it.
    pub fn resolve(&self, pack_id: &str) -> Option<&PackCatalogEntry> {
        self.embedded
            .get(pack_id)
            .or_else(|| self.local.get(pack_id))
            .or_else(|| self.peer.get(pack_id))
            .or_else(|| self.registry.get(pack_id))
    }

    /// Resolve and return the TOML content via CAS lookup.
    pub fn resolve_content(&self, pack_id: &str) -> Option<&str> {
        let entry = self.resolve(pack_id)?;
        self.cas_store
            .get(&entry.content_address)
            .map(|s| s.as_str())
    }

    /// Get all packs from a specific tier.
    pub fn by_tier(&self, tier: PackSourceTier) -> Vec<&PackCatalogEntry> {
        let tier_map = match tier {
            PackSourceTier::Embedded => &self.embedded,
            PackSourceTier::Local => &self.local,
            PackSourceTier::Peer => &self.peer,
            PackSourceTier::Registry => &self.registry,
        };
        tier_map.values().collect()
    }

    /// All resolved pack IDs across all tiers (deduplicated by highest priority).
    pub fn all_pack_ids(&self) -> Vec<&str> {
        let mut seen = std::collections::HashSet::new();
        let mut ids = Vec::new();
        for tier in [
            PackSourceTier::Embedded,
            PackSourceTier::Local,
            PackSourceTier::Peer,
            PackSourceTier::Registry,
        ] {
            for entry in self.by_tier(tier) {
                if seen.insert(entry.pack_id.as_str()) {
                    ids.push(entry.pack_id.as_str());
                }
            }
        }
        ids
    }

    /// Total number of unique packs across all tiers.
    pub fn total_packs(&self) -> usize {
        self.all_pack_ids().len()
    }

    /// Search packs by tag across all tiers.
    pub fn search_by_tag(&self, tag: &str) -> Vec<&PackCatalogEntry> {
        let tag_lower = tag.to_lowercase();
        let mut results = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for tier in [
            PackSourceTier::Embedded,
            PackSourceTier::Local,
            PackSourceTier::Peer,
            PackSourceTier::Registry,
        ] {
            for entry in self.by_tier(tier) {
                if !seen.contains(entry.pack_id.as_str())
                    && entry.tags.iter().any(|t| t.to_lowercase() == tag_lower)
                {
                    results.push(entry);
                    seen.insert(entry.pack_id.as_str());
                }
            }
        }
        results
    }
}

impl Default for PackResolver {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Pack Sync ──────────────────────────────────────────────────────────────

/// Result of a pack catalog sync operation.
#[derive(Debug, Clone)]
pub struct PackSyncResult {
    pub packs_added: usize,
    pub packs_updated: usize,
    pub packs_removed: usize,
    pub merkle_root: String,
}

/// Compute Merkle root for a set of pack catalog entries.
pub fn compute_pack_merkle_root(entries: &[PackCatalogEntry]) -> String {
    let mut leaf_hashes: Vec<String> = entries
        .iter()
        .map(|e| {
            let mut hasher = Sha256::new();
            hasher.update(e.pack_id.as_bytes());
            hasher.update(b":");
            hasher.update(e.version.as_bytes());
            hasher.update(b":");
            hasher.update(e.content_address.as_str().as_bytes());
            hex::encode(hasher.finalize())
        })
        .collect();

    leaf_hashes.sort();

    if leaf_hashes.is_empty() {
        return "0".repeat(64);
    }

    // Simple binary tree Merkle construction.
    while leaf_hashes.len() > 1 {
        let mut next_level = Vec::new();
        let mut i = 0;
        while i < leaf_hashes.len() {
            let mut hasher = Sha256::new();
            hasher.update(leaf_hashes[i].as_bytes());
            if i + 1 < leaf_hashes.len() {
                hasher.update(leaf_hashes[i + 1].as_bytes());
            }
            next_level.push(hex::encode(hasher.finalize()));
            i += 2;
        }
        leaf_hashes = next_level;
    }

    leaf_hashes.into_iter().next().unwrap_or_else(|| "0".repeat(64))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(pack_id: &str, tier: PackSourceTier) -> PackCatalogEntry {
        PackCatalogEntry {
            pack_id: pack_id.into(),
            display_name: format!("Pack {}", pack_id),
            description: "A test pack".into(),
            content_address: PackContentAddress(format!("hash_{}", pack_id)),
            version: "1.0.0".into(),
            source_tier: tier,
            author: "test".into(),
            verified: true,
            size_bytes: 100,
            skill_count: 5,
            tags: vec!["test".into()],
        }
    }

    #[test]
    fn test_content_address() {
        let addr1 = PackContentAddress::from_str("hello world");
        let addr2 = PackContentAddress::from_str("hello world");
        let addr3 = PackContentAddress::from_str("different content");
        assert_eq!(addr1, addr2);
        assert_ne!(addr1, addr3);
        assert_eq!(addr1.as_str().len(), 64); // SHA-256 = 32 bytes = 64 hex chars
    }

    #[test]
    fn test_tier_priority_resolution() {
        let mut resolver = PackResolver::new();

        // Same pack_id at two tiers — embedded should win.
        resolver.register(
            sample_entry("productivity/writer", PackSourceTier::Registry),
            "[pack]\nname = \"writer\"\nversion = \"1.0\"",
        );
        resolver.register(
            sample_entry("productivity/writer", PackSourceTier::Embedded),
            "[pack]\nname = \"writer\"\nversion = \"1.1\"",
        );

        let entry = resolver.resolve("productivity/writer").unwrap();
        assert_eq!(entry.source_tier, PackSourceTier::Embedded);
    }

    #[test]
    fn test_all_pack_ids_dedup() {
        let mut resolver = PackResolver::new();
        resolver.register(
            sample_entry("eng/coder", PackSourceTier::Embedded),
            "embedded",
        );
        resolver.register(
            sample_entry("eng/coder", PackSourceTier::Registry),
            "registry",
        );
        resolver.register(
            sample_entry("eng/devops", PackSourceTier::Local),
            "local",
        );

        let ids = resolver.all_pack_ids();
        assert_eq!(ids.len(), 2); // deduped
    }

    #[test]
    fn test_search_by_tag() {
        let mut resolver = PackResolver::new();
        let mut entry = sample_entry("prod/writer", PackSourceTier::Embedded);
        entry.tags = vec!["writing".into(), "productivity".into()];
        resolver.register(entry, "content");

        let results = resolver.search_by_tag("writing");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].pack_id, "prod/writer");

        let empty = resolver.search_by_tag("nonexistent");
        assert!(empty.is_empty());
    }

    #[test]
    fn test_merkle_root_deterministic() {
        let entries = vec![
            sample_entry("a/one", PackSourceTier::Embedded),
            sample_entry("b/two", PackSourceTier::Embedded),
        ];
        let root1 = compute_pack_merkle_root(&entries);
        let root2 = compute_pack_merkle_root(&entries);
        assert_eq!(root1, root2);
        assert_eq!(root1.len(), 64);
    }

    #[test]
    fn test_merkle_root_order_independent() {
        let entries_a = vec![
            sample_entry("a/one", PackSourceTier::Embedded),
            sample_entry("b/two", PackSourceTier::Embedded),
        ];
        let entries_b = vec![
            sample_entry("b/two", PackSourceTier::Embedded),
            sample_entry("a/one", PackSourceTier::Embedded),
        ];
        // Leaf hashes are sorted, so order shouldn't matter.
        assert_eq!(
            compute_pack_merkle_root(&entries_a),
            compute_pack_merkle_root(&entries_b)
        );
    }
}
