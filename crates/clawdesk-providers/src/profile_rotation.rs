//! Profile rotation — multi-credential rotation with exponential-backoff cooldown.
//!
//! Wraps the `AuthProfileStore` from `clawdesk-security` with provider-level
//! rotation logic, including:
//!
//! - **Weighted round-robin** with failure penalty
//! - **Truncated exponential backoff** with decorrelated jitter
//! - **Concurrent-safe** reads via clone-on-access pattern
//! - **System availability** monitoring
//!
//! ## Algorithm
//!
//! Selection weight:
//! ```text
//! w_i = base_priority_i × health_factor_i × recency_factor_i
//! health_factor_i = 1 / (1 + failure_count_i)
//! recency_factor_i = 1 - (now - last_used_i) / max_idle
//! ```
//!
//! Cooldown delay (truncated exponential backoff with jitter):
//! ```text
//! delay_i = min(base × 2^(f_i - 1) + jitter, max_delay)
//! jitter ~ Uniform(0, base × 2^(f_i - 1) × 0.1)
//! ```
//!
//! System unavailability (all N profiles in cooldown):
//! ```text
//! P(all_cooldown) = Π P(cooldown_i)
//! ```
//! With N=3, λ_fail=0.01/s, avg d=30s: P ≈ 0.012 → 98.8% availability.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Configuration for profile rotation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationConfig {
    /// Base cooldown delay in seconds.
    #[serde(default = "default_base_delay")]
    pub base_delay_secs: u64,
    /// Maximum cooldown delay in seconds.
    #[serde(default = "default_max_delay")]
    pub max_delay_secs: u64,
    /// Maximum idle time before recency factor kicks in (seconds).
    #[serde(default = "default_max_idle")]
    pub max_idle_secs: u64,
    /// Jitter factor (0.0-1.0) for decorrelated backoff.
    #[serde(default = "default_jitter_factor")]
    pub jitter_factor: f64,
}

fn default_base_delay() -> u64 { 5 }
fn default_max_delay() -> u64 { 300 }
fn default_max_idle() -> u64 { 3600 }
fn default_jitter_factor() -> f64 { 0.1 }

impl Default for RotationConfig {
    fn default() -> Self {
        Self {
            base_delay_secs: default_base_delay(),
            max_delay_secs: default_max_delay(),
            max_idle_secs: default_max_idle(),
            jitter_factor: default_jitter_factor(),
        }
    }
}

/// A rotatable credential with state tracking.
#[derive(Debug, Clone)]
pub struct RotatableProfile {
    /// Profile identifier.
    pub id: String,
    /// The API key.
    pub api_key: String,
    /// Optional organization ID.
    pub org_id: Option<String>,
    /// Base priority weight (higher = preferred).
    pub base_priority: f64,
    /// Consecutive failure count.
    pub failure_count: u32,
    /// Total requests made with this profile.
    pub total_requests: u64,
    /// Total failures on this profile.
    pub total_failures: u64,
    /// Whether currently in cooldown.
    pub in_cooldown: bool,
    /// When cooldown expires.
    pub cooldown_until: Option<Instant>,
    /// When last used.
    pub last_used: Option<Instant>,
    /// Whether marked as permanently expired.
    pub is_expired: bool,
}

impl RotatableProfile {
    pub fn new(id: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            api_key: api_key.into(),
            org_id: None,
            base_priority: 1.0,
            failure_count: 0,
            total_requests: 0,
            total_failures: 0,
            in_cooldown: false,
            cooldown_until: None,
            last_used: None,
            is_expired: false,
        }
    }

    pub fn with_priority(mut self, priority: f64) -> Self {
        self.base_priority = priority;
        self
    }

    /// Compute effective selection weight.
    ///
    /// w = base_priority × health_factor × recency_factor
    /// health_factor = 1 / (1 + failure_count)
    /// recency_factor = 1 - elapsed / max_idle (clamped to [0.1, 1.0])
    pub fn effective_weight(&self, max_idle: Duration) -> f64 {
        let health = 1.0 / (1.0 + self.failure_count as f64);
        let recency = match self.last_used {
            Some(last) => {
                let elapsed = last.elapsed().as_secs_f64();
                let max = max_idle.as_secs_f64().max(1.0);
                (1.0 - elapsed / max).clamp(0.1, 1.0)
            }
            None => 0.5, // never used → neutral
        };
        self.base_priority * health * recency
    }

    /// Whether this profile is available for selection.
    pub fn is_available(&self) -> bool {
        if self.is_expired {
            return false;
        }
        if !self.in_cooldown {
            return true;
        }
        match self.cooldown_until {
            Some(until) => Instant::now() >= until,
            None => true,
        }
    }
}

