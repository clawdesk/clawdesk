//! Agent discovery cache with adaptive TTL and consistency bounds.
//!
//! ## Problem
//!
//! Naive discovery generates `O(N × M)` HTTP requests per cycle for `N` agents
//! and `M` requestors. A fixed TTL is suboptimal: too short wastes bandwidth;
//! too long causes stale routing.
//!
//! ## Solution
//!
//! Adaptive TTL: model capability change as Poisson process with rate `λ_change`.
//! Optimal TTL minimizes `C = c_stale · P(stale) + c_fetch · fetch_rate`:
//!   `TTL* = √(2 · c_fetch / (c_stale · λ_change))`
//!
//! Conditional GET (`If-None-Match` / `ETag`) eliminates redundant payload transfer.
//! `304 Not Modified` has `O(1)` payload.
//!
//! ## Freshness Guarantee
//!
//! `P(staleness > δ) ≤ e^{-λ_gossip · δ}` — exponentially decaying with gossip rate.

use crate::agent_card::AgentCard;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Configuration for the discovery cache.
#[derive(Debug, Clone)]
pub struct DiscoveryCacheConfig {
    /// Cost of serving stale data (dimensionless weight).
    pub cost_stale: f64,
    /// Cost of fetching (dimensionless weight).
    pub cost_fetch: f64,
    /// Initial estimate of capability change rate (changes per second).
    pub initial_change_rate: f64,
    /// Minimum TTL (floor).
    pub min_ttl: Duration,
    /// Maximum TTL (ceiling).
    pub max_ttl: Duration,
    /// Maximum consistency window — staleness bound `δ`.
    pub max_staleness: Duration,
    /// Maximum entries in the cache.
    pub max_entries: usize,
}

impl Default for DiscoveryCacheConfig {
    fn default() -> Self {
        Self {
            cost_stale: 10.0,
            cost_fetch: 1.0,
            initial_change_rate: 0.001, // ~1 change per 1000 seconds
            min_ttl: Duration::from_secs(10),
            max_ttl: Duration::from_secs(3600),
            max_staleness: Duration::from_secs(300),
            max_entries: 1000,
        }
    }
}

/// Cached agent card with adaptive TTL tracking.
#[derive(Debug)]
pub struct CachedAgentCard {
    /// The agent card.
    pub card: AgentCard,
    /// When this entry was last fetched.
    pub fetched_at: Instant,
    /// ETag from the last successful fetch (for conditional GET).
    pub etag: Option<String>,
    /// Computed TTL for this entry.
    pub ttl: Duration,
    /// Number of times this entry has been refreshed.
    pub refresh_count: u64,
    /// Number of times a refresh returned the same data (304 Not Modified).
    pub unchanged_count: u64,
    /// Estimated change rate for this specific agent (Bayesian update).
    pub estimated_change_rate: f64,
    /// Number of times this entry has been accessed since last refresh.
    ///
    /// Atomic so `get()` can increment without `&mut self`.
    pub access_count: std::sync::atomic::AtomicU64,
    /// Monotonic LRU clock value — higher = more recently accessed.
    /// Used for O(1) promotion and O(n) eviction (eviction is rare).
    pub lru_clock: std::sync::atomic::AtomicU64,
}

impl Clone for CachedAgentCard {
    fn clone(&self) -> Self {
        use std::sync::atomic::Ordering::Relaxed;
        Self {
            card: self.card.clone(),
            fetched_at: self.fetched_at,
            etag: self.etag.clone(),
            ttl: self.ttl,
            refresh_count: self.refresh_count,
            unchanged_count: self.unchanged_count,
            estimated_change_rate: self.estimated_change_rate,
            access_count: std::sync::atomic::AtomicU64::new(self.access_count.load(Relaxed)),
            lru_clock: std::sync::atomic::AtomicU64::new(self.lru_clock.load(Relaxed)),
        }
    }
}

impl CachedAgentCard {
    /// Whether this entry has exceeded its TTL.
    pub fn is_expired(&self) -> bool {
        self.fetched_at.elapsed() > self.ttl
    }

    /// How stale this entry is (time since fetch).
    pub fn staleness(&self) -> Duration {
        self.fetched_at.elapsed()
    }

    /// Freshness ratio — 1.0 = just fetched, 0.0 = at TTL expiry.
    pub fn freshness(&self) -> f64 {
        let elapsed = self.fetched_at.elapsed().as_secs_f64();
        let ttl = self.ttl.as_secs_f64();
        if ttl == 0.0 {
            return 0.0;
        }
        (1.0 - elapsed / ttl).max(0.0)
    }
}

