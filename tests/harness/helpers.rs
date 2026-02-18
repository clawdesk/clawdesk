//! Test helper utilities for integration tests.

use std::time::Duration;

/// Wait for a condition to become true, with timeout.
///
/// Polls every `interval` until `condition` returns true or `timeout` is reached.
pub async fn wait_for<F, Fut>(timeout: Duration, interval: Duration, condition: F) -> bool
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    loop {
        if condition().await {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Assert that an operation completes within a timeout.
pub async fn assert_completes_within<F, Fut, T>(timeout: Duration, description: &str, f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    tokio::time::timeout(timeout, f())
        .await
        .unwrap_or_else(|_| panic!("{description} did not complete within {timeout:?}"))
}

/// A simple latency tracker for performance assertions.
pub struct LatencyTracker {
    samples: Vec<Duration>,
}

impl LatencyTracker {
    pub fn new() -> Self {
        Self { samples: Vec::new() }
    }

    pub fn record(&mut self, d: Duration) {
        self.samples.push(d);
    }

    pub fn p50(&self) -> Duration {
        self.percentile(50)
    }

    pub fn p95(&self) -> Duration {
        self.percentile(95)
    }

    pub fn p99(&self) -> Duration {
        self.percentile(99)
    }

    pub fn mean(&self) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let total: Duration = self.samples.iter().sum();
        total / self.samples.len() as u32
    }

    fn percentile(&self, p: usize) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let mut sorted = self.samples.clone();
        sorted.sort();
        let idx = (p * sorted.len() / 100).min(sorted.len() - 1);
        sorted[idx]
    }

    pub fn assert_p95_under(&self, max: Duration) {
        let p95 = self.p95();
        assert!(
            p95 <= max,
            "p95 latency {p95:?} exceeds maximum {max:?}"
        );
    }

    pub fn assert_mean_under(&self, max: Duration) {
        let mean = self.mean();
        assert!(
            mean <= max,
            "mean latency {mean:?} exceeds maximum {max:?}"
        );
    }
}

impl Default for LatencyTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_wait_for_immediate() {
        let result = wait_for(
            Duration::from_secs(1),
            Duration::from_millis(10),
            || async { true },
        ).await;
        assert!(result);
    }

    #[tokio::test]
    async fn test_wait_for_timeout() {
        let result = wait_for(
            Duration::from_millis(50),
            Duration::from_millis(10),
            || async { false },
        ).await;
        assert!(!result);
    }

    #[tokio::test]
    async fn test_assert_completes_within_ok() {
        let val = assert_completes_within(
            Duration::from_secs(1),
            "simple add",
            || async { 2 + 2 },
        ).await;
        assert_eq!(val, 4);
    }

    #[test]
    fn test_latency_tracker() {
        let mut tracker = LatencyTracker::new();
        for i in 1..=100 {
            tracker.record(Duration::from_millis(i));
        }
        assert!(tracker.p50() <= Duration::from_millis(51));
        assert!(tracker.p95() <= Duration::from_millis(96));
        assert!(tracker.mean() > Duration::from_millis(40));
        assert!(tracker.mean() < Duration::from_millis(60));
    }
}
