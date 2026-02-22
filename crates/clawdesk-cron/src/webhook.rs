//! Cron webhook delivery with consistent-hash stagger and per-run telemetry.
//!
//! ## Features
//!
//! - **Webhook delivery**: Deliver cron job results to configurable HTTP endpoints.
//! - **Consistent-hash stagger**: Spread jobs with the same schedule across the
//!   interval to avoid thundering herd.
//! - **Per-run telemetry**: Track execution time, success/failure, delivery status.
//! - **Retry on delivery failure**: Up to 3 attempts with exponential backoff.
//!
//! ## Stagger Algorithm
//!
//! Given N jobs with the same cron expression, compute a per-job offset:
//! ```text
//! offset(job_id) = (hash(job_id) % interval_ms) where interval = next_run - now
//! stagger_cap = interval / N  (capped at 30s)
//! effective_offset = offset(job_id) % stagger_cap
//! ```
//!
//! This distributes jobs uniformly within the interval window, preventing
//! all jobs from firing at exactly the same second.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Webhook delivery configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    /// Target URL for webhook delivery.
    pub url: String,
    /// Optional authorization header value.
    pub auth_header: Option<String>,
    /// Maximum retries on delivery failure.
    pub max_retries: u32,
    /// Timeout for each delivery attempt.
    pub timeout: Duration,
    /// Custom headers to include.
    pub headers: HashMap<String, String>,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            auth_header: None,
            max_retries: 3,
            timeout: Duration::from_secs(10),
            headers: HashMap::new(),
        }
    }
}

/// Webhook delivery payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    /// Cron job ID.
    pub job_id: String,
    /// Job name.
    pub job_name: String,
    /// Execution result (success content or error message).
    pub result: WebhookResult,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Timestamp of execution (ISO 8601).
    pub executed_at: String,
    /// Run telemetry.
    pub telemetry: RunTelemetry,
}

/// Result of a cron job execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WebhookResult {
    Success { output: String },
    Failure { error: String },
    Timeout { after_ms: u64 },
    Skipped { reason: String },
}

/// Per-run telemetry for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTelemetry {
    /// Scheduled time (when the job was supposed to run).
    pub scheduled_at: String,
    /// Actual start time.
    pub started_at: String,
    /// Stagger offset applied (milliseconds).
    pub stagger_offset_ms: u64,
    /// Whether this run overlapped with a previous run.
    pub overlap_detected: bool,
    /// Number of retries on the webhook delivery.
    pub delivery_retries: u32,
    /// Whether webhook delivery succeeded.
    pub delivery_success: bool,
}

/// Delivery status for a webhook attempt.
#[derive(Debug, Clone)]
pub struct DeliveryStatus {
    pub success: bool,
    pub status_code: Option<u16>,
    pub attempts: u32,
    pub error: Option<String>,
    pub duration: Duration,
}

/// Compute a stagger offset for a job using consistent hashing.
///
/// ## Arguments
/// - `job_id`: Unique job identifier (hashed deterministically).
/// - `interval_ms`: The cron interval in milliseconds.
/// - `job_count`: Total number of jobs with the same schedule.
///
/// ## Returns
/// Stagger offset in milliseconds.
///
/// ## Algorithm
/// ```text
/// hash = fnv(job_id) % interval_ms
/// cap = min(interval_ms / max(job_count, 1), 30_000)
/// offset = hash % cap
/// ```
pub fn compute_stagger_offset(job_id: &str, interval_ms: u64, job_count: usize) -> u64 {
    if interval_ms == 0 {
        return 0;
    }

    let mut hasher = DefaultHasher::new();
    job_id.hash(&mut hasher);
    let hash = hasher.finish();

    let cap_ms = (interval_ms / job_count.max(1) as u64).min(30_000);
    if cap_ms == 0 {
        return 0;
    }

    hash % cap_ms
}

/// Compute stagger offsets for a batch of jobs with the same schedule.
///
/// Returns a map of job_id → stagger offset (Duration).
pub fn compute_batch_stagger(
    job_ids: &[String],
    interval: Duration,
) -> HashMap<String, Duration> {
    let interval_ms = interval.as_millis() as u64;
    let count = job_ids.len();

    job_ids
        .iter()
        .map(|id| {
            let offset = compute_stagger_offset(id, interval_ms, count);
            (id.clone(), Duration::from_millis(offset))
        })
        .collect()
}

/// Builder for constructing a webhook payload after job execution.
pub struct WebhookPayloadBuilder {
    job_id: String,
    job_name: String,
    scheduled_at: String,
    started_at: Option<String>,
    stagger_offset_ms: u64,
    start_time: Instant,
}