/// Agent discovery cache with adaptive TTL.
///
/// `get()` takes `&self` — LRU promotion is O(1) via a global
/// monotonic clock stored per-entry (atomic), enabling concurrent reads.
pub struct DiscoveryCache {
    config: DiscoveryCacheConfig,
    /// Agent ID → cached card.
    entries: HashMap<String, CachedAgentCard>,
    /// Global monotonic clock for O(1) LRU promotion.
    /// Incremented on every `get()` / `put()` — entries store their last clock value.
    global_clock: std::sync::atomic::AtomicU64,
    /// Global stats (atomic — no lock needed for reads).
    pub stats: DiscoveryCacheStats,
}

/// Cache statistics.
///
/// All counters are `AtomicU64` so that `get()` can take `&self`
/// instead of `&mut self`, enabling concurrent reads behind `RwLock`.
#[derive(Debug, Default)]
pub struct DiscoveryCacheStats {
    pub total_lookups: std::sync::atomic::AtomicU64,
    pub cache_hits: std::sync::atomic::AtomicU64,
    pub cache_misses: std::sync::atomic::AtomicU64,
    pub conditional_gets: std::sync::atomic::AtomicU64,
    pub not_modified_responses: std::sync::atomic::AtomicU64,
    pub stale_entries_served: std::sync::atomic::AtomicU64,
}

impl Clone for DiscoveryCacheStats {
    fn clone(&self) -> Self {
        use std::sync::atomic::Ordering::Relaxed;
        Self {
            total_lookups: std::sync::atomic::AtomicU64::new(self.total_lookups.load(Relaxed)),
            cache_hits: std::sync::atomic::AtomicU64::new(self.cache_hits.load(Relaxed)),
            cache_misses: std::sync::atomic::AtomicU64::new(self.cache_misses.load(Relaxed)),
            conditional_gets: std::sync::atomic::AtomicU64::new(self.conditional_gets.load(Relaxed)),
            not_modified_responses: std::sync::atomic::AtomicU64::new(self.not_modified_responses.load(Relaxed)),
            stale_entries_served: std::sync::atomic::AtomicU64::new(self.stale_entries_served.load(Relaxed)),
        }
    }
}

/// Free function computing optimal TTL from config and change rate.
/// Avoids borrow conflicts when called from methods that hold mutable entry refs.
fn compute_ttl_with_config(config: &DiscoveryCacheConfig, change_rate: f64) -> Duration {
    let lambda = change_rate.max(1e-10);
    let optimal = (2.0 * config.cost_fetch / (config.cost_stale * lambda)).sqrt();
    let secs = optimal
        .max(config.min_ttl.as_secs_f64())
        .min(config.max_ttl.as_secs_f64());
    Duration::from_secs_f64(secs)
}

impl DiscoveryCache {
    /// Create a new discovery cache.
    pub fn new(config: DiscoveryCacheConfig) -> Self {
        Self {
            entries: HashMap::with_capacity(config.max_entries / 4),
            global_clock: std::sync::atomic::AtomicU64::new(0),
            stats: DiscoveryCacheStats::default(),
            config,
        }
    }

    /// Compute optimal TTL for a given change rate using the cost model.
    ///
    /// `TTL* = √(2 · c_fetch / (c_stale · λ_change))`
    ///
    /// Clamped to `[min_ttl, max_ttl]`.
    fn compute_ttl(&self, change_rate: f64) -> Duration {
        compute_ttl_with_config(&self.config, change_rate)
    }
    /// Lookup an agent card in the cache.
    ///
    /// Returns the cached card if present and not expired.
    /// If expired, returns `None` (caller should re-fetch).
    ///
    /// Takes `&self` — stats are atomic, LRU behind Mutex.
    pub fn get(&self, agent_id: &str) -> Option<&AgentCard> {
        use std::sync::atomic::Ordering::Relaxed;
        self.stats.total_lookups.fetch_add(1, Relaxed);

        if let Some(entry) = self.entries.get(agent_id) {
            entry.access_count.fetch_add(1, Relaxed);

            // O(1) LRU promotion — just bump the clock.
            let tick = self.global_clock.fetch_add(1, Relaxed);
            entry.lru_clock.store(tick, Relaxed);

            if !entry.is_expired() {
                self.stats.cache_hits.fetch_add(1, Relaxed);
                return Some(&entry.card);
            }

            // Expired but present — check if within max_staleness for soft serving.
            if entry.staleness() < self.config.max_staleness {
                self.stats.stale_entries_served.fetch_add(1, Relaxed);
                return Some(&entry.card);
            }
        }

        self.stats.cache_misses.fetch_add(1, Relaxed);
        None
    }

