//! CRDT (Conflict-free Replicated Data Types) for multi-agent shared state.
//!
//! Implements the four CRDTs recommended in the architecture analysis:
//!
//! 1. **G-Counter** — Grow-only counter (one slot per agent, merge = max).
//! 2. **LWW-Register** — Last-Writer-Wins register (timestamp comparison).
//! 3. **OR-Set** — Observed-Remove Set (unique tags, add wins over remove).
//! 4. **RGA** — Replicated Growable Array (for ordered sequences).
//!
//! ## Semilattice property
//!
//! All CRDTs satisfy the semilattice axioms:
//! - **Commutativity**: merge(a, b) = merge(b, a)
//! - **Associativity**: merge(merge(a, b), c) = merge(a, merge(b, c))  
//! - **Idempotency**: merge(a, a) = a
//!
//! ## Complexity
//!
//! - Space: O(n × k) where n = agents, k = distinct keys/elements.
//! - Merge: O(k) per operation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

// ───────────────────────────────────────────────────────────────
// Trait: Lattice merge
// ───────────────────────────────────────────────────────────────

/// A join-semilattice with an idempotent, commutative, associative merge.
pub trait Lattice: Clone {
    /// Merge another replica's state into this one.
    /// After merge, `self` contains the least upper bound of both.
    fn merge(&mut self, other: &Self);
}

// ───────────────────────────────────────────────────────────────
// 1. G-Counter (Grow-only Counter)
// ───────────────────────────────────────────────────────────────

/// Grow-only counter — each agent has its own monotonically increasing slot.
///
/// ```text
/// value = Σ_i counts[i]
/// merge(a, b).counts[i] = max(a.counts[i], b.counts[i])
/// ```
///
/// Space: O(n) where n = number of agents that have incremented.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GCounter {
    /// Per-agent counts. Key = agent_id, Value = local count.
    counts: BTreeMap<String, u64>,
}

impl GCounter {
    pub fn new() -> Self {
        Self {
            counts: BTreeMap::new(),
        }
    }

    /// Increment the counter for `agent_id`.
    pub fn increment(&mut self, agent_id: &str) {
        *self.counts.entry(agent_id.to_string()).or_insert(0) += 1;
    }

    /// Increment by a specific amount.
    pub fn increment_by(&mut self, agent_id: &str, amount: u64) {
        *self.counts.entry(agent_id.to_string()).or_insert(0) += amount;
    }

    /// Get the total count across all agents.
    pub fn value(&self) -> u64 {
        self.counts.values().sum()
    }

    /// Get the count for a specific agent.
    pub fn agent_count(&self, agent_id: &str) -> u64 {
        self.counts.get(agent_id).copied().unwrap_or(0)
    }
}

impl Default for GCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl Lattice for GCounter {
    fn merge(&mut self, other: &Self) {
        for (agent, &count) in &other.counts {
            let entry = self.counts.entry(agent.clone()).or_insert(0);
            *entry = (*entry).max(count);
        }
    }
}

// ───────────────────────────────────────────────────────────────
// 2. LWW-Register (Last-Writer-Wins Register)
// ───────────────────────────────────────────────────────────────

/// Last-Writer-Wins register — stores a single value with a timestamp.
///
/// On merge, the value with the higher timestamp wins. Ties are broken by
/// comparing agent IDs lexicographically (deterministic total order).
///
/// ```text
/// merge(a, b) = if a.ts > b.ts then a
///               elif b.ts > a.ts then b
///               else max_by_agent(a, b)
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LwwRegister<T: Clone + Serialize> {
    /// The current value.
    pub value: T,
    /// Timestamp of the last write.
    pub timestamp: DateTime<Utc>,
    /// Agent that performed the last write (for tie-breaking).
    pub writer: String,
}

impl<T: Clone + Serialize + for<'de> Deserialize<'de>> LwwRegister<T> {
    pub fn new(value: T, writer: impl Into<String>) -> Self {
        Self {
            value,
            timestamp: Utc::now(),
            writer: writer.into(),
        }
    }

    /// Set a new value with the current timestamp.
    pub fn set(&mut self, value: T, writer: impl Into<String>) {
        self.value = value;
        self.timestamp = Utc::now();
        self.writer = writer.into();
    }
}

