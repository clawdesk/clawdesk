//! Circuit breaker with sliding window counter.
//!
//! Tracks failures in the last W seconds using a fixed-size ring of time
//! buckets. If failures/total > threshold, opens the circuit for exponential
//! backoff: T_k = T_base · 2^k, capped at T_max.
//!
//! Avoids the "cliff" problem of fixed windows — a burst of failures at a
//! boundary doesn't get split and undercounted.
//! Space: O(W/bucket_size) = O(6) per adapter.
//!
//! ## Thread Safety
//!
//! All methods take `&self` — no external `Mutex` required. Bucket counters
//! are packed into `AtomicU64` (high 32 = successes, low 32 = failures).
//! State transitions use `AtomicU8` with `compare_exchange`.

use std::sync::atomic::{AtomicU64, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CircuitState {
    /// Normal operation — requests pass through
    Closed = 0,
    /// Failure threshold exceeded — requests blocked
    Open = 1,
    /// Testing — allow one probe request
    HalfOpen = 2,
}

impl CircuitState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => CircuitState::Closed,
            1 => CircuitState::Open,
            2 => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }
}

/// Atomic bucket: high 32 bits = successes, low 32 bits = failures.
struct AtomicBucket {
    /// Packed: (successes << 32) | failures
    packed: AtomicU64,
    /// Bucket start time in ms since process start (0 = uninitialized).
    start_ms: AtomicU64,
}

impl AtomicBucket {
    fn new() -> Self {
        Self {
            packed: AtomicU64::new(0),
            start_ms: AtomicU64::new(0),
        }
    }

    fn successes(&self) -> u32 {
        (self.packed.load(Ordering::Relaxed) >> 32) as u32
    }

    fn failures(&self) -> u32 {
        self.packed.load(Ordering::Relaxed) as u32
    }

    fn add_success(&self) {
        self.packed.fetch_add(1 << 32, Ordering::Relaxed);
    }

    fn add_failure(&self) {
        self.packed.fetch_add(1, Ordering::Relaxed);
    }

    fn reset(&self, start_ms: u64) {
        self.packed.store(0, Ordering::Relaxed);
        self.start_ms.store(start_ms, Ordering::Relaxed);
    }
}

// Safety: AtomicBucket is composed entirely of atomics.
unsafe impl Send for AtomicBucket {}
unsafe impl Sync for AtomicBucket {}

