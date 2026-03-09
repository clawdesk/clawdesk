//! Benchmark harness for measuring latency and throughput of ClawDesk subsystems.
//!
//! Provides a lightweight, dependency-free benchmark framework that captures
//! percentile distributions, throughput, and regression detection.

use std::time::{Duration, Instant};

/// Configuration for a benchmark run.
#[derive(Debug, Clone)]
pub struct BenchConfig {
    /// Number of warmup iterations (discarded).
    pub warmup: u64,
    /// Number of measured iterations.
    pub iterations: u64,
    /// Optional name for the benchmark.
    pub name: String,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            warmup: 100,
            iterations: 1_000,
            name: String::new(),
        }
    }
}

/// Result of a single benchmark.
#[derive(Debug, Clone)]
pub struct BenchResult {
    pub name: String,
    pub iterations: u64,
    pub total: Duration,
    /// Sorted sample durations.
    samples: Vec<Duration>,
}

impl BenchResult {
    /// Mean duration per iteration.
    pub fn mean(&self) -> Duration {
        self.total / self.iterations as u32
    }

    /// Throughput in operations per second.
    pub fn ops_per_sec(&self) -> f64 {
        self.iterations as f64 / self.total.as_secs_f64()
    }

    /// Percentile (0–100).
    pub fn percentile(&self, pct: f64) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let idx = ((pct / 100.0) * (self.samples.len() - 1) as f64).round() as usize;
        self.samples[idx.min(self.samples.len() - 1)]
    }

    /// Median (p50).
    pub fn median(&self) -> Duration {
        self.percentile(50.0)
    }

    /// p99 latency.
    pub fn p99(&self) -> Duration {
        self.percentile(99.0)
    }

    /// Print a human-readable summary.
    pub fn summary(&self) -> String {
        format!(
            "{}: {} iters, mean={:?}, median={:?}, p99={:?}, {:.0} ops/s",
            self.name,
            self.iterations,
            self.mean(),
            self.median(),
            self.p99(),
            self.ops_per_sec(),
        )
    }
}

/// Run a synchronous benchmark.
pub fn bench_sync<F>(config: &BenchConfig, mut f: F) -> BenchResult
where
    F: FnMut(),
{
    // Warmup
    for _ in 0..config.warmup {
        f();
    }

    let mut samples = Vec::with_capacity(config.iterations as usize);
    let start = Instant::now();

    for _ in 0..config.iterations {
        let iter_start = Instant::now();
        f();
        samples.push(iter_start.elapsed());
    }

    let total = start.elapsed();
    samples.sort();

    BenchResult {
        name: config.name.clone(),
        iterations: config.iterations,
        total,
        samples,
    }
}

/// Run an async benchmark.
pub async fn bench_async<F, Fut>(config: &BenchConfig, mut f: F) -> BenchResult
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // Warmup
    for _ in 0..config.warmup {
        f().await;
    }

    let mut samples = Vec::with_capacity(config.iterations as usize);
    let start = Instant::now();

    for _ in 0..config.iterations {
        let iter_start = Instant::now();
        f().await;
        samples.push(iter_start.elapsed());
    }

    let total = start.elapsed();
    samples.sort();

    BenchResult {
        name: config.name.clone(),
        iterations: config.iterations,
        total,
        samples,
    }
}

/// Regression check: compare two benchmark results.
#[derive(Debug)]
pub struct RegressionReport {
    pub baseline_name: String,
    pub candidate_name: String,
    /// Ratio of candidate mean / baseline mean. >1.0 = slower.
    pub mean_ratio: f64,
    /// Ratio of candidate p99 / baseline p99.
    pub p99_ratio: f64,
    /// Whether this is considered a regression.
    pub is_regression: bool,
}

/// Compare two benchmark results. `threshold` is the max acceptable slowdown (e.g., 1.10 = 10%).
pub fn check_regression(
    baseline: &BenchResult,
    candidate: &BenchResult,
    threshold: f64,
) -> RegressionReport {
    let mean_ratio = candidate.mean().as_secs_f64() / baseline.mean().as_secs_f64();
    let p99_ratio = candidate.p99().as_secs_f64() / baseline.p99().as_secs_f64();
    let is_regression = mean_ratio > threshold || p99_ratio > threshold;

    RegressionReport {
        baseline_name: baseline.name.clone(),
        candidate_name: candidate.name.clone(),
        mean_ratio,
        p99_ratio,
        is_regression,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_sync_captures_samples() {
        let config = BenchConfig {
            warmup: 5,
            iterations: 100,
            name: "noop".into(),
        };
        let result = bench_sync(&config, || {});
        assert_eq!(result.iterations, 100);
        assert!(result.ops_per_sec() > 0.0);
        assert!(result.median() <= result.p99());
    }

    #[test]
    fn percentile_bounds() {
        let config = BenchConfig {
            warmup: 0,
            iterations: 50,
            name: "sleep_test".into(),
        };
        let result = bench_sync(&config, || {
            std::hint::black_box(42);
        });
        assert!(result.percentile(0.0) <= result.percentile(100.0));
    }

    #[test]
    fn regression_check_ok() {
        let config = BenchConfig {
            warmup: 0,
            iterations: 100,
            name: "baseline".into(),
        };
        let baseline = bench_sync(&config, || { let _ = std::hint::black_box(1 + 1); });
        let candidate = bench_sync(
            &BenchConfig {
                name: "candidate".into(),
                ..config
            },
            || { let _ = std::hint::black_box(1 + 1); },
        );

        let report = check_regression(&baseline, &candidate, 2.0);
        assert!(!report.is_regression);
    }

    #[tokio::test]
    async fn bench_async_works() {
        let config = BenchConfig {
            warmup: 2,
            iterations: 20,
            name: "async_noop".into(),
        };
        let result = bench_async(&config, || async {}).await;
        assert_eq!(result.iterations, 20);
        assert!(result.total > Duration::ZERO);
    }

    #[test]
    fn summary_format() {
        let config = BenchConfig {
            warmup: 0,
            iterations: 10,
            name: "fmt_test".into(),
        };
        let result = bench_sync(&config, || {});
        let s = result.summary();
        assert!(s.contains("fmt_test"));
        assert!(s.contains("ops/s"));
    }
}
