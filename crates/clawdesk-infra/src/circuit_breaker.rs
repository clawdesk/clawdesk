//! Generic circuit breaker for all external dependencies.
//!
//! Unlike `clawdesk-providers/src/health.rs` which is provider-specific,
//! this module provides a reusable circuit breaker that can protect any
//! external dependency: embedding APIs, channel platforms, SochDB, MCP
//! servers, etc.
//!
//! ## State Machine
//!
//! ```text
//!  CLOSED ──(k failures in window w)──→ OPEN
//!  OPEN   ──(timeout t elapsed)──────→ HALF_OPEN
//!  HALF_OPEN ──(probe success)───────→ CLOSED
//!  HALF_OPEN ──(probe failure)───────→ OPEN (with increased timeout)
//! ```
//!
//! ## Performance
//!
//! - `should_allow()`: O(1) — no allocation, no lock on read path
//! - `record_success/failure()`: O(1) amortized via circular buffer
//! - Sliding window: O(1) per update with O(w) memory
//!
//! ## Optimal Parameters
//!
//! Default: w = 60s, k = 5, t = 30s with exponential increase.
//! E[MTTR] with circuit breaker = t_detect + t_failover + t_probe ≈ 37s

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CircuitState {
    /// Normal operation — all requests pass through.
    Closed,
    /// Dependency is unhealthy — requests are rejected immediately.
    Open,
    /// Allowing a single probe to test recovery.
    HalfOpen,
}

/// Configuration for a circuit breaker instance.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures in the sliding window before opening.
    pub failure_threshold: u32,
    /// Duration after which an open circuit transitions to half-open.
    pub half_open_timeout: Duration,
    /// Maximum half-open timeout after repeated failures.
    pub max_half_open_timeout: Duration,
    /// Number of consecutive successes in half-open to close the circuit.
    pub recovery_threshold: u32,
    /// Sliding window size (number of slots).
    pub window_size: usize,
    /// Minimum success rate to maintain closed state (0.0–1.0).
    pub min_success_rate: f64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            half_open_timeout: Duration::from_secs(30),
            max_half_open_timeout: Duration::from_secs(300),
            recovery_threshold: 2,
            window_size: 20,
            min_success_rate: 0.5,
        }
    }
}

/// Type of external dependency being protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DependencyKind {
    /// LLM provider (Claude, GPT, etc.).
    LlmProvider,
    /// Embedding API (OpenAI, Voyage, etc.).
    EmbeddingApi,
    /// Channel platform (Slack, Discord, Telegram, etc.).
    ChannelPlatform,
    /// Vector database (SochDB).
    VectorDatabase,
    /// MCP tool server.
    McpServer,
    /// External webhook.
    Webhook,
    /// Custom dependency.
    Custom,
}

/// A degradation strategy when a dependency is unavailable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DegradationStrategy {
    /// Fail the request immediately.
    FailFast,
    /// Use a fallback (e.g., Claude down → use GPT).
    Fallback(String),
    /// Serve from cache.
    ServeCache,
    /// Queue the request for later.
    QueueForLater,
    /// Use a degraded alternative (e.g., keyword search instead of vector).
    Degrade(String),
}

// ─────────────────────────────────────────────────────────────────────────────
// Circuit Breaker
// ─────────────────────────────────────────────────────────────────────────────