/// Thread-safe sliding window circuit breaker.
///
/// All public methods take `&self` — no external `Mutex` wrapper needed.
/// This enables concurrent adapter calls to proceed without serialization.
pub struct CircuitBreaker {
    /// Number of buckets in the sliding window
    bucket_count: usize,
    /// Duration of each bucket in ms
    bucket_duration_ms: u64,
    /// Ring of atomic time buckets
    buckets: Vec<AtomicBucket>,
    /// Current bucket index (ring position)
    current_index: AtomicUsize,
    /// Failure rate threshold (0.0 - 1.0), stored as u32 (threshold * 1000)
    threshold_millipct: u32,
    /// Current circuit state (0=Closed, 1=Open, 2=HalfOpen)
    state: AtomicU8,
    /// When the circuit was last opened (epoch ms since process start, 0 = never)
    opened_at_ms: AtomicU64,
    /// Number of consecutive open cycles (for exponential backoff)
    open_count: AtomicU32,
    /// Base backoff duration in ms
    base_backoff_ms: u64,
    /// Maximum backoff duration in ms
    max_backoff_ms: u64,
    /// Process-local epoch for time calculations
    epoch: Instant,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    ///
    /// # Arguments
    /// - `window_secs`: Total sliding window duration
    /// - `bucket_count`: Number of buckets (e.g., 6 for 10s each in 60s window)
    /// - `threshold`: Failure rate to trigger open (e.g., 0.5 = 50%)
    pub fn new(window_secs: u64, bucket_count: usize, threshold: f64) -> Self {
        let bucket_duration_ms = (window_secs * 1000) / bucket_count as u64;
        let buckets: Vec<AtomicBucket> = (0..bucket_count).map(|_| AtomicBucket::new()).collect();
        Self {
            bucket_count,
            bucket_duration_ms,
            buckets,
            current_index: AtomicUsize::new(0),
            threshold_millipct: (threshold * 1000.0) as u32,
            state: AtomicU8::new(CircuitState::Closed as u8),
            opened_at_ms: AtomicU64::new(0),
            open_count: AtomicU32::new(0),
            base_backoff_ms: 5_000,
            max_backoff_ms: 300_000,
            epoch: Instant::now(),
        }
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Check if a request should be allowed.
    pub fn allow_request(&self) -> bool {
        self.advance_window();

        let state = CircuitState::from_u8(self.state.load(Ordering::Acquire));
        match state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let opened = self.opened_at_ms.load(Ordering::Relaxed);
                if opened > 0 {
                    let backoff = self.current_backoff_ms();
                    let elapsed = self.now_ms().saturating_sub(opened);
                    if elapsed >= backoff {
                        // Try to transition to HalfOpen
                        let _ = self.state.compare_exchange(
                            CircuitState::Open as u8,
                            CircuitState::HalfOpen as u8,
                            Ordering::AcqRel,
                            Ordering::Relaxed,
                        );
                        true // allow one probe
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => false, // only one probe at a time
        }
    }

    /// Record a successful request.
    pub fn record_success(&self) {
        self.advance_window();
        let idx = self.current_index.load(Ordering::Relaxed);
        self.buckets[idx].add_success();

        let state = CircuitState::from_u8(self.state.load(Ordering::Acquire));
        if state == CircuitState::HalfOpen {
            // Probe succeeded — close the circuit
            if self.state.compare_exchange(
                CircuitState::HalfOpen as u8,
                CircuitState::Closed as u8,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ).is_ok() {
                self.open_count.store(0, Ordering::Relaxed);
                self.opened_at_ms.store(0, Ordering::Relaxed);
            }
        }
    }

    /// Record a failed request.
    pub fn record_failure(&self) {
        self.advance_window();
        let idx = self.current_index.load(Ordering::Relaxed);
        self.buckets[idx].add_failure();

        let state = CircuitState::from_u8(self.state.load(Ordering::Acquire));
        if state == CircuitState::HalfOpen {
            // Probe failed — re-open with incremented backoff
            if self.state.compare_exchange(
                CircuitState::HalfOpen as u8,
                CircuitState::Open as u8,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ).is_ok() {
                self.open_count.fetch_add(1, Ordering::Relaxed);
                self.opened_at_ms.store(self.now_ms(), Ordering::Relaxed);
            }
            return;
        }

        // Check if we should trip
        if state == CircuitState::Closed {
            let (total, failures) = self.window_counts();
            if total >= 5 {
                let failure_millipct = (failures as u64 * 1000) / total as u64;
                if failure_millipct as u32 > self.threshold_millipct {
                    if self.state.compare_exchange(
                        CircuitState::Closed as u8,
                        CircuitState::Open as u8,
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    ).is_ok() {
                        self.open_count.fetch_add(1, Ordering::Relaxed);
                        self.opened_at_ms.store(self.now_ms(), Ordering::Relaxed);
                    }
                }
            }
        }
    }

    /// Current circuit state.
    pub fn state(&self) -> CircuitState {
        CircuitState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Current backoff duration in ms: T_base · 2^k, capped at T_max.
    fn current_backoff_ms(&self) -> u64 {
        let k = self.open_count.load(Ordering::Relaxed).min(10);
        let multiplier = 1u64 << k;
        let backoff = self.base_backoff_ms.saturating_mul(multiplier);
        backoff.min(self.max_backoff_ms)
    }

    /// Advance the window if the current bucket has expired.
    fn advance_window(&self) {
        let now = self.now_ms();
        let idx = self.current_index.load(Ordering::Relaxed);
        let bucket_start = self.buckets[idx].start_ms.load(Ordering::Relaxed);

        if bucket_start == 0 {
            // Uninitialized — set start time
            self.buckets[idx].start_ms.store(now, Ordering::Relaxed);
            return;
        }

        if now.saturating_sub(bucket_start) >= self.bucket_duration_ms {
            // Move to next bucket, clearing it
            let next = (idx + 1) % self.bucket_count;
            // CAS on the index to avoid double-advance
            if self.current_index.compare_exchange(
                idx,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ).is_ok() {
                self.buckets[next].reset(now);
            }
        }
    }

    /// Total successes and failures across the sliding window.
    fn window_counts(&self) -> (u32, u32) {
        let mut total = 0u32;
        let mut failures = 0u32;
        for bucket in &self.buckets {
            total += bucket.successes() + bucket.failures();
            failures += bucket.failures();
        }
        (total, failures)
    }
}

// CircuitBreaker is Send+Sync because all fields are atomic or immutable.
unsafe impl Send for CircuitBreaker {}
unsafe impl Sync for CircuitBreaker {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_trips_on_failures() {
        let cb = CircuitBreaker::new(60, 6, 0.5);
        // 10 failures should trip the circuit
        for _ in 0..10 {
            cb.record_failure();
        }
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow_request());
    }

    #[test]
    fn healthy_circuit_stays_closed() {
        let cb = CircuitBreaker::new(60, 6, 0.5);
        for _ in 0..20 {
            cb.record_success();
        }
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow_request());
    }

    #[test]
    fn circuit_breaker_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CircuitBreaker>();
    }

    #[test]
    fn concurrent_failure_recording() {
        use std::sync::Arc;
        let cb = Arc::new(CircuitBreaker::new(60, 6, 0.5));

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let cb = Arc::clone(&cb);
                std::thread::spawn(move || {
                    cb.record_failure();
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Should be open after 10 failures
        assert_eq!(cb.state(), CircuitState::Open);
    }
}
