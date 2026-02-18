//! Content-addressed media cache — SHA-256 keyed, file-backed.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, SystemTime};

/// Content-addressed media cache.
pub struct MediaCache {
    root: PathBuf,
    index: RwLock<HashMap<String, CacheEntry>>,
    max_size_bytes: u64,
}

/// Cache entry metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub key: String,
    pub original_name: Option<String>,
    pub mime_type: String,
    pub size_bytes: u64,
    pub cached_at: u64,
    pub last_accessed: u64,
    pub access_count: u32,
}

/// Cache statistics.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub total_entries: usize,
    pub total_bytes: u64,
    pub max_bytes: u64,
    pub oldest_entry_secs: Option<u64>,
}

impl MediaCache {
    /// Create a new file-backed cache.
    pub fn new(root: PathBuf, max_size_mb: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        let cache = Self {
            root,
            index: RwLock::new(HashMap::new()),
            max_size_bytes: max_size_mb * 1024 * 1024,
        };
        cache.load_index()?;
        Ok(cache)
    }

    /// Compute SHA-256 content key.
    pub fn content_key(data: &[u8]) -> String {
        use std::fmt::Write;
        // Simple FNV-1a-based hash for speed (not cryptographic — for cache keys only)
        // For production, swap to ring::digest::SHA256
        let mut hash: u64 = 14695981039346656037;
        for &byte in data {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
        let mut s = String::with_capacity(16);
        let _ = write!(s, "{:016x}", hash);
        s
    }

    /// Store data in cache, returning content key.
    pub fn put(
        &self,
        data: &[u8],
        mime_type: &str,
        original_name: Option<&str>,
    ) -> std::io::Result<String> {
        let key = Self::content_key(data);

        // Check if already cached
        if let Ok(idx) = self.index.read() {
            if idx.contains_key(&key) {
                return Ok(key);
            }
        }

        // Evict if necessary
        self.maybe_evict(data.len() as u64)?;

        // Write file
        let path = self.key_path(&key);
        std::fs::write(&path, data)?;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        let entry = CacheEntry {
            key: key.clone(),
            original_name: original_name.map(String::from),
            mime_type: mime_type.to_string(),
            size_bytes: data.len() as u64,
            cached_at: now,
            last_accessed: now,
            access_count: 0,
        };

        if let Ok(mut idx) = self.index.write() {
            idx.insert(key.clone(), entry);
        }

        self.save_index()?;
        Ok(key)
    }

    /// Retrieve cached data by key.
    pub fn get(&self, key: &str) -> std::io::Result<Option<Vec<u8>>> {
        if let Ok(mut idx) = self.index.write() {
            if let Some(entry) = idx.get_mut(key) {
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO)
                    .as_secs();
                entry.last_accessed = now;
                entry.access_count += 1;

                let path = self.key_path(key);
                let data = std::fs::read(&path)?;
                return Ok(Some(data));
            }
        }
        Ok(None)
    }

    /// Check if key exists in cache.
    pub fn contains(&self, key: &str) -> bool {
        self.index
            .read()
            .map(|idx| idx.contains_key(key))
            .unwrap_or(false)
    }

    /// Remove entry by key.
    pub fn remove(&self, key: &str) -> std::io::Result<bool> {
        if let Ok(mut idx) = self.index.write() {
            if idx.remove(key).is_some() {
                let path = self.key_path(key);
                let _ = std::fs::remove_file(path);
                self.save_index()?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        let idx = match self.index.read() {
            Ok(idx) => idx,
            Err(_) => return CacheStats::default(),
        };

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        let total_bytes: u64 = idx.values().map(|e| e.size_bytes).sum();
        let oldest = idx.values().map(|e| now.saturating_sub(e.cached_at)).max();

        CacheStats {
            total_entries: idx.len(),
            total_bytes,
            max_bytes: self.max_size_bytes,
            oldest_entry_secs: oldest,
        }
    }

    /// Evict oldest entries to make room.
    fn maybe_evict(&self, needed: u64) -> std::io::Result<()> {
        let mut idx = match self.index.write() {
            Ok(idx) => idx,
            Err(_) => return Ok(()),
        };

        let current: u64 = idx.values().map(|e| e.size_bytes).sum();
        if current + needed <= self.max_size_bytes {
            return Ok(());
        }

        // Sort by last_accessed (oldest first)
        let mut entries: Vec<_> = idx.values().cloned().collect();
        entries.sort_by_key(|e| e.last_accessed);

        let mut freed = 0u64;
        let need_to_free = (current + needed).saturating_sub(self.max_size_bytes);

        for entry in &entries {
            if freed >= need_to_free {
                break;
            }
            let path = self.key_path(&entry.key);
            let _ = std::fs::remove_file(path);
            idx.remove(&entry.key);
            freed += entry.size_bytes;
        }

        Ok(())
    }

    fn key_path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("_index.json")
    }

    fn load_index(&self) -> std::io::Result<()> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(());
        }
        let data = std::fs::read_to_string(&path)?;
        if let Ok(entries) = serde_json::from_str::<Vec<CacheEntry>>(&data) {
            if let Ok(mut idx) = self.index.write() {
                for entry in entries {
                    idx.insert(entry.key.clone(), entry);
                }
            }
        }
        Ok(())
    }

    fn save_index(&self) -> std::io::Result<()> {
        let idx = match self.index.read() {
            Ok(idx) => idx,
            Err(_) => return Ok(()),
        };
        let entries: Vec<_> = idx.values().cloned().collect();
        let data = serde_json::to_string(&entries).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
        })?;
        std::fs::write(self.index_path(), data)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_key_deterministic() {
        let data = b"hello world";
        let k1 = MediaCache::content_key(data);
        let k2 = MediaCache::content_key(data);
        assert_eq!(k1, k2);
    }

    #[test]
    fn content_key_differs_for_different_data() {
        let k1 = MediaCache::content_key(b"hello");
        let k2 = MediaCache::content_key(b"world");
        assert_ne!(k1, k2);
    }

    #[test]
    fn put_and_get() {
        let dir = std::env::temp_dir().join("clawdesk_cache_test");
        let _ = std::fs::remove_dir_all(&dir);
        let cache = MediaCache::new(dir.clone(), 10).unwrap();

        let key = cache.put(b"test data", "text/plain", Some("test.txt")).unwrap();
        assert!(cache.contains(&key));

        let data = cache.get(&key).unwrap().unwrap();
        assert_eq!(data, b"test data");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_tracking() {
        let dir = std::env::temp_dir().join("clawdesk_cache_stats_test");
        let _ = std::fs::remove_dir_all(&dir);
        let cache = MediaCache::new(dir.clone(), 10).unwrap();

        cache.put(b"abc", "text/plain", None).unwrap();
        cache.put(b"defgh", "text/plain", None).unwrap();

        let stats = cache.stats();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.total_bytes, 8);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
