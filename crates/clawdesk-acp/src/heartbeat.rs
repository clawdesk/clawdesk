//! Ping-Pong heartbeat protocol — periodic health checks for A2A agents.
//!
//! ## Protocol
//!
//! Each monitored agent is pinged on a configurable interval. The ping carries
//! a random `nonce`; a valid pong must echo the same nonce. If an agent fails
//! to respond within `timeout`, the failure is recorded and propagated to the
//! circuit breaker / health tracking system.
//!
//! ## Architecture
//!
//! ```text
//!    HeartbeatMonitor
//!         │
//!         ├─ ping (POST /a2a/ping {nonce}) ──▶ Agent B
//!         │◀─ pong {nonce} ──────────────────│
//!         │
//!         ├─ on success: record_success(agent_id)
//!         └─ on failure: record_failure(agent_id)
//! ```
//!
//! ## RTT tracking
//!
//! For each agent, the monitor keeps an exponentially weighted moving average
//! (EWMA) of the round-trip-time:
//!     RTT_ewma := α · RTT_sample + (1 − α) · RTT_ewma
//! with α = 0.3 (reacts to recent changes while smoothing noise).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Configuration for the heartbeat monitor.
#[derive(Debug, Clone)]
pub struct HeartbeatConfig {
    /// Interval between pings per agent.
    pub interval: Duration,
    /// Timeout for a single ping-pong exchange.
    pub timeout: Duration,
    /// Maximum consecutive failures before marking agent unhealthy.
    pub max_consecutive_failures: u32,
    /// EWMA smoothing factor for RTT (0 < α < 1).
    pub rtt_alpha: f64,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            timeout: Duration::from_secs(5),
            max_consecutive_failures: 3,
            rtt_alpha: 0.3,
        }
    }
}

/// Per-agent heartbeat state.
#[derive(Debug, Clone)]
pub struct AgentHeartbeat {
    /// Agent identifier.
    pub agent_id: String,
    /// Agent's A2A endpoint URL.
    pub endpoint_url: String,
    /// Whether the agent is currently considered healthy.
    pub healthy: bool,
    /// Consecutive failure count.
    pub consecutive_failures: u32,
    /// Total successful pings.
    pub total_successes: u64,
    /// Total failed pings.
    pub total_failures: u64,
    /// Last successful ping time.
    pub last_success: Option<DateTime<Utc>>,
    /// Last failure time.
    pub last_failure: Option<DateTime<Utc>>,
    /// Exponentially weighted moving average RTT in milliseconds.
    pub rtt_ewma_ms: f64,
    /// Last raw RTT sample in milliseconds.
    pub last_rtt_ms: Option<f64>,
    /// When the next ping is due.
    pub next_ping: Instant,
}

/// Ping request payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingPayload {
    pub nonce: u64,
    pub sender_id: String,
    pub timestamp: DateTime<Utc>,
}

/// Pong response payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PongPayload {
    pub nonce: u64,
    pub responder_id: String,
    pub timestamp: DateTime<Utc>,
}

/// Result of a single ping-pong exchange.
#[derive(Debug, Clone)]
pub enum PingResult {
    /// Pong received with matching nonce.
    Success { rtt_ms: f64 },
    /// Pong received but nonce mismatch.
    NonceMismatch { expected: u64, got: u64 },
    /// Request timed out.
    Timeout,
    /// Network or protocol error.
    Error(String),
}

/// Callback trait for heartbeat status changes.
///
/// Implementations wire heartbeat events to circuit breakers,
/// `AgentDirectory.update_health()`, metrics, etc.
pub trait HeartbeatCallback: Send + Sync {
    /// Called when an agent's health status changes.
    fn on_health_change(&self, agent_id: &str, healthy: bool);
    /// Called on each successful ping (for metrics).
    fn on_ping_success(&self, agent_id: &str, rtt_ms: f64);
    /// Called on each failed ping.
    fn on_ping_failure(&self, agent_id: &str, reason: &str);
}

/// No-op callback for testing / headless operation.
pub struct NoOpHeartbeatCallback;
impl HeartbeatCallback for NoOpHeartbeatCallback {
    fn on_health_change(&self, _agent_id: &str, _healthy: bool) {}
    fn on_ping_success(&self, _agent_id: &str, _rtt_ms: f64) {}
    fn on_ping_failure(&self, _agent_id: &str, _reason: &str) {}
}

