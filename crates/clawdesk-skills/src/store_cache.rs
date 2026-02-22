//! Offline-first persistent cache for the skill store catalog.
//!
//! ## Architecture
//!
//! ```text
//! StoreBackend (in-memory HashMap)
//!       ↕  write-through
//! StoreCache (SochStore key-value)
//!       ↕  on startup
//! Hydration (loads from SochStore into StoreBackend)
//! ```
//!
//! The cache provides:
//! 1. **Write-through**: Every `upsert`/`remove` in StoreBackend is mirrored to SochStore.
//! 2. **Hydration**: On startup, load the full catalog from SochStore before network sync.
//! 3. **Offline-first**: If network sync fails, the cached catalog is still available.
//!
//! ## Storage Layout
//!
//! - Collection: `store_catalog`
//! - Key: skill_id string
//! - Value: JSON-serialized `StoreEntry`
//! - Metadata: `sync_state` key stores the `SyncState` JSON

use crate::store::{StoreBackend, StoreEntry};
use crate::store_sync::SyncState;
use serde_json;
use tracing::{debug, info, warn};

/// Cache key prefix for catalog entries.
const CATALOG_COLLECTION: &str = "store_catalog";
/// Key for persisted sync state.
const SYNC_STATE_KEY: &str = "__sync_state__";

/// A persistent cache layer for the store catalog.
///
/// Wraps a `StoreBackend` and persists changes to a JSON file.
/// In production this would use SochStore, but this implementation
/// uses a simple JSON file cache for portability.
pub struct StoreCache {
    /// Path to the cache file.
    cache_path: std::path::PathBuf,
}

impl StoreCache {
    /// Create a new cache at the given path.
    pub fn new(cache_dir: impl Into<std::path::PathBuf>) -> Self {
        let cache_path = cache_dir.into().join("store_catalog.json");
        Self { cache_path }
    }

    /// Create a cache using the default location (`~/.clawdesk/cache/`).
    pub fn default_location() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let cache_dir = std::path::PathBuf::from(home)
            .join(".clawdesk")
            .join("cache");
        Self::new(cache_dir)
    }

    /// Hydrate a `StoreBackend` from the persistent cache.
    ///
    /// Returns the number of entries loaded, or 0 if no cache exists.
    pub fn hydrate(&self, store: &mut StoreBackend) -> usize {
        let data = match std::fs::read_to_string(&self.cache_path) {
            Ok(d) => d,
            Err(e) => {
                debug!(path = %self.cache_path.display(), err = %e, "no cache file found — starting fresh");
                return 0;
            }
        };

        let cached: CacheFile = match serde_json::from_str(&data) {
            Ok(c) => c,
            Err(e) => {
                warn!(err = %e, "cache file corrupted — discarding");
                return 0;
            }
        };

        let count = cached.entries.len();
        for entry in cached.entries {
            store.upsert(entry);
        }

        info!(entries = count, "catalog hydrated from cache");
        count
    }

    /// Load the persisted sync state.
    pub fn load_sync_state(&self) -> SyncState {
        let state_path = self.cache_path.with_file_name("sync_state.json");
        match std::fs::read_to_string(&state_path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => SyncState::default(),
        }
    }

    /// Persist the current catalog to disk (write-through).
    pub fn persist(&self, store: &StoreBackend) -> Result<(), CacheError> {
        // Ensure directory exists
        if let Some(parent) = self.cache_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CacheError::Io(e.to_string()))?;
        }

        let cache_file = CacheFile {
            version: 1,
            entries: store.all_entries().into_iter().cloned().collect(),
        };

        let json = serde_json::to_string_pretty(&cache_file)
            .map_err(|e| CacheError::Serialize(e.to_string()))?;

        // Atomic write: write to temp file, then rename
        let tmp_path = self.cache_path.with_extension("tmp");
        std::fs::write(&tmp_path, &json)
            .map_err(|e| CacheError::Io(e.to_string()))?;
        std::fs::rename(&tmp_path, &self.cache_path)
            .map_err(|e| CacheError::Io(e.to_string()))?;

        debug!(entries = cache_file.entries.len(), "catalog persisted to cache");
        Ok(())
    }

    /// Persist the sync state to disk.
    pub fn persist_sync_state(&self, state: &SyncState) -> Result<(), CacheError> {
        if let Some(parent) = self.cache_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CacheError::Io(e.to_string()))?;
        }

        let state_path = self.cache_path.with_file_name("sync_state.json");
        let json = serde_json::to_string_pretty(state)
            .map_err(|e| CacheError::Serialize(e.to_string()))?;
        std::fs::write(&state_path, json)
            .map_err(|e| CacheError::Io(e.to_string()))?;

        debug!("sync state persisted to cache");
        Ok(())
    }

    /// Write-through: upsert an entry to both store and cache.
    pub fn upsert_through(
        &self,
        store: &mut StoreBackend,
        entry: StoreEntry,
    ) -> Result<(), CacheError> {
        store.upsert(entry);
        self.persist(store)
    }

    /// Write-through: remove an entry from both store and cache.
    pub fn remove_through(
        &self,
        store: &mut StoreBackend,
        skill_id: &str,
    ) -> Result<bool, CacheError> {
        let removed = store.remove(skill_id);
        self.persist(store)?;
        Ok(removed)
    }

    /// Invalidate the cache (delete the file).
    pub fn invalidate(&self) -> Result<(), CacheError> {
        if self.cache_path.exists() {
            std::fs::remove_file(&self.cache_path)
                .map_err(|e| CacheError::Io(e.to_string()))?;
        }
        let state_path = self.cache_path.with_file_name("sync_state.json");
        if state_path.exists() {
            let _ = std::fs::remove_file(&state_path);
        }
        info!("cache invalidated");
        Ok(())
    }
}