impl<T: Clone + Serialize + for<'de> Deserialize<'de>> Lattice for LwwRegister<T> {
    fn merge(&mut self, other: &Self) {
        if other.timestamp > self.timestamp
            || (other.timestamp == self.timestamp && other.writer > self.writer)
        {
            self.value = other.value.clone();
            self.timestamp = other.timestamp;
            self.writer = other.writer.clone();
        }
    }
}

// ───────────────────────────────────────────────────────────────
// 3. OR-Set (Observed-Remove Set)
// ───────────────────────────────────────────────────────────────

/// Observed-Remove Set — concurrent add/remove with add-wins semantics.
///
/// Each element addition is tagged with a unique ID. Removals record the
/// set of tags they observed. An element is present iff it has at least
/// one tag not in the removed set.
///
/// ```text
/// elements(s) = { e | ∃ tag ∈ s.adds[e] : tag ∉ s.removes }
/// ```
///
/// Space: O(k × t) where k = distinct elements, t = total add operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrSet<T: Clone + Eq + std::hash::Hash + Serialize> {
    /// Element → set of active tags.
    adds: HashMap<T, HashSet<String>>,
    /// Set of removed tags (tombstones).
    removes: HashSet<String>,
    /// Monotonic counter for generating unique tags.
    tag_counter: u64,
    /// Agent that owns this replica (used for tag generation).
    node_id: String,
}

impl<T: Clone + Eq + std::hash::Hash + Serialize + for<'de> Deserialize<'de>> OrSet<T> {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            adds: HashMap::new(),
            removes: HashSet::new(),
            tag_counter: 0,
            node_id: node_id.into(),
        }
    }

    /// Add an element. Returns the unique tag.
    pub fn add(&mut self, element: T) -> String {
        self.tag_counter += 1;
        let tag = format!("{}:{}", self.node_id, self.tag_counter);
        self.adds
            .entry(element)
            .or_insert_with(HashSet::new)
            .insert(tag.clone());
        tag
    }

    /// Remove an element (observes all its current tags).
    pub fn remove(&mut self, element: &T) {
        if let Some(tags) = self.adds.get(element) {
            self.removes.extend(tags.iter().cloned());
        }
    }

    /// Check if an element is present.
    pub fn contains(&self, element: &T) -> bool {
        if let Some(tags) = self.adds.get(element) {
            tags.iter().any(|tag| !self.removes.contains(tag))
        } else {
            false
        }
    }

    /// Get all present elements.
    pub fn elements(&self) -> Vec<&T> {
        self.adds
            .iter()
            .filter(|(_, tags)| tags.iter().any(|tag| !self.removes.contains(tag)))
            .map(|(elem, _)| elem)
            .collect()
    }

    /// Number of present elements.
    pub fn len(&self) -> usize {
        self.elements().len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: Clone + Eq + std::hash::Hash + Serialize + for<'de> Deserialize<'de>> Lattice for OrSet<T> {
    fn merge(&mut self, other: &Self) {
        // Merge adds.
        for (elem, tags) in &other.adds {
            let entry = self.adds.entry(elem.clone()).or_insert_with(HashSet::new);
            entry.extend(tags.iter().cloned());
        }
        // Merge removes (union of tombstones).
        self.removes.extend(other.removes.iter().cloned());
    }
}

// ───────────────────────────────────────────────────────────────
// 4. RGA (Replicated Growable Array)
// ───────────────────────────────────────────────────────────────

/// Unique identifier for an RGA element.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RgaId {
    /// Logical timestamp (Lamport clock).
    pub timestamp: u64,
    /// Node (agent) ID for tie-breaking.
    pub node_id: String,
}

/// An element in the RGA.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RgaElement<T: Clone> {
    pub id: RgaId,
    pub value: T,
    /// Whether this element has been tombstoned (deleted).
    pub deleted: bool,
    /// ID of the element this was inserted after (None = head).
    pub after: Option<RgaId>,
}

/// Replicated Growable Array (RGA) — ordered sequence CRDT.
///
/// Supports concurrent insert-after and delete operations. Uses Lamport
/// timestamps for ordering: concurrent inserts after the same element
/// are ordered by (timestamp DESC, node_id DESC) — later timestamps
/// appear first.
///
/// The array is stored as a flat Vec with tombstones for deletions.
/// This trades space for simplicity — production implementations would
/// use a linked structure for O(log n) insertion.
///
/// Space: O(total_inserts) including tombstones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rga<T: Clone> {
    elements: Vec<RgaElement<T>>,
    /// Lamport clock for this node.
    clock: u64,
    /// Node (agent) ID.
    node_id: String,
}

