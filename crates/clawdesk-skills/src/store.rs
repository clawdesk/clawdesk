//! B1: Extension Store Backend.
//!
//! Provides a searchable catalog of skills available for installation,
//! with categories, ratings, and installation state tracking.
//!
//! ## Architecture
//!
//! ```text
//! Store Catalog (JSON index)
//!     ↓
//! StoreBackend (search, filter, install)
//!     ↓
//! SkillRegistry (activation)
//! ```
//!
//! The store maintains a local catalog that can be synced from remote
//! registries. Each store entry includes metadata for UI display
//! (description, screenshots, install count) beyond what the SkillManifest provides.

use crate::definition::SkillId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

// ═══════════════════════════════════════════════════════════════════════════
// Store catalog types
// ═══════════════════════════════════════════════════════════════════════════

/// A skill listing in the store catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreEntry {
    /// Skill identifier.
    pub skill_id: SkillId,
    /// Display name.
    pub display_name: String,
    /// Short description (1-2 sentences).
    pub short_description: String,
    /// Long description (markdown).
    pub long_description: String,
    /// Category for browsing.
    pub category: StoreCategory,
    /// Tags for search.
    pub tags: Vec<String>,
    /// Author or organization.
    pub author: String,
    /// Version string.
    pub version: String,
    /// Installation state on this device.
    pub install_state: InstallState,
    /// Community rating (0.0 - 5.0).
    pub rating: f32,
    /// Number of installations.
    pub install_count: u64,
    /// When this entry was last updated.
    pub updated_at: String,
    /// Icon URL or emoji.
    pub icon: String,
    /// Whether this skill is verified/official.
    pub verified: bool,
    /// License identifier.
    pub license: Option<String>,
    /// Source repository URL.
    pub source_url: Option<String>,
    /// Minimum ClawDesk version required.
    pub min_version: Option<String>,
}

/// Store categories for browsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreCategory {
    /// Productivity and workflow automation.
    Productivity,
    /// Software development and coding.
    Development,
    /// Data analysis and visualization.
    Analytics,
    /// Writing, content creation, editing.
    Writing,
    /// Research and information gathering.
    Research,
    /// Communication and collaboration.
    Communication,
    /// System administration and DevOps.
    DevOps,
    /// Creative arts, design, media.
    Creative,
    /// Finance and accounting.
    Finance,
    /// Education and learning.
    Education,
    /// Other / uncategorized.
    Other,
}

impl StoreCategory {
    /// All categories for UI enumeration.
    pub fn all() -> &'static [StoreCategory] {
        &[
            Self::Productivity,
            Self::Development,
            Self::Analytics,
            Self::Writing,
            Self::Research,
            Self::Communication,
            Self::DevOps,
            Self::Creative,
            Self::Finance,
            Self::Education,
            Self::Other,
        ]
    }

    /// Display label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Productivity => "Productivity",
            Self::Development => "Development",
            Self::Analytics => "Analytics",
            Self::Writing => "Writing",
            Self::Research => "Research",
            Self::Communication => "Communication",
            Self::DevOps => "DevOps",
            Self::Creative => "Creative",
            Self::Finance => "Finance",
            Self::Education => "Education",
            Self::Other => "Other",
        }
    }

    /// Emoji icon for category.
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Productivity => "⚡",
            Self::Development => "💻",
            Self::Analytics => "📊",
            Self::Writing => "✍️",
            Self::Research => "🔍",
            Self::Communication => "💬",
            Self::DevOps => "🔧",
            Self::Creative => "🎨",
            Self::Finance => "💰",
            Self::Education => "📚",
            Self::Other => "📦",
        }
    }
}

impl std::fmt::Display for StoreCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Installation state of a store skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallState {
    /// Not installed.
    Available,
    /// Currently downloading/installing.
    Installing,
    /// Installed and ready to activate.
    Installed,
    /// Installed and currently active.
    Active,
    /// Update available.
    UpdateAvailable,
    /// Installation failed.
    Failed,
}

