//! Snapshot versioning for skill selection results.
//!
//! ## Snapshot Versioning (P2)
//!
//! Skill selection (greedy knapsack) is O(k log k) per invocation. When the
//! skill set hasn't changed between consecutive messages, re-running selection
//! is wasted work. This module provides a version-gated cache:
//!
//! ```text
//! if snapshot.version == registry.version() {
//!     return snapshot.clone();     // O(1) — cache hit
//! }
//! snapshot = recompute();          // O(k log k) — cache miss
//! snapshot.version = registry.version();
//! ```
//!
//! This gives O(1) amortized cost per message for the common case where
//! skills don't change mid-conversation.
//!
//! ## Invariant
//!
//! `snapshot.version` is monotonically increasing. A snapshot is only valid
//! when its version matches the current registry version.

use crate::definition::{Skill, SkillId};
use crate::selector::{SelectionResult, SelectedSkill, SkillSelector};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::debug;

/// Global version counter for the skill registry.
///
/// Incremented whenever:
/// - Skills are added or removed from the registry
/// - A skill's activation state changes
/// - A hot-reload replaces skill contents
static GLOBAL_VERSION: AtomicU64 = AtomicU64::new(0);

/// Bump the global version and return the new value.
pub fn bump_version() -> u64 {
    let v = GLOBAL_VERSION.fetch_add(1, Ordering::SeqCst) + 1;
    debug!(version = v, "skill registry version bumped");
    v
}

/// Get the current global version without bumping.
pub fn current_version() -> u64 {
    GLOBAL_VERSION.load(Ordering::SeqCst)
}

/// Reset the version counter (test only).
#[cfg(test)]
pub(crate) fn reset_version() {
    GLOBAL_VERSION.store(0, Ordering::SeqCst);
}

/// A cached snapshot of a skill selection result.
///
/// Valid only when `version == current_version()`.
#[derive(Debug, Clone)]
pub struct SkillSnapshot {
    /// Version at which this snapshot was computed.
    version: u64,
    /// Token budget used for this snapshot.
    budget: usize,
    /// The cached selection result.
    result: SelectionResult,
    /// Optional filter fingerprint — if the filter changes, cache is invalid.
    filter_fingerprint: Option<u64>,
}

impl SkillSnapshot {
    /// Create a new snapshot from a selection result.
    pub fn new(result: SelectionResult, budget: usize) -> Self {
        Self {
            version: current_version(),
            budget,
            result,
            filter_fingerprint: None,
        }
    }

    /// Create a snapshot with a filter fingerprint.
    pub fn with_filter(mut self, fingerprint: u64) -> Self {
        self.filter_fingerprint = Some(fingerprint);
        self
    }

    /// Check if this snapshot is still valid.
    pub fn is_valid(&self, budget: usize, filter_fingerprint: Option<u64>) -> bool {
        self.version == current_version()
            && self.budget == budget
            && self.filter_fingerprint == filter_fingerprint
    }

    /// Get the cached selection result (only if valid).
    pub fn get(&self, budget: usize, filter_fingerprint: Option<u64>) -> Option<&SelectionResult> {
        if self.is_valid(budget, filter_fingerprint) {
            Some(&self.result)
        } else {
            None
        }
    }

    /// The version at which this snapshot was computed.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The cached result, regardless of validity.
    pub fn result(&self) -> &SelectionResult {
        &self.result
    }

    /// Total tokens in the cached selection.
    pub fn total_tokens(&self) -> usize {
        self.result.total_tokens
    }

    /// Number of selected skills in the cache.
    pub fn selected_count(&self) -> usize {
        self.result.selected.len()
    }
}

/// Version-gated skill selection cache.
///
/// Wraps `SkillSelector` with automatic cache invalidation based on the
/// global registry version counter.
pub struct CachedSelector {
    /// Current cached snapshot (if any).
    snapshot: Option<SkillSnapshot>,
    /// Cache hit counter (for diagnostics).
    hits: u64,
    /// Cache miss counter.
    misses: u64,
}