impl<T: Clone + Serialize + for<'de> Deserialize<'de>> Rga<T> {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            elements: Vec::new(),
            clock: 0,
            node_id: node_id.into(),
        }
    }

    /// Insert a value after the element with `after_id`. Pass `None` to insert at head.
    pub fn insert_after(&mut self, after_id: Option<&RgaId>, value: T) -> RgaId {
        self.clock += 1;
        let id = RgaId {
            timestamp: self.clock,
            node_id: self.node_id.clone(),
        };
        let element = RgaElement {
            id: id.clone(),
            value,
            deleted: false,
            after: after_id.cloned(),
        };

        // Find insertion position: after the `after_id` element, but before any
        // element with lower timestamp (or same timestamp but lower node_id).
        let insert_pos = if let Some(aid) = after_id {
            let after_pos = self
                .elements
                .iter()
                .position(|e| e.id == *aid)
                .map(|p| p + 1)
                .unwrap_or(self.elements.len());
            // Skip past any concurrent inserts with higher priority.
            let mut pos = after_pos;
            while pos < self.elements.len() {
                let existing = &self.elements[pos];
                if existing.after.as_ref() != Some(aid) {
                    break;
                }
                // Higher timestamp = higher priority = comes first.
                if existing.id.timestamp < id.timestamp
                    || (existing.id.timestamp == id.timestamp
                        && existing.id.node_id < id.node_id)
                {
                    break;
                }
                pos += 1;
            }
            pos
        } else {
            // Insert at head — before all existing head elements with lower priority.
            let mut pos = 0;
            while pos < self.elements.len() {
                let existing = &self.elements[pos];
                if existing.after.is_some() {
                    break;
                }
                if existing.id.timestamp < id.timestamp
                    || (existing.id.timestamp == id.timestamp
                        && existing.id.node_id < id.node_id)
                {
                    break;
                }
                pos += 1;
            }
            pos
        };

        self.elements.insert(insert_pos, element);
        id
    }

    /// Delete the element with the given ID (tombstone).
    pub fn delete(&mut self, id: &RgaId) -> bool {
        for elem in &mut self.elements {
            if elem.id == *id && !elem.deleted {
                elem.deleted = true;
                return true;
            }
        }
        false
    }

    /// Get the visible (non-deleted) elements in order.
    pub fn to_vec(&self) -> Vec<&T> {
        self.elements
            .iter()
            .filter(|e| !e.deleted)
            .map(|e| &e.value)
            .collect()
    }

    /// Number of visible elements.
    pub fn len(&self) -> usize {
        self.elements.iter().filter(|e| !e.deleted).count()
    }

    /// Whether the array has no visible elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Update the Lamport clock after observing a remote timestamp.
    pub fn observe_timestamp(&mut self, remote_ts: u64) {
        self.clock = self.clock.max(remote_ts) + 1;
    }

    /// Apply a remote insert operation.
    pub fn apply_insert(&mut self, element: RgaElement<T>) {
        self.observe_timestamp(element.id.timestamp);

        // Find correct position.
        let insert_pos = if let Some(ref aid) = element.after {
            let after_pos = self
                .elements
                .iter()
                .position(|e| e.id == *aid)
                .map(|p| p + 1)
                .unwrap_or(self.elements.len());
            let mut pos = after_pos;
            while pos < self.elements.len() {
                let existing = &self.elements[pos];
                if existing.after.as_ref() != Some(aid) {
                    break;
                }
                if existing.id < element.id {
                    break;
                }
                pos += 1;
            }
            pos
        } else {
            let mut pos = 0;
            while pos < self.elements.len() {
                let existing = &self.elements[pos];
                if existing.after.is_some() {
                    break;
                }
                if existing.id < element.id {
                    break;
                }
                pos += 1;
            }
            pos
        };

        self.elements.insert(insert_pos, element);
    }

    /// Apply a remote delete operation.
    pub fn apply_delete(&mut self, id: &RgaId) {
        self.delete(id);
    }
}

// ───────────────────────────────────────────────────────────────
// PN-Counter (bonus: used internally for bidirectional counting)
// ───────────────────────────────────────────────────────────────