impl std::fmt::Display for InstallState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available => write!(f, "Available"),
            Self::Installing => write!(f, "Installing…"),
            Self::Installed => write!(f, "Installed"),
            Self::Active => write!(f, "Active"),
            Self::UpdateAvailable => write!(f, "Update Available"),
            Self::Failed => write!(f, "Failed"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Search and filtering
// ═══════════════════════════════════════════════════════════════════════════

/// Search/filter query for the store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoreQuery {
    /// Free-text search (matches name, description, tags).
    pub search: Option<String>,
    /// Filter by category.
    pub category: Option<StoreCategory>,
    /// Filter by installation state.
    pub install_state: Option<InstallState>,
    /// Filter to verified-only skills.
    pub verified_only: bool,
    /// Sort order.
    pub sort: StoreSortOrder,
    /// Pagination offset.
    pub offset: usize,
    /// Max results to return.
    pub limit: usize,
}

/// Sort order for store listings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreSortOrder {
    /// Most popular (install count).
    #[default]
    Popular,
    /// Highest rated.
    Rating,
    /// Most recently updated.
    Recent,
    /// Alphabetical by name.
    Name,
}

/// Search results from the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreSearchResult {
    /// Matching entries.
    pub entries: Vec<StoreEntry>,
    /// Total matching count (for pagination).
    pub total_count: usize,
    /// Categories with counts.
    pub category_counts: HashMap<String, usize>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Store backend
// ═══════════════════════════════════════════════════════════════════════════

/// The skill store backend — manages the catalog and search.
pub struct StoreBackend {
    /// All entries in the catalog, keyed by skill ID string.
    catalog: HashMap<String, StoreEntry>,
    /// Trigram index for accelerated text search. Rebuilt on catalog changes.
    trigram_index: Option<crate::trigram_index::TrigramIndex>,
    /// Ordered keys for mapping trigram index doc IDs back to entries.
    index_keys: Vec<String>,
}

impl StoreBackend {
    /// Create an empty store backend.
    pub fn new() -> Self {
        Self {
            catalog: HashMap::new(),
            trigram_index: None,
            index_keys: Vec::new(),
        }
    }

    /// Add or update an entry in the catalog.
    pub fn upsert(&mut self, entry: StoreEntry) {
        let key = entry.skill_id.as_str().to_string();
        debug!(skill = %key, "store catalog upsert");
        self.catalog.insert(key, entry);
        self.invalidate_index();
    }

    /// Get a specific entry.
    pub fn get(&self, skill_id: &str) -> Option<&StoreEntry> {
        self.catalog.get(skill_id)
    }