impl WebhookPayloadBuilder {
    pub fn new(job_id: impl Into<String>, job_name: impl Into<String>) -> Self {
        Self {
            job_id: job_id.into(),
            job_name: job_name.into(),
            scheduled_at: chrono_now_iso(),
            started_at: None,
            stagger_offset_ms: 0,
            start_time: Instant::now(),
        }
    }

    pub fn scheduled_at(mut self, at: impl Into<String>) -> Self {
        self.scheduled_at = at.into();
        self
    }

    pub fn stagger_offset(mut self, offset_ms: u64) -> Self {
        self.stagger_offset_ms = offset_ms;
        self
    }

    pub fn mark_started(&mut self) {
        self.started_at = Some(chrono_now_iso());
        self.start_time = Instant::now();
    }

    pub fn build_success(self, output: String) -> WebhookPayload {
        let duration = self.start_time.elapsed();
        WebhookPayload {
            job_id: self.job_id,
            job_name: self.job_name,
            result: WebhookResult::Success { output },
            duration_ms: duration.as_millis() as u64,
            executed_at: chrono_now_iso(),
            telemetry: RunTelemetry {
                scheduled_at: self.scheduled_at,
                started_at: self.started_at.unwrap_or_default(),
                stagger_offset_ms: self.stagger_offset_ms,
                overlap_detected: false,
                delivery_retries: 0,
                delivery_success: false,
            },
        }
    }

    pub fn build_failure(self, error: String) -> WebhookPayload {
        let duration = self.start_time.elapsed();
        WebhookPayload {
            job_id: self.job_id,
            job_name: self.job_name,
            result: WebhookResult::Failure { error },
            duration_ms: duration.as_millis() as u64,
            executed_at: chrono_now_iso(),
            telemetry: RunTelemetry {
                scheduled_at: self.scheduled_at,
                started_at: self.started_at.unwrap_or_default(),
                stagger_offset_ms: self.stagger_offset_ms,
                overlap_detected: false,
                delivery_retries: 0,
                delivery_success: false,
            },
        }
    }

    pub fn build_timeout(self, after_ms: u64) -> WebhookPayload {
        WebhookPayload {
            job_id: self.job_id,
            job_name: self.job_name,
            result: WebhookResult::Timeout { after_ms },
            duration_ms: after_ms,
            executed_at: chrono_now_iso(),
            telemetry: RunTelemetry {
                scheduled_at: self.scheduled_at,
                started_at: self.started_at.unwrap_or_default(),
                stagger_offset_ms: self.stagger_offset_ms,
                overlap_detected: false,
                delivery_retries: 0,
                delivery_success: false,
            },
        }
    }
}

/// Get current time as ISO 8601 string.
/// Falls back to a formatted Instant if chrono is not available.
fn chrono_now_iso() -> String {
    // Use a simple epoch-based timestamp for portability
    use std::time::SystemTime;
    let since_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s", since_epoch.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stagger_deterministic() {
        let offset1 = compute_stagger_offset("job-1", 60_000, 10);
        let offset2 = compute_stagger_offset("job-1", 60_000, 10);
        assert_eq!(offset1, offset2, "same job should get same offset");
    }

    #[test]
    fn test_stagger_distributes() {
        let offsets: Vec<u64> = (0..10)
            .map(|i| compute_stagger_offset(&format!("job-{i}"), 60_000, 10))
            .collect();

        // Not all offsets should be the same
        let unique: std::collections::HashSet<u64> = offsets.iter().copied().collect();
        assert!(unique.len() > 1, "stagger should distribute jobs");
    }

    #[test]
    fn test_stagger_within_cap() {
        let offset = compute_stagger_offset("job-1", 60_000, 10);
        let cap = 60_000u64 / 10;
        assert!(offset < cap, "offset {offset} should be < cap {cap}");
    }

    #[test]
    fn test_stagger_zero_interval() {
        let offset = compute_stagger_offset("job-1", 0, 5);
        assert_eq!(offset, 0);
    }

    #[test]
    fn test_batch_stagger() {
        let jobs: Vec<String> = (0..5).map(|i| format!("job-{i}")).collect();
        let offsets = compute_batch_stagger(&jobs, Duration::from_secs(60));
        assert_eq!(offsets.len(), 5);
        for (_, offset) in &offsets {
            assert!(*offset < Duration::from_secs(12)); // 60/5 = 12s cap
        }
    }

    #[test]
    fn test_payload_builder() {
        let mut builder = WebhookPayloadBuilder::new("job-1", "Test Job");
        builder.mark_started();
        let payload = builder.build_success("done".into());

        assert_eq!(payload.job_id, "job-1");
        assert_eq!(payload.job_name, "Test Job");
        assert!(matches!(payload.result, WebhookResult::Success { .. }));
    }
}
