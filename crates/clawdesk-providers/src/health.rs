//! Provider health monitoring and circuit breaker.
//!
//! Tracks the health of each provider endpoint using a sliding window of
//! request outcomes and implements a three-state circuit breaker:
//!
//! ```text
//!   ┌────────┐  failure_threshold   ┌──────┐  half_open_after   ┌───────────┐
//!   │ Closed │ ──────────────────→  │ Open │ ────────────────→  │ HalfOpen  │
//!   │(normal)│                      │(fail)│                    │ (probing) │
//!   └────────┘  ←──────────────────  └──────┘  ←────────────────  └───────────┘
//!       ▲           probe succeeds                probe fails        │
//!       └────────────────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation — requests pass through.
    Closed,
    /// Provider is marked unhealthy — requests are rejected.
    Open,
    /// Allowing a single probe request to test recovery.
    HalfOpen,
}

/// Configuration for health monitoring.
#[derive(Debug, Clone)]
pub struct HealthConfig {
    /// Number of consecutive failures before opening the circuit.
    pub failure_threshold: u32,
    /// Duration after which an open circuit transitions to half-open.
    pub half_open_after: Duration,
    /// Number of successes in half-open state required to close the circuit.
    pub recovery_successes: u32,
    /// Sliding window size for tracking request outcomes.
    pub window_size: usize,
    /// Health check interval for background probing.
    pub check_interval: Duration,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            half_open_after: Duration::from_secs(30),
            recovery_successes: 2,
            window_size: 20,
            check_interval: Duration::from_secs(60),
        }
    }
}

/// Outcome of a single request.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    Success,
    Failure,
}

/// Per-provider health state.
#[derive(Debug)]
pub struct ProviderHealth {
    /// Provider identifier.
    pub provider_id: String,
    /// Current circuit breaker state.
    pub state: CircuitState,
    /// Sliding window of recent outcomes.
    window: Vec<(Instant, Outcome)>,
    /// Consecutive failures.
    consecutive_failures: u32,
    /// Consecutive successes (used in HalfOpen).
    consecutive_successes: u32,
    /// When the circuit was opened.
    opened_at: Option<Instant>,
    /// Last successful request time.
    pub last_success: Option<Instant>,
    /// Last failure time.
    pub last_failure: Option<Instant>,
    /// Total requests.
    pub total_requests: u64,
    /// Total failures.
    pub total_failures: u64,
    /// Configuration.
    config: HealthConfig,
}

impl ProviderHealth {
    fn new(provider_id: String, config: HealthConfig) -> Self {
        Self {
            provider_id,
            state: CircuitState::Closed,
            window: Vec::with_capacity(config.window_size),
            consecutive_failures: 0,
            consecutive_successes: 0,
            opened_at: None,
            last_success: None,
            last_failure: None,
            total_requests: 0,
            total_failures: 0,
            config,
        }
    }

    /// Record a successful request.
    pub fn record_success(&mut self) {
        let now = Instant::now();
        self.push_outcome(now, Outcome::Success);
        self.total_requests += 1;
        self.last_success = Some(now);
        self.consecutive_failures = 0;
        self.consecutive_successes += 1;

        match self.state {
            CircuitState::HalfOpen => {
                if self.consecutive_successes >= self.config.recovery_successes {
                    self.state = CircuitState::Closed;
                    self.opened_at = None;
                    self.consecutive_successes = 0;
                }
            }
            CircuitState::Open => {
                // Shouldn't happen, but if it does, transition to half-open.
                self.state = CircuitState::HalfOpen;
            }
            CircuitState::Closed => {}
        }
    }

    /// Record a failed request.
    pub fn record_failure(&mut self) {
        let now = Instant::now();
        self.push_outcome(now, Outcome::Failure);
        self.total_requests += 1;
        self.total_failures += 1;
        self.last_failure = Some(now);
        self.consecutive_failures += 1;
        self.consecutive_successes = 0;

        match self.state {
            CircuitState::Closed => {
                if self.consecutive_failures >= self.config.failure_threshold {
                    self.state = CircuitState::Open;
                    self.opened_at = Some(now);
                }
            }
            CircuitState::HalfOpen => {
                // Probe failed, re-open.
                self.state = CircuitState::Open;
                self.opened_at = Some(now);
            }
            CircuitState::Open => {}
        }
    }

    /// Check if a request should be allowed through.
    pub fn should_allow(&mut self) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if enough time has passed to try half-open.
                if let Some(opened) = self.opened_at {
                    if opened.elapsed() >= self.config.half_open_after {
                        self.state = CircuitState::HalfOpen;
                        self.consecutive_successes = 0;
                        return true;
                    }
                }
                false
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// Success rate over the sliding window (0.0–1.0).
    pub fn success_rate(&self) -> f64 {
        if self.window.is_empty() {
            return 1.0;
        }
        let successes = self
            .window
            .iter()
            .filter(|(_, o)| matches!(o, Outcome::Success))
            .count();
        successes as f64 / self.window.len() as f64
    }

    /// Maintain the sliding window.
    fn push_outcome(&mut self, at: Instant, outcome: Outcome) {
        if self.window.len() >= self.config.window_size {
            self.window.remove(0);
        }
        self.window.push((at, outcome));
    }
}

/// Aggregated health status for a provider.
#[derive(Debug, Clone)]
pub struct HealthStatus {
    pub provider_id: String,
    pub circuit_state: CircuitState,
    pub success_rate: f64,
    pub total_requests: u64,
    pub total_failures: u64,
    pub is_available: bool,
}