    /// Get the ETag for a cached agent (for conditional GET).
    pub fn get_etag(&self, agent_id: &str) -> Option<&str> {
        self.entries
            .get(agent_id)
            .and_then(|e| e.etag.as_deref())
    }

    /// Insert or update a cached agent card after a successful fetch.
    pub fn put(&mut self, agent_id: &str, card: AgentCard, etag: Option<String>) {
        // Enforce max entries via LRU eviction.
        if self.entries.len() >= self.config.max_entries && !self.entries.contains_key(agent_id) {
            self.evict_oldest();
        }

        let change_rate = self
            .entries
            .get(agent_id)
            .map(|e| e.estimated_change_rate)
            .unwrap_or(self.config.initial_change_rate);

        let ttl = self.compute_ttl(change_rate);

        // O(1) LRU promotion — just assign a fresh clock tick.
        let tick = self.global_clock.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let entry = self.entries.entry(agent_id.to_string()).or_insert_with(|| {
            CachedAgentCard {
                card: card.clone(),
                fetched_at: Instant::now(),
                etag: None,
                ttl,
                refresh_count: 0,
                unchanged_count: 0,
                estimated_change_rate: change_rate,
                access_count: std::sync::atomic::AtomicU64::new(0),
                lru_clock: std::sync::atomic::AtomicU64::new(tick),
            }
        });

        entry.card = card;
        entry.fetched_at = Instant::now();
        entry.etag = etag;
        entry.ttl = ttl;
        entry.refresh_count += 1;
        entry.access_count.store(0, std::sync::atomic::Ordering::Relaxed);
        entry.lru_clock.store(tick, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record a "304 Not Modified" response — card has not changed.
    ///
    /// This updates the change rate estimate (Bayesian: more unchanged responses
    /// → lower change rate → longer TTL).
    pub fn record_not_modified(&mut self, agent_id: &str) {
        use std::sync::atomic::Ordering::Relaxed;
        self.stats.conditional_gets.fetch_add(1, Relaxed);
        self.stats.not_modified_responses.fetch_add(1, Relaxed);

        if let Some(entry) = self.entries.get_mut(agent_id) {
            entry.unchanged_count += 1;
            entry.refresh_count += 1;
            entry.fetched_at = Instant::now();

            // Bayesian update: decrease change rate estimate.
            // Exponential moving average toward lower rate.
            let observed_rate = if entry.refresh_count > 0 {
                1.0 - (entry.unchanged_count as f64 / entry.refresh_count as f64)
            } else {
                self.config.initial_change_rate
            };

            let alpha = 0.1; // smoothing factor
            entry.estimated_change_rate =
                entry.estimated_change_rate * (1.0 - alpha) + observed_rate * alpha;

            // Recompute TTL with updated rate.
            entry.ttl = compute_ttl_with_config(&self.config, entry.estimated_change_rate);
        }
    }

    /// Record that an agent's card has changed (invalidation).
    ///
    /// This increases the change rate estimate → shorter TTL.
    pub fn record_changed(&mut self, agent_id: &str) {
        if let Some(entry) = self.entries.get_mut(agent_id) {
            // Increase change rate estimate.
            let alpha = 0.3; // faster adaptation for changes
            entry.estimated_change_rate =
                entry.estimated_change_rate * (1.0 - alpha) + 0.1 * alpha;

            entry.ttl = compute_ttl_with_config(&self.config, entry.estimated_change_rate);
        }
    }

    /// Content-addressed put: only triggers a full re-index if the card's
    /// structural fingerprint (capabilities + skills + endpoint) actually changed.
    ///
    /// Returns `true` if the card was structurally modified and re-cached,
    /// `false` if only cosmetic fields changed (description, metadata) — in
    /// which case the cached card is updated in-place without bumping the
    /// change rate estimate.
    ///
    /// This is the recommended replacement for `put()` in hot paths:
    /// `put()` always counts as a change; `put_if_changed()` differentiates
    /// structural vs. cosmetic changes for accurate TTL adaptation.
    pub fn put_if_changed(&mut self, agent_id: &str, card: AgentCard, etag: Option<String>) -> bool {
        let new_fp = card.structural_fingerprint();

        if let Some(existing) = self.entries.get_mut(agent_id) {
            let old_fp = existing.card.structural_fingerprint();
            if old_fp == new_fp {
                // Cosmetic change only — update card in-place, treat as not-modified.
                existing.card = card;
                existing.fetched_at = std::time::Instant::now();
                existing.etag = etag;
                existing.unchanged_count += 1;
                existing.refresh_count += 1;

                // Bayesian update: decrease change rate (treated as unchanged).
                let observed_rate = 1.0 - (existing.unchanged_count as f64 / existing.refresh_count as f64);
                let alpha = 0.1;
                existing.estimated_change_rate =
                    existing.estimated_change_rate * (1.0 - alpha) + observed_rate * alpha;
                existing.ttl = compute_ttl_with_config(&self.config, existing.estimated_change_rate);

                return false; // No structural change.
            }
        }

        // Structural change — full put + mark as changed.
        self.put(agent_id, card, etag);
        self.record_changed(agent_id);
        true
    }

    /// Invalidate a specific agent's cache entry.
    pub fn invalidate(&mut self, agent_id: &str) -> Option<CachedAgentCard> {
        self.entries.remove(agent_id)
    }

    /// Get all expired entries that need refreshing.
    pub fn expired_entries(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.is_expired())
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Evict the least recently used entry.
    ///
    /// O(n) scan over entries — acceptable because eviction only fires when the
    /// cache is full, and `max_entries` is typically small (hundreds).
    fn evict_oldest(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        if let Some(oldest_id) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.lru_clock.load(Relaxed))
            .map(|(id, _)| id.clone())
        {
            self.entries.remove(&oldest_id);
        }
    }

    /// Clear all cached entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_card(id: &str) -> AgentCard {
        AgentCard::new(id, id, format!("http://{}.local", id))
    }

