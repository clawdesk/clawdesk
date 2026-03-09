//! Load testing framework for concurrent workload simulation.
//!
//! Simulates concurrent request patterns to identify bottlenecks,
//! measure throughput under load, and verify backpressure behavior.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Configuration for a load test.
#[derive(Debug, Clone)]
pub struct LoadTestConfig {
    /// Total number of requests to send.
    pub total_requests: u64,
    /// Maximum number of concurrent in-flight requests.
    pub concurrency: u32,
    /// Optional ramp-up period: linearly increase concurrency over this duration.
    pub ramp_up: Duration,
    /// Name of the load test.
    pub name: String,
}

impl Default for LoadTestConfig {
    fn default() -> Self {
        Self {
            total_requests: 1_000,
            concurrency: 16,
            ramp_up: Duration::ZERO,
            name: String::new(),
        }
    }
}

/// Outcome of a single request in the load test.
#[derive(Debug, Clone)]
pub struct RequestOutcome {
    /// Elapsed time for this request.
    pub latency: Duration,
    /// Whether the request succeeded.
    pub success: bool,
    /// Optional error message on failure.
    pub error: Option<String>,
}

/// Aggregated load test results.
#[derive(Debug, Clone)]
pub struct LoadTestResult {
    pub name: String,
    pub total_requests: u64,
    pub successful: u64,
    pub failed: u64,
    pub total_time: Duration,
    /// Sorted latencies for all requests.
    latencies: Vec<Duration>,
}

impl LoadTestResult {
    /// Mean latency.
    pub fn mean_latency(&self) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }
        let sum: Duration = self.latencies.iter().sum();
        sum / self.latencies.len() as u32
    }

    /// Percentile latency.
    pub fn percentile(&self, pct: f64) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }
        let idx = ((pct / 100.0) * (self.latencies.len() - 1) as f64).round() as usize;
        self.latencies[idx.min(self.latencies.len() - 1)]
    }

    /// Throughput in requests per second.
    pub fn rps(&self) -> f64 {
        self.total_requests as f64 / self.total_time.as_secs_f64()
    }

    /// Error rate ∈ [0, 1].
    pub fn error_rate(&self) -> f64 {
        if self.total_requests == 0 {
            return 0.0;
        }
        self.failed as f64 / self.total_requests as f64
    }

    /// Summary string.
    pub fn summary(&self) -> String {
        format!(
            "{}: {} reqs, {:.0} rps, mean={:?}, p50={:?}, p99={:?}, err={:.1}%",
            self.name,
            self.total_requests,
            self.rps(),
            self.mean_latency(),
            self.percentile(50.0),
            self.percentile(99.0),
            self.error_rate() * 100.0,
        )
    }
}

/// Run a load test with the given async work function.
///
/// The function `f` is called `total_requests` times with up to `concurrency`
/// calls in flight simultaneously.
pub async fn run_load_test<F, Fut>(config: &LoadTestConfig, f: F) -> LoadTestResult
where
    F: Fn(u64) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = RequestOutcome> + Send,
{
    let f = Arc::new(f);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(config.concurrency as usize));
    let completed = Arc::new(AtomicU64::new(0));
    let success_count = Arc::new(AtomicU64::new(0));
    let fail_count = Arc::new(AtomicU64::new(0));
    let latencies = Arc::new(std::sync::Mutex::new(Vec::with_capacity(
        config.total_requests as usize,
    )));

    let start = Instant::now();
    let mut handles = Vec::with_capacity(config.total_requests as usize);

    for i in 0..config.total_requests {
        // Ramp-up: stagger initial requests.
        if !config.ramp_up.is_zero() && i < config.concurrency as u64 {
            let delay = config.ramp_up.mul_f64(i as f64 / config.concurrency as f64);
            tokio::time::sleep(delay).await;
        }

        let permit = semaphore.clone().acquire_owned().await;
        let f = Arc::clone(&f);
        let completed = Arc::clone(&completed);
        let success_count = Arc::clone(&success_count);
        let fail_count = Arc::clone(&fail_count);
        let latencies = Arc::clone(&latencies);

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let outcome = f(i).await;

            if outcome.success {
                success_count.fetch_add(1, Ordering::Relaxed);
            } else {
                fail_count.fetch_add(1, Ordering::Relaxed);
            }

            if let Ok(mut lats) = latencies.lock() {
                lats.push(outcome.latency);
            }
            completed.fetch_add(1, Ordering::Relaxed);
        }));
    }

    // Wait for all requests to complete.
    for handle in handles {
        let _ = handle.await;
    }

    let total_time = start.elapsed();
    let mut sorted_latencies = match Arc::try_unwrap(latencies) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    };
    sorted_latencies.sort();

    let successful = success_count.load(Ordering::Relaxed);
    let failed = fail_count.load(Ordering::Relaxed);

    LoadTestResult {
        name: config.name.clone(),
        total_requests: config.total_requests,
        successful,
        failed,
        total_time,
        latencies: sorted_latencies,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn load_test_all_succeed() {
        let config = LoadTestConfig {
            total_requests: 50,
            concurrency: 8,
            name: "all_ok".into(),
            ..Default::default()
        };

        let result = run_load_test(&config, |_i| async {
            RequestOutcome {
                latency: Duration::from_micros(100),
                success: true,
                error: None,
            }
        })
        .await;

        assert_eq!(result.total_requests, 50);
        assert_eq!(result.successful, 50);
        assert_eq!(result.failed, 0);
        assert!(result.error_rate() < 0.001);
    }

    #[tokio::test]
    async fn load_test_with_failures() {
        let config = LoadTestConfig {
            total_requests: 100,
            concurrency: 4,
            name: "partial_fail".into(),
            ..Default::default()
        };

        let result = run_load_test(&config, |i| async move {
            RequestOutcome {
                latency: Duration::from_micros(50),
                success: i % 5 != 0,
                error: if i % 5 == 0 {
                    Some("simulated error".into())
                } else {
                    None
                },
            }
        })
        .await;

        assert_eq!(result.total_requests, 100);
        assert_eq!(result.failed, 20);
        assert!((result.error_rate() - 0.20).abs() < 0.01);
    }

    #[tokio::test]
    async fn load_test_respects_concurrency() {
        let in_flight = Arc::new(AtomicU64::new(0));
        let max_in_flight = Arc::new(AtomicU64::new(0));
        let in_flight_clone = Arc::clone(&in_flight);
        let max_clone = Arc::clone(&max_in_flight);

        let config = LoadTestConfig {
            total_requests: 50,
            concurrency: 4,
            name: "concurrency_check".into(),
            ..Default::default()
        };

        run_load_test(&config, move |_i| {
            let in_flight = Arc::clone(&in_flight_clone);
            let max_seen = Arc::clone(&max_clone);
            async move {
                let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(1)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                RequestOutcome {
                    latency: Duration::from_millis(1),
                    success: true,
                    error: None,
                }
            }
        })
        .await;

        // Max in-flight should not exceed concurrency (4) + small scheduling margin
        assert!(max_in_flight.load(Ordering::SeqCst) <= 5);
    }

    #[test]
    fn load_result_summary_format() {
        let result = LoadTestResult {
            name: "test".into(),
            total_requests: 100,
            successful: 90,
            failed: 10,
            total_time: Duration::from_secs(1),
            latencies: (0..100).map(|i| Duration::from_micros(i * 10)).collect(),
        };
        let s = result.summary();
        assert!(s.contains("test"));
        assert!(s.contains("100 reqs"));
        assert!(s.contains("err=10.0%"));
    }
}