/// Heartbeat monitor — tracks health of registered A2A agents.
///
/// The monitor doesn't own a background task; the caller drives it by
/// calling `tick()` periodically or `ping_agent()` directly. This
/// keeps the monitor testable without needing a tokio runtime for the
/// scheduling loop itself.
pub struct HeartbeatMonitor {
    config: HeartbeatConfig,
    agents: HashMap<String, AgentHeartbeat>,
    callback: Box<dyn HeartbeatCallback>,
}

impl HeartbeatMonitor {
    /// Create a new heartbeat monitor.
    pub fn new(config: HeartbeatConfig, callback: Box<dyn HeartbeatCallback>) -> Self {
        Self {
            config,
            agents: HashMap::new(),
            callback,
        }
    }

    /// Create with no-op callback (for tests).
    pub fn with_defaults() -> Self {
        Self::new(HeartbeatConfig::default(), Box::new(NoOpHeartbeatCallback))
    }

    /// Register an agent for health monitoring.
    pub fn register(&mut self, agent_id: &str, endpoint_url: &str) {
        let state = AgentHeartbeat {
            agent_id: agent_id.to_string(),
            endpoint_url: endpoint_url.to_string(),
            healthy: true, // assume healthy until proven otherwise
            consecutive_failures: 0,
            total_successes: 0,
            total_failures: 0,
            last_success: None,
            last_failure: None,
            rtt_ewma_ms: 0.0,
            last_rtt_ms: None,
            next_ping: Instant::now(),
        };
        info!(agent = agent_id, endpoint = endpoint_url, "registered for heartbeat");
        self.agents.insert(agent_id.to_string(), state);
    }

    /// Deregister an agent.
    pub fn deregister(&mut self, agent_id: &str) -> bool {
        self.agents.remove(agent_id).is_some()
    }

    /// Get the heartbeat state for an agent.
    pub fn get(&self, agent_id: &str) -> Option<&AgentHeartbeat> {
        self.agents.get(agent_id)
    }

    /// Get all registered agents.
    pub fn agents(&self) -> impl Iterator<Item = &AgentHeartbeat> {
        self.agents.values()
    }

    /// Get agents that are due for a ping now.
    pub fn agents_due(&self) -> Vec<String> {
        let now = Instant::now();
        self.agents
            .iter()
            .filter(|(_, state)| now >= state.next_ping)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Record the result of a ping for an agent.
    ///
    /// This updates health status, RTT metrics, and fires callbacks
    /// when health changes. Called by the async ping executor after
    /// each ping-pong exchange completes.
    pub fn record_result(&mut self, agent_id: &str, result: &PingResult) {
        let Some(state) = self.agents.get_mut(agent_id) else {
            return;
        };

        let was_healthy = state.healthy;

        match result {
            PingResult::Success { rtt_ms } => {
                state.consecutive_failures = 0;
                state.total_successes += 1;
                state.last_success = Some(Utc::now());
                state.last_rtt_ms = Some(*rtt_ms);
                state.healthy = true;

                // EWMA update
                if state.rtt_ewma_ms == 0.0 {
                    state.rtt_ewma_ms = *rtt_ms;
                } else {
                    let alpha = self.config.rtt_alpha;
                    state.rtt_ewma_ms = alpha * rtt_ms + (1.0 - alpha) * state.rtt_ewma_ms;
                }

                debug!(
                    agent = agent_id,
                    rtt_ms = rtt_ms,
                    ewma_ms = state.rtt_ewma_ms,
                    "ping success"
                );
                self.callback.on_ping_success(agent_id, *rtt_ms);
            }
            PingResult::NonceMismatch { expected, got } => {
                state.consecutive_failures += 1;
                state.total_failures += 1;
                state.last_failure = Some(Utc::now());
                let reason = format!("nonce mismatch: expected {}, got {}", expected, got);
                warn!(agent = agent_id, %reason, "ping nonce mismatch");
                self.callback.on_ping_failure(agent_id, &reason);
            }
            PingResult::Timeout => {
                state.consecutive_failures += 1;
                state.total_failures += 1;
                state.last_failure = Some(Utc::now());
                warn!(agent = agent_id, "ping timeout");
                self.callback.on_ping_failure(agent_id, "timeout");
            }
            PingResult::Error(e) => {
                state.consecutive_failures += 1;
                state.total_failures += 1;
                state.last_failure = Some(Utc::now());
                warn!(agent = agent_id, error = %e, "ping error");
                self.callback.on_ping_failure(agent_id, e);
            }
        }

        // Check for health state change
        let state = self.agents.get_mut(agent_id).unwrap();
        if state.consecutive_failures >= self.config.max_consecutive_failures {
            state.healthy = false;
        }

        // Schedule next ping
        state.next_ping = Instant::now() + self.config.interval;

        if was_healthy != state.healthy {
            info!(
                agent = agent_id,
                healthy = state.healthy,
                "agent health changed"
            );
            self.callback.on_health_change(agent_id, state.healthy);
        }
    }

    /// Build a ping payload for an agent.
    pub fn build_ping(&self, sender_id: &str) -> PingPayload {
        PingPayload {
            nonce: rand_nonce(),
            sender_id: sender_id.to_string(),
            timestamp: Utc::now(),
        }
    }

    /// Validate a pong against the original ping nonce.
    pub fn validate_pong(ping: &PingPayload, pong: &PongPayload) -> PingResult {
        if ping.nonce == pong.nonce {
            // RTT is caller-measured; this just validates the nonce
            PingResult::Success { rtt_ms: 0.0 }
        } else {
            PingResult::NonceMismatch {
                expected: ping.nonce,
                got: pong.nonce,
            }
        }
    }

    /// Get the monitor config.
    pub fn config(&self) -> &HeartbeatConfig {
        &self.config
    }

    /// Summary snapshot for observability.
    pub fn summary(&self) -> Vec<HeartbeatSummary> {
        self.agents
            .values()
            .map(|s| HeartbeatSummary {
                agent_id: s.agent_id.clone(),
                healthy: s.healthy,
                rtt_ewma_ms: s.rtt_ewma_ms,
                consecutive_failures: s.consecutive_failures,
                total_successes: s.total_successes,
                total_failures: s.total_failures,
            })
            .collect()
    }
}

/// Observability snapshot of an agent's heartbeat.
#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatSummary {
    pub agent_id: String,
    pub healthy: bool,
    pub rtt_ewma_ms: f64,
    pub consecutive_failures: u32,
    pub total_successes: u64,
    pub total_failures: u64,
}

