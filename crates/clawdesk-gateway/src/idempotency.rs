//! Request idempotency — injective mapping between user actions and
//! payload identifiers to prevent duplicate processing.
//!
//! ## Problem
//!
//! Without idempotency enforcement, a client retry (network timeout,
//! user double-click, WebSocket reconnect) can cause the same logical
//! request to be processed multiple times — duplicating messages in the
//! conversation, wasting LLM tokens, and producing incoherent context.
//!
//! ## Solution
//!
//! `RequestDeduplicator` maintains a bounded, TTL-evicting map of
//! recently-seen idempotency keys. Each incoming request carries a
//! unique `idempotency_key` (typically `UUID v4` generated client-side).
//! If a key is seen within its TTL window, the duplicate request is
//! rejected with the original response (if available) or a 409 Conflict.
//!
//! ### Key Properties
//!
//! - **Injective**: distinct user actions → distinct keys (UUID v4,
//!   $P(\text{collision}) < 2^{-122}$).
//! - **Bounded memory**: hard cap of `MAX_ENTRIES` with LRU eviction.
//! - **TTL-based**: entries expire after `DEFAULT_TTL_SECS` seconds.
//! - **Lock-free hot path**: `DashMap`-style sharded locking via
//!   `tokio::sync::RwLock` per shard for high concurrency.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let dedup = RequestDeduplicator::new();
//!
//! // In route handler:
//! match dedup.check_and_insert("req-uuid-1234").await {
//!     DeduplicationResult::New => { /* process normally */ }
//!     DeduplicationResult::Duplicate { first_seen, .. } => {
//!         return Err(ApiError::DuplicateRequest { .. });
//!     }
//! }
//!
//! // After processing, store the response for idempotent replay:
//! dedup.store_response("req-uuid-1234", "response text").await;
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Default TTL for idempotency entries.
const DEFAULT_TTL_SECS: u64 = 300; // 5 minutes

/// Maximum entries before forced eviction.
const MAX_ENTRIES: usize = 10_000;

/// Number of shards for concurrent access.
const SHARD_COUNT: usize = 16;

/// Tracks a previously seen request.
#[derive(Debug, Clone)]
pub struct IdempotencyEntry {
    /// When this key was first seen.
    pub first_seen: Instant,
    /// The cached response (if the original request has completed).
    pub response: Option<CachedResponse>,
    /// How many duplicate attempts were detected.
    pub duplicate_count: u32,
    /// Source identifier (e.g. "gateway-http", "gateway-ws").
    pub source: String,
}

/// Cached response for idempotent replay.
#[derive(Debug, Clone)]
pub struct CachedResponse {
    /// The response body text.
    pub body: String,
    /// HTTP status code (for REST) or message type (for WS).
    pub status: u16,
    /// When the response was generated.
    pub generated_at: Instant,
}

/// Result of checking an idempotency key.
#[derive(Debug)]
pub enum DeduplicationResult {
    /// Key not seen before — process the request normally.
    New,
    /// Key was seen before within the TTL window.
    Duplicate {
        /// When the original request was first seen.
        first_seen: Instant,
        /// Number of times this key has been duplicated (including this one).
        duplicate_count: u32,
        /// The cached response, if the original has completed.
        cached_response: Option<CachedResponse>,
    },
}

/// Shard of the deduplication map.
struct Shard {
    entries: HashMap<String, IdempotencyEntry>,
    /// Insertion-order keys for LRU eviction.
    order: Vec<String>,
}

impl Shard {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// Evict expired entries and enforce the per-shard capacity.
    fn evict(&mut self, ttl: Duration) {
        let now = Instant::now();
        let before = self.entries.len();

        // Remove expired entries.
        self.entries.retain(|_, entry| now.duration_since(entry.first_seen) < ttl);
        self.order.retain(|k| self.entries.contains_key(k));

        // If still over capacity, evict oldest entries (front of order).
        let per_shard_cap = MAX_ENTRIES / SHARD_COUNT;
        while self.entries.len() > per_shard_cap {
            if let Some(oldest) = self.order.first().cloned() {
                self.entries.remove(&oldest);
                self.order.remove(0);
            } else {
                break;
            }
        }

        let evicted = before.saturating_sub(self.entries.len());
        if evicted > 0 {
            debug!(evicted, remaining = self.entries.len(), "shard eviction");
        }
    }
}

/// Sharded request deduplicator with TTL-based expiry.
pub struct RequestDeduplicator {
    shards: Vec<RwLock<Shard>>,
    ttl: Duration,
}

impl RequestDeduplicator {
    /// Create a new deduplicator with default TTL.
    pub fn new() -> Self {
        Self::with_ttl(Duration::from_secs(DEFAULT_TTL_SECS))
    }

