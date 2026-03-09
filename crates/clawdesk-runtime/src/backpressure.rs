//! Backpressure and flow control for the agent runtime.
//!
//! Implements token-bucket rate limiting and in-flight request tracking
//! to prevent overloading providers or exhausting local resources.
//!
//! # Mechanisms
//!
//! 1. **Token bucket**: Limits throughput (requests/sec or tokens/sec).
//! 2. **Semaphore-based concurrency**: Caps in-flight requests.
//! 3. **Queue depth monitoring**: Rejects work when queues grow too deep.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// Configuration for backpressure control.
#[derive(Debug, Clone)]
pub struct BackpressureConfig {
    /// Maximum concurrent in-flight requests.
    pub max_concurrent: u32,
    /// Token bucket: max burst capacity.
    pub bucket_capacity: u64,
    /// Token bucket: refill rate (tokens per second).
    pub refill_rate: f64,
    /// Maximum pending queue depth before rejecting.
    pub max_queue_depth: usize,
}

impl Default for BackpressureConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 64,
            bucket_capacity: 100,
            refill_rate: 50.0,
            max_queue_depth: 256,
        }
    }
}

/// Decision from the backpressure controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Request is admitted.
    Admit,
    /// Request should be shed (rejected).
    Shed,
    /// Request should be queued and retried.
    Retry { after: Duration },
}

/// Token bucket rate limiter.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: u64,
    tokens: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Create a new token bucket.
    pub fn new(capacity: u64, refill_rate: f64) -> Self {
        Self {
            capacity,
            tokens: capacity as f64,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// Try to acquire one token. Returns `true` if acquired.
    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Time until the next token is available.
    pub fn time_until_available(&mut self) -> Duration {
        self.refill();
        if self.tokens >= 1.0 {
            Duration::ZERO
        } else {
            let needed = 1.0 - self.tokens;
            Duration::from_secs_f64(needed / self.refill_rate)
        }
    }

    /// Current fill level (0.0–1.0).
    pub fn fill_level(&mut self) -> f64 {
        self.refill();
        self.tokens / self.capacity as f64
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity as f64);
        self.last_refill = now;
    }
}

/// Backpressure controller combining rate limiting and concurrency control.
pub struct BackpressureController {
    config: BackpressureConfig,
    bucket: TokenBucket,
    concurrency: Arc<Semaphore>,
    queue_depth: AtomicU64,
    total_admitted: AtomicU64,
    total_shed: AtomicU64,
}

impl BackpressureController {
    /// Create a new backpressure controller.
    pub fn new(config: BackpressureConfig) -> Self {
        let sem = Arc::new(Semaphore::new(config.max_concurrent as usize));
        let bucket = TokenBucket::new(config.bucket_capacity, config.refill_rate);
        Self {
            config,
            bucket,
            concurrency: sem,
            queue_depth: AtomicU64::new(0),
            total_admitted: AtomicU64::new(0),
            total_shed: AtomicU64::new(0),
        }
    }

    /// Check admission for an incoming request.
    ///
    /// Returns `Admit` if the request should proceed,
    /// `Shed` if it should be rejected, or `Retry` with a delay hint.
    pub fn check_admission(&mut self) -> Admission {
        let depth = self.queue_depth.load(Ordering::Relaxed) as usize;

        // If queue is too deep, shed immediately.
        if depth >= self.config.max_queue_depth {
            self.total_shed.fetch_add(1, Ordering::Relaxed);
            return Admission::Shed;
        }

        // Check concurrency semaphore (non-blocking).
        if self.concurrency.available_permits() == 0 {
            self.total_shed.fetch_add(1, Ordering::Relaxed);
            return Admission::Shed;
        }

        // Check token bucket.
        if !self.bucket.try_acquire() {
            let wait = self.bucket.time_until_available();
            return Admission::Retry { after: wait };
        }

        self.total_admitted.fetch_add(1, Ordering::Relaxed);
        Admission::Admit
    }