/// Generate a random nonce using a fast non-crypto RNG.
///
/// Uses the xorshift64* algorithm seeded from the current time for
/// unpredictable-enough nonces without requiring `rand` crate.
fn rand_nonce() -> u64 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    // xorshift64*
    let mut x = seed;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    x.wrapping_mul(0x2545F4914F6CDD1D)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    struct TestCallback {
        health_changes: Arc<AtomicU32>,
        successes: Arc<AtomicU32>,
        failures: Arc<AtomicU32>,
    }

    impl TestCallback {
        fn new() -> (Self, Arc<AtomicU32>, Arc<AtomicU32>, Arc<AtomicU32>) {
            let h = Arc::new(AtomicU32::new(0));
            let s = Arc::new(AtomicU32::new(0));
            let f = Arc::new(AtomicU32::new(0));
            (
                Self {
                    health_changes: Arc::clone(&h),
                    successes: Arc::clone(&s),
                    failures: Arc::clone(&f),
                },
                h,
                s,
                f,
            )
        }
    }

    impl HeartbeatCallback for TestCallback {
        fn on_health_change(&self, _agent_id: &str, _healthy: bool) {
            self.health_changes.fetch_add(1, Ordering::Relaxed);
        }
        fn on_ping_success(&self, _agent_id: &str, _rtt_ms: f64) {
            self.successes.fetch_add(1, Ordering::Relaxed);
        }
        fn on_ping_failure(&self, _agent_id: &str, _reason: &str) {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn register_and_get() {
        let mut hb = HeartbeatMonitor::with_defaults();
        hb.register("agent-a", "http://a.local:18789");
        assert!(hb.get("agent-a").is_some());
        assert!(hb.get("agent-a").unwrap().healthy);
    }

    #[test]
    fn successful_pings_update_rtt() {
        let mut hb = HeartbeatMonitor::with_defaults();
        hb.register("a", "http://a.local");
        hb.record_result("a", &PingResult::Success { rtt_ms: 10.0 });
        let state = hb.get("a").unwrap();
        assert_eq!(state.total_successes, 1);
        assert_eq!(state.rtt_ewma_ms, 10.0); // first sample = raw value

        hb.record_result("a", &PingResult::Success { rtt_ms: 20.0 });
        let state = hb.get("a").unwrap();
        // EWMA: 0.3 * 20 + 0.7 * 10 = 13.0
        assert!((state.rtt_ewma_ms - 13.0).abs() < 0.001);
    }

    #[test]
    fn consecutive_failures_mark_unhealthy() {
        let config = HeartbeatConfig {
            max_consecutive_failures: 3,
            ..Default::default()
        };
        let (cb, health_changes, _, failures) = TestCallback::new();
        let mut hb = HeartbeatMonitor::new(config, Box::new(cb));
        hb.register("flaky", "http://flaky.local");

        // First 2 failures: still healthy
        hb.record_result("flaky", &PingResult::Timeout);
        assert!(hb.get("flaky").unwrap().healthy);
        hb.record_result("flaky", &PingResult::Timeout);
        assert!(hb.get("flaky").unwrap().healthy);

        // 3rd failure: trips to unhealthy
        hb.record_result("flaky", &PingResult::Timeout);
        assert!(!hb.get("flaky").unwrap().healthy);
        assert_eq!(health_changes.load(Ordering::Relaxed), 1);
        assert_eq!(failures.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn success_resets_failure_count() {
        let config = HeartbeatConfig {
            max_consecutive_failures: 3,
            ..Default::default()
        };
        let mut hb = HeartbeatMonitor::new(config, Box::new(NoOpHeartbeatCallback));
        hb.register("a", "http://a.local");

        hb.record_result("a", &PingResult::Timeout);
        hb.record_result("a", &PingResult::Timeout);
        // 2 failures, then a success resets
        hb.record_result("a", &PingResult::Success { rtt_ms: 5.0 });
        assert_eq!(hb.get("a").unwrap().consecutive_failures, 0);
        assert!(hb.get("a").unwrap().healthy);
    }

    #[test]
    fn nonce_validation() {
        let ping = PingPayload {
            nonce: 42,
            sender_id: "self".into(),
            timestamp: Utc::now(),
        };

        // Valid pong
        let pong_ok = PongPayload {
            nonce: 42,
            responder_id: "other".into(),
            timestamp: Utc::now(),
        };
        assert!(matches!(
            HeartbeatMonitor::validate_pong(&ping, &pong_ok),
            PingResult::Success { .. }
        ));

        // Invalid nonce
        let pong_bad = PongPayload {
            nonce: 99,
            responder_id: "other".into(),
            timestamp: Utc::now(),
        };
        assert!(matches!(
            HeartbeatMonitor::validate_pong(&ping, &pong_bad),
            PingResult::NonceMismatch { expected: 42, got: 99 }
        ));
    }

    #[test]
    fn deregister_removes_agent() {
        let mut hb = HeartbeatMonitor::with_defaults();
        hb.register("a", "http://a.local");
        assert!(hb.deregister("a"));
        assert!(hb.get("a").is_none());
        assert!(!hb.deregister("a")); // already gone
    }

    #[test]
    fn agents_due_respects_interval() {
        let config = HeartbeatConfig {
            interval: Duration::from_secs(3600), // long interval
            ..Default::default()
        };
        let mut hb = HeartbeatMonitor::new(config, Box::new(NoOpHeartbeatCallback));
        hb.register("a", "http://a.local");

        // Should be due immediately (next_ping = Instant::now())
        assert_eq!(hb.agents_due().len(), 1);

        // After recording a result, next_ping is pushed forward
        hb.record_result("a", &PingResult::Success { rtt_ms: 5.0 });
        assert!(hb.agents_due().is_empty());
    }

    #[test]
    fn summary_snapshot() {
        let mut hb = HeartbeatMonitor::with_defaults();
        hb.register("a", "http://a.local");
        hb.register("b", "http://b.local");
        hb.record_result("a", &PingResult::Success { rtt_ms: 10.0 });

        let summary = hb.summary();
        assert_eq!(summary.len(), 2);
        let a = summary.iter().find(|s| s.agent_id == "a").unwrap();
        assert!(a.healthy);
        assert_eq!(a.total_successes, 1);
    }

    #[test]
    fn rand_nonce_produces_nonzero() {
        // Not a perfect test, but ensures the generator works
        let n1 = rand_nonce();
        let n2 = rand_nonce();
        // Extremely unlikely to be both zero
        assert!(n1 != 0 || n2 != 0);
    }
}