/// Reason a profile was put on cooldown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureReason {
    /// API rate limit exceeded.
    RateLimit,
    /// Authentication failed (bad key, expired token).
    AuthError,
    /// Billing/quota exceeded.
    BillingError,
    /// Server error (5xx).
    ServerError,
    /// Network timeout.
    Timeout,
    /// Unknown error.
    Unknown,
}

impl FailureReason {
    /// Whether this failure type should trigger profile rotation.
    pub fn should_rotate(&self) -> bool {
        matches!(
            self,
            FailureReason::RateLimit
                | FailureReason::AuthError
                | FailureReason::BillingError
        )
    }

    /// Whether this failure type means the credential is permanently invalid.
    pub fn is_permanent(&self) -> bool {
        matches!(self, FailureReason::AuthError | FailureReason::BillingError)
    }
}

/// Profile rotation engine for a single provider.
///
/// Thread-safe: uses `RwLock` internally. Multiple agent runners can
/// call `select()` concurrently. Writes (failure/success recording)
/// are serialized but fast (O(1) per operation).
pub struct ProfileRotator {
    /// Provider name.
    provider: String,
    /// Profiles indexed by ID.
    profiles: DashMap<String, RotatableProfile>,
    /// Configuration.
    config: RotationConfig,
}

impl ProfileRotator {
    /// Create a new rotator for a provider.
    pub fn new(provider: impl Into<String>, config: RotationConfig) -> Self {
        Self {
            provider: provider.into(),
            profiles: DashMap::new(),
            config,
        }
    }

    /// Add or update a profile.
    pub fn upsert(&self, profile: RotatableProfile) {
        let id = profile.id.clone();
        self.profiles.insert(id, profile);
    }

    /// Remove a profile by ID.
    pub fn remove(&self, id: &str) -> Option<RotatableProfile> {
        self.profiles.remove(id).map(|(_, v)| v)
    }

