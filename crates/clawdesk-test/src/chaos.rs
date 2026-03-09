//! Chaos test helpers — fault injection utilities for resilience testing.
//!
//! Provides building blocks for chaos engineering: random failure injection,
//! latency injection, and partition simulation. Used in integration tests to
//! verify the system degrades gracefully under adverse conditions.
//!
//! # Example
//!
//! ```ignore
//! let injector = FaultInjector::new(FaultConfig {
//!     failure_rate: 0.1,   // 10% failures
//!     latency_range: Some(Duration::from_millis(50)..Duration::from_millis(200)),
//!     ..Default::default()
//! });
//!
//! // In test:
//! if injector.should_fail() {
//!     return Err(ProviderError::server_error("chaos", 500));
//! }
//! injector.inject_latency().await;
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Configuration for fault injection.
#[derive(Debug, Clone)]
pub struct FaultConfig {
    /// Probability of injecting a failure (0.0–1.0).
    pub failure_rate: f64,
    /// Latency range to inject (None = no latency injection).
    pub latency_range: Option<(Duration, Duration)>,
    /// Maximum number of failures to inject (None = unlimited).
    pub max_failures: Option<u64>,
    /// Whether to simulate connection resets.
    pub connection_reset: bool,
    /// Probability of simulating a timeout.
    pub timeout_rate: f64,
}

impl Default for FaultConfig {
    fn default() -> Self {
        Self {
            failure_rate: 0.0,
            latency_range: None,
            max_failures: None,
            connection_reset: false,
            timeout_rate: 0.0,
        }
    }
}

/// Type of fault injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultType {
    /// Server error (500-class).
    ServerError,
    /// Connection reset / network failure.
    ConnectionReset,
    /// Request timeout.
    Timeout,
    /// Added latency.
    Latency,
}

/// Record of an injected fault for test assertions.
#[derive(Debug, Clone)]
pub struct FaultRecord {
    pub fault_type: FaultType,
    pub sequence: u64,
}

/// Fault injector for chaos testing.
pub struct FaultInjector {
    config: FaultConfig,
    call_count: AtomicU64,
    failure_count: AtomicU64,
}

impl FaultInjector {
    /// Create a new fault injector.
    pub fn new(config: FaultConfig) -> Self {
        Self {
            config,
            call_count: AtomicU64::new(0),
            failure_count: AtomicU64::new(0),
        }
    }

    /// Create a no-op injector (no faults).
    pub fn noop() -> Self {
        Self::new(FaultConfig::default())
    }

    /// Check if this call should fail.
    pub fn should_fail(&self) -> bool {
        let seq = self.call_count.fetch_add(1, Ordering::Relaxed);

        // Check max failures.
        if let Some(max) = self.config.max_failures {
            if self.failure_count.load(Ordering::Relaxed) >= max {
                return false;
            }
        }

        // Use the sequence number to produce pseudo-random decisions.
        let rand = cheap_hash(seq);
        let threshold = (self.config.failure_rate * u64::MAX as f64) as u64;

        if rand < threshold {
            self.failure_count.fetch_add(1, Ordering::Relaxed);
            return true;
        }

        false
    }

    /// Determine the type of fault to inject (when `should_fail()` returns true).
    pub fn fault_type(&self) -> FaultType {
        let seq = self.call_count.load(Ordering::Relaxed);

        if self.config.connection_reset && cheap_hash(seq.wrapping_mul(7)) % 3 == 0 {
            FaultType::ConnectionReset
        } else if self.config.timeout_rate > 0.0 {
            let rand = cheap_hash(seq.wrapping_mul(13));
            let threshold = (self.config.timeout_rate * u64::MAX as f64) as u64;
            if rand < threshold {
                return FaultType::Timeout;
            }
            FaultType::ServerError
        } else {
            FaultType::ServerError
        }
    }

    /// Get the latency to inject for this call.
    pub fn latency_to_inject(&self) -> Option<Duration> {
        let (min, max) = self.config.latency_range.as_ref()?;
        let seq = self.call_count.load(Ordering::Relaxed);
        let rand = cheap_hash(seq.wrapping_mul(31));
        let range_ms = max.as_millis().saturating_sub(min.as_millis());
        if range_ms == 0 {
            return Some(*min);
        }
        let offset = (rand % range_ms as u64) as u64;
        Some(*min + Duration::from_millis(offset))
    }

