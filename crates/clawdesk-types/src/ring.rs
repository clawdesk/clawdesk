//! A bounded ring buffer that drops the oldest entries on overflow.
//!
//! # Motivation
//!
//! The codebase has 7+ independent implementations of "push to a bounded
//! buffer, evict the oldest when full". Some use `VecDeque` (correct, O(1)
//! eviction) while others use `Vec::remove(0)` (O(N) shift). This type
//! provides a single, well-tested abstraction that:
//!
//! - Guarantees **O(1)** `push` (amortized), `pop_front`, `back`, and `front`
//! - Enforces a **compile-time-decided capacity** (set at construction)
//! - Drops the oldest entry when capacity is reached ("drop-oldest" policy)
//! - Is `Clone`, `Debug`, `Serialize`, `Deserialize` (when `T` is)
//! - Supports efficient iteration, `Drain`, `Index`, and `Extend`
//!
//! # Examples
//!
//! ```
//! use clawdesk_types::ring::DropOldest;
//!
//! let mut buf = DropOldest::new(3);
//! buf.push(10);
//! buf.push(20);
//! buf.push(30);
//! assert_eq!(buf.len(), 3);
//!
//! // Next push drops the oldest (10)
//! buf.push(40);
//! assert_eq!(buf.len(), 3);
//! assert_eq!(buf[0], 20);
//! assert_eq!(buf[2], 40);
//! ```

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::VecDeque;
use std::fmt;
use std::ops::Index;

/// A bounded FIFO buffer that drops the oldest entry when capacity is reached.
///
/// Backed by a [`VecDeque<T>`], so all operations are O(1) amortized.
#[derive(Clone)]
pub struct DropOldest<T> {
    buf: VecDeque<T>,
    cap: usize,
}

/// Serialization helper — stores capacity alongside items.
#[derive(Serialize, Deserialize)]
struct DropOldestSerde<T> {
    capacity: usize,
    items: Vec<T>,
}

impl<T: Serialize> Serialize for DropOldest<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let helper = DropOldestSerde {
            capacity: self.cap,
            items: self.buf.iter().collect::<Vec<_>>(),
        };
        helper.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for DropOldest<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let helper = DropOldestSerde::<T>::deserialize(deserializer)?;
        let mut ring = Self::new(helper.capacity.max(1));
        for item in helper.items {
            ring.push(item);
        }
        Ok(ring)
    }
}

impl<T: fmt::Debug> fmt::Debug for DropOldest<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DropOldest")
            .field("capacity", &self.cap)
            .field("len", &self.buf.len())
            .field("items", &self.buf)
            .finish()
    }
}

impl<T> DropOldest<T> {
    /// Create an empty ring buffer with the given maximum capacity.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is 0.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "DropOldest capacity must be > 0");
        Self {
            buf: VecDeque::with_capacity(capacity),
            cap: capacity,
        }
    }

    /// Push an item, dropping the oldest if at capacity.
    ///
    /// Returns the evicted item (if any).
    pub fn push(&mut self, item: T) -> Option<T> {
        let evicted = if self.buf.len() >= self.cap {
            self.buf.pop_front()
        } else {
            None
        };
        self.buf.push_back(item);
        evicted
    }

    /// Remove and return the oldest item.
    pub fn pop_front(&mut self) -> Option<T> {
        self.buf.pop_front()
    }

    /// Reference to the oldest item.
    pub fn front(&self) -> Option<&T> {
        self.buf.front()
    }

    /// Reference to the newest item.
    pub fn back(&self) -> Option<&T> {
        self.buf.back()
    }

    /// Current number of items.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Whether the buffer is at capacity.
    pub fn is_full(&self) -> bool {
        self.buf.len() >= self.cap
    }

    /// Maximum capacity.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Iterate over items from oldest to newest.
    pub fn iter(&self) -> std::collections::vec_deque::Iter<'_, T> {
        self.buf.iter()
    }

    /// Mutable iterate over items from oldest to newest.
    pub fn iter_mut(&mut self) -> std::collections::vec_deque::IterMut<'_, T> {
        self.buf.iter_mut()
    }

    /// Drain all items from oldest to newest.
    pub fn drain(&mut self) -> std::collections::vec_deque::Drain<'_, T> {
        self.buf.drain(..)
    }

    /// Remove all items.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Get a contiguous slice pair (VecDeque may be split internally).
    ///
    /// Returns `(front_half, back_half)` where concatenation gives
    /// oldest-to-newest ordering.
    pub fn as_slices(&self) -> (&[T], &[T]) {
        self.buf.as_slices()
    }

    /// Collect contiguous items into a `Vec`.
    pub fn to_vec(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.buf.iter().cloned().collect()
    }

    /// Iterate over the most recent `n` items (newest last).
    pub fn tail(&self, n: usize) -> impl Iterator<Item = &T> {
        let skip = self.buf.len().saturating_sub(n);
        self.buf.iter().skip(skip)
    }

    /// Retain only elements for which the predicate returns `true`.
    pub fn retain<F: FnMut(&T) -> bool>(&mut self, f: F) {
        self.buf.retain(f);
    }

    /// Access the underlying `VecDeque` (read-only).
    ///
    /// Useful for callers that need `VecDeque`-specific APIs like
    /// `make_contiguous()` or `binary_search()`.
    pub fn as_deque(&self) -> &VecDeque<T> {
        &self.buf
    }
}