/// Single circuit breaker instance for one external dependency.
pub struct CircuitBreaker {
    name: String,
    kind: DependencyKind,
    config: CircuitBreakerConfig,
    state: CircuitState,
    /// Circular buffer of request outcomes (true = success).
    window: Vec<bool>,
    window_pos: usize,
    window_count: usize,
    /// Consecutive failure count.
    consecutive_failures: u32,
    /// Consecutive success count in half-open state.
    half_open_successes: u32,
    /// When the circuit was opened.
    opened_at: Option<Instant>,
    /// Current half-open timeout (increases with repeated failures).
    current_timeout: Duration,
    /// Total failure count since creation.
    total_failures: u64,
    /// Total success count since creation.
    total_successes: u64,
    /// When the last state transition happened.
    last_transition: Instant,
    /// Degradation strategy when open.
    degradation: DegradationStrategy,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    pub fn new(
        name: impl Into<String>,
        kind: DependencyKind,
        config: CircuitBreakerConfig,
        degradation: DegradationStrategy,
    ) -> Self {
        let window_size = config.window_size;
        let timeout = config.half_open_timeout;
        Self {
            name: name.into(),
            kind,
            config,
            state: CircuitState::Closed,
            window: vec![true; window_size],
            window_pos: 0,
            window_count: 0,
            consecutive_failures: 0,
            half_open_successes: 0,
            opened_at: None,
            current_timeout: timeout,
            total_failures: 0,
            total_successes: 0,
            last_transition: Instant::now(),
            degradation,
        }
    }

    /// Check if a request should be allowed through, O(1).
    pub fn should_allow(&mut self) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if enough time has passed to try half-open
                if let Some(opened_at) = self.opened_at {
                    if opened_at.elapsed() >= self.current_timeout {
                        self.transition_to(CircuitState::HalfOpen);
                        true // Allow one probe
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                // Only allow one probe at a time (already allowed by transition)
                false
            }
        }
    }

    /// Record a successful request, O(1) amortized.
    pub fn record_success(&mut self) {
        self.total_successes += 1;
        self.consecutive_failures = 0;
        self.record_outcome(true);

        match self.state {
            CircuitState::HalfOpen => {
                self.half_open_successes += 1;
                if self.half_open_successes >= self.config.recovery_threshold {
                    self.transition_to(CircuitState::Closed);
                    // Reset timeout on successful recovery
                    self.current_timeout = self.config.half_open_timeout;
                }
            }
            CircuitState::Closed => {}
            CircuitState::Open => {}
        }
    }

    /// Record a failed request, O(1) amortized.
    pub fn record_failure(&mut self) {
        self.total_failures += 1;
        self.consecutive_failures += 1;
        self.record_outcome(false);

        match self.state {
            CircuitState::Closed => {
                if self.consecutive_failures >= self.config.failure_threshold {
                    self.transition_to(CircuitState::Open);
                } else if self.window_count >= self.config.window_size {
                    let success_rate = self.success_rate();
                    if success_rate < self.config.min_success_rate {
                        self.transition_to(CircuitState::Open);
                    }
                }
            }
            CircuitState::HalfOpen => {
                // Probe failed — go back to open with increased timeout
                self.current_timeout = std::cmp::min(
                    self.current_timeout * 2,
                    self.config.max_half_open_timeout,
                );
                self.transition_to(CircuitState::Open);
            }
            CircuitState::Open => {}
        }
    }

    /// Current circuit state.
    pub fn state(&self) -> CircuitState {
        self.state
    }

    /// Name of the protected dependency.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Kind of dependency.
    pub fn kind(&self) -> DependencyKind {
        self.kind
    }

    /// Get the degradation strategy.
    pub fn degradation_strategy(&self) -> &DegradationStrategy {
        &self.degradation
    }

    /// Success rate in the current window.
    pub fn success_rate(&self) -> f64 {
        if self.window_count == 0 {
            return 1.0;
        }
        let successes = self.window.iter().take(self.window_count).filter(|&&s| s).count();
        successes as f64 / self.window_count as f64
    }

    /// Status summary for monitoring.
    pub fn status(&self) -> CircuitStatus {
        CircuitStatus {
            name: self.name.clone(),
            kind: self.kind,
            state: self.state,
            success_rate: self.success_rate(),
            total_successes: self.total_successes,
            total_failures: self.total_failures,
            consecutive_failures: self.consecutive_failures,
            time_in_state: self.last_transition.elapsed(),
        }
    }

    fn record_outcome(&mut self, success: bool) {
        if self.window_count < self.config.window_size {
            self.window_count += 1;
        }
        self.window[self.window_pos] = success;
        self.window_pos = (self.window_pos + 1) % self.config.window_size;
    }

    fn transition_to(&mut self, new_state: CircuitState) {
        let old = self.state;
        self.state = new_state;
        self.last_transition = Instant::now();

        match new_state {
            CircuitState::Open => {
                self.opened_at = Some(Instant::now());
                self.half_open_successes = 0;
                warn!(
                    name = %self.name,
                    kind = ?self.kind,
                    failures = self.consecutive_failures,
                    "Circuit breaker OPENED"
                );
            }
            CircuitState::HalfOpen => {
                info!(
                    name = %self.name,
                    timeout = ?self.current_timeout,
                    "Circuit breaker HALF-OPEN — sending probe"
                );
            }
            CircuitState::Closed => {
                self.opened_at = None;
                self.consecutive_failures = 0;
                self.half_open_successes = 0;
                info!(name = %self.name, "Circuit breaker CLOSED — recovered");
            }
        }

        debug!(
            name = %self.name,
            from = ?old,
            to = ?new_state,
            "Circuit breaker state transition"
        );
    }
}