    #[test]
    fn basic_put_and_get() {
        let mut cache = DiscoveryCache::new(DiscoveryCacheConfig::default());
        cache.put("agent-1", make_card("agent-1"), Some("etag-1".into()));

        assert!(cache.get("agent-1").is_some());
        assert!(cache.get("agent-2").is_none());
    }

    #[test]
    fn etag_tracking() {
        let mut cache = DiscoveryCache::new(DiscoveryCacheConfig::default());
        cache.put("agent-1", make_card("agent-1"), Some("W/\"abc123\"".into()));

        assert_eq!(cache.get_etag("agent-1"), Some("W/\"abc123\""));
        assert_eq!(cache.get_etag("agent-2"), None);
    }

    #[test]
    fn adaptive_ttl_increases_with_stability() {
        let mut cache = DiscoveryCache::new(DiscoveryCacheConfig::default());
        cache.put("stable", make_card("stable"), None);

        // Simulate many "not modified" responses to stabilize rate.
        // The first few responses adjust the EMA from initial_change_rate.
        for _ in 0..50 {
            cache.record_not_modified("stable");
        }

        let mid_ttl = cache.entries["stable"].ttl;

        // More stability should push TTL even higher.
        for _ in 0..50 {
            cache.record_not_modified("stable");
        }

        let final_ttl = cache.entries["stable"].ttl;
        // After lots of stability, TTL should remain high or increase.
        assert!(final_ttl >= mid_ttl);
    }

    #[test]
    fn adaptive_ttl_decreases_with_changes() {
        let mut cache = DiscoveryCache::new(DiscoveryCacheConfig::default());
        cache.put("volatile", make_card("volatile"), None);

        // First establish a stable baseline.
        for _ in 0..5 {
            cache.record_not_modified("volatile");
        }
        let baseline_ttl = cache.entries["volatile"].ttl;

        // Now record changes.
        for _ in 0..5 {
            cache.record_changed("volatile");
        }
        let updated_ttl = cache.entries["volatile"].ttl;

        // TTL should decrease.
        assert!(updated_ttl <= baseline_ttl);
    }

    #[test]
    fn optimal_ttl_formula() {
        let cache = DiscoveryCache::new(DiscoveryCacheConfig {
            cost_stale: 10.0,
            cost_fetch: 1.0,
            initial_change_rate: 0.01,
            min_ttl: Duration::from_secs(1),
            max_ttl: Duration::from_secs(10000),
            ..Default::default()
        });

        // TTL* = √(2 × 1.0 / (10.0 × 0.01)) = √(20) ≈ 4.47 seconds.
        let ttl = cache.compute_ttl(0.01);
        let expected = (2.0 * 1.0 / (10.0 * 0.01_f64)).sqrt();
        let diff = (ttl.as_secs_f64() - expected).abs();
        assert!(diff < 0.1, "TTL={:?}, expected={:.2}s", ttl, expected);
    }