/// PN-Counter — increment and decrement counter built from two G-Counters.
///
/// ```text
/// value = P.value() - N.value()
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PnCounter {
    /// Positive (increment) counter.
    p: GCounter,
    /// Negative (decrement) counter.
    n: GCounter,
}

impl PnCounter {
    pub fn new() -> Self {
        Self {
            p: GCounter::new(),
            n: GCounter::new(),
        }
    }

    pub fn increment(&mut self, agent_id: &str) {
        self.p.increment(agent_id);
    }

    pub fn decrement(&mut self, agent_id: &str) {
        self.n.increment(agent_id);
    }

    pub fn value(&self) -> i64 {
        self.p.value() as i64 - self.n.value() as i64
    }
}

impl Default for PnCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl Lattice for PnCounter {
    fn merge(&mut self, other: &Self) {
        self.p.merge(&other.p);
        self.n.merge(&other.n);
    }
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gcounter_merge_commutativity() {
        let mut a = GCounter::new();
        a.increment("agent1");
        a.increment("agent1");

        let mut b = GCounter::new();
        b.increment("agent2");

        let mut ab = a.clone();
        ab.merge(&b);

        let mut ba = b.clone();
        ba.merge(&a);

        assert_eq!(ab.value(), ba.value());
        assert_eq!(ab.value(), 3);
    }

    #[test]
    fn test_gcounter_merge_idempotency() {
        let mut a = GCounter::new();
        a.increment("agent1");
        a.increment_by("agent2", 5);

        let before = a.value();
        a.merge(&a.clone());
        assert_eq!(a.value(), before, "merge with self should be no-op");
    }

    #[test]
    fn test_lww_register_latest_wins() {
        let mut r1 = LwwRegister::new(10i32, "agent1");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let r2 = LwwRegister::new(20i32, "agent2");

        r1.merge(&r2);
        assert_eq!(r1.value, 20, "later timestamp should win");
    }

    #[test]
    fn test_orset_add_wins() {
        let mut set1: OrSet<String> = OrSet::new("node1");
        let mut set2: OrSet<String> = OrSet::new("node2");

        set1.add("hello".into());

        // Sync set1 → set2.
        set2.merge(&set1);
        assert!(set2.contains(&"hello".to_string()));

        // set2 removes "hello", but set1 concurrently re-adds it.
        set2.remove(&"hello".to_string());
        set1.add("hello".into()); // New tag not in set2's removes.

        // Merge: add-wins — the new tag from set1 survives.
        set2.merge(&set1);
        assert!(
            set2.contains(&"hello".to_string()),
            "add should win over concurrent remove"
        );
    }

    #[test]
    fn test_orset_remove_then_add() {
        let mut set: OrSet<String> = OrSet::new("node1");
        set.add("item".into());
        assert!(set.contains(&"item".to_string()));

        set.remove(&"item".to_string());
        assert!(!set.contains(&"item".to_string()));

        set.add("item".into()); // Re-add with new tag.
        assert!(set.contains(&"item".to_string()));
    }

    #[test]
    fn test_rga_ordered_insert() {
        let mut rga: Rga<String> = Rga::new("node1");
        let id1 = rga.insert_after(None, "hello".to_string());
        let _id2 = rga.insert_after(Some(&id1), "world".to_string());

        assert_eq!(rga.to_vec(), vec![&"hello".to_string(), &"world".to_string()]);
    }

    #[test]
    fn test_rga_delete_tombstone() {
        let mut rga: Rga<String> = Rga::new("node1");
        let id1 = rga.insert_after(None, "a".to_string());
        let id2 = rga.insert_after(Some(&id1), "b".to_string());
        let _id3 = rga.insert_after(Some(&id2), "c".to_string());

        rga.delete(&id2);
        assert_eq!(rga.to_vec(), vec![&"a".to_string(), &"c".to_string()]);
        assert_eq!(rga.len(), 2);
    }

    #[test]
    fn test_pn_counter() {
        let mut a = PnCounter::new();
        a.increment("agent1");
        a.increment("agent1");
        a.decrement("agent2");

        assert_eq!(a.value(), 1); // 2 - 1

        let mut b = PnCounter::new();
        b.increment("agent3");
        b.decrement("agent1");

        a.merge(&b);
        assert_eq!(a.value(), 1); // (2+1) - (1+1) = 3-2 = 1
    }
}