    /// Select the best available profile.
    ///
    /// Uses weighted selection: profiles sorted by effective_weight descending,
    /// first available profile is returned. O(N) where N is typically 2-5.
    ///
    /// Returns `None` if all profiles are on cooldown or expired.
    pub fn select(&self) -> Option<RotatableProfile> {
        let max_idle = Duration::from_secs(self.config.max_idle_secs);

        self.profiles
            .iter()
            .filter(|e| e.value().is_available())
            .max_by(|a, b| {
                let wa = a.value().effective_weight(max_idle);
                let wb = b.value().effective_weight(max_idle);
                wa.partial_cmp(&wb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|e| e.value().clone())
    }

    /// Record a successful request on a profile.
    pub fn record_success(&self, profile_id: &str) {
        if let Some(mut p) = self.profiles.get_mut(profile_id) {
            p.failure_count = 0;
            p.total_requests += 1;
            p.in_cooldown = false;
            p.cooldown_until = None;
            p.last_used = Some(Instant::now());
            debug!(provider = %self.provider, profile = %profile_id, "Profile success recorded");
        }
    }

    /// Record a failure on a profile with exponential backoff cooldown.
    ///
    /// Cooldown: delay = min(base × 2^(f-1) + jitter, max_delay)
    /// Where jitter ~ Uniform(0, base × 2^(f-1) × jitter_factor)
    pub fn record_failure(
        &self,
        profile_id: &str,
        reason: FailureReason,
        server_retry_after: Option<Duration>,
    ) {
        if let Some(mut p) = self.profiles.get_mut(profile_id) {
            p.failure_count += 1;
            p.total_requests += 1;
            p.total_failures += 1;
            p.last_used = Some(Instant::now());

            // Mark permanently expired for auth/billing errors
            if reason.is_permanent() && p.failure_count >= 3 {
                p.is_expired = true;
                warn!(
                    provider = %self.provider,
                    profile = %profile_id,
                    reason = ?reason,
                    "Profile marked permanently expired after {} failures",
                    p.failure_count
                );
                return;
            }

            // Calculate cooldown duration
            let cooldown = server_retry_after.unwrap_or_else(|| {
                self.compute_backoff_delay(p.failure_count)
            });

            p.in_cooldown = true;
            p.cooldown_until = Some(Instant::now() + cooldown);

            info!(
                provider = %self.provider,
                profile = %profile_id,
                reason = ?reason,
                cooldown_secs = cooldown.as_secs(),
                failure_count = p.failure_count,
                "Profile cooldown applied"
            );
        }
    }

    /// Compute truncated exponential backoff with jitter.
    ///
    /// `delay = min(base × 2^(f-1) + jitter, max_delay)`
    /// `jitter ~ Uniform(0, base × 2^(f-1) × jitter_factor)`
    ///
    /// Uses `std::hash::RandomState` (OS-entropy-seeded per
    /// process) to produce per-instance decorrelated jitter — no `rand`
    /// crate needed. Each call hashes a monotonic counter through the
    /// process-unique hasher, so two processes that started at the same
    /// wall-clock instant still get independent jitter sequences.
    fn compute_backoff_delay(&self, failure_count: u32) -> Duration {
        use std::hash::{BuildHasher, Hasher};

        // Process-wide, OS-entropy-seeded hasher factory.
        // Initialized once — the seed is random per process, which is
        // exactly the decorrelation axis we need against thundering herd.
        static RANDOM_STATE: std::sync::OnceLock<std::collections::hash_map::RandomState> =
            std::sync::OnceLock::new();
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

        let state = RANDOM_STATE.get_or_init(std::collections::hash_map::RandomState::new);

        let base = self.config.base_delay_secs as f64;
        let max = self.config.max_delay_secs as f64;
        let exponent = (failure_count.saturating_sub(1)).min(20) as f64;
        let base_delay = base * 2.0_f64.powf(exponent);

        let jitter_range = base_delay * self.config.jitter_factor;

        // Hash a monotonic counter → different value every call, decorrelated
        // across processes because RandomState's seed differs per process.
        let tick = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut hasher = state.build_hasher();
        hasher.write_u64(tick);
        let hash = hasher.finish();
        let frac = (hash >> 33) as f64 / (1u64 << 31) as f64; // [0, 1)
        let jitter = jitter_range * frac;

        let delay = (base_delay + jitter).min(max);
        Duration::from_secs_f64(delay)
    }

    /// Number of profiles.
    pub fn profile_count(&self) -> usize {
        self.profiles.len()
    }

    /// Number of available (non-cooldown, non-expired) profiles.
    pub fn available_count(&self) -> usize {
        self.profiles
            .iter()
            .filter(|e| e.value().is_available())
            .count()
    }

    /// Whether all profiles are exhausted (all in cooldown or expired).
    pub fn all_exhausted(&self) -> bool {
        self.available_count() == 0 && self.profile_count() > 0
    }

    /// Get the provider name.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Snapshot of all profiles for diagnostics.
    pub fn snapshot(&self) -> Vec<ProfileSnapshot> {
        let max_idle = Duration::from_secs(self.config.max_idle_secs);
        self.profiles
            .iter()
            .map(|e| {
                let p = e.value();
                ProfileSnapshot {
                    id: p.id.clone(),
                    effective_weight: p.effective_weight(max_idle),
                    failure_count: p.failure_count,
                    total_requests: p.total_requests,
                    is_available: p.is_available(),
                    is_expired: p.is_expired,
                    in_cooldown: p.in_cooldown,
                }
            })
            .collect()
    }
}

/// Diagnostic snapshot of a profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSnapshot {
    pub id: String,
    pub effective_weight: f64,
    pub failure_count: u32,
    pub total_requests: u64,
    pub is_available: bool,
    pub is_expired: bool,
    pub in_cooldown: bool,
}

/// Multi-provider profile rotation manager.
///
/// Manages `ProfileRotator` instances for multiple providers.
pub struct RotationManager {
    rotators: HashMap<String, Arc<ProfileRotator>>,
}

impl RotationManager {
    pub fn new() -> Self {
        Self {
            rotators: HashMap::new(),
        }
    }

