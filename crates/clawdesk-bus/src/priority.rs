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