/// Registry-level health monitor that tracks all provider health states.
pub struct HealthMonitor {
    providers: HashMap<String, ProviderHealth>,
    config: HealthConfig,
}

impl HealthMonitor {
    /// Create a new health monitor with default config.
    pub fn new(config: HealthConfig) -> Self {
        Self {
            providers: HashMap::new(),
            config,
        }
    }

    /// Register a provider for health tracking.
    pub fn register(&mut self, provider_id: impl Into<String>) {
        let id = provider_id.into();
        self.providers
            .entry(id.clone())
            .or_insert_with(|| ProviderHealth::new(id, self.config.clone()));
    }

    /// Record a successful request for a provider.
    pub fn record_success(&mut self, provider_id: &str) {
        if let Some(health) = self.providers.get_mut(provider_id) {
            health.record_success();
        }
    }

    /// Record a failed request for a provider.
    pub fn record_failure(&mut self, provider_id: &str) {
        if let Some(health) = self.providers.get_mut(provider_id) {
            health.record_failure();
        }
    }

    /// Check if a provider should be allowed to serve requests.
    pub fn should_allow(&mut self, provider_id: &str) -> bool {
        self.providers
            .get_mut(provider_id)
            .map_or(true, |h| h.should_allow())
    }

    /// Get health status for a specific provider.
    pub fn status(&mut self, provider_id: &str) -> Option<HealthStatus> {
        self.providers.get_mut(provider_id).map(|h| HealthStatus {
            provider_id: h.provider_id.clone(),
            circuit_state: h.state,
            success_rate: h.success_rate(),
            total_requests: h.total_requests,
            total_failures: h.total_failures,
            is_available: h.should_allow(),
        })
    }

    /// Get all provider health statuses.
    pub fn all_statuses(&mut self) -> Vec<HealthStatus> {
        let ids: Vec<String> = self.providers.keys().cloned().collect();
        ids.iter().filter_map(|id| self.status(id)).collect()
    }

    /// Get IDs of all healthy (available) providers.
    pub fn healthy_providers(&mut self) -> Vec<String> {
        let ids: Vec<String> = self.providers.keys().cloned().collect();
        ids.into_iter()
            .filter(|id| self.should_allow(id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> HealthConfig {
        HealthConfig {
            failure_threshold: 3,
            half_open_after: Duration::from_millis(10),
            recovery_successes: 2,
            window_size: 10,
            check_interval: Duration::from_secs(60),
        }
    }

    #[test]
    fn circuit_stays_closed_on_success() {
        let mut health = ProviderHealth::new("test".into(), test_config());
        for _ in 0..10 {
            health.record_success();
        }
        assert_eq!(health.state, CircuitState::Closed);
        assert!(health.should_allow());
    }

    #[test]
    fn circuit_opens_after_threshold() {
        let mut health = ProviderHealth::new("test".into(), test_config());
        health.record_failure();
        health.record_failure();
        assert_eq!(health.state, CircuitState::Closed);

        health.record_failure(); // 3rd failure = threshold
        assert_eq!(health.state, CircuitState::Open);
        assert!(!health.should_allow());
    }

    #[test]
    fn circuit_transitions_to_half_open() {
        let mut health = ProviderHealth::new("test".into(), test_config());
        for _ in 0..3 {
            health.record_failure();
        }
        assert_eq!(health.state, CircuitState::Open);

        // Wait for half_open_after.
        std::thread::sleep(Duration::from_millis(15));

        assert!(health.should_allow()); // Should transition to HalfOpen.
        assert_eq!(health.state, CircuitState::HalfOpen);
    }

    #[test]
    fn half_open_recovers_on_success() {
        let mut health = ProviderHealth::new("test".into(), test_config());
        for _ in 0..3 {
            health.record_failure();
        }
        std::thread::sleep(Duration::from_millis(15));
        health.should_allow(); // Move to HalfOpen.

        health.record_success();
        assert_eq!(health.state, CircuitState::HalfOpen); // Need 2 successes.
        health.record_success();
        assert_eq!(health.state, CircuitState::Closed); // Recovered!
    }

    #[test]
    fn half_open_reopens_on_failure() {
        let mut health = ProviderHealth::new("test".into(), test_config());
        for _ in 0..3 {
            health.record_failure();
        }
        std::thread::sleep(Duration::from_millis(15));
        health.should_allow();
        assert_eq!(health.state, CircuitState::HalfOpen);

        health.record_failure();
        assert_eq!(health.state, CircuitState::Open);
    }

    #[test]
    fn monitor_tracks_multiple_providers() {
        let mut monitor = HealthMonitor::new(test_config());
        monitor.register("anthropic");
        monitor.register("openai");

        monitor.record_success("anthropic");
        for _ in 0..3 {
            monitor.record_failure("openai");
        }

        assert!(monitor.should_allow("anthropic"));
        assert!(!monitor.should_allow("openai"));

        let healthy = monitor.healthy_providers();
        assert!(healthy.contains(&"anthropic".to_string()));
        assert!(!healthy.contains(&"openai".to_string()));
    }

    #[test]
    fn success_rate_calculation() {
        let mut health = ProviderHealth::new("test".into(), test_config());
        health.record_success();
        health.record_success();
        health.record_failure();

        let rate = health.success_rate();
        assert!((rate - 0.666).abs() < 0.01);
    }
}
