//! Memory generation counter for cache invalidation.
//!
//! Provides a monotonically increasing generation counter that is
//! bumped every time a memory write occurs. Consumers (semantic cache, BM25
//! index, prompt builder) can check the generation to know if their cached
//! state is stale.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let gen = MemoryGeneration::new();
//!
//! // On memory write:
//! gen.bump();
//!
//! // On cache check:
//! if gen.get() != cached_generation {
//!     // Cached data is stale — refresh
//! }
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

/// An atomic generation counter for memory invalidation.
///
/// Thread-safe and lock-free. Every `bump()` is a single atomic increment.
#[derive(Debug)]
pub struct MemoryGeneration {
    counter: AtomicU64,
}

impl MemoryGeneration {
    /// Create a new generation counter starting at 0.
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }

    /// Create with a specific starting generation (e.g., loaded from disk).
    pub fn with_initial(value: u64) -> Self {
        Self {
            counter: AtomicU64::new(value),
        }
    }

    /// Get the current generation.
    pub fn get(&self) -> u64 {
        self.counter.load(Ordering::SeqCst)
    }

    /// Bump the generation counter. Returns the new generation value.
    pub fn bump(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Check if a cached generation is stale.
    pub fn is_stale(&self, cached_generation: u64) -> bool {
        self.get() != cached_generation
    }
}

impl Default for MemoryGeneration {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_zero() {
        let gen = MemoryGeneration::new();
        assert_eq!(gen.get(), 0);
    }

    #[test]
    fn bump_increments() {
        let gen = MemoryGeneration::new();
        assert_eq!(gen.bump(), 1);
        assert_eq!(gen.bump(), 2);
        assert_eq!(gen.get(), 2);
    }

    #[test]
    fn stale_detection() {
        let gen = MemoryGeneration::new();
        let snapshot = gen.get(); // 0
        assert!(!gen.is_stale(snapshot));

        gen.bump(); // now 1
        assert!(gen.is_stale(snapshot));
        assert!(!gen.is_stale(gen.get()));
    }

    #[test]
    fn with_initial() {
        let gen = MemoryGeneration::with_initial(42);
        assert_eq!(gen.get(), 42);
        assert_eq!(gen.bump(), 43);
    }

    #[test]
    fn thread_safe() {
        use std::sync::Arc;
        let gen = Arc::new(MemoryGeneration::new());

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let gen = gen.clone();
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        gen.bump();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(gen.get(), 1000);
    }
}
