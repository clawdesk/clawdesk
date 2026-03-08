//! Hierarchical Memory Tiering — episodic → semantic → procedural consolidation.
//!
//! Implements the three-tier memory hierarchy from the architecture analysis:
//!
//! 1. **Episodic** — Short-term, event-by-event memories (raw interactions).
//!    High detail, rapid decay. Automatically consolidated after cooling period.
//!
//! 2. **Semantic** — Medium-term, factual knowledge extracted from episodes.
//!    Moderate decay. Distilled from clusters of episodic memories.
//!
//! 3. **Procedural** — Long-term, skill/pattern memories.
//!    Slow decay. Emerges from repeated semantic patterns.
//!
//! ## Consolidation triggers
//!
//! - **Episodic → Semantic**: when an episodic memory's access count exceeds
//!   `semantic_threshold` or when its age exceeds `consolidation_age_days`.
//! - **Semantic → Procedural**: when a semantic cluster is accessed
//!   `procedural_threshold` times and spans ≥ `min_cluster_size` memories.
//!
//! ## Ebbinghaus forgetting curve
//!
//! Retention: `R(t) = e^{-t/S}`
//!
//! Where S (strength) increases with each review:
//! `S_{n+1} = S_n × (1 + c × S_n^{-0.05})`
//!
//! This replaces the simple half-life decay with a spaced-repetition–aware
//! forgetting curve.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ───────────────────────────────────────────────────────────────
// Memory tiers
// ───────────────────────────────────────────────────────────────

/// Which tier a memory entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryTier {
    Episodic,
    Semantic,
    Procedural,
}

impl MemoryTier {
    /// Default initial strength (S_0) for the Ebbinghaus model.
    pub fn initial_strength(&self) -> f64 {
        match self {
            MemoryTier::Episodic => 1.0,     // Rapid decay.
            MemoryTier::Semantic => 7.0,      // Moderate.
            MemoryTier::Procedural => 30.0,   // Slow decay.
        }
    }
}

/// A memory entry with tiered metadata and spaced-repetition tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TieredMemoryEntry {
    /// Unique identifier.
    pub id: String,
    /// Current tier.
    pub tier: MemoryTier,
    /// Content (text or compressed).
    pub content: String,
    /// Semantic embedding (for similarity search).
    pub embedding: Option<Vec<f32>>,
    /// When the memory was first created.
    pub created_at: DateTime<Utc>,
    /// When the memory was last accessed/reviewed.
    pub last_accessed: DateTime<Utc>,
    /// Total access count.
    pub access_count: u32,
    /// Ebbinghaus strength parameter S.
    pub strength: f64,
    /// Metadata tags (e.g., agent_id, topic).
    pub tags: HashMap<String, String>,
    /// Source memory IDs (for consolidated memories).
    pub source_ids: Vec<String>,
}

impl TieredMemoryEntry {
    /// Create a new episodic memory.
    pub fn new_episodic(id: impl Into<String>, content: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: id.into(),
            tier: MemoryTier::Episodic,
            content: content.into(),
            embedding: None,
            created_at: now,
            last_accessed: now,
            access_count: 0,
            strength: MemoryTier::Episodic.initial_strength(),
            tags: HashMap::new(),
            source_ids: Vec::new(),
        }
    }

    /// Compute the Ebbinghaus retention R(t) = e^{-t/S}.
    ///
    /// Returns a value in [0, 1] indicating how well this memory is retained.
    pub fn retention(&self, now: &DateTime<Utc>) -> f64 {
        let age_days = now
            .signed_duration_since(self.last_accessed)
            .num_seconds() as f64
            / 86400.0;
        if age_days <= 0.0 {
            return 1.0;
        }
        (-age_days / self.strength).exp()
    }

    /// Record an access (review), strengthening the memory.
    ///
    /// Updates strength: S_{n+1} = S_n × (1 + c × S_n^{-0.05})
    /// where c is the consolidation factor (default 0.21 from Ebbinghaus data).
    pub fn record_access(&mut self) {
        self.access_count += 1;
        self.last_accessed = Utc::now();
        self.strengthen(0.21);
    }

    /// Strengthen the memory with a given consolidation factor.
    fn strengthen(&mut self, c: f64) {
        self.strength *= 1.0 + c * self.strength.powf(-0.05);
    }

    /// Whether this episodic memory should be consolidated to semantic.
    pub fn should_consolidate_to_semantic(&self, config: &ConsolidationConfig) -> bool {
        if self.tier != MemoryTier::Episodic {
            return false;
        }
        let age_days = Utc::now()
            .signed_duration_since(self.created_at)
            .num_seconds() as f64
            / 86400.0;
        self.access_count >= config.semantic_threshold || age_days >= config.consolidation_age_days
    }
}

