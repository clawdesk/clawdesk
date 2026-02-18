//! Metrics collection and aggregation.
//!
//! Tracks request counts, latencies, error rates, token usage, and system health.
//!
//! ## Windowed histograms
//!
//! `WindowedHistogram` maintains a circular buffer of epoch histograms, each
//! covering `epoch_secs` seconds. Queries like `mean()` and `percentile()` only
//! consider data within the rolling window (default: 60 epochs × 60s = 1 hour).
//! Old epochs are lazily rotated on `observe()`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// A tagged metric counter.
#[derive(Debug, Default)]
pub struct Counter {
    value: AtomicU64,
}

impl Counter {
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_by(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// A histogram for tracking distributions (latency, etc.).
#[derive(Debug)]
pub struct Histogram {
    /// Sorted bucket boundaries in microseconds.
    buckets: Vec<u64>,
    /// Count per bucket (one extra for +Inf).
    counts: Vec<AtomicU64>,
    sum: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    /// Create with default latency buckets (in ms).
    pub fn latency() -> Self {
        Self::new(vec![10, 50, 100, 250, 500, 1000, 2500, 5000, 10_000])
    }

    pub fn new(buckets: Vec<u64>) -> Self {
        let n = buckets.len() + 1; // +1 for +Inf bucket.
        let counts = (0..n).map(|_| AtomicU64::new(0)).collect();
        Self {
            buckets,
            counts,
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    /// Record a value.
    pub fn observe(&self, value: u64) {
        self.sum.fetch_add(value, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);

        let idx = self
            .buckets
            .iter()
            .position(|&b| value <= b)
            .unwrap_or(self.buckets.len());
        self.counts[idx].fetch_add(1, Ordering::Relaxed);
    }

    /// Get total count.
    pub fn total_count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Get mean value.
    pub fn mean(&self) -> f64 {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return 0.0;
        }
        self.sum.load(Ordering::Relaxed) as f64 / count as f64
    }

    /// Get approximate p50/p95/p99 using bucket interpolation.
    pub fn percentile(&self, p: f64) -> u64 {
        let total = self.total_count();
        if total == 0 {
            return 0;
        }
        let target = (total as f64 * p) as u64;
        let mut cumulative = 0u64;
        for (i, count) in self.counts.iter().enumerate() {
            cumulative += count.load(Ordering::Relaxed);
            if cumulative >= target {
                return if i < self.buckets.len() {
                    self.buckets[i]
                } else {
                    self.buckets.last().copied().unwrap_or(0) * 2
                };
            }
        }
        self.buckets.last().copied().unwrap_or(0) * 2
    }
}

/// A snapshot of all metrics at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub timestamp: DateTime<Utc>,
    pub uptime_secs: u64,
    pub counters: HashMap<String, u64>,
    pub latency_mean_ms: f64,
    pub latency_p50_ms: u64,
    pub latency_p95_ms: u64,
    pub latency_p99_ms: u64,
    pub channel_msg_counts: HashMap<String, u64>,
    pub provider_token_usage: HashMap<String, u64>,
}

// ---------------------------------------------------------------------------
// Windowed histogram — rolling-window aggregation via epoch ring buffer
// ---------------------------------------------------------------------------

/// A single epoch bucket holding histogram counts for one time slot.
struct EpochBucket {
    /// Count per histogram bucket (same layout as `Histogram`).
    counts: Vec<AtomicU64>,
    sum: AtomicU64,
    count: AtomicU64,
}

impl EpochBucket {
    fn new(num_buckets: usize) -> Self {
        Self {
            counts: (0..num_buckets).map(|_| AtomicU64::new(0)).collect(),
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    fn reset(&self) {
        for c in &self.counts {
            c.store(0, Ordering::Relaxed);
        }
        self.sum.store(0, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
    }
}

/// A histogram that only considers data within a rolling time window.
///
/// Internally keeps a ring of `num_epochs` epoch buckets, each covering
/// `epoch_secs` seconds. Calling `observe()` lazily rotates expired epochs.
/// Queries (`mean`, `percentile`, `total_count`) aggregate only live epochs.
pub struct WindowedHistogram {
    /// Sorted bucket boundaries (same semantics as `Histogram`).
    buckets: Vec<u64>,
    /// Ring buffer of epoch buckets.
    epochs: Vec<EpochBucket>,
    /// Number of epoch slots.
    num_epochs: usize,
    /// Duration of each epoch in seconds.
    epoch_secs: u64,
    /// Index of the current (head) epoch.
    head: AtomicU64,
    /// Monotonic instant when the histogram was created.
    start: std::time::Instant,
}

impl WindowedHistogram {
    /// Create a windowed histogram.
    ///
    /// * `buckets` — sorted bucket boundaries (e.g., latency in ms).
    /// * `num_epochs` — number of epoch slots in the ring (default: 60).
    /// * `epoch_secs` — duration of each epoch in seconds (default: 60).
    ///
    /// Total window = `num_epochs * epoch_secs` (default: 1 hour).
    pub fn new(buckets: Vec<u64>, num_epochs: usize, epoch_secs: u64) -> Self {
        let n = buckets.len() + 1; // +1 for +Inf bucket.
        let epochs: Vec<EpochBucket> = (0..num_epochs).map(|_| EpochBucket::new(n)).collect();
        Self {
            buckets,
            epochs,
            num_epochs,
            epoch_secs,
            head: AtomicU64::new(0),
            start: std::time::Instant::now(),
        }
    }

    /// Create a windowed latency histogram (default latency buckets, 1h window).
    pub fn latency() -> Self {
        Self::new(
            vec![10, 50, 100, 250, 500, 1000, 2500, 5000, 10_000],
            60,
            60,
        )
    }

    /// Current epoch index based on elapsed time.
    fn current_epoch(&self) -> u64 {
        if self.epoch_secs == 0 {
            return 0;
        }
        self.start.elapsed().as_secs() / self.epoch_secs
    }

    /// Rotate head forward, resetting any epochs we skip over.
    fn rotate(&self) {
        let now_epoch = self.current_epoch();
        let old_head = self.head.load(Ordering::Relaxed);
        if now_epoch <= old_head {
            return;
        }
        // CAS to claim the rotation.
        if self
            .head
            .compare_exchange(old_head, now_epoch, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return; // Another thread rotated; we'll catch up on next observe().
        }
        // Reset epochs between old_head+1 and now_epoch (inclusive).
        let start = (old_head + 1) as usize;
        let end = now_epoch as usize;
        for e in start..=end {
            self.epochs[e % self.num_epochs].reset();
        }
    }

    /// Record a value.
    pub fn observe(&self, value: u64) {
        self.rotate();
        let epoch = self.current_epoch() as usize % self.num_epochs;
        let bucket = &self.epochs[epoch];
        bucket.sum.fetch_add(value, Ordering::Relaxed);
        bucket.count.fetch_add(1, Ordering::Relaxed);
        let idx = self
            .buckets
            .iter()
            .position(|&b| value <= b)
            .unwrap_or(self.buckets.len());
        bucket.counts[idx].fetch_add(1, Ordering::Relaxed);
    }

    /// Get total count across all live epochs.
    pub fn total_count(&self) -> u64 {
        let now = self.current_epoch();
        let oldest = now.saturating_sub(self.num_epochs as u64 - 1);
        let mut total = 0u64;
        for e in oldest..=now {
            total += self.epochs[e as usize % self.num_epochs]
                .count
                .load(Ordering::Relaxed);
        }
        total
    }

    /// Get mean value across the window.
    pub fn mean(&self) -> f64 {
        let now = self.current_epoch();
        let oldest = now.saturating_sub(self.num_epochs as u64 - 1);
        let mut sum = 0u64;
        let mut count = 0u64;
        for e in oldest..=now {
            let bucket = &self.epochs[e as usize % self.num_epochs];
            sum += bucket.sum.load(Ordering::Relaxed);
            count += bucket.count.load(Ordering::Relaxed);
        }
        if count == 0 {
            0.0
        } else {
            sum as f64 / count as f64
        }
    }

    /// Get approximate percentile across the window.
    pub fn percentile(&self, p: f64) -> u64 {
        let total = self.total_count();
        if total == 0 {
            return 0;
        }
        let target = (total as f64 * p) as u64;
        let now = self.current_epoch();
        let oldest = now.saturating_sub(self.num_epochs as u64 - 1);
        let num_buckets = self.buckets.len() + 1;

        // Aggregate counts across all live epochs for each histogram bucket.
        let mut agg = vec![0u64; num_buckets];
        for e in oldest..=now {
            let epoch_bucket = &self.epochs[e as usize % self.num_epochs];
            for (i, c) in epoch_bucket.counts.iter().enumerate() {
                agg[i] += c.load(Ordering::Relaxed);
            }
        }

        let mut cumulative = 0u64;
        for (i, &c) in agg.iter().enumerate() {
            cumulative += c;
            if cumulative >= target {
                return if i < self.buckets.len() {
                    self.buckets[i]
                } else {
                    self.buckets.last().copied().unwrap_or(0) * 2
                };
            }
        }
        self.buckets.last().copied().unwrap_or(0) * 2
    }
}

/// Collects and aggregates metrics across the system.
pub struct MetricsCollector {
    start_time: std::time::Instant,
    /// Named counters.
    counters: Arc<RwLock<HashMap<String, Arc<Counter>>>>,
    /// Request latency histogram.
    pub latency: Arc<Histogram>,
    /// Per-channel message counts.
    channel_counts: Arc<RwLock<HashMap<String, Arc<Counter>>>>,
    /// Per-provider token usage.
    token_usage: Arc<RwLock<HashMap<String, Arc<Counter>>>>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            start_time: std::time::Instant::now(),
            counters: Arc::new(RwLock::new(HashMap::new())),
            latency: Arc::new(Histogram::latency()),
            channel_counts: Arc::new(RwLock::new(HashMap::new())),
            token_usage: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get or create a named counter.
    pub async fn counter(&self, name: &str) -> Arc<Counter> {
        let counters = self.counters.read().await;
        if let Some(c) = counters.get(name) {
            return c.clone();
        }
        drop(counters);
        let mut counters = self.counters.write().await;
        counters
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Counter::default()))
            .clone()
    }

    /// Record a request latency in milliseconds.
    pub fn record_latency(&self, ms: u64) {
        self.latency.observe(ms);
    }

    /// Increment the message count for a channel.
    pub async fn record_channel_message(&self, channel: &str) {
        let counts = self.channel_counts.read().await;
        if let Some(c) = counts.get(channel) {
            c.inc();
            return;
        }
        drop(counts);
        let mut counts = self.channel_counts.write().await;
        counts
            .entry(channel.to_string())
            .or_insert_with(|| Arc::new(Counter::default()))
            .inc();
    }

    /// Record token usage for a provider.
    pub async fn record_tokens(&self, provider: &str, tokens: u64) {
        let usage = self.token_usage.read().await;
        if let Some(c) = usage.get(provider) {
            c.inc_by(tokens);
            return;
        }
        drop(usage);
        let mut usage = self.token_usage.write().await;
        usage
            .entry(provider.to_string())
            .or_insert_with(|| Arc::new(Counter::default()))
            .inc_by(tokens);
    }

    /// Take a snapshot of current metrics.
    pub async fn snapshot(&self) -> MetricsSnapshot {
        let counters = self.counters.read().await;
        let counter_values: HashMap<String, u64> =
            counters.iter().map(|(k, v)| (k.clone(), v.get())).collect();
        drop(counters);

        let channels = self.channel_counts.read().await;
        let channel_msg_counts: HashMap<String, u64> =
            channels.iter().map(|(k, v)| (k.clone(), v.get())).collect();
        drop(channels);

        let tokens = self.token_usage.read().await;
        let provider_token_usage: HashMap<String, u64> =
            tokens.iter().map(|(k, v)| (k.clone(), v.get())).collect();
        drop(tokens);

        MetricsSnapshot {
            timestamp: Utc::now(),
            uptime_secs: self.start_time.elapsed().as_secs(),
            counters: counter_values,
            latency_mean_ms: self.latency.mean(),
            latency_p50_ms: self.latency.percentile(0.50),
            latency_p95_ms: self.latency.percentile(0.95),
            latency_p99_ms: self.latency.percentile(0.99),
            channel_msg_counts,
            provider_token_usage,
        }
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter() {
        let c = Counter::default();
        assert_eq!(c.get(), 0);
        c.inc();
        c.inc();
        assert_eq!(c.get(), 2);
        c.inc_by(10);
        assert_eq!(c.get(), 12);
    }

    #[test]
    fn test_histogram() {
        let h = Histogram::new(vec![10, 50, 100, 500]);
        h.observe(5);
        h.observe(25);
        h.observe(75);
        h.observe(200);
        h.observe(1000);
        assert_eq!(h.total_count(), 5);
        assert!(h.mean() > 0.0);
        // p50 should be in a middle bucket.
        let p50 = h.percentile(0.50);
        assert!(p50 <= 100);
    }

    #[tokio::test]
    async fn test_metrics_collector() {
        let m = MetricsCollector::new();

        let req_counter = m.counter("requests.total").await;
        req_counter.inc();
        req_counter.inc();

        m.record_latency(50);
        m.record_latency(100);
        m.record_latency(200);

        m.record_channel_message("telegram").await;
        m.record_channel_message("telegram").await;
        m.record_channel_message("discord").await;

        m.record_tokens("anthropic", 1500).await;
        m.record_tokens("openai", 800).await;

        let snap = m.snapshot().await;
        assert_eq!(snap.counters["requests.total"], 2);
        assert_eq!(snap.channel_msg_counts["telegram"], 2);
        assert_eq!(snap.channel_msg_counts["discord"], 1);
        assert_eq!(snap.provider_token_usage["anthropic"], 1500);
        assert!(snap.latency_mean_ms > 0.0);
    }

    #[test]
    fn test_windowed_histogram() {
        // Use a very large epoch_secs so all observations stay in epoch 0.
        let wh = WindowedHistogram::new(vec![10, 50, 100, 500], 4, 3600);
        wh.observe(5);
        wh.observe(25);
        wh.observe(75);
        wh.observe(200);
        wh.observe(1000);

        assert_eq!(wh.total_count(), 5);
        assert!(wh.mean() > 0.0);

        let p50 = wh.percentile(0.50);
        assert!(p50 <= 100);
        let p99 = wh.percentile(0.99);
        assert!(p99 >= 100);
    }
}
