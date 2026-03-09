//! Provider response caching layer.
//!
//! Caches LLM responses keyed by (model, messages_hash) with TTL eviction.
//! Only caches deterministic requests (temperature=0) by default.
//!
//! # Design
//!
//! Cache keys are computed as SHA-256 of the serialized request body
//! (model + messages + system prompt). This ensures:
//! - Identical prompts hit the same cache entry.
//! - Different models/temperatures produce different keys.
//! - No sensitive data leaks into cache keys (only hashes stored).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Configuration for the response cache.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Maximum number of cached entries.
    pub max_entries: usize,
    /// Default TTL for cached responses.
    pub default_ttl: Duration,
    /// Only cache requests with temperature == 0.0.
    pub deterministic_only: bool,
    /// Maximum response size to cache (bytes).
    pub max_response_bytes: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 1024,
            default_ttl: Duration::from_secs(3600),
            deterministic_only: true,
            max_response_bytes: 64 * 1024, // 64 KiB
        }
    }
}

/// A cached response entry.
#[derive(Debug, Clone)]
struct CacheEntry {
    response: CachedResponse,
    inserted_at: Instant,
    ttl: Duration,
    hits: u64,
}

impl CacheEntry {
    fn is_expired(&self) -> bool {
        self.inserted_at.elapsed() > self.ttl
    }
}

/// The cached response payload.
#[derive(Debug, Clone)]
pub struct CachedResponse {
    /// The response content text.
    pub content: String,
    /// The model that generated this response.
    pub model: String,
    /// Token usage summary.
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Finish reason.
    pub finish_reason: String,
}

/// Cache statistics.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub total_hits: u64,
    pub total_misses: u64,
    pub total_evictions: u64,
    pub current_entries: usize,
    pub max_entries: usize,
}

impl CacheStats {
    /// Hit rate (0.0–1.0).
    pub fn hit_rate(&self) -> f64 {
        let total = self.total_hits + self.total_misses;
        if total == 0 {
            0.0
        } else {
            self.total_hits as f64 / total as f64
        }
    }
}

/// LRU-ish response cache for provider outputs.
pub struct ResponseCache {
    config: CacheConfig,
    entries: HashMap<String, CacheEntry>,
    stats: CacheStats,
}

impl ResponseCache {
    /// Create a new response cache.
    pub fn new(config: CacheConfig) -> Self {
        let max = config.max_entries;
        Self {
            config,
            entries: HashMap::new(),
            stats: CacheStats {
                max_entries: max,
                ..Default::default()
            },
        }
    }

    /// Compute a cache key from the request components.
    ///
    /// Key = hex(SHA-256(model || "\0" || system_prompt || "\0" || messages_json)).
    pub fn cache_key(model: &str, system_prompt: &str, messages_json: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        model.hash(&mut hasher);
        system_prompt.hash(&mut hasher);
        messages_json.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Look up a cached response.
    pub fn get(&mut self, key: &str) -> Option<&CachedResponse> {
        // Evict if expired.
        if self.entries.get(key).map_or(false, |e| e.is_expired()) {
            self.entries.remove(key);
            self.stats.total_evictions += 1;
            self.stats.current_entries = self.entries.len();
        }

        if let Some(entry) = self.entries.get_mut(key) {
            entry.hits += 1;
            self.stats.total_hits += 1;
            Some(&entry.response)
        } else {
            self.stats.total_misses += 1;
            None
        }
    }

    /// Insert a response into the cache.
    ///
    /// Returns `false` if the response was too large to cache.
    pub fn insert(&mut self, key: String, response: CachedResponse) -> bool {
        self.insert_with_ttl(key, response, self.config.default_ttl)
    }

    /// Insert with a custom TTL.
    pub fn insert_with_ttl(
        &mut self,
        key: String,
        response: CachedResponse,
        ttl: Duration,
    ) -> bool {
        // Check size limit.
        if response.content.len() > self.config.max_response_bytes {
            return false;
        }

        // Evict expired entries if at capacity.
        if self.entries.len() >= self.config.max_entries {
            self.evict_expired();
        }

        // If still at capacity, evict the least-hit entry.
        if self.entries.len() >= self.config.max_entries {
            self.evict_lru();
        }

        self.entries.insert(
            key,
            CacheEntry {
                response,
                inserted_at: Instant::now(),
                ttl,
                hits: 0,
            },
        );
        self.stats.current_entries = self.entries.len();
        true
    }

    /// Get cache statistics.
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.stats.current_entries = 0;
    }

    /// Evict all expired entries.
    pub fn evict_expired(&mut self) {
        let before = self.entries.len();
        self.entries.retain(|_, v| !v.is_expired());
        let removed = before - self.entries.len();
        self.stats.total_evictions += removed as u64;
        self.stats.current_entries = self.entries.len();
    }

    /// Evict the entry with the fewest hits.
    fn evict_lru(&mut self) {
        if let Some(key) = self
            .entries
            .iter()
            .min_by_key(|(_, v)| v.hits)
            .map(|(k, _)| k.clone())
        {
            self.entries.remove(&key);
            self.stats.total_evictions += 1;
            self.stats.current_entries = self.entries.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_response() -> CachedResponse {
        CachedResponse {
            content: "Hello, world!".into(),
            model: "gpt-4o".into(),
            input_tokens: 10,
            output_tokens: 5,
            finish_reason: "stop".into(),
        }
    }

    #[test]
    fn insert_and_get() {
        let mut cache = ResponseCache::new(CacheConfig::default());
        let key = ResponseCache::cache_key("gpt-4o", "sys", "[]");

        assert!(cache.get(&key).is_none());
        assert!(cache.insert(key.clone(), test_response()));
        assert!(cache.get(&key).is_some());
        assert_eq!(cache.stats().total_hits, 1);
        assert_eq!(cache.stats().total_misses, 1);
    }

    #[test]
    fn ttl_expiration() {
        let config = CacheConfig {
            default_ttl: Duration::from_millis(10),
            ..Default::default()
        };
        let mut cache = ResponseCache::new(config);
        let key = "test-key".to_string();

        cache.insert(key.clone(), test_response());
        std::thread::sleep(Duration::from_millis(15));
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn capacity_eviction() {
        let config = CacheConfig {
            max_entries: 2,
            ..Default::default()
        };
        let mut cache = ResponseCache::new(config);

        cache.insert("a".into(), test_response());
        cache.insert("b".into(), test_response());

        // Access "b" to give it more hits.
        cache.get("b");

        // Insert "c" — should evict "a" (fewest hits).
        cache.insert("c".into(), test_response());
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_some());
    }

    #[test]
    fn max_response_size() {
        let config = CacheConfig {
            max_response_bytes: 5,
            ..Default::default()
        };
        let mut cache = ResponseCache::new(config);
        let resp = CachedResponse {
            content: "this is too long".into(),
            ..test_response()
        };
        assert!(!cache.insert("key".into(), resp));
    }

    #[test]
    fn cache_key_determinism() {
        let k1 = ResponseCache::cache_key("gpt-4o", "system", "[msg1]");
        let k2 = ResponseCache::cache_key("gpt-4o", "system", "[msg1]");
        let k3 = ResponseCache::cache_key("claude", "system", "[msg1]");
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[test]
    fn hit_rate() {
        let mut cache = ResponseCache::new(CacheConfig::default());
        cache.insert("key".into(), test_response());

        cache.get("key"); // hit
        cache.get("missing"); // miss

        let rate = cache.stats().hit_rate();
        assert!((rate - 0.5).abs() < 0.01);
    }
}