impl CachedSelector {
    /// Create a new cached selector with no initial snapshot.
    pub fn new() -> Self {
        Self {
            snapshot: None,
            hits: 0,
            misses: 0,
        }
    }

    /// Select skills, using the cache if valid.
    ///
    /// Returns the selection result (either from cache or freshly computed).
    pub fn select(
        &mut self,
        candidates: &[Arc<Skill>],
        budget: usize,
        filter_fingerprint: Option<u64>,
    ) -> &SelectionResult {
        // Check if cache is valid
        let cache_valid = self
            .snapshot
            .as_ref()
            .map(|s| s.is_valid(budget, filter_fingerprint))
            .unwrap_or(false);

        if cache_valid {
            self.hits += 1;
            debug!(
                hits = self.hits,
                "skill selection cache hit"
            );
        } else {
            // Cache miss — recompute
            self.misses += 1;
            let result = SkillSelector::select(candidates, budget);
            debug!(
                misses = self.misses,
                selected = result.selected.len(),
                tokens = result.total_tokens,
                "skill selection cache miss — recomputed"
            );

            let mut snapshot = SkillSnapshot::new(result, budget);
            if let Some(fp) = filter_fingerprint {
                snapshot = snapshot.with_filter(fp);
            }
            self.snapshot = Some(snapshot);
        }

        &self.snapshot.as_ref().unwrap().result
    }

    /// Invalidate the cache (forces recomputation on next select).
    pub fn invalidate(&mut self) {
        self.snapshot = None;
    }

    /// Cache hit count since creation.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Cache miss count since creation.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Cache hit ratio (0.0 to 1.0, or NaN if no calls).
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return f64::NAN;
        }
        self.hits as f64 / total as f64
    }

    /// Current cached snapshot version (None if no cache).
    pub fn cached_version(&self) -> Option<u64> {
        self.snapshot.as_ref().map(|s| s.version)
    }
}