impl<T> Index<usize> for DropOldest<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        &self.buf[index]
    }
}

impl<T> Extend<T> for DropOldest<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for item in iter {
            self.push(item);
        }
    }
}

impl<T> IntoIterator for DropOldest<T> {
    type Item = T;
    type IntoIter = std::collections::vec_deque::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.buf.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a DropOldest<T> {
    type Item = &'a T;
    type IntoIter = std::collections::vec_deque::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.buf.iter()
    }
}

// ═══════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_push_and_len() {
        let mut buf = DropOldest::new(3);
        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());

        buf.push(1);
        buf.push(2);
        buf.push(3);
        assert_eq!(buf.len(), 3);
        assert!(buf.is_full());
        assert!(!buf.is_empty());
    }

    #[test]
    fn drops_oldest_on_overflow() {
        let mut buf = DropOldest::new(3);
        buf.push(10);
        buf.push(20);
        buf.push(30);

        let evicted = buf.push(40);
        assert_eq!(evicted, Some(10));
        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 20);
        assert_eq!(buf[1], 30);
        assert_eq!(buf[2], 40);
    }

    #[test]
    fn multiple_evictions() {
        let mut buf = DropOldest::new(2);
        for i in 0..100 {
            buf.push(i);
        }
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0], 98);
        assert_eq!(buf[1], 99);
    }

    #[test]
    fn front_and_back() {
        let mut buf = DropOldest::new(5);
        assert!(buf.front().is_none());
        assert!(buf.back().is_none());

        buf.push("a");
        buf.push("b");
        buf.push("c");
        assert_eq!(buf.front(), Some(&"a"));
        assert_eq!(buf.back(), Some(&"c"));
    }

    #[test]
    fn pop_front() {
        let mut buf = DropOldest::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);

        assert_eq!(buf.pop_front(), Some(1));
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0], 2);
    }

    #[test]
    fn iter_is_oldest_to_newest() {
        let mut buf = DropOldest::new(3);
        buf.push(10);
        buf.push(20);
        buf.push(30);
        buf.push(40); // evicts 10

        let items: Vec<_> = buf.iter().copied().collect();
        assert_eq!(items, vec![20, 30, 40]);
    }

    #[test]
    fn tail_n() {
        let mut buf = DropOldest::new(10);
        for i in 0..10 {
            buf.push(i);
        }
        let last3: Vec<_> = buf.tail(3).copied().collect();
        assert_eq!(last3, vec![7, 8, 9]);

        // tail(0) returns nothing
        assert_eq!(buf.tail(0).count(), 0);

        // tail(100) clamps to all items
        assert_eq!(buf.tail(100).count(), 10);
    }

    #[test]
    fn extend_drops_oldest() {
        let mut buf = DropOldest::new(3);
        buf.extend(0..10);
        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 7);
        assert_eq!(buf[1], 8);
        assert_eq!(buf[2], 9);
    }

    #[test]
    fn to_vec_and_into_iter() {
        let mut buf = DropOldest::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);

        assert_eq!(buf.to_vec(), vec![1, 2, 3]);

        let collected: Vec<_> = buf.into_iter().collect();
        assert_eq!(collected, vec![1, 2, 3]);
    }

    #[test]
    fn clear_and_drain() {
        let mut buf = DropOldest::new(5);
        buf.extend(0..5);

        let drained: Vec<_> = buf.drain().collect();
        assert_eq!(drained, vec![0, 1, 2, 3, 4]);
        assert!(buf.is_empty());

        buf.extend(10..15);
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.capacity(), 5);
    }

    #[test]
    fn retain() {
        let mut buf = DropOldest::new(10);
        buf.extend(0..10);
        buf.retain(|x| x % 2 == 0);
        assert_eq!(buf.to_vec(), vec![0, 2, 4, 6, 8]);
    }

    #[test]
    fn index_access() {
        let mut buf = DropOldest::new(3);
        buf.push("x");
        buf.push("y");
        buf.push("z");
        assert_eq!(buf[0], "x");
        assert_eq!(buf[2], "z");
    }

    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn zero_capacity_panics() {
        let _: DropOldest<i32> = DropOldest::new(0);
    }

    #[test]
    fn capacity_one() {
        let mut buf = DropOldest::new(1);
        buf.push(1);
        buf.push(2);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], 2);
    }

    #[test]
    fn serde_roundtrip() {
        let mut buf = DropOldest::new(3);
        buf.push(10);
        buf.push(20);
        buf.push(30);

        let json = serde_json::to_string(&buf).unwrap();
        let restored: DropOldest<i32> = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.capacity(), 3);
        assert_eq!(restored.to_vec(), vec![10, 20, 30]);
    }
}
