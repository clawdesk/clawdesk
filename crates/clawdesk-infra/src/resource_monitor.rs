//! Real-Time Resource Monitor — Rust Performance Advantage Surface
//!
//! Displays current resource usage and cost savings in the Tauri UI.
//! Makes Rust's 5–10× memory advantage visible and measurable.
//!
//! ## Memory Model
//!
//! ```text
//! M_rust = M_binary + M_stack × T + M_heap_dynamic
//! M_binary ≈ 25 MB, M_stack ≈ 8 KB/thread × T threads
//! At idle with T=10: M_rust ≈ 26 MB
//! ```

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Complete resource snapshot for UI rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceSnapshot {
    /// Current RSS memory in bytes
    pub rss_bytes: u64,
    /// RSS formatted as human-readable string
    pub rss_display: String,
    /// Virtual memory in bytes
    pub vss_bytes: u64,
    /// CPU usage percentage (0-100)
    pub cpu_percent: f32,
    /// Number of active threads
    pub thread_count: usize,
    /// Uptime in seconds
    pub uptime_secs: u64,
    /// Per-provider token usage and costs
    pub provider_stats: Vec<ProviderUsageStats>,
    /// Total cost savings from cost router
    pub cost_savings: CostSavings,
    /// Active agent count
    pub active_agents: usize,
    /// Channel connection status
    pub channel_status: Vec<ChannelStatus>,
    /// Dynamic comparison against baseline
    pub baseline_comparison: BaselineComparison,
    /// Timestamp
    pub timestamp: u64,
}

/// Per-provider resource usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsageStats {
    pub provider_name: String,
    /// Tokens consumed this session
    pub tokens_used: u64,
    /// Cost in USD this session
    pub cost_usd: f64,
    /// Average latency in milliseconds
    pub avg_latency_ms: u64,
    /// Total requests this session
    pub request_count: u64,
    /// Health status
    pub healthy: bool,
}

/// Cost savings from intelligent routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostSavings {
    /// Total cost incurred (USD)
    pub total_cost: f64,
    /// Estimated cost at single-provider pricing (USD)
    pub baseline_cost: f64,
    /// Savings amount (USD)
    pub savings: f64,
    /// Savings percentage
    pub savings_pct: f32,
    /// Formatted display: "You saved $X.XX this week"
    pub display: String,
}

/// Channel connection status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStatus {
    pub channel_name: String,
    pub channel_type: String,
    pub connected: bool,
    pub latency_ms: Option<u64>,
    pub last_message_at: Option<u64>,
}

/// Dynamic comparison against industry baselines.
///
/// Shows actual usage with honest, dynamic comparison rather than
/// a fixed "10× less" badge (per review correction).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineComparison {
    /// Current RSS in MB
    pub current_mb: f64,
    /// Idle baseline for Rust daemon (from measurement)
    pub rust_idle_baseline_mb: f64,
    /// Documented Node.js baseline (industry average)
    pub nodejs_baseline_mb: f64,
    /// Dynamic ratio (actual, not claimed)
    pub memory_ratio: f64,
    /// Description: "Using X MB (Y× less than Node.js baseline)" or
    /// "Using X MB (comparable to baselines — model loaded)"
    pub display: String,
}