impl Default for CachedSelector {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute a simple fingerprint for a skill filter list.
///
/// This lets us detect when the filter changes without comparing vectors.
pub fn filter_fingerprint(filter: &[String]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for item in filter {
        item.hash(&mut hasher);
    }
    hasher.finish()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::*;

    fn make_skill(id: &str, priority: f64, prompt: &str) -> Arc<Skill> {
        Arc::new(Skill {
            manifest: SkillManifest {
                id: SkillId::from(id),
                display_name: id.to_string(),
                description: format!("Test skill: {}", id),
                version: "0.1.0".into(),
                author: None,
                dependencies: vec![],
                required_tools: vec![],
                parameters: vec![],
                triggers: vec![SkillTrigger::Always],
                estimated_tokens: 100,
                priority_weight: priority,
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
        })
    }

    #[test]
    fn version_bumps() {
        reset_version();
        let v0 = current_version();
        let v1 = bump_version();
        assert_eq!(v1, v0 + 1);
        let v2 = bump_version();
        assert_eq!(v2, v1 + 1);
    }

    #[test]
    fn snapshot_valid_at_current_version() {
        reset_version();
        bump_version();
        let skills = vec![make_skill("a", 1.0, "prompt")];
        let result = SkillSelector::select(&skills, 10000);
        let snap = SkillSnapshot::new(result, 10000);

        assert!(snap.is_valid(10000, None));
        assert!(snap.get(10000, None).is_some());
    }

    #[test]
    fn snapshot_invalid_after_bump() {
        reset_version();
        bump_version();
        let skills = vec![make_skill("a", 1.0, "prompt")];
        let result = SkillSelector::select(&skills, 10000);
        let snap = SkillSnapshot::new(result, 10000);

        // Bump version — snapshot should be invalid
        bump_version();
        assert!(!snap.is_valid(10000, None));
        assert!(snap.get(10000, None).is_none());
    }

    #[test]
    fn snapshot_invalid_with_different_budget() {
        reset_version();
        bump_version();
        let skills = vec![make_skill("a", 1.0, "prompt")];
        let result = SkillSelector::select(&skills, 10000);
        let snap = SkillSnapshot::new(result, 10000);

        // Same version but different budget
        assert!(!snap.is_valid(5000, None));
    }

    #[test]
    fn cached_selector_hits_and_misses() {
        reset_version();
        bump_version();
        let skills = vec![
            make_skill("a", 5.0, "prompt a"),
            make_skill("b", 3.0, "prompt b"),
        ];

        let mut sel = CachedSelector::new();

        // First call — cache miss
        let r1 = sel.select(&skills, 10000, None);
        assert_eq!(r1.selected.len(), 2);
        assert_eq!(sel.misses(), 1);
        assert_eq!(sel.hits(), 0);

        // Second call — cache hit (version hasn't changed)
        let r2 = sel.select(&skills, 10000, None);
        assert_eq!(r2.selected.len(), 2);
        assert_eq!(sel.hits(), 1);
        assert_eq!(sel.misses(), 1);

        // Third call — still cached
        let _ = sel.select(&skills, 10000, None);
        assert_eq!(sel.hits(), 2);
    }

    #[test]
    fn cached_selector_invalidates_on_version_bump() {
        reset_version();
        bump_version();
        let skills = vec![make_skill("a", 1.0, "prompt")];
        let mut sel = CachedSelector::new();

        let _ = sel.select(&skills, 10000, None);
        assert_eq!(sel.misses(), 1);

        // Bump → cache invalid
        bump_version();
        let _ = sel.select(&skills, 10000, None);
        assert_eq!(sel.misses(), 2);
    }

    #[test]
    fn cached_selector_invalidates_on_filter_change() {
        reset_version();
        bump_version();
        let skills = vec![make_skill("a", 1.0, "prompt")];
        let mut sel = CachedSelector::new();

        let fp1 = filter_fingerprint(&["a".to_string()]);
        let _ = sel.select(&skills, 10000, Some(fp1));
        assert_eq!(sel.misses(), 1);

        // Same filter → hit
        let _ = sel.select(&skills, 10000, Some(fp1));
        assert_eq!(sel.hits(), 1);

        // Different filter → miss
        let fp2 = filter_fingerprint(&["a".to_string(), "b".to_string()]);
        let _ = sel.select(&skills, 10000, Some(fp2));
        assert_eq!(sel.misses(), 2);
    }

    #[test]
    fn hit_ratio() {
        reset_version();
        bump_version();
        let skills = vec![make_skill("a", 1.0, "prompt")];
        let mut sel = CachedSelector::new();

        let _ = sel.select(&skills, 10000, None);  // miss
        let _ = sel.select(&skills, 10000, None);  // hit
        let _ = sel.select(&skills, 10000, None);  // hit
        let _ = sel.select(&skills, 10000, None);  // hit

        assert!((sel.hit_ratio() - 0.75).abs() < 0.01);
    }

    #[test]
    fn filter_fingerprint_stable() {
        let fp1 = filter_fingerprint(&["a".to_string(), "b".to_string()]);
        let fp2 = filter_fingerprint(&["a".to_string(), "b".to_string()]);
        assert_eq!(fp1, fp2);

        let fp3 = filter_fingerprint(&["b".to_string(), "a".to_string()]);
        // Order matters — different fingerprint
        assert_ne!(fp1, fp3);
    }

    #[test]
    fn manual_invalidation() {
        reset_version();
        bump_version();
        let skills = vec![make_skill("a", 1.0, "prompt")];
        let mut sel = CachedSelector::new();

        let _ = sel.select(&skills, 10000, None);
        assert!(sel.cached_version().is_some());

        sel.invalidate();
        assert!(sel.cached_version().is_none());

        let _ = sel.select(&skills, 10000, None);
        assert_eq!(sel.misses(), 2); // both were misses
    }
}