    /// Get or create a rotator for a provider.
    pub fn rotator(&mut self, provider: &str, config: RotationConfig) -> Arc<ProfileRotator> {
        self.rotators
            .entry(provider.to_string())
            .or_insert_with(|| Arc::new(ProfileRotator::new(provider, config)))
            .clone()
    }

    /// Get an existing rotator for a provider.
    pub fn get_rotator(&self, provider: &str) -> Option<Arc<ProfileRotator>> {
        self.rotators.get(provider).cloned()
    }

    /// List all providers with rotators.
    pub fn providers(&self) -> Vec<&str> {
        self.rotators.keys().map(|k| k.as_str()).collect()
    }
}

impl Default for RotationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_weight_degrades_on_failure() {
        let max_idle = Duration::from_secs(3600);
        let mut p = RotatableProfile::new("p1", "key-1");
        p.last_used = Some(Instant::now());
        let w0 = p.effective_weight(max_idle);

        p.failure_count = 3;
        let w3 = p.effective_weight(max_idle);
        assert!(w3 < w0, "weight should decrease with failures");
    }

    #[test]
    fn test_select_best_profile() {
        let rotator = ProfileRotator::new("anthropic", RotationConfig::default());
        rotator.upsert(RotatableProfile::new("low", "key-1").with_priority(0.5));
        rotator.upsert(RotatableProfile::new("high", "key-2").with_priority(2.0));

        let selected = rotator.select().unwrap();
        assert_eq!(selected.id, "high");
    }

    #[test]
    fn test_cooldown_excludes_profile() {
        let rotator = ProfileRotator::new("openai", RotationConfig::default());
        rotator.upsert(RotatableProfile::new("p1", "key-1").with_priority(2.0));
        rotator.upsert(RotatableProfile::new("p2", "key-2").with_priority(1.0));

        rotator.record_failure("p1", FailureReason::RateLimit, Some(Duration::from_secs(60)));

        let selected = rotator.select().unwrap();
        assert_eq!(selected.id, "p2"); // p1 is on cooldown
    }

    #[test]
    fn test_success_clears_cooldown() {
        let rotator = ProfileRotator::new("openai", RotationConfig::default());
        rotator.upsert(RotatableProfile::new("p1", "key-1"));

        rotator.record_failure("p1", FailureReason::RateLimit, Some(Duration::from_secs(60)));
        assert_eq!(rotator.available_count(), 0);

        rotator.record_success("p1");
        assert_eq!(rotator.available_count(), 1);
    }

    #[test]
    fn test_permanent_expiry_after_repeated_auth_failures() {
        let rotator = ProfileRotator::new("openai", RotationConfig::default());
        rotator.upsert(RotatableProfile::new("p1", "key-1"));

        rotator.record_failure("p1", FailureReason::AuthError, None);
        rotator.record_failure("p1", FailureReason::AuthError, None);
        rotator.record_failure("p1", FailureReason::AuthError, None);

        // After 3 auth failures, profile should be permanently expired
        assert!(rotator.all_exhausted());
    }

    #[test]
    fn test_backoff_delay_increases() {
        let rotator = ProfileRotator::new("test", RotationConfig {
            base_delay_secs: 5,
            max_delay_secs: 300,
            ..Default::default()
        });

        let d1 = rotator.compute_backoff_delay(1);
        let d2 = rotator.compute_backoff_delay(2);
        let d3 = rotator.compute_backoff_delay(3);

        assert!(d2 > d1, "delay should increase with failure count");
        assert!(d3 > d2);
        assert!(d3.as_secs() <= 300, "delay should be capped");
    }

    #[test]
    fn test_rotation_manager() {
        let mut mgr = RotationManager::new();
        let r = mgr.rotator("anthropic", RotationConfig::default());
        r.upsert(RotatableProfile::new("p1", "key-1"));

        assert!(mgr.get_rotator("anthropic").is_some());
        assert!(mgr.get_rotator("nonexistent").is_none());
        assert_eq!(mgr.providers().len(), 1);
    }
}
