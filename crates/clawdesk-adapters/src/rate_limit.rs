//! Adaptive rate limiting — atomic token bucket + PID feedback controller.
//!
//! ## Token Bucket (Lock-Free)
//!
//! Tokens are stored as `AtomicI64` (millitokens for sub-token precision).
//! `try_acquire` uses CAS (`compare_exchange_weak`) — no mutex, no cache-line
//! bouncing under contention. Inspired by SochDB's `admission_control.rs`.
//!
//! ## PID Controller
//!
//! ```text
//! R(t) = R_base + Kp·e(t) + Ki·∫e(t)dt + Kd·de/dt
//! ```
//!
//! Feedback signal: observed latency vs. target latency.
//! - Kp reacts to immediate latency spikes
//! - Ki corrects long-term drift
//! - Kd dampens oscillations (prevents thundering herd)
//!
//! The controller adjusts `refill_rate` dynamically so throughput tracks
//! the provider's actual capacity.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Instant;

/// Millitokens per token — allows sub-token precision in atomic ops.
const MILLI: i64 = 1000;

// ---------------------------------------------------------------------------
// Lock-free atomic token bucket
// ---------------------------------------------------------------------------

/// Thread-safe token bucket using atomic CAS. No mutex required.
///
/// Internal representation uses millitokens (`i64 × 1000`) for precision
/// without floating-point atomics.
pub struct TokenBucket {
    /// Maximum capacity in millitokens.
    capacity_milli: i64,
    /// Refill rate in millitokens per second. Mutable via PID controller.
    refill_rate_milli: AtomicI64,
    /// Current token count in millitokens.
    tokens_milli: AtomicI64,
    /// Last refill timestamp (nanos since an arbitrary epoch).
    last_refill_nanos: AtomicU64,
    /// Instant used as epoch for nanos calculation.
    epoch: Instant,
}

impl TokenBucket {
    /// Create a new token bucket.
    ///
    /// # Arguments
    /// - `capacity`: Maximum tokens (burst size)
    /// - `refill_rate`: Tokens added per second
    pub fn new(capacity: f64, refill_rate: f64) -> Self {
        let now = Instant::now();
        Self {
            capacity_milli: (capacity * MILLI as f64) as i64,
            refill_rate_milli: AtomicI64::new((refill_rate * MILLI as f64) as i64),
            tokens_milli: AtomicI64::new((capacity * MILLI as f64) as i64), // start full
            last_refill_nanos: AtomicU64::new(0),
            epoch: now,
        }
    }

    /// Create from requests-per-minute.
    pub fn from_rpm(rpm: u32) -> Self {
        let rate = rpm as f64 / 60.0;
        Self::new(rpm as f64, rate)
    }

    /// Atomically refill tokens based on elapsed time.
    fn refill(&self) {
        let now_nanos = self.epoch.elapsed().as_nanos() as u64;
        let prev_nanos = self.last_refill_nanos.swap(now_nanos, Ordering::AcqRel);
        let elapsed_secs = (now_nanos.saturating_sub(prev_nanos)) as f64 / 1_000_000_000.0;

        let rate = self.refill_rate_milli.load(Ordering::Relaxed);
        let add = (rate as f64 * elapsed_secs) as i64;
        if add > 0 {
            let mut current = self.tokens_milli.load(Ordering::Relaxed);
            loop {
                let new = (current + add).min(self.capacity_milli);
                match self.tokens_milli.compare_exchange_weak(
                    current,
                    new,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(actual) => current = actual,
                }
            }
        }
    }

    /// Try to acquire one token. Returns true if allowed. Thread-safe.
    pub fn try_acquire(&self) -> bool {
        self.try_acquire_n(1.0)
    }