    /// Acquire a concurrency permit. Returns a guard that releases on drop.
    pub async fn acquire(&self) -> Result<tokio::sync::OwnedSemaphorePermit, BackpressureError> {
        self.concurrency
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| BackpressureError::Shutdown)
    }

    /// Increment queue depth (call when enqueuing work).
    pub fn enqueue(&self) {
        self.queue_depth.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement queue depth (call when work is dequeued).
    pub fn dequeue(&self) {
        self.queue_depth.fetch_sub(1, Ordering::Relaxed);
    }

    /// Get current metrics.
    pub fn metrics(&self) -> BackpressureMetrics {
        BackpressureMetrics {
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
            available_permits: self.concurrency.available_permits() as u32,
            max_concurrent: self.config.max_concurrent,
            total_admitted: self.total_admitted.load(Ordering::Relaxed),
            total_shed: self.total_shed.load(Ordering::Relaxed),
        }
    }
}

/// Backpressure error types.
#[derive(Debug, Clone)]
pub enum BackpressureError {
    /// The system is shutting down — semaphore closed.
    Shutdown,
}

impl std::fmt::Display for BackpressureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shutdown => write!(f, "backpressure controller is shut down"),
        }
    }
}

impl std::error::Error for BackpressureError {}

/// Snapshot of backpressure metrics.
#[derive(Debug, Clone)]
pub struct BackpressureMetrics {
    pub queue_depth: u64,
    pub available_permits: u32,
    pub max_concurrent: u32,
    pub total_admitted: u64,
    pub total_shed: u64,
}

impl BackpressureMetrics {
    /// Shedding rate = shed / (admitted + shed).
    pub fn shed_rate(&self) -> f64 {
        let total = self.total_admitted + self.total_shed;
        if total == 0 {
            0.0
        } else {
            self.total_shed as f64 / total as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bucket_acquire() {
        let mut bucket = TokenBucket::new(5, 10.0);
        // Should have 5 tokens initially.
        for _ in 0..5 {
            assert!(bucket.try_acquire());
        }
        // 6th should fail.
        assert!(!bucket.try_acquire());
    }

    #[test]
    fn token_bucket_refill() {
        let mut bucket = TokenBucket::new(10, 1000.0); // 1000/sec
        // Drain all tokens.
        while bucket.try_acquire() {}

        // Wait a bit for refill.
        std::thread::sleep(Duration::from_millis(10));
        assert!(bucket.try_acquire());
    }

    #[test]
    fn admission_admits_under_capacity() {
        let config = BackpressureConfig {
            max_concurrent: 10,
            bucket_capacity: 100,
            refill_rate: 100.0,
            max_queue_depth: 50,
        };
        let mut controller = BackpressureController::new(config);
        assert_eq!(controller.check_admission(), Admission::Admit);
    }

    #[test]
    fn admission_sheds_on_queue_depth() {
        let config = BackpressureConfig {
            max_concurrent: 10,
            bucket_capacity: 100,
            refill_rate: 100.0,
            max_queue_depth: 2,
        };
        let mut controller = BackpressureController::new(config);
        controller.enqueue();
        controller.enqueue();
        assert_eq!(controller.check_admission(), Admission::Shed);
    }

    #[test]
    fn metrics_shed_rate() {
        let metrics = BackpressureMetrics {
            queue_depth: 0,
            available_permits: 10,
            max_concurrent: 10,
            total_admitted: 90,
            total_shed: 10,
        };
        assert!((metrics.shed_rate() - 0.1).abs() < 0.001);
    }

    #[tokio::test]
    async fn concurrency_permit() {
        let config = BackpressureConfig {
            max_concurrent: 2,
            ..Default::default()
        };
        let controller = BackpressureController::new(config);

        let _p1 = controller.acquire().await.unwrap();
        let _p2 = controller.acquire().await.unwrap();

        // Third acquire would block, check permits.
        assert_eq!(controller.concurrency.available_permits(), 0);
    }
}