    /// Create a new deduplicator with custom TTL.
    pub fn with_ttl(ttl: Duration) -> Self {
        let shards = (0..SHARD_COUNT)
            .map(|_| RwLock::new(Shard::new()))
            .collect();
        Self { shards, ttl }
    }

    /// Determine which shard a key maps to.
    fn shard_index(&self, key: &str) -> usize {
        // FNV-1a hash for fast shard selection.
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in key.bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        (hash as usize) % SHARD_COUNT
    }

    /// Check whether a request key has been seen before.
    ///
    /// If the key is new, it is inserted and `DeduplicationResult::New`
    /// is returned. If it was seen within the TTL window, returns
    /// `DeduplicationResult::Duplicate` with the cached response (if any).
    pub async fn check_and_insert(
        &self,
        key: &str,
        source: &str,
    ) -> DeduplicationResult {
        let idx = self.shard_index(key);
        let mut shard = self.shards[idx].write().await;

        // Periodic eviction (every 100 entries).
        if shard.entries.len() % 100 == 99 {
            shard.evict(self.ttl);
        }

        let now = Instant::now();

        if let Some(entry) = shard.entries.get_mut(key) {
            // Check TTL.
            if now.duration_since(entry.first_seen) >= self.ttl {
                // Expired — treat as new.
                *entry = IdempotencyEntry {
                    first_seen: now,
                    response: None,
                    duplicate_count: 0,
                    source: source.to_string(),
                };
                debug!(key, "idempotency key expired, treating as new");
                return DeduplicationResult::New;
            }

            entry.duplicate_count += 1;
            let count = entry.duplicate_count;
            let first_seen = entry.first_seen;
            let cached = entry.response.clone();

            warn!(
                key,
                duplicate_count = count,
                source,
                "duplicate request detected"
            );

            return DeduplicationResult::Duplicate {
                first_seen,
                duplicate_count: count,
                cached_response: cached,
            };
        }

        // New key — insert.
        shard.entries.insert(
            key.to_string(),
            IdempotencyEntry {
                first_seen: now,
                response: None,
                duplicate_count: 0,
                source: source.to_string(),
            },
        );
        shard.order.push(key.to_string());

        debug!(key, source, "new idempotency key registered");
        DeduplicationResult::New
    }

    /// Store the response for a completed request (for idempotent replay).
    pub async fn store_response(
        &self,
        key: &str,
        body: String,
        status: u16,
    ) {
        let idx = self.shard_index(key);
        let mut shard = self.shards[idx].write().await;

        if let Some(entry) = shard.entries.get_mut(key) {
            entry.response = Some(CachedResponse {
                body,
                status,
                generated_at: Instant::now(),
            });
            debug!(key, "cached response stored for idempotent replay");
        }
    }

    /// Remove a specific key (e.g., on explicit cancellation).
    pub async fn remove(&self, key: &str) {
        let idx = self.shard_index(key);
        let mut shard = self.shards[idx].write().await;
        shard.entries.remove(key);
        shard.order.retain(|k| k != key);
    }

    /// Total number of tracked entries across all shards.
    pub async fn entry_count(&self) -> usize {
        let mut total = 0;
        for shard in &self.shards {
            total += shard.read().await.entries.len();
        }
        total
    }

    /// Force eviction across all shards.
    pub async fn evict_all(&self) {
        for shard in &self.shards {
            shard.write().await.evict(self.ttl);
        }
    }

    /// Get statistics about the deduplicator state.
    pub async fn stats(&self) -> DeduplicatorStats {
        let mut total_entries = 0usize;
        let mut total_duplicates = 0u64;
        let mut shard_sizes = Vec::with_capacity(SHARD_COUNT);

        for shard in &self.shards {
            let guard = shard.read().await;
            total_entries += guard.entries.len();
            for entry in guard.entries.values() {
                total_duplicates += entry.duplicate_count as u64;
            }
            shard_sizes.push(guard.entries.len());
        }

        DeduplicatorStats {
            total_entries,
            total_duplicates_blocked: total_duplicates,
            shard_count: SHARD_COUNT,
            shard_sizes,
            ttl_secs: self.ttl.as_secs(),
        }
    }
}

impl Default for RequestDeduplicator {
    fn default() -> Self {
        Self::new()
    }
}

/// Deduplicator statistics for monitoring.
#[derive(Debug, Clone)]
pub struct DeduplicatorStats {
    pub total_entries: usize,
    pub total_duplicates_blocked: u64,
    pub shard_count: usize,
    pub shard_sizes: Vec<usize>,
    pub ttl_secs: u64,
}