    /// Try to acquire N tokens. Thread-safe (CAS loop).
    pub fn try_acquire_n(&self, n: f64) -> bool {
        self.refill();
        let cost = (n * MILLI as f64) as i64;
        let mut current = self.tokens_milli.load(Ordering::Relaxed);
        loop {
            if current < cost {
                return false;
            }
            match self.tokens_milli.compare_exchange_weak(
                current,
                current - cost,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// Seconds until the next token is available.
    pub fn wait_time(&self) -> f64 {
        self.refill();
        let current = self.tokens_milli.load(Ordering::Relaxed);
        if current >= MILLI {
            0.0
        } else {
            let deficit = MILLI - current;
            let rate = self.refill_rate_milli.load(Ordering::Relaxed);
            if rate <= 0 {
                return f64::INFINITY;
            }
            deficit as f64 / rate as f64
        }
    }

    /// Current available tokens.
    pub fn available(&self) -> f64 {
        self.refill();
        self.tokens_milli.load(Ordering::Relaxed) as f64 / MILLI as f64
    }

    /// Update the refill rate (called by PID controller).
    pub fn set_refill_rate(&self, tokens_per_second: f64) {
        let clamped = tokens_per_second.max(0.1); // floor at 0.1 tps
        self.refill_rate_milli
            .store((clamped * MILLI as f64) as i64, Ordering::Release);
    }

    /// Current refill rate in tokens per second.
    pub fn refill_rate(&self) -> f64 {
        self.refill_rate_milli.load(Ordering::Relaxed) as f64 / MILLI as f64
    }

    /// Utilization ratio: (capacity - available) / capacity.
    pub fn utilization(&self) -> f64 {
        let avail = self.tokens_milli.load(Ordering::Relaxed) as f64;
        1.0 - (avail / self.capacity_milli as f64).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// PID feedback controller for adaptive rate limiting
// ---------------------------------------------------------------------------

/// PID controller gains.
#[derive(Debug, Clone)]
pub struct PidGains {
    /// Proportional gain — reacts to immediate error.
    pub kp: f64,
    /// Integral gain — corrects accumulated drift.
    pub ki: f64,
    /// Derivative gain — dampens oscillations.
    pub kd: f64,
}

impl Default for PidGains {
    fn default() -> Self {
        Self {
            kp: 0.5,
            ki: 0.05,
            kd: 0.1,
        }
    }
}

/// Adaptive rate controller using PID feedback on observed latency.
///
/// Adjusts a `TokenBucket`'s refill rate so throughput tracks the downstream
/// provider's actual capacity without triggering 429s.
pub struct AdaptiveRateController {
    /// PID gains.
    gains: PidGains,
    /// Target latency in seconds.
    target_latency: f64,
    /// Base refill rate (tokens/sec) — the setpoint before PID adjustment.
    base_rate: f64,
    /// Minimum allowed rate (tokens/sec).
    min_rate: f64,
    /// Maximum allowed rate (tokens/sec).
    max_rate: f64,
    /// Accumulated integral term.
    integral: f64,
    /// Previous error (for derivative).
    prev_error: f64,
    /// Timestamp of last update.
    last_update: Instant,
    /// Count of consecutive 429/rate-limit errors.
    consecutive_429s: u32,
}

impl AdaptiveRateController {
    /// Create a new adaptive rate controller.
    ///
    /// # Arguments
    /// - `base_rate`: Starting refill rate (tokens/sec)
    /// - `target_latency_ms`: Target p50 latency in milliseconds
    /// - `gains`: PID gains (or use `PidGains::default()`)
    pub fn new(base_rate: f64, target_latency_ms: f64, gains: PidGains) -> Self {
        Self {
            gains,
            target_latency: target_latency_ms / 1000.0,
            base_rate,
            min_rate: base_rate * 0.1, // floor at 10% of base
            max_rate: base_rate * 3.0, // ceiling at 3× base
            integral: 0.0,
            prev_error: 0.0,
            last_update: Instant::now(),
            consecutive_429s: 0,
        }
    }

    /// Record an observed response and compute the new rate.
    ///
    /// Call this after every provider response with the observed latency
    /// and whether a rate-limit error (429) occurred.
    ///
    /// Returns the new suggested refill rate (tokens/sec).
    pub fn observe(&mut self, latency_secs: f64, is_rate_limited: bool) -> f64 {
        let now = Instant::now();
        let dt = now.duration_since(self.last_update).as_secs_f64().max(0.001);
        self.last_update = now;

        if is_rate_limited {
            // Hard backoff: halve the rate immediately, accumulate 429 count.
            self.consecutive_429s += 1;
            let backoff_factor = 0.5_f64.powi(self.consecutive_429s.min(4) as i32);
            self.integral = 0.0; // reset integral to prevent windup
            let new_rate = (self.base_rate * backoff_factor).max(self.min_rate);
            self.base_rate = new_rate;
            return new_rate;
        }

        // Clear 429 counter on successful response.
        self.consecutive_429s = 0;

        // PID error: positive = latency too low (can go faster),
        //            negative = latency too high (must slow down).
        let error = self.target_latency - latency_secs;

        // Proportional term.
        let p = self.gains.kp * error;

        // Integral term with anti-windup clamp.
        self.integral = (self.integral + error * dt).clamp(-10.0, 10.0);
        let i = self.gains.ki * self.integral;

        // Derivative term.
        let d = self.gains.kd * (error - self.prev_error) / dt;
        self.prev_error = error;

        // Compute adjusted rate.
        let adjustment = p + i + d;
        let new_rate = (self.base_rate + adjustment).clamp(self.min_rate, self.max_rate);

        new_rate
    }

    /// Apply the controller's output to a token bucket.
    pub fn apply(&mut self, bucket: &TokenBucket, latency_secs: f64, is_rate_limited: bool) {
        let new_rate = self.observe(latency_secs, is_rate_limited);
        bucket.set_refill_rate(new_rate);
    }

    /// Current base rate.
    pub fn base_rate(&self) -> f64 {
        self.base_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_rate_limiting() {
        let bucket = TokenBucket::new(2.0, 1.0);
        assert!(bucket.try_acquire()); // 1 left
        assert!(bucket.try_acquire()); // 0 left
        assert!(!bucket.try_acquire()); // denied
    }

    #[test]
    fn from_rpm_creates_valid_bucket() {
        let bucket = TokenBucket::from_rpm(60);
        // 60 RPM = 1 token/sec, capacity = 60
        assert!(bucket.available() > 59.0);
        assert!(bucket.try_acquire_n(30.0));
        assert!(bucket.available() > 29.0);
    }

    #[test]
    fn utilization_reflects_consumption() {
        let bucket = TokenBucket::new(10.0, 1.0);
        assert!(bucket.utilization() < 0.01); // full bucket = low utilization
        bucket.try_acquire_n(5.0);
        let util = bucket.utilization();
        assert!(util > 0.4 && util < 0.6, "utilization should be ~0.5, got {}", util);
    }

    #[test]
    fn set_refill_rate_updates_rate() {
        let bucket = TokenBucket::new(10.0, 1.0);
        assert!((bucket.refill_rate() - 1.0).abs() < 0.01);
        bucket.set_refill_rate(5.0);
        assert!((bucket.refill_rate() - 5.0).abs() < 0.01);
    }

    #[test]
    fn pid_backs_off_on_429() {
        let mut controller = AdaptiveRateController::new(10.0, 500.0, PidGains::default());
        let rate = controller.observe(0.0, true); // 429
        assert!(rate < 10.0, "rate should decrease on 429, got {}", rate);
        let rate2 = controller.observe(0.0, true); // another 429
        assert!(rate2 < rate, "rate should decrease further, got {}", rate2);
    }

    #[test]
    fn pid_speeds_up_when_latency_below_target() {
        let mut controller = AdaptiveRateController::new(10.0, 500.0, PidGains::default());
        // Observe latency well below target (100ms vs 500ms target)
        let rate = controller.observe(0.1, false);
        assert!(rate >= 10.0, "rate should increase when latency is below target, got {}", rate);
    }
}