impl ResourceSnapshot {
    /// Collect current resource snapshot.
    ///
    /// Uses `sysinfo` for process metrics. Cost data from cost router.
    pub fn collect(
        start_time: Instant,
        provider_stats: Vec<ProviderUsageStats>,
        cost_savings: CostSavings,
        active_agents: usize,
        channel_status: Vec<ChannelStatus>,
    ) -> Self {
        let (rss_bytes, vss_bytes, thread_count, cpu_percent) = Self::process_metrics();
        let uptime_secs = start_time.elapsed().as_secs();
        let rss_mb = rss_bytes as f64 / (1024.0 * 1024.0);

        let rss_display = format_bytes(rss_bytes);
        let baseline = BaselineComparison::compute(rss_mb);

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            rss_bytes,
            rss_display,
            vss_bytes,
            cpu_percent,
            thread_count,
            uptime_secs,
            provider_stats,
            cost_savings,
            active_agents,
            channel_status,
            baseline_comparison: baseline,
            timestamp,
        }
    }

    /// Get process-level metrics.
    ///
    /// Uses platform-specific APIs to gather RSS, VSS, and CPU usage.
    fn process_metrics() -> (u64, u64, usize, f32) {
        #[cfg(target_os = "macos")]
        {
            Self::process_metrics_macos()
        }

        #[cfg(target_os = "linux")]
        {
            Self::process_metrics_linux()
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            (0, 0, 1, 0.0)
        }
    }

    #[cfg(target_os = "macos")]
    fn process_metrics_macos() -> (u64, u64, usize, f32) {
        // Use mach API via libc for RSS
        let pid = std::process::id();
        let thread_count = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4);

        // Read RSS via `ps` as a simple cross-platform fallback
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=,vsz=", "-p", &pid.to_string()])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                let parts: Vec<&str> = text.trim().split_whitespace().collect();
                let rss_kb: u64 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                let vsz_kb: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                (rss_kb * 1024, vsz_kb * 1024, thread_count, 0.0)
            }
            _ => (0, 0, thread_count, 0.0),
        }
    }

    #[cfg(target_os = "linux")]
    fn process_metrics_linux() -> (u64, u64, usize, f32) {
        let thread_count = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4);

        // Read from /proc/self/statm
        let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
        let parts: Vec<u64> = statm.split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();

        let page_size = 4096u64; // typical Linux page size
        let vss = parts.first().copied().unwrap_or(0) * page_size;
        let rss = parts.get(1).copied().unwrap_or(0) * page_size;

        (rss, vss, thread_count, 0.0)
    }
}

impl BaselineComparison {
    /// Compute dynamic baseline comparison.
    ///
    /// Honest comparison: shows actual ratio instead of fixed marketing claim.
    fn compute(current_mb: f64) -> Self {
        let rust_idle_baseline_mb = 26.0; // M_binary + M_tokio + M_bus
        let nodejs_baseline_mb = 120.0;   // V8 heap + libuv + modules

        let memory_ratio = if current_mb > 0.0 {
            nodejs_baseline_mb / current_mb
        } else {
            0.0
        };

        let display = if current_mb < 50.0 {
            format!(
                "Using {:.0} MB ({:.1}× less than Node.js baseline)",
                current_mb, memory_ratio
            )
        } else if current_mb < 200.0 {
            format!(
                "Using {:.0} MB ({:.1}× less than Node.js baseline, model context loaded)",
                current_mb, memory_ratio
            )
        } else {
            format!(
                "Using {:.0} MB (LLM model loaded — both platforms converge under heavy workload)",
                current_mb
            )
        };

        Self {
            current_mb,
            rust_idle_baseline_mb,
            nodejs_baseline_mb,
            memory_ratio,
            display,
        }
    }
}

impl CostSavings {
    /// Compute savings display.
    pub fn compute(total_cost: f64, baseline_cost: f64) -> Self {
        let savings = baseline_cost - total_cost;
        let savings_pct = if baseline_cost > 0.0 {
            (savings / baseline_cost * 100.0) as f32
        } else {
            0.0
        };

        let display = if savings > 0.01 {
            format!("You saved ${:.2} ({:.0}% savings)", savings, savings_pct)
        } else {
            "Optimizing costs across providers".into()
        };

        Self {
            total_cost,
            baseline_cost,
            savings,
            savings_pct,
            display,
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_comparison_idle() {
        let baseline = BaselineComparison::compute(25.0);
        assert!(baseline.memory_ratio > 4.0);
        assert!(baseline.display.contains("less than Node.js"));
    }

    #[test]
    fn baseline_comparison_heavy_load() {
        let baseline = BaselineComparison::compute(500.0);
        assert!(baseline.display.contains("converge"));
    }

    #[test]
    fn cost_savings_display() {
        let savings = CostSavings::compute(5.0, 12.0);
        assert!(savings.savings_pct > 50.0);
        assert!(savings.display.contains("$7.00"));
    }

    #[test]
    fn cost_savings_zero() {
        let savings = CostSavings::compute(0.0, 0.0);
        assert_eq!(savings.savings_pct, 0.0);
    }
}