// ───────────────────────────────────────────────────────────────
// Consolidation configuration
// ───────────────────────────────────────────────────────────────

/// Configuration for memory consolidation triggers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationConfig {
    /// Access count to trigger episodic → semantic consolidation.
    pub semantic_threshold: u32,
    /// Age in days to trigger episodic → semantic consolidation.
    pub consolidation_age_days: f64,
    /// Access count on semantic memory to trigger → procedural.
    pub procedural_threshold: u32,
    /// Minimum number of source memories for procedural consolidation.
    pub min_cluster_size: usize,
    /// Retention threshold below which memories are garbage-collected.
    pub gc_retention_threshold: f64,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            semantic_threshold: 5,
            consolidation_age_days: 7.0,
            procedural_threshold: 15,
            min_cluster_size: 3,
            gc_retention_threshold: 0.01,
        }
    }
}

// ───────────────────────────────────────────────────────────────
// Hierarchical memory store
// ───────────────────────────────────────────────────────────────

/// Hierarchical memory store with three tiers and automatic consolidation.
pub struct HierarchicalMemory {
    /// All memories indexed by ID.
    entries: HashMap<String, TieredMemoryEntry>,
    /// Configuration.
    config: ConsolidationConfig,
}

impl HierarchicalMemory {
    pub fn new(config: ConsolidationConfig) -> Self {
        Self {
            entries: HashMap::new(),
            config,
        }
    }

    /// Insert a new episodic memory.
    pub fn insert_episodic(&mut self, entry: TieredMemoryEntry) {
        self.entries.insert(entry.id.clone(), entry);
    }

