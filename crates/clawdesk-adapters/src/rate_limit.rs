//! Token bucket rate limiter — O(1) per request, O(1) space per adapter.
//!
//! ```text
//! tokens(t) = min(B, tokens(t_last) + r · (t - t_last))
//! ```

use std::time::Instant;

/// Token bucket rate limiter.
pub struct TokenBucket {
    /// Maximum bucket capacity
    capacity: f64,
    /// Refill rate (tokens per second)
    refill_rate: f64,
    /// Current token count
    tokens: f64,
    /// Last refill time
    last_refill: Instant,
}

impl TokenBucket {
    /// Create a new token bucket.
    ///
    /// # Arguments
    /// - `capacity`: Maximum tokens (burst size)
    /// - `refill_rate`: Tokens added per second
    pub fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            capacity,
            refill_rate,
            tokens: capacity, // start full
            last_refill: Instant::now(),
        }
    }

    /// Create from requests-per-minute.
    pub fn from_rpm(rpm: u32) -> Self {
        let rate = rpm as f64 / 60.0;
        Self::new(rpm as f64, rate)
    }

    /// Refill tokens based on elapsed time.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + self.refill_rate * elapsed).min(self.capacity);
        self.last_refill = now;
    }

    /// Try to acquire one token. Returns true if allowed.
    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Try to acquire N tokens.
    pub fn try_acquire_n(&mut self, n: f64) -> bool {
        self.refill();
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }

    /// Seconds until the next token is available.
    pub fn wait_time(&mut self) -> f64 {
        self.refill();
        if self.tokens >= 1.0 {
            0.0
        } else {
            (1.0 - self.tokens) / self.refill_rate
        }
    }

    /// Current available tokens.
    pub fn available(&mut self) -> f64 {
        self.refill();
        self.tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_rate_limiting() {
        let mut bucket = TokenBucket::new(2.0, 1.0);
        assert!(bucket.try_acquire()); // 1 left
        assert!(bucket.try_acquire()); // 0 left
        assert!(!bucket.try_acquire()); // denied
    }
}