/// Status snapshot for monitoring dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct CircuitStatus {
    pub name: String,
    pub kind: DependencyKind,
    pub state: CircuitState,
    pub success_rate: f64,
    pub total_successes: u64,
    pub total_failures: u64,
    pub consecutive_failures: u32,
    #[serde(skip)]
    pub time_in_state: Duration,
}

// ─────────────────────────────────────────────────────────────────────────────
// Circuit Breaker Registry
// ─────────────────────────────────────────────────────────────────────────────

/// Registry of all circuit breakers for external dependencies.
///
/// Provides a centralized view of system health and degradation state.
pub struct CircuitBreakerRegistry {
    breakers: RwLock<HashMap<String, CircuitBreaker>>,
}

impl CircuitBreakerRegistry {
    pub fn new() -> Self {
        Self {
            breakers: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new circuit breaker.
    pub async fn register(
        &self,
        name: impl Into<String>,
        kind: DependencyKind,
        config: CircuitBreakerConfig,
        degradation: DegradationStrategy,
    ) {
        let name = name.into();
        let breaker = CircuitBreaker::new(name.clone(), kind, config, degradation);
        self.breakers.write().await.insert(name, breaker);
    }

    /// Check if a named dependency should allow a request.
    pub async fn should_allow(&self, name: &str) -> bool {
        let mut breakers = self.breakers.write().await;
        breakers
            .get_mut(name)
            .map(|b| b.should_allow())
            .unwrap_or(true) // Unknown dependencies default to allow
    }

    /// Record success for a named dependency.
    pub async fn record_success(&self, name: &str) {
        let mut breakers = self.breakers.write().await;
        if let Some(b) = breakers.get_mut(name) {
            b.record_success();
        }
    }

    /// Record failure for a named dependency.
    pub async fn record_failure(&self, name: &str) {
        let mut breakers = self.breakers.write().await;
        if let Some(b) = breakers.get_mut(name) {
            b.record_failure();
        }
    }

    /// Get the degradation strategy for a dependency that's currently open.
    pub async fn degradation_for(&self, name: &str) -> Option<DegradationStrategy> {
        let breakers = self.breakers.read().await;
        breakers.get(name).and_then(|b| {
            if b.state() == CircuitState::Open {
                Some(b.degradation_strategy().clone())
            } else {
                None
            }
        })
    }

    /// Get status of all circuit breakers.
    pub async fn all_statuses(&self) -> Vec<CircuitStatus> {
        let breakers = self.breakers.read().await;
        breakers.values().map(|b| b.status()).collect()
    }

    /// Get names of all healthy (closed) dependencies.
    pub async fn healthy_dependencies(&self) -> Vec<String> {
        let breakers = self.breakers.read().await;
        breakers
            .iter()
            .filter(|(_, b)| b.state() == CircuitState::Closed)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Get names of all unhealthy (open) dependencies.
    pub async fn unhealthy_dependencies(&self) -> Vec<String> {
        let breakers = self.breakers.read().await;
        breakers
            .iter()
            .filter(|(_, b)| b.state() == CircuitState::Open)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Whether the system is fully healthy (all breakers closed).
    pub async fn is_fully_healthy(&self) -> bool {
        let breakers = self.breakers.read().await;
        breakers.values().all(|b| b.state() == CircuitState::Closed)
    }
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_threshold: 3,
            half_open_timeout: Duration::from_millis(50),
            max_half_open_timeout: Duration::from_secs(1),
            recovery_threshold: 2,
            window_size: 10,
            min_success_rate: 0.5,
        }
    }

    #[test]
    fn starts_closed() {
        let cb = CircuitBreaker::new(
            "test",
            DependencyKind::LlmProvider,
            test_config(),
            DegradationStrategy::FailFast,
        );
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn opens_after_threshold_failures() {
        let mut cb = CircuitBreaker::new(
            "test",
            DependencyKind::LlmProvider,
            test_config(),
            DegradationStrategy::FailFast,
        );
        assert!(cb.should_allow());

        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn open_rejects_requests() {
        let mut cb = CircuitBreaker::new(
            "test",
            DependencyKind::LlmProvider,
            test_config(),
            DegradationStrategy::FailFast,
        );
        for _ in 0..3 {
            cb.record_failure();
        }
        assert!(!cb.should_allow());
    }

    #[test]
    fn half_open_after_timeout() {
        let mut cb = CircuitBreaker::new(
            "test",
            DependencyKind::EmbeddingApi,
            test_config(),
            DegradationStrategy::Degrade("keyword_search".to_string()),
        );
        for _ in 0..3 {
            cb.record_failure();
        }
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(60));
        assert!(cb.should_allow());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn recovery_closes_circuit() {
        let mut cb = CircuitBreaker::new(
            "test",
            DependencyKind::ChannelPlatform,
            test_config(),
            DegradationStrategy::QueueForLater,
        );
        for _ in 0..3 {
            cb.record_failure();
        }
        std::thread::sleep(Duration::from_millis(60));
        cb.should_allow(); // transition to half-open

        cb.record_success();
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn half_open_failure_reopens_with_increased_timeout() {
        let mut cb = CircuitBreaker::new(
            "test",
            DependencyKind::VectorDatabase,
            test_config(),
            DegradationStrategy::ServeCache,
        );
        let initial_timeout = cb.current_timeout;

        for _ in 0..3 {
            cb.record_failure();
        }
        std::thread::sleep(Duration::from_millis(60));
        cb.should_allow();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(cb.current_timeout > initial_timeout);
    }

    #[test]
    fn success_resets_consecutive_failures() {
        let mut cb = CircuitBreaker::new(
            "test",
            DependencyKind::LlmProvider,
            test_config(),
            DegradationStrategy::FailFast,
        );
        cb.record_failure();
        cb.record_failure();
        cb.record_success(); // resets consecutive count
        cb.record_failure();
        // Only 1 consecutive failure, threshold is 3
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn registry_basic_operations() {
        let registry = CircuitBreakerRegistry::new();
        registry
            .register(
                "claude",
                DependencyKind::LlmProvider,
                test_config(),
                DegradationStrategy::Fallback("gpt-4".to_string()),
            )
            .await;

        assert!(registry.should_allow("claude").await);
        assert!(registry.is_fully_healthy().await);

        for _ in 0..3 {
            registry.record_failure("claude").await;
        }
        assert!(!registry.should_allow("claude").await);
        assert!(!registry.is_fully_healthy().await);

        let unhealthy = registry.unhealthy_dependencies().await;
        assert!(unhealthy.contains(&"claude".to_string()));
    }
}
