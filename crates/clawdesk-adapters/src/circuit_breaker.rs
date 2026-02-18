//! Circuit breaker with sliding window counter.
//!
//! Tracks failures in the last W seconds using a fixed-size ring of time
//! buckets. If failures/total > threshold, opens the circuit for exponential
//! backoff: T_k = T_base · 2^k, capped at T_max.
//!
//! Avoids the "cliff" problem of fixed windows — a burst of failures at a
//! boundary doesn't get split and undercounted.
//! Space: O(W/bucket_size) = O(6) per adapter.

use std::time::{Duration, Instant};

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation — requests pass through
    Closed,
    /// Failure threshold exceeded — requests blocked
    Open,
    /// Testing — allow one probe request
    HalfOpen,
}

/// A time bucket in the sliding window.
#[derive(Debug, Clone, Copy, Default)]
struct Bucket {
    successes: u32,
    failures: u32,
    start: Option<Instant>,
}

/// Sliding window circuit breaker.
pub struct CircuitBreaker {
    /// Number of buckets in the sliding window
    bucket_count: usize,
    /// Duration of each bucket
    bucket_duration: Duration,
    /// Ring of time buckets
    buckets: Vec<Bucket>,
    /// Current bucket index
    current_index: usize,
    /// Failure rate threshold (0.0 - 1.0)
    threshold: f64,
    /// Current circuit state
    state: CircuitState,
    /// When the circuit was opened
    opened_at: Option<Instant>,
    /// Number of consecutive open cycles (for exponential backoff)
    open_count: u32,
    /// Base backoff duration
    base_backoff: Duration,
    /// Maximum backoff duration
    max_backoff: Duration,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    ///
    /// # Arguments
    /// - `window_secs`: Total sliding window duration
    /// - `bucket_count`: Number of buckets (e.g., 6 for 10s each in 60s window)
    /// - `threshold`: Failure rate to trigger open (e.g., 0.5 = 50%)
    pub fn new(window_secs: u64, bucket_count: usize, threshold: f64) -> Self {
        let bucket_duration = Duration::from_secs(window_secs / bucket_count as u64);
        Self {
            bucket_count,
            bucket_duration,
            buckets: vec![Bucket::default(); bucket_count],
            current_index: 0,
            threshold,
            state: CircuitState::Closed,
            opened_at: None,
            open_count: 0,
            base_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_secs(300),
        }
    }

    /// Check if a request should be allowed.
    pub fn allow_request(&mut self) -> bool {
        self.advance_window();

        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if backoff has elapsed
                if let Some(opened) = self.opened_at {
                    let backoff = self.current_backoff();
                    if opened.elapsed() >= backoff {
                        self.state = CircuitState::HalfOpen;
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
    pub fn record_success(&mut self) {
        self.advance_window();
        self.buckets[self.current_index].successes += 1;

        if self.state == CircuitState::HalfOpen {
            // Probe succeeded — close the circuit
            self.state = CircuitState::Closed;
            self.open_count = 0;
            self.opened_at = None;
        }
    }

    /// Record a failed request.
    pub fn record_failure(&mut self) {
        self.advance_window();
        self.buckets[self.current_index].failures += 1;

        if self.state == CircuitState::HalfOpen {
            // Probe failed — re-open with incremented backoff
            self.state = CircuitState::Open;
            self.open_count += 1;
            self.opened_at = Some(Instant::now());
            return;
        }

        // Check if we should trip
        if self.state == CircuitState::Closed {
            let (total, failures) = self.window_counts();
            if total >= 5 && (failures as f64 / total as f64) > self.threshold {
                self.state = CircuitState::Open;
                self.open_count += 1;
                self.opened_at = Some(Instant::now());
            }
        }
    }

    /// Current circuit state.
    pub fn state(&self) -> CircuitState {
        self.state
    }

    /// Current backoff duration: T_base · 2^k, capped at T_max.
    fn current_backoff(&self) -> Duration {
        let multiplier = 2u64.saturating_pow(self.open_count.min(10));
        let backoff = self.base_backoff.saturating_mul(multiplier as u32);
        backoff.min(self.max_backoff)
    }

    /// Advance the window if the current bucket has expired.
    fn advance_window(&mut self) {
        let now = Instant::now();
        let bucket = &self.buckets[self.current_index];

        if let Some(start) = bucket.start {
            if now.duration_since(start) >= self.bucket_duration {
                // Move to next bucket, clearing it
                self.current_index = (self.current_index + 1) % self.bucket_count;
                self.buckets[self.current_index] = Bucket {
                    successes: 0,
                    failures: 0,
                    start: Some(now),
                };
            }
        } else {
            self.buckets[self.current_index].start = Some(now);
        }
    }

    /// Total successes and failures across the sliding window.
    fn window_counts(&self) -> (u32, u32) {
        let total: u32 = self.buckets.iter().map(|b| b.successes + b.failures).sum();
        let failures: u32 = self.buckets.iter().map(|b| b.failures).sum();
        (total, failures)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_trips_on_failures() {
        let mut cb = CircuitBreaker::new(60, 6, 0.5);
        // 10 failures should trip the circuit
        for _ in 0..10 {
            cb.record_failure();
        }
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow_request());
    }

    #[test]
    fn healthy_circuit_stays_closed() {
        let mut cb = CircuitBreaker::new(60, 6, 0.5);
        for _ in 0..20 {
            cb.record_success();
        }
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow_request());
    }
}
