//! Weighted Fair Queuing priority dispatcher.
//!
//! Implements WFQ across priority classes using virtual finish times.
//! Virtual finish time for event i in class k:
//!   F_i = max(F_{i-1}, arrival_i) + size_i / w_k
//!
//! O(log K) dispatch per event where K = number of priority classes (≤4).

use crate::event::Priority;
use std::cmp::Ordering as CmpOrd;
use std::collections::BinaryHeap;

/// A priority-tagged item awaiting dispatch.
#[derive(Debug, Clone)]
pub struct PriorityItem<T> {
    /// The item payload
    pub item: T,
    /// Virtual finish time (lower = should be dispatched sooner)
    pub virtual_finish_time: f64,
    /// Priority class
    pub priority: Priority,
}

impl<T> PartialEq for PriorityItem<T> {
    fn eq(&self, other: &Self) -> bool {
        self.virtual_finish_time == other.virtual_finish_time
    }
}

impl<T> Eq for PriorityItem<T> {}

impl<T> PartialOrd for PriorityItem<T> {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrd> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for PriorityItem<T> {
    fn cmp(&self, other: &Self) -> CmpOrd {
        // Min-heap: reverse ordering so smallest VFT comes first
        other
            .virtual_finish_time
            .partial_cmp(&self.virtual_finish_time)
            .unwrap_or(CmpOrd::Equal)
    }
}

/// Weighted Fair Queuing scheduler.
///
/// Maintains per-class virtual finish times and dispatches items
/// in weighted-fair order. Lower virtual finish time = dispatched first.
///
/// Priority weights: Urgent=8, Standard=4, Batch=1.
pub struct WfqScheduler<T> {
    heap: BinaryHeap<PriorityItem<T>>,
    /// Last virtual finish time per priority class
    last_vft: [f64; 3],
}

impl<T> WfqScheduler<T> {
    /// Create a new empty scheduler.
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            last_vft: [0.0; 3],
        }
    }

    /// Enqueue an item with a given priority.
    ///
    /// Virtual finish time: F_i = max(F_{i-1}, now) + 1/weight
    pub fn enqueue(&mut self, item: T, priority: Priority, arrival_time: f64) {
        let class = priority as usize;
        let weight = priority.weight() as f64;
        let prev_vft = self.last_vft[class];
        let vft = f64::max(prev_vft, arrival_time) + 1.0 / weight;
        self.last_vft[class] = vft;

        self.heap.push(PriorityItem {
            item,
            virtual_finish_time: vft,
            priority,
        });
    }

    /// Dequeue the next item to dispatch (lowest virtual finish time).
    pub fn dequeue(&mut self) -> Option<PriorityItem<T>> {
        self.heap.pop()
    }

    /// Peek at the virtual finish time of the next item without removing it.
    ///
    /// O(1) — used by `ShardedWfqScheduler` for K-way merge selection.
    pub fn peek_vft(&self) -> Option<f64> {
        self.heap.peek().map(|pi| pi.virtual_finish_time)
    }

    /// Number of pending items.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether the scheduler is empty.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

impl<T> Default for WfqScheduler<T> {
    fn default() -> Self {
        Self::new()
    }
}

// ── Sharded WFQ Scheduler ──────────────────────────────────────────

/// K-sharded WFQ scheduler — one independent heap per priority class.
///
/// Each of the K=3 priority classes (Urgent, Standard, Batch) has its own
/// `WfqScheduler` behind an independent `tokio::sync::Mutex`, eliminating
/// cross-class lock contention on the hot `enqueue()` path (publish).
///
/// Enqueue contention drops from O(N) to O(N/K) for evenly-distributed
/// priorities, since concurrent publishes with different priorities never
/// contend on the same mutex.
///
/// Dequeue locks all K=3 shards and performs a K-way merge by virtual
/// finish time, preserving the global WFQ ordering invariant.
pub struct ShardedWfqScheduler<T> {
    shards: [tokio::sync::Mutex<WfqScheduler<T>>; 3],
}

impl<T> ShardedWfqScheduler<T> {
    /// Create a new sharded scheduler with K=3 empty shards.
    pub fn new() -> Self {
        Self {
            shards: [
                tokio::sync::Mutex::new(WfqScheduler::new()),
                tokio::sync::Mutex::new(WfqScheduler::new()),
                tokio::sync::Mutex::new(WfqScheduler::new()),
            ],
        }
    }

    /// Enqueue an item — only locks the shard for the item's priority class.
    ///
    /// Contention is limited to other events of the *same* priority class,
    /// so Urgent publishes never block behind Batch enqueues.
    pub async fn enqueue(&self, item: T, priority: Priority, arrival_time: f64) {
        let class = priority as usize;
        self.shards[class]
            .lock()
            .await
            .enqueue(item, priority, arrival_time);
    }

    /// Drain up to `max` items in global VFT order across all shards.
    ///
    /// Locks all K=3 shards, then performs a K-way merge by peeking at the
    /// min-VFT item in each shard and popping from the overall minimum.
    /// Total cost: O(max × K) with K=3, i.e., effectively O(max).
    pub async fn drain(&self, max: usize) -> Vec<PriorityItem<T>> {
        let mut s0 = self.shards[0].lock().await;
        let mut s1 = self.shards[1].lock().await;
        let mut s2 = self.shards[2].lock().await;

        let total = s0.len() + s1.len() + s2.len();
        let mut items = Vec::with_capacity(max.min(total));

        for _ in 0..max {
            let vfts = [s0.peek_vft(), s1.peek_vft(), s2.peek_vft()];

            // K-way merge: find the shard with the minimum VFT
            let best = vfts
                .iter()
                .enumerate()
                .filter_map(|(i, v)| v.map(|vft| (i, vft)))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            match best {
                Some((0, _)) => items.push(s0.dequeue().unwrap()),
                Some((1, _)) => items.push(s1.dequeue().unwrap()),
                Some((2, _)) => items.push(s2.dequeue().unwrap()),
                _ => break,
            }
        }

        items
    }

    /// Total number of pending items across all shards.
    pub async fn len(&self) -> usize {
        let (s0, s1, s2) = tokio::join!(
            self.shards[0].lock(),
            self.shards[1].lock(),
            self.shards[2].lock()
        );
        s0.len() + s1.len() + s2.len()
    }

    /// Whether all shards are empty.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

impl<T> Default for ShardedWfqScheduler<T> {
    fn default() -> Self {
        Self::new()
    }
}