    /// Access a memory (increments count and strengthens it).
    pub fn access(&mut self, id: &str) -> Option<&TieredMemoryEntry> {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.record_access();
        }
        self.entries.get(id)
    }

    /// Get a memory without recording access.
    pub fn get(&self, id: &str) -> Option<&TieredMemoryEntry> {
        self.entries.get(id)
    }

    /// Query memories by tier, sorted by retention (highest first).
    pub fn query_tier(&self, tier: MemoryTier) -> Vec<&TieredMemoryEntry> {
        let now = Utc::now();
        let mut entries: Vec<_> = self
            .entries
            .values()
            .filter(|e| e.tier == tier)
            .collect();
        entries.sort_by(|a, b| {
            b.retention(&now)
                .partial_cmp(&a.retention(&now))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        entries
    }

    /// Run consolidation pass: promote eligible memories to higher tiers.
    ///
    /// Returns the IDs of memories that were promoted.
    pub fn consolidate(&mut self) -> Vec<(String, MemoryTier, MemoryTier)> {
        let mut promotions = Vec::new();

        // Collect episodic → semantic candidates.
        let semantic_candidates: Vec<String> = self
            .entries
            .values()
            .filter(|e| e.should_consolidate_to_semantic(&self.config))
            .map(|e| e.id.clone())
            .collect();

        for id in semantic_candidates {
            if let Some(entry) = self.entries.get_mut(&id) {
                let old_tier = entry.tier;
                entry.tier = MemoryTier::Semantic;
                entry.strength = MemoryTier::Semantic.initial_strength();
                promotions.push((id, old_tier, MemoryTier::Semantic));
            }
        }

        // Collect semantic → procedural candidates.
        let procedural_candidates: Vec<String> = self
            .entries
            .values()
            .filter(|e| {
                e.tier == MemoryTier::Semantic
                    && e.access_count >= self.config.procedural_threshold
                    && e.source_ids.len() >= self.config.min_cluster_size
            })
            .map(|e| e.id.clone())
            .collect();

        for id in procedural_candidates {
            if let Some(entry) = self.entries.get_mut(&id) {
                let old_tier = entry.tier;
                entry.tier = MemoryTier::Procedural;
                entry.strength = MemoryTier::Procedural.initial_strength();
                promotions.push((id, old_tier, MemoryTier::Procedural));
            }
        }

        promotions
    }

    /// Garbage-collect memories with retention below threshold.
    ///
    /// Returns the IDs of removed memories.
    pub fn garbage_collect(&mut self) -> Vec<String> {
        let now = Utc::now();
        let threshold = self.config.gc_retention_threshold;

        let to_remove: Vec<String> = self
            .entries
            .values()
            .filter(|e| e.retention(&now) < threshold)
            .map(|e| e.id.clone())
            .collect();

        for id in &to_remove {
            self.entries.remove(id);
        }

        to_remove
    }

    /// Get statistics per tier.
    pub fn tier_stats(&self) -> HashMap<MemoryTier, TierStats> {
        let now = Utc::now();
        let mut stats: HashMap<MemoryTier, TierStats> = HashMap::new();

        for entry in self.entries.values() {
            let s = stats.entry(entry.tier).or_insert_with(|| TierStats {
                count: 0,
                avg_retention: 0.0,
                avg_strength: 0.0,
                avg_access_count: 0.0,
            });
            s.count += 1;
            s.avg_retention += entry.retention(&now);
            s.avg_strength += entry.strength;
            s.avg_access_count += entry.access_count as f64;
        }

        for s in stats.values_mut() {
            if s.count > 0 {
                let n = s.count as f64;
                s.avg_retention /= n;
                s.avg_strength /= n;
                s.avg_access_count /= n;
            }
        }

        stats
    }

    /// Total number of memories.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Per-tier statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierStats {
    pub count: usize,
    pub avg_retention: f64,
    pub avg_strength: f64,
    pub avg_access_count: f64,
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ebbinghaus_retention_at_zero() {
        let entry = TieredMemoryEntry::new_episodic("m1", "test");
        let now = Utc::now();
        let r = entry.retention(&now);
        assert!((r - 1.0).abs() < 0.01, "retention at t=0 should be ~1.0");
    }

    #[test]
    fn test_strength_increases_on_access() {
        let mut entry = TieredMemoryEntry::new_episodic("m1", "test");
        let s0 = entry.strength;
        entry.record_access();
        assert!(entry.strength > s0, "strength should increase after access");
    }

    #[test]
    fn test_consolidation_by_access_count() {
        let config = ConsolidationConfig {
            semantic_threshold: 3,
            ..Default::default()
        };
        let mut store = HierarchicalMemory::new(config);

        let mut entry = TieredMemoryEntry::new_episodic("m1", "test memory");
        entry.access_count = 5; // Above threshold.
        store.insert_episodic(entry);

        let promotions = store.consolidate();
        assert_eq!(promotions.len(), 1);
        assert_eq!(promotions[0].2, MemoryTier::Semantic);

        let mem = store.get("m1").unwrap();
        assert_eq!(mem.tier, MemoryTier::Semantic);
    }

    #[test]
    fn test_procedural_promotion() {
        let config = ConsolidationConfig {
            procedural_threshold: 2,
            min_cluster_size: 2,
            ..Default::default()
        };
        let mut store = HierarchicalMemory::new(config);

        let mut entry = TieredMemoryEntry::new_episodic("m1", "merged knowledge");
        entry.tier = MemoryTier::Semantic;
        entry.access_count = 5;
        entry.source_ids = vec!["src1".into(), "src2".into(), "src3".into()];
        store.insert_episodic(entry);

        let promotions = store.consolidate();
        assert_eq!(promotions.len(), 1);
        assert_eq!(promotions[0].2, MemoryTier::Procedural);
    }

    #[test]
    fn test_tier_stats() {
        let mut store = HierarchicalMemory::new(ConsolidationConfig::default());
        store.insert_episodic(TieredMemoryEntry::new_episodic("e1", "episodic 1"));
        store.insert_episodic(TieredMemoryEntry::new_episodic("e2", "episodic 2"));

        let mut semantic = TieredMemoryEntry::new_episodic("s1", "semantic 1");
        semantic.tier = MemoryTier::Semantic;
        store.insert_episodic(semantic);

        let stats = store.tier_stats();
        assert_eq!(stats.get(&MemoryTier::Episodic).unwrap().count, 2);
        assert_eq!(stats.get(&MemoryTier::Semantic).unwrap().count, 1);
    }
}