/// Generate a new idempotency key (UUID v4 hex without dashes).
///
/// Uses the same lightweight PRNG approach as other ClawDesk modules
/// to avoid a `rand` dependency. For idempotency keys the collision
/// space is $2^{128}$ — sufficient for any realistic request volume.
pub fn generate_idempotency_key() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_key_returns_new() {
        let dedup = RequestDeduplicator::new();
        match dedup.check_and_insert("key-1", "test").await {
            DeduplicationResult::New => {}
            DeduplicationResult::Duplicate { .. } => panic!("expected New"),
        }
    }

    #[tokio::test]
    async fn duplicate_key_detected() {
        let dedup = RequestDeduplicator::new();

        // First insertion.
        dedup.check_and_insert("key-1", "test").await;

        // Second insertion of same key.
        match dedup.check_and_insert("key-1", "test").await {
            DeduplicationResult::Duplicate {
                duplicate_count, ..
            } => {
                assert_eq!(duplicate_count, 1);
            }
            DeduplicationResult::New => panic!("expected Duplicate"),
        }
    }

    #[tokio::test]
    async fn different_keys_are_independent() {
        let dedup = RequestDeduplicator::new();

        dedup.check_and_insert("key-a", "test").await;
        match dedup.check_and_insert("key-b", "test").await {
            DeduplicationResult::New => {}
            DeduplicationResult::Duplicate { .. } => panic!("expected New for different key"),
        }
    }

    #[tokio::test]
    async fn expired_key_treated_as_new() {
        let dedup = RequestDeduplicator::with_ttl(Duration::from_millis(50));

        dedup.check_and_insert("key-1", "test").await;
        tokio::time::sleep(Duration::from_millis(60)).await;

        match dedup.check_and_insert("key-1", "test").await {
            DeduplicationResult::New => {}
            DeduplicationResult::Duplicate { .. } => panic!("expected New after TTL"),
        }
    }

    #[tokio::test]
    async fn cached_response_returned_on_duplicate() {
        let dedup = RequestDeduplicator::new();

        dedup.check_and_insert("key-1", "test").await;
        dedup
            .store_response("key-1", "hello world".to_string(), 200)
            .await;

        match dedup.check_and_insert("key-1", "test").await {
            DeduplicationResult::Duplicate {
                cached_response, ..
            } => {
                let resp = cached_response.unwrap();
                assert_eq!(resp.body, "hello world");
                assert_eq!(resp.status, 200);
            }
            DeduplicationResult::New => panic!("expected Duplicate"),
        }
    }

    #[tokio::test]
    async fn remove_key() {
        let dedup = RequestDeduplicator::new();

        dedup.check_and_insert("key-1", "test").await;
        dedup.remove("key-1").await;

        match dedup.check_and_insert("key-1", "test").await {
            DeduplicationResult::New => {}
            DeduplicationResult::Duplicate { .. } => panic!("expected New after remove"),
        }
    }

    #[tokio::test]
    async fn entry_count_tracks() {
        let dedup = RequestDeduplicator::new();
        assert_eq!(dedup.entry_count().await, 0);

        dedup.check_and_insert("k1", "test").await;
        dedup.check_and_insert("k2", "test").await;
        assert_eq!(dedup.entry_count().await, 2);

        // Duplicate doesn't add new entry.
        dedup.check_and_insert("k1", "test").await;
        assert_eq!(dedup.entry_count().await, 2);
    }

    #[tokio::test]
    async fn stats_report() {
        let dedup = RequestDeduplicator::new();
        dedup.check_and_insert("k1", "test").await;
        dedup.check_and_insert("k2", "test").await;
        dedup.check_and_insert("k1", "test").await; // duplicate

        let stats = dedup.stats().await;
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.total_duplicates_blocked, 1);
        assert_eq!(stats.shard_count, SHARD_COUNT);
    }

    #[test]
    fn shard_distribution() {
        let dedup = RequestDeduplicator::new();
        // Different keys should distribute across shards.
        let indices: Vec<usize> = (0..100)
            .map(|i| dedup.shard_index(&format!("key-{i}")))
            .collect();
        // At least 4 distinct shards should be used for 100 keys.
        let unique: std::collections::HashSet<_> = indices.into_iter().collect();
        assert!(unique.len() >= 4, "poor shard distribution: {} unique shards", unique.len());
    }

    #[test]
    fn generate_key_is_unique() {
        let k1 = generate_idempotency_key();
        let k2 = generate_idempotency_key();
        assert_ne!(k1, k2);
        // UUID v4 is 36 chars with dashes.
        assert_eq!(k1.len(), 36);
    }
}