    /// Inject latency (async, actually sleeps).
    pub async fn inject_latency(&self) {
        if let Some(delay) = self.latency_to_inject() {
            tokio::time::sleep(delay).await;
        }
    }

    /// Get counters for assertions.
    pub fn stats(&self) -> (u64, u64) {
        (
            self.call_count.load(Ordering::Relaxed),
            self.failure_count.load(Ordering::Relaxed),
        )
    }

    /// Reset counters.
    pub fn reset(&self) {
        self.call_count.store(0, Ordering::Relaxed);
        self.failure_count.store(0, Ordering::Relaxed);
    }
}

/// Simple non-cryptographic hash for deterministic pseudo-randomness in tests.
fn cheap_hash(x: u64) -> u64 {
    let mut h = x;
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51afd7ed558ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
    h ^= h >> 33;
    h
}

/// Test scenario builder for chaos testing patterns.
pub struct ChaosScenario {
    pub name: String,
    pub injectors: Vec<(String, FaultInjector)>,
}

impl ChaosScenario {
    /// Create a "provider brownout" scenario where a single provider has intermittent failures.
    pub fn provider_brownout(provider_name: &str, failure_rate: f64) -> Self {
        Self {
            name: format!("{}_brownout", provider_name),
            injectors: vec![(
                provider_name.to_string(),
                FaultInjector::new(FaultConfig {
                    failure_rate,
                    latency_range: Some((
                        Duration::from_millis(100),
                        Duration::from_millis(500),
                    )),
                    ..Default::default()
                }),
            )],
        }
    }

    /// Create a "network partition" scenario where all providers fail simultaneously.
    pub fn network_partition(providers: &[&str]) -> Self {
        Self {
            name: "network_partition".into(),
            injectors: providers
                .iter()
                .map(|p| {
                    (
                        p.to_string(),
                        FaultInjector::new(FaultConfig {
                            failure_rate: 1.0,
                            connection_reset: true,
                            ..Default::default()
                        }),
                    )
                })
                .collect(),
        }
    }

    /// Create a "slow provider" scenario with high latency.
    pub fn slow_provider(provider_name: &str) -> Self {
        Self {
            name: format!("{}_slow", provider_name),
            injectors: vec![(
                provider_name.to_string(),
                FaultInjector::new(FaultConfig {
                    latency_range: Some((
                        Duration::from_secs(2),
                        Duration::from_secs(10),
                    )),
                    ..Default::default()
                }),
            )],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_failure_rate_never_fails() {
        let injector = FaultInjector::new(FaultConfig {
            failure_rate: 0.0,
            ..Default::default()
        });
        for _ in 0..1000 {
            assert!(!injector.should_fail());
        }
    }

    #[test]
    fn full_failure_rate_always_fails() {
        let injector = FaultInjector::new(FaultConfig {
            failure_rate: 1.0,
            ..Default::default()
        });
        for _ in 0..100 {
            assert!(injector.should_fail());
        }
    }

    #[test]
    fn max_failures_respected() {
        let injector = FaultInjector::new(FaultConfig {
            failure_rate: 1.0,
            max_failures: Some(3),
            ..Default::default()
        });

        let mut failures = 0;
        for _ in 0..100 {
            if injector.should_fail() {
                failures += 1;
            }
        }
        assert_eq!(failures, 3);
    }

    #[test]
    fn latency_in_range() {
        let injector = FaultInjector::new(FaultConfig {
            latency_range: Some((
                Duration::from_millis(10),
                Duration::from_millis(100),
            )),
            ..Default::default()
        });

        for _ in 0..100 {
            // Force different call counts.
            injector.should_fail();
            if let Some(delay) = injector.latency_to_inject() {
                assert!(delay >= Duration::from_millis(10));
                assert!(delay <= Duration::from_millis(100));
            }
        }
    }

    #[test]
    fn chaos_scenario_brownout() {
        let scenario = ChaosScenario::provider_brownout("anthropic", 0.2);
        assert_eq!(scenario.injectors.len(), 1);
        assert_eq!(scenario.injectors[0].0, "anthropic");
    }

    #[test]
    fn chaos_scenario_partition() {
        let scenario = ChaosScenario::network_partition(&["anthropic", "openai"]);
        assert_eq!(scenario.injectors.len(), 2);
        // All providers should always fail.
        for (_, injector) in &scenario.injectors {
            assert!(injector.should_fail());
        }
    }
}