    /// Search the catalog with the given query.
    pub fn search(&self, query: &StoreQuery) -> StoreSearchResult {
        let mut matches: Vec<&StoreEntry> = self.catalog.values().collect();

        // Filter by category
        if let Some(cat) = &query.category {
            matches.retain(|e| &e.category == cat);
        }

        // Filter by install state
        if let Some(state) = &query.install_state {
            matches.retain(|e| &e.install_state == state);
        }

        // Filter by verified
        if query.verified_only {
            matches.retain(|e| e.verified);
        }

        // Filter by search text
        if let Some(ref search) = query.search {
            let lower = search.to_lowercase();
            matches.retain(|e| {
                e.display_name.to_lowercase().contains(&lower)
                    || e.short_description.to_lowercase().contains(&lower)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&lower))
                    || e.author.to_lowercase().contains(&lower)
            });
        }

        let total_count = matches.len();

        // Compute category counts before sorting/pagination
        let mut category_counts: HashMap<String, usize> = HashMap::new();
        for entry in &matches {
            *category_counts
                .entry(entry.category.label().to_string())
                .or_default() += 1;
        }

        // Sort
        match query.sort {
            StoreSortOrder::Popular => {
                matches.sort_by(|a, b| b.install_count.cmp(&a.install_count));
            }
            StoreSortOrder::Rating => {
                matches.sort_by(|a, b| {
                    b.rating
                        .partial_cmp(&a.rating)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            StoreSortOrder::Recent => {
                matches.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            }
            StoreSortOrder::Name => {
                matches.sort_by(|a, b| a.display_name.cmp(&b.display_name));
            }
        }

        // Paginate
        let entries: Vec<StoreEntry> = matches
            .into_iter()
            .skip(query.offset)
            .take(if query.limit > 0 { query.limit } else { 50 })
            .cloned()
            .collect();

        StoreSearchResult {
            entries,
            total_count,
            category_counts,
        }
    }

    /// Update the install state of a skill.
    pub fn set_install_state(
        &mut self,
        skill_id: &str,
        state: InstallState,
    ) -> Option<()> {
        let entry = self.catalog.get_mut(skill_id)?;
        entry.install_state = state;
        Some(())
    }

    /// Total number of entries in the catalog.
    pub fn entry_count(&self) -> usize {
        self.catalog.len()
    }

    /// Return all skill IDs that are in `Active` install state.
    pub fn active_skill_ids(&self) -> Vec<String> {
        self.catalog
            .iter()
            .filter(|(_, e)| e.install_state == InstallState::Active)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Return all keys in the catalog.
    pub fn all_keys(&self) -> Vec<String> {
        self.catalog.keys().cloned().collect()
    }

    /// Remove an entry from the catalog. Returns true if the entry existed.
    pub fn remove(&mut self, skill_id: &str) -> bool {
        let removed = self.catalog.remove(skill_id).is_some();
        if removed {
            self.invalidate_index();
        }
        removed
    }

    /// Return all entries as a slice-like iterator.
    pub fn all_entries(&self) -> Vec<&StoreEntry> {
        self.catalog.values().collect()
    }

    /// Invalidate the trigram index (called on catalog mutations).
    fn invalidate_index(&mut self) {
        self.trigram_index = None;
        self.index_keys.clear();
    }

    /// Rebuild the trigram index from the current catalog.
    pub fn rebuild_index(&mut self) {
        use crate::trigram_index::{TrigramIndex, entry_to_search_text};

        let mut keys = Vec::with_capacity(self.catalog.len());
        let mut texts = Vec::with_capacity(self.catalog.len());

        for (key, entry) in &self.catalog {
            keys.push(key.clone());
            texts.push(entry_to_search_text(
                &entry.display_name,
                &entry.short_description,
                &entry.author,
                &entry.tags,
            ));
        }

        let index = TrigramIndex::build(&texts);
        debug!(
            docs = index.document_count(),
            trigrams = index.trigram_count(),
            "trigram index rebuilt"
        );
        self.index_keys = keys;
        self.trigram_index = Some(index);
    }

    /// Search using the trigram index (with fallback to linear scan).
    pub fn search_trigram(&mut self, query: &str) -> Vec<String> {
        // Ensure index is built
        if self.trigram_index.is_none() {
            self.rebuild_index();
        }

        if let Some(ref index) = self.trigram_index {
            let doc_ids = index.search(query);
            doc_ids
                .into_iter()
                .filter_map(|id| self.index_keys.get(id as usize).cloned())
                .collect()
        } else {
            vec![]
        }
    }

    /// Load catalog from a JSON string.
    pub fn load_from_json(&mut self, json: &str) -> Result<usize, serde_json::Error> {
        let entries: Vec<StoreEntry> = serde_json::from_str(json)?;
        let count = entries.len();
        for entry in entries {
            self.upsert(entry);
        }
        info!(entries = count, "store catalog loaded from JSON");
        Ok(count)
    }

    /// Serialize catalog to JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let entries: Vec<&StoreEntry> = self.catalog.values().collect();
        serde_json::to_string_pretty(&entries)
    }
}

impl Default for StoreBackend {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// B3: Package format types
// ═══════════════════════════════════════════════════════════════════════════

/// A skill package — the distributable unit for the store.
///
/// Format: `.clawpkg` (ZIP archive with standard structure).
///
/// ```text
/// my-skill.clawpkg/
///   manifest.toml     — SkillManifest (signed)
///   prompt.md         — Prompt fragment
///   references/       — Reference documents
///   tools/            — Custom tool definitions
///   assets/           — Icons, screenshots
///   SIGNATURE         — Detached Ed25519 signature
///   CHECKSUM          — SHA-256 of all files
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManifest {
    /// Package format version.
    pub format_version: u32,
    /// Skill identifier.
    pub skill_id: String,
    /// Version.
    pub version: String,
    /// Author.
    pub author: String,
    /// License.
    pub license: String,
    /// Files included in the package.
    pub files: Vec<PackageFile>,
    /// SHA-256 checksum of the package archive.
    pub checksum: Option<String>,
    /// Ed25519 signature of the checksum.
    pub signature: Option<String>,
    /// Publisher public key.
    pub publisher_key: Option<String>,
}

/// A file entry in a package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageFile {
    /// Relative path within the package.
    pub path: String,
    /// SHA-256 hash of the file content.
    pub hash: String,
    /// File size in bytes.
    pub size: u64,
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(name: &str, category: StoreCategory) -> StoreEntry {
        StoreEntry {
            skill_id: SkillId::new("store", name),
            display_name: name.to_string(),
            short_description: format!("A {} skill", name),
            long_description: String::new(),
            category,
            tags: vec![name.to_string()],
            author: "test".into(),
            version: "1.0.0".into(),
            install_state: InstallState::Available,
            rating: 4.0,
            install_count: 100,
            updated_at: "2026-01-01".into(),
            icon: "📦".into(),
            verified: true,
            license: None,
            source_url: None,
            min_version: None,
        }
    }

    #[test]
    fn search_by_text() {
        let mut store = StoreBackend::new();
        store.upsert(test_entry("weather", StoreCategory::Productivity));
        store.upsert(test_entry("code-review", StoreCategory::Development));

        let result = store.search(&StoreQuery {
            search: Some("weather".into()),
            ..Default::default()
        });
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].display_name, "weather");
    }

    #[test]
    fn search_by_category() {
        let mut store = StoreBackend::new();
        store.upsert(test_entry("weather", StoreCategory::Productivity));
        store.upsert(test_entry("code-review", StoreCategory::Development));
        store.upsert(test_entry("lint", StoreCategory::Development));

        let result = store.search(&StoreQuery {
            category: Some(StoreCategory::Development),
            ..Default::default()
        });
        assert_eq!(result.entries.len(), 2);
    }

    #[test]
    fn sort_by_rating() {
        let mut store = StoreBackend::new();
        let mut e1 = test_entry("low", StoreCategory::Other);
        e1.rating = 2.0;
        let mut e2 = test_entry("high", StoreCategory::Other);
        e2.rating = 5.0;

        store.upsert(e1);
        store.upsert(e2);

        let result = store.search(&StoreQuery {
            sort: StoreSortOrder::Rating,
            ..Default::default()
        });
        assert_eq!(result.entries[0].display_name, "high");
    }

    #[test]
    fn install_state_update() {
        let mut store = StoreBackend::new();
        store.upsert(test_entry("test-skill", StoreCategory::Other));
        let key = "store/test-skill";
        store.set_install_state(key, InstallState::Installed);
        assert_eq!(store.get(key).unwrap().install_state, InstallState::Installed);
    }

    #[test]
    fn category_labels() {
        assert_eq!(StoreCategory::Development.label(), "Development");
        assert_eq!(StoreCategory::Development.icon(), "💻");
        assert_eq!(StoreCategory::all().len(), 11);
    }

    #[test]
    fn pagination() {
        let mut store = StoreBackend::new();
        for i in 0..10 {
            store.upsert(test_entry(&format!("skill-{}", i), StoreCategory::Other));
        }

        let result = store.search(&StoreQuery {
            offset: 3,
            limit: 4,
            ..Default::default()
        });
        assert_eq!(result.entries.len(), 4);
        assert_eq!(result.total_count, 10);
    }

    #[test]
    fn json_roundtrip() {
        let mut store = StoreBackend::new();
        store.upsert(test_entry("test", StoreCategory::Productivity));

        let json = store.to_json().unwrap();
        let mut store2 = StoreBackend::new();
        let count = store2.load_from_json(&json).unwrap();
        assert_eq!(count, 1);
    }
}