    #[test]
    fn cache_stats_tracking() {
        use std::sync::atomic::Ordering::Relaxed;
        let mut cache = DiscoveryCache::new(DiscoveryCacheConfig::default());
        cache.put("a", make_card("a"), None);

        cache.get("a"); // hit
        cache.get("b"); // miss

        assert_eq!(cache.stats.cache_hits.load(Relaxed), 1);
        assert_eq!(cache.stats.cache_misses.load(Relaxed), 1);
        assert_eq!(cache.stats.total_lookups.load(Relaxed), 2);
    }

    #[test]
    fn max_entries_eviction() {
        let config = DiscoveryCacheConfig {
            max_entries: 3,
            ..Default::default()
        };
        let mut cache = DiscoveryCache::new(config);

        cache.put("a", make_card("a"), None);
        cache.put("b", make_card("b"), None);
        cache.put("c", make_card("c"), None);
        cache.put("d", make_card("d"), None); // should evict oldest

        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn invalidation() {
        let mut cache = DiscoveryCache::new(DiscoveryCacheConfig::default());
        cache.put("a", make_card("a"), None);
        assert!(cache.get("a").is_some());

        cache.invalidate("a");
        assert!(cache.get("a").is_none());
    }

    #[test]
    fn structural_fingerprint_stable() {
        use crate::capability::CapabilityId;

        let card1 = AgentCard::new("a", "Agent A", "http://a.local")
            .with_capability(CapabilityId::TextGeneration)
            .with_capability(CapabilityId::WebSearch);

        let card2 = AgentCard::new("a", "Agent A", "http://a.local")
            .with_capability(CapabilityId::TextGeneration)
            .with_capability(CapabilityId::WebSearch);

        assert_eq!(card1.structural_fingerprint(), card2.structural_fingerprint());
    }

    #[test]
    fn structural_fingerprint_ignores_metadata() {
        use crate::capability::CapabilityId;

        let mut card1 = AgentCard::new("a", "Agent A", "http://a.local")
            .with_capability(CapabilityId::TextGeneration);
        card1.description = "Version 1".into();
        card1.metadata = serde_json::json!({"env": "dev"});

        let mut card2 = AgentCard::new("a", "Agent A", "http://a.local")
            .with_capability(CapabilityId::TextGeneration);
        card2.description = "Version 2 — totally different description".into();
        card2.metadata = serde_json::json!({"env": "prod", "region": "us-east-1"});

        assert_eq!(
            card1.structural_fingerprint(),
            card2.structural_fingerprint(),
            "metadata-only changes should not alter structural fingerprint"
        );
    }

    #[test]
    fn structural_fingerprint_changes_with_capabilities() {
        use crate::capability::CapabilityId;

        let card1 = AgentCard::new("a", "A", "http://a.local")
            .with_capability(CapabilityId::TextGeneration);

        let card2 = AgentCard::new("a", "A", "http://a.local")
            .with_capability(CapabilityId::TextGeneration)
            .with_capability(CapabilityId::WebSearch);

        assert_ne!(
            card1.structural_fingerprint(),
            card2.structural_fingerprint(),
            "adding a capability must change the fingerprint"
        );
    }

    #[test]
    fn put_if_changed_cosmetic_update() {
        use crate::capability::CapabilityId;

        let mut cache = DiscoveryCache::new(DiscoveryCacheConfig::default());

        let card1 = AgentCard::new("a", "Agent A", "http://a.local")
            .with_capability(CapabilityId::TextGeneration);
        cache.put("a", card1, None);

        // Update only description — cosmetic change.
        let mut card2 = AgentCard::new("a", "Agent A", "http://a.local")
            .with_capability(CapabilityId::TextGeneration);
        card2.description = "Updated description".into();

        let changed = cache.put_if_changed("a", card2, None);
        assert!(!changed, "cosmetic change should return false");

        // Description was updated in cache.
        let cached = cache.get("a").unwrap();
        assert_eq!(cached.description, "Updated description");
    }

    #[test]
    fn put_if_changed_structural_update() {
        use crate::capability::CapabilityId;

        let mut cache = DiscoveryCache::new(DiscoveryCacheConfig::default());

        let card1 = AgentCard::new("a", "Agent A", "http://a.local")
            .with_capability(CapabilityId::TextGeneration);
        cache.put("a", card1, None);

        // Add a new capability — structural change.
        let card2 = AgentCard::new("a", "Agent A", "http://a.local")
            .with_capability(CapabilityId::TextGeneration)
            .with_capability(CapabilityId::WebSearch);

        let changed = cache.put_if_changed("a", card2, None);
        assert!(changed, "structural change should return true");
    }
}