/// Serialized cache file format.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CacheFile {
    /// Format version for forward compatibility.
    version: u32,
    /// All catalog entries.
    entries: Vec<StoreEntry>,
}

/// Errors from cache operations.
#[derive(Debug, Clone)]
pub enum CacheError {
    /// I/O error (file read/write).
    Io(String),
    /// Serialization error.
    Serialize(String),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cache I/O error: {}", e),
            Self::Serialize(e) => write!(f, "cache serialization error: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::SkillId;
    use crate::store::{StoreCategory, InstallState};

    fn test_entry(name: &str) -> StoreEntry {
        StoreEntry {
            skill_id: SkillId::new("store", name),
            display_name: name.to_string(),
            short_description: format!("A {} skill", name),
            long_description: String::new(),
            category: StoreCategory::Other,
            tags: vec![],
            author: "test".into(),
            version: "1.0.0".into(),
            install_state: InstallState::Available,
            rating: 4.0,
            install_count: 0,
            updated_at: "2026-01-01".into(),
            icon: "📦".into(),
            verified: true,
            license: None,
            source_url: None,
            min_version: None,
        }
    }

    #[test]
    fn cache_roundtrip() {
        let dir = std::env::temp_dir().join("clawdesk_cache_test");
        let _ = std::fs::remove_dir_all(&dir);

        let cache = StoreCache::new(&dir);
        let mut store = StoreBackend::new();
        store.upsert(test_entry("alpha"));
        store.upsert(test_entry("beta"));

        // Persist
        cache.persist(&store).unwrap();
        assert!(cache.cache_path.exists());

        // Hydrate into fresh store
        let mut store2 = StoreBackend::new();
        let count = cache.hydrate(&mut store2);
        assert_eq!(count, 2);
        assert!(store2.get("store/alpha").is_some());
        assert!(store2.get("store/beta").is_some());

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_through_upsert() {
        let dir = std::env::temp_dir().join("clawdesk_cache_wt_test");
        let _ = std::fs::remove_dir_all(&dir);

        let cache = StoreCache::new(&dir);
        let mut store = StoreBackend::new();

        cache.upsert_through(&mut store, test_entry("gamma")).unwrap();
        assert_eq!(store.entry_count(), 1);

        // Re-hydrate should find the entry
        let mut store2 = StoreBackend::new();
        let count = cache.hydrate(&mut store2);
        assert_eq!(count, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_invalidation() {
        let dir = std::env::temp_dir().join("clawdesk_cache_inv_test");
        let _ = std::fs::remove_dir_all(&dir);

        let cache = StoreCache::new(&dir);
        let mut store = StoreBackend::new();
        store.upsert(test_entry("delta"));
        cache.persist(&store).unwrap();

        cache.invalidate().unwrap();
        assert!(!cache.cache_path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_state_roundtrip() {
        let dir = std::env::temp_dir().join("clawdesk_cache_ss_test");
        let _ = std::fs::remove_dir_all(&dir);

        let cache = StoreCache::new(&dir);
        let state = SyncState {
            last_etag: Some("etag-abc".into()),
            last_merkle_root: Some("abc123".into()),
            last_sync_at: Some("2026-01-01T00:00:00Z".into()),
        };

        cache.persist_sync_state(&state).unwrap();
        let loaded = cache.load_sync_state();
        assert_eq!(loaded.last_etag, Some("etag-abc".into()));
        assert_eq!(loaded.last_merkle_root, Some("abc123".into()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
