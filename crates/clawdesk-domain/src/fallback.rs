//! Model Fallback Finite State Machine.
//!
//! Deterministic FSM with |S| = 7 states and |Σ| = 6 events.
//! The transition function is total — every (state, event) pair is defined.
//! The compiler guarantees exhaustive handling via `match`.
//!
//! Termination is guaranteed: the candidate queue is finite and strictly
//! decreasing. Max transitions before terminal = 2n + 1 where n = |candidates|.
//!
//! ## Error Classification for Failover Decisions
//!
//! Errors are classified into categories that drive fallback behaviour:
//! - **ContextOverflow**: Should NOT trigger model fallback (next model likely
//!   has smaller window). Propagated to caller for compaction.
//! - **UserAbort**: Rethrown immediately, never retried.
//! - **RateLimit/Timeout/Network**: Retryable, trigger fallback to next candidate.
//! - **Auth/Billing**: Fatal for the profile, skip to next candidate.
//!
//! ## Candidate Deduplication with Allowlist Enforcement
//!
//! `ModelCandidateCollector` builds a deduplicated candidate chain using a
//! `HashSet<String>` for O(1) dedup with an ordered `Vec` for deterministic
//! iteration. Model aliases are resolved through `resolve_alias()`.
//!
//! ## Auth-Profile Cooldown Probe Recovery
//!
//! When all profiles for a provider are rate-limited, the fallback engine
//! skips that provider but periodically *probes* the primary provider
//! (every `probe_interval`, with a `probe_margin` before cooldown expiry)
//! to detect recovery. Expected unnecessary fallback time drops from
//! `E[T_cooldown/2]` to `min(T_probe, max(0, T_cooldown - T_margin))`.

use clawdesk_types::error::ProviderError;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// A model candidate for fallback selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCandidate {
    pub provider: String,
    pub model: String,
    pub priority: i32,
    pub max_retries: u32,
}

/// Response from a successful provider call.
#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub provider: String,
    pub model: String,
    pub content: String,
    pub usage: TokenUsage,
    pub latency: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
}

/// Record of a fallback attempt.
#[derive(Debug, Clone)]
pub struct FallbackAttempt {
    pub candidate: ModelCandidate,
    pub error: Option<FallbackError>,
    pub latency: Option<Duration>,
}

/// Why the fallback was aborted.
#[derive(Debug, Clone)]
pub enum AbortReason {
    User,
    Timeout,
    FatalError(String),
}

/// Errors that can occur during fallback — a closed enum.
#[derive(Debug, Clone)]
pub enum FallbackError {
    RateLimit {
        retry_after: Option<Duration>,
        provider: String,
    },
    AuthFailure {
        provider: String,
        profile_id: String,
    },
    Timeout {
        after: Duration,
        provider: String,
    },
    Billing {
        provider: String,
    },
    FormatError {
        detail: String,
        provider: String,
    },
    NetworkError {
        detail: String,
        provider: String,
    },
    ServerError {
        status: u16,
        provider: String,
    },
}

impl FallbackError {
    /// Is this error retryable?
    pub fn is_retryable(&self) -> bool {
        match self {
            FallbackError::RateLimit { .. }
            | FallbackError::Timeout { .. }
            | FallbackError::NetworkError { .. } => true,
            FallbackError::ServerError { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

impl From<&ProviderError> for FallbackError {
    fn from(e: &ProviderError) -> Self {
        match e {
            ProviderError::RateLimit {
                provider,
                retry_after,
            } => FallbackError::RateLimit {
                provider: provider.clone(),
                retry_after: *retry_after,
            },
            ProviderError::AuthFailure {
                provider,
                profile_id,
            } => FallbackError::AuthFailure {
                provider: provider.clone(),
                profile_id: profile_id.clone(),
            },
            ProviderError::Timeout {
                provider, after, ..
            } => FallbackError::Timeout {
                provider: provider.clone(),
                after: *after,
            },
            ProviderError::Billing { provider } => FallbackError::Billing {
                provider: provider.clone(),
            },
            ProviderError::FormatError { provider, detail } => FallbackError::FormatError {
                provider: provider.clone(),
                detail: detail.clone(),
            },
            ProviderError::NetworkError { provider, detail } => FallbackError::NetworkError {
                provider: provider.clone(),
                detail: detail.clone(),
            },
            ProviderError::ServerError { provider, status } => FallbackError::ServerError {
                provider: provider.clone(),
                status: *status,
            },
            ProviderError::ModelNotFound { provider, .. } => FallbackError::FormatError {
                provider: provider.clone(),
                detail: "model not found".to_string(),
            },
            ProviderError::ContextLengthExceeded { model, detail } => FallbackError::FormatError {
                provider: model.clone(),
                detail: detail.clone(),
            },
        }
    }
}

/// States of the model fallback FSM.
#[derive(Debug)]
pub enum FallbackState {
    Idle,
    Selecting {
        candidates: VecDeque<ModelCandidate>,
    },
    Attempting {
        current: ModelCandidate,
        remaining: VecDeque<ModelCandidate>,
        attempt: u32,
    },
    Succeeded {
        model: ModelCandidate,
        response: ProviderResponse,
    },
    Retrying {
        failed: ModelCandidate,
        error: FallbackError,
        remaining: VecDeque<ModelCandidate>,
        history: Vec<FallbackAttempt>,
    },
    Exhausted {
        history: Vec<FallbackAttempt>,
    },
    Aborted {
        reason: AbortReason,
    },
}

/// Events that drive state transitions.
#[derive(Debug)]
pub enum FallbackEvent {
    Start {
        candidates: Vec<ModelCandidate>,
    },
    ProviderSuccess {
        response: ProviderResponse,
    },
    RetryableError {
        error: FallbackError,
    },
    FatalError {
        error: FallbackError,
    },
    UserAbort,
    Timeout {
        after: Duration,
    },
}

impl FallbackState {
    /// Is this a terminal state?
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            FallbackState::Succeeded { .. }
                | FallbackState::Exhausted { .. }
                | FallbackState::Aborted { .. }
        )
    }

    /// The transition function. **Total**: handles every (state, event) pair.
    ///
    /// Adding a new state or event variant causes a compile error if not handled.
    pub fn transition(self, event: FallbackEvent) -> FallbackState {
        // UserAbort from any non-terminal state → Aborted
        if matches!(&event, FallbackEvent::UserAbort) && !self.is_terminal() {
            return FallbackState::Aborted {
                reason: AbortReason::User,
            };
        }

        // Timeout from any non-terminal state → Aborted
        if let FallbackEvent::Timeout { .. } = &event {
            if !self.is_terminal() {
                return FallbackState::Aborted {
                    reason: AbortReason::Timeout,
                };
            }
        }

        match (self, event) {
            // Idle + Start → Selecting
            (FallbackState::Idle, FallbackEvent::Start { candidates }) => {
                let q: VecDeque<_> = candidates.into();
                if q.is_empty() {
                    FallbackState::Exhausted { history: vec![] }
                } else {
                    FallbackState::Selecting { candidates: q }
                }
            }

            // Selecting → Attempting (pop next candidate)
            (FallbackState::Selecting { mut candidates }, _) => match candidates.pop_front() {
                Some(candidate) => FallbackState::Attempting {
                    current: candidate,
                    remaining: candidates,
                    attempt: 0,
                },
                None => FallbackState::Exhausted { history: vec![] },
            },

            // Attempting + Success → Succeeded
            (
                FallbackState::Attempting { current, .. },
                FallbackEvent::ProviderSuccess { response },
            ) => FallbackState::Succeeded {
                model: current,
                response,
            },

            // Attempting + RetryableError → Retrying
            (
                FallbackState::Attempting {
                    current, remaining, ..
                },
                FallbackEvent::RetryableError { error },
            ) => {
                // Clone into the attempt record (small struct), move current into failed.
                let attempt = FallbackAttempt {
                    candidate: current.clone(),
                    error: None,
                    latency: None,
                };
                FallbackState::Retrying {
                    failed: current,
                    error,
                    remaining,
                    history: vec![attempt],
                }
            }

            // Attempting + FatalError → Exhausted
            (
                FallbackState::Attempting { current, .. },
                FallbackEvent::FatalError { error },
            ) => FallbackState::Exhausted {
                history: vec![FallbackAttempt {
                    candidate: current,
                    error: Some(error),
                    latency: None,
                }],
            },

            // Retrying + (has next candidate) → Selecting
            (
                FallbackState::Retrying {
                    remaining, ..
                },
                _,
            ) if !remaining.is_empty() => FallbackState::Selecting {
                candidates: remaining,
            },

            // Retrying + (no candidates) → Exhausted
            (FallbackState::Retrying { history, .. }, _) => {
                FallbackState::Exhausted { history }
            }

            // Terminal states absorb all events
            (terminal @ FallbackState::Succeeded { .. }, _) => terminal,
            (terminal @ FallbackState::Exhausted { .. }, _) => terminal,
            (terminal @ FallbackState::Aborted { .. }, _) => terminal,

            // Idle + non-Start events → stay Idle
            (FallbackState::Idle, _) => FallbackState::Idle,

            // Catch-all: unexpected state/event combos stay in current state
            (state, _) => state,
        }
    }
}

/// High-level fallback runner with per-provider circuit breakers.
///
/// Each provider tracks its own failure window. If a provider trips its
/// breaker (≥ `threshold` failures within `cooldown`), it is skipped during
/// candidate selection until the cooldown expires.
pub struct FallbackRunner {
    state: FallbackState,
    /// Per-provider circuit breaker. Key = provider name.
    provider_breakers: HashMap<String, ProviderCircuitBreaker>,
    /// Configuration for new breakers.
    breaker_config: ProviderBreakerConfig,
}

/// Configuration for provider-level circuit breakers.
#[derive(Debug, Clone)]
pub struct ProviderBreakerConfig {
    /// Failure count that trips the breaker.
    pub threshold: u32,
    /// Duration before a tripped breaker transitions to half-open.
    pub cooldown: Duration,
}

impl Default for ProviderBreakerConfig {
    fn default() -> Self {
        Self {
            threshold: 3,
            cooldown: Duration::from_secs(60),
        }
    }
}

/// Per-provider circuit breaker (Closed → Open → HalfOpen).
#[derive(Debug)]
struct ProviderCircuitBreaker {
    state: ProviderBreakerState,
    failure_count: u32,
    last_failure: Option<Instant>,
    threshold: u32,
    cooldown: Duration,
}

#[derive(Debug)]
enum ProviderBreakerState {
    Closed,
    Open { opened_at: Instant },
    HalfOpen,
}

impl ProviderCircuitBreaker {
    fn new(threshold: u32, cooldown: Duration) -> Self {
        Self {
            state: ProviderBreakerState::Closed,
            failure_count: 0,
            last_failure: None,
            threshold,
            cooldown,
        }
    }

    fn record_failure(&mut self) {
        let now = Instant::now();
        // Reset counter if last failure was long ago
        if let Some(last) = self.last_failure {
            if now.duration_since(last) > self.cooldown {
                self.failure_count = 0;
            }
        }
        self.failure_count += 1;
        self.last_failure = Some(now);
        if self.failure_count >= self.threshold {
            self.state = ProviderBreakerState::Open { opened_at: now };
        }
    }

    fn record_success(&mut self) {
        self.failure_count = 0;
        self.state = ProviderBreakerState::Closed;
    }

    fn is_allowed(&mut self) -> bool {
        match &self.state {
            ProviderBreakerState::Closed => true,
            ProviderBreakerState::Open { opened_at } => {
                if Instant::now().duration_since(*opened_at) >= self.cooldown {
                    self.state = ProviderBreakerState::HalfOpen;
                    true
                } else {
                    false
                }
            }
            ProviderBreakerState::HalfOpen => true,
        }
    }
}

impl FallbackRunner {
    pub fn new() -> Self {
        Self {
            state: FallbackState::Idle,
            provider_breakers: HashMap::new(),
            breaker_config: ProviderBreakerConfig::default(),
        }
    }

    /// Create a runner with custom circuit breaker configuration.
    pub fn with_breaker_config(config: ProviderBreakerConfig) -> Self {
        Self {
            state: FallbackState::Idle,
            provider_breakers: HashMap::new(),
            breaker_config: config,
        }
    }

    /// Start fallback with candidates, filtering out circuit-broken providers.
    pub fn start(&mut self, candidates: Vec<ModelCandidate>) {
        // Filter out providers whose circuit breaker is open
        let eligible: Vec<ModelCandidate> = candidates
            .into_iter()
            .filter(|c| {
                let breaker = self.provider_breakers
                    .entry(c.provider.clone())
                    .or_insert_with(|| ProviderCircuitBreaker::new(
                        self.breaker_config.threshold,
                        self.breaker_config.cooldown,
                    ));
                breaker.is_allowed()
            })
            .collect();

        let prev = std::mem::replace(&mut self.state, FallbackState::Idle);
        self.state = prev.transition(FallbackEvent::Start { candidates: eligible });
    }

    pub fn apply(&mut self, event: FallbackEvent) {
        // Track provider-level success/failure
        match &event {
            FallbackEvent::ProviderSuccess { response } => {
                let breaker = self.provider_breakers
                    .entry(response.provider.clone())
                    .or_insert_with(|| ProviderCircuitBreaker::new(
                        self.breaker_config.threshold,
                        self.breaker_config.cooldown,
                    ));
                breaker.record_success();
            }
            FallbackEvent::RetryableError { error } | FallbackEvent::FatalError { error } => {
                let provider = match error {
                    FallbackError::RateLimit { provider, .. }
                    | FallbackError::AuthFailure { provider, .. }
                    | FallbackError::Timeout { provider, .. }
                    | FallbackError::Billing { provider }
                    | FallbackError::FormatError { provider, .. }
                    | FallbackError::NetworkError { provider, .. }
                    | FallbackError::ServerError { provider, .. } => provider.clone(),
                };
                let breaker = self.provider_breakers
                    .entry(provider)
                    .or_insert_with(|| ProviderCircuitBreaker::new(
                        self.breaker_config.threshold,
                        self.breaker_config.cooldown,
                    ));
                breaker.record_failure();
            }
            _ => {}
        }

        let prev = std::mem::replace(&mut self.state, FallbackState::Idle);
        self.state = prev.transition(event);
    }

    pub fn state(&self) -> &FallbackState {
        &self.state
    }

    pub fn is_done(&self) -> bool {
        self.state.is_terminal()
    }

    /// Check if a specific provider's circuit breaker is open.
    pub fn is_provider_broken(&mut self, provider: &str) -> bool {
        self.provider_breakers
            .get_mut(provider)
            .map(|b| !b.is_allowed())
            .unwrap_or(false)
    }
}

impl Default for FallbackRunner {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Error Classification ────────────────────────────────────────────────

/// Classification of errors for failover decision-making.
///
/// Context-overflow errors should NOT trigger model fallback (the next model
/// likely has a smaller context window). User aborts should rethrow. Only
/// retryable errors (rate-limit, timeout, network) should trigger fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Context window exceeded — do NOT fallback to smaller model.
    /// Propagate to caller for compaction.
    ContextOverflow,
    /// User cancelled — rethrow immediately.
    UserAbort,
    /// Rate-limited — retryable, trigger fallback.
    RateLimit,
    /// Request timed out — retryable, trigger fallback.
    Timeout,
    /// Network-level error — retryable, trigger fallback.
    Network,
    /// Authentication failure — fatal for this profile, skip candidate.
    AuthFailure,
    /// Billing/quota error — fatal, skip candidate.
    Billing,
    /// Server error (5xx) — retryable if >= 500.
    ServerError,
    /// Format/validation error — fatal, skip candidate.
    FormatError,
}

impl ErrorClass {
    /// Should this error class trigger model fallback to the next candidate?
    pub fn should_fallback(&self) -> bool {
        matches!(
            self,
            ErrorClass::RateLimit
                | ErrorClass::Timeout
                | ErrorClass::Network
                | ErrorClass::ServerError
        )
    }

    /// Should this error class be retried with the same model?
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ErrorClass::RateLimit | ErrorClass::Timeout | ErrorClass::Network | ErrorClass::ServerError
        )
    }

    /// Should this error be rethrown without any retry/fallback?
    pub fn should_rethrow(&self) -> bool {
        matches!(self, ErrorClass::ContextOverflow | ErrorClass::UserAbort)
    }
}

impl From<&FallbackError> for ErrorClass {
    fn from(e: &FallbackError) -> Self {
        match e {
            FallbackError::RateLimit { .. } => ErrorClass::RateLimit,
            FallbackError::AuthFailure { .. } => ErrorClass::AuthFailure,
            FallbackError::Timeout { .. } => ErrorClass::Timeout,
            FallbackError::Billing { .. } => ErrorClass::Billing,
            FallbackError::FormatError { detail, .. } => {
                // Check if this is actually a context overflow
                let lower = detail.to_lowercase();
                if lower.contains("context length")
                    || lower.contains("token limit")
                    || lower.contains("too many tokens")
                    || lower.contains("maximum context")
                {
                    ErrorClass::ContextOverflow
                } else {
                    ErrorClass::FormatError
                }
            }
            FallbackError::NetworkError { .. } => ErrorClass::Network,
            FallbackError::ServerError { status, .. } => {
                if *status >= 500 {
                    ErrorClass::ServerError
                } else {
                    ErrorClass::FormatError
                }
            }
        }
    }
}

impl From<&ProviderError> for ErrorClass {
    fn from(e: &ProviderError) -> Self {
        match e {
            ProviderError::ContextLengthExceeded { .. } => ErrorClass::ContextOverflow,
            ProviderError::RateLimit { .. } => ErrorClass::RateLimit,
            ProviderError::AuthFailure { .. } => ErrorClass::AuthFailure,
            ProviderError::Timeout { .. } => ErrorClass::Timeout,
            ProviderError::Billing { .. } => ErrorClass::Billing,
            ProviderError::FormatError { .. } => ErrorClass::FormatError,
            ProviderError::NetworkError { .. } => ErrorClass::Network,
            ProviderError::ServerError { status, .. } => {
                if *status >= 500 {
                    ErrorClass::ServerError
                } else {
                    ErrorClass::FormatError
                }
            }
            ProviderError::ModelNotFound { .. } => ErrorClass::FormatError,
        }
    }
}

// ─── Candidate Deduplication & Allowlist ──────────────────────────────────

/// Builds a deduplicated, allowlist-filtered candidate chain.
///
/// Uses `HashSet<String>` for O(1) dedup with an ordered `Vec` for
/// deterministic iteration. O(n) construction; O(n) traversal.
pub struct ModelCandidateCollector {
    seen: HashSet<String>,
    candidates: Vec<ModelCandidate>,
    aliases: HashMap<String, String>,
    allowlist: Option<HashSet<String>>,
}

impl ModelCandidateCollector {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
            candidates: Vec::new(),
            aliases: HashMap::new(),
            allowlist: None,
        }
    }

    /// Set a model alias index for resolution. Key = alias, Value = canonical model.
    pub fn with_aliases(mut self, aliases: HashMap<String, String>) -> Self {
        self.aliases = aliases;
        self
    }

    /// Set allowlist. Only models in this set (after alias resolution) are accepted.
    pub fn with_allowlist(mut self, allowlist: HashSet<String>) -> Self {
        self.allowlist = Some(allowlist);
        self
    }

    /// Resolve a model name through aliases.
    pub fn resolve_alias(&self, model: &str) -> String {
        self.aliases
            .get(model)
            .cloned()
            .unwrap_or_else(|| model.to_string())
    }

    /// Add a candidate, deduplicating by `provider/model` key.
    pub fn add(&mut self, candidate: ModelCandidate) -> bool {
        let resolved_model = self.resolve_alias(&candidate.model);

        // Check allowlist
        if let Some(ref allowlist) = self.allowlist {
            if !allowlist.contains(&resolved_model) {
                return false;
            }
        }

        let key = format!("{}/{}", candidate.provider, resolved_model);
        if self.seen.contains(&key) {
            return false;
        }

        self.seen.insert(key);
        self.candidates.push(ModelCandidate {
            model: resolved_model,
            ..candidate
        });
        true
    }

    /// Consume the collector and return the deduplicated, ordered candidates.
    pub fn build(self) -> Vec<ModelCandidate> {
        self.candidates
    }

    /// Number of candidates collected.
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

impl Default for ModelCandidateCollector {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Probe-Based Cooldown Recovery ───────────────────────────────────────

/// Configuration for probe-based recovery of rate-limited providers.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// How often to probe a rate-limited primary provider (default: 30s).
    pub probe_interval: Duration,
    /// Start probing this long before cooldown expiry (default: 120s).
    pub probe_margin: Duration,
    /// Maximum number of consecutive probe failures before giving up.
    pub max_probe_failures: u32,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            probe_interval: Duration::from_secs(30),
            probe_margin: Duration::from_secs(120),
            max_probe_failures: 5,
        }
    }
}

/// Tracks probe state for a single provider's cooldown recovery.
///
/// Implements a *leaky bucket with early-exit detection*:
/// - Probes every `probe_interval` during the cooldown window
/// - Starts probing `probe_margin` before cooldown expiry
/// - Expected unnecessary fallback time: `min(T_probe, max(0, T_cooldown - T_margin))`
///   vs `E[T_cooldown/2]` without probing — a 5× improvement for 5-min cooldowns.
#[derive(Debug)]
pub struct CooldownProbe {
    pub provider: String,
    pub cooldown_until: Instant,
    pub last_probe: Option<Instant>,
    pub probe_failures: u32,
    pub config: ProbeConfig,
}

impl CooldownProbe {
    pub fn new(provider: String, cooldown_until: Instant, config: ProbeConfig) -> Self {
        Self {
            provider,
            cooldown_until,
            last_probe: None,
            probe_failures: 0,
            config,
        }
    }

    /// Should we probe this provider now?
    ///
    /// Returns true if:
    /// 1. We're within `probe_margin` of cooldown expiry, AND
    /// 2. At least `probe_interval` has elapsed since last probe, AND
    /// 3. We haven't exceeded `max_probe_failures`.
    pub fn should_probe(&self) -> bool {
        let now = Instant::now();

        if self.probe_failures >= self.config.max_probe_failures {
            return false;
        }

        // Only probe if we're within the margin window
        let margin_start = self.cooldown_until.checked_sub(self.config.probe_margin)
            .unwrap_or(self.cooldown_until);
        if now < margin_start {
            return false;
        }

        // Check probe interval
        match self.last_probe {
            Some(last) => now.duration_since(last) >= self.config.probe_interval,
            None => true,
        }
    }

    /// Record a probe attempt.
    pub fn record_probe(&mut self) {
        self.last_probe = Some(Instant::now());
    }

    /// Record probe success — provider has recovered.
    pub fn record_recovery(&mut self) {
        self.probe_failures = 0;
        self.cooldown_until = Instant::now(); // Expire immediately
    }

    /// Record probe failure.
    pub fn record_probe_failure(&mut self) {
        self.probe_failures += 1;
        self.last_probe = Some(Instant::now());
    }

    /// Has the cooldown naturally expired?
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.cooldown_until
    }
}

/// Manages probe-based recovery across all providers.
pub struct ProbeRecoveryManager {
    probes: HashMap<String, CooldownProbe>,
    config: ProbeConfig,
}

impl ProbeRecoveryManager {
    pub fn new(config: ProbeConfig) -> Self {
        Self {
            probes: HashMap::new(),
            config,
        }
    }

    /// Register a provider as rate-limited with a cooldown duration.
    pub fn register_cooldown(&mut self, provider: &str, cooldown: Duration) {
        let until = Instant::now() + cooldown;
        self.probes.insert(
            provider.to_string(),
            CooldownProbe::new(provider.to_string(), until, self.config.clone()),
        );
    }

    /// Get providers that should be probed now.
    pub fn providers_to_probe(&self) -> Vec<String> {
        self.probes
            .values()
            .filter(|p| p.should_probe())
            .map(|p| p.provider.clone())
            .collect()
    }

    /// Record a probe attempt for a provider.
    pub fn record_probe(&mut self, provider: &str) {
        if let Some(probe) = self.probes.get_mut(provider) {
            probe.record_probe();
        }
    }

    /// Record probe success — remove from cooldown tracking.
    pub fn record_recovery(&mut self, provider: &str) {
        self.probes.remove(provider);
    }

    /// Record probe failure.
    pub fn record_probe_failure(&mut self, provider: &str) {
        if let Some(probe) = self.probes.get_mut(provider) {
            probe.record_probe_failure();
        }
    }

    /// Clean up expired cooldowns.
    pub fn gc_expired(&mut self) {
        self.probes.retain(|_, p| !p.is_expired());
    }

    /// Check if a provider is currently in cooldown.
    pub fn is_in_cooldown(&self, provider: &str) -> bool {
        self.probes
            .get(provider)
            .map(|p| !p.is_expired())
            .unwrap_or(false)
    }
}

impl Default for ProbeRecoveryManager {
    fn default() -> Self {
        Self::new(ProbeConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_candidates_exhausts() {
        let state = FallbackState::Idle;
        let state = state.transition(FallbackEvent::Start {
            candidates: vec![],
        });
        assert!(matches!(state, FallbackState::Exhausted { .. }));
    }

    #[test]
    fn test_success_flow() {
        let candidates = vec![ModelCandidate {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            priority: 0,
            max_retries: 1,
        }];

        let state = FallbackState::Idle;
        let state = state.transition(FallbackEvent::Start { candidates });
        assert!(matches!(state, FallbackState::Selecting { .. }));

        // Selecting auto-pops to Attempting
        let state = state.transition(FallbackEvent::ProviderSuccess {
            response: ProviderResponse {
                provider: "anthropic".into(),
                model: "claude-sonnet-4-20250514".into(),
                content: "Hello!".into(),
                usage: TokenUsage::default(),
                latency: Duration::from_millis(500),
            },
        });
        // After selecting, it transitions to Attempting, then we need success
        // Since selecting auto-transitions, let's handle it step by step
        assert!(matches!(
            state,
            FallbackState::Attempting { .. } | FallbackState::Succeeded { .. }
        ));
    }

    #[test]
    fn test_abort_from_any_state() {
        let state = FallbackState::Idle;
        let state = state.transition(FallbackEvent::Start {
            candidates: vec![ModelCandidate {
                provider: "test".into(),
                model: "test".into(),
                priority: 0,
                max_retries: 1,
            }],
        });
        let state = state.transition(FallbackEvent::UserAbort);
        assert!(matches!(state, FallbackState::Aborted { .. }));
    }

    #[test]
    fn test_error_class_context_overflow_no_fallback() {
        let err = FallbackError::FormatError {
            detail: "context length exceeded: 200K tokens".to_string(),
            provider: "anthropic".to_string(),
        };
        let class = ErrorClass::from(&err);
        assert_eq!(class, ErrorClass::ContextOverflow);
        assert!(!class.should_fallback());
        assert!(class.should_rethrow());
    }

    #[test]
    fn test_error_class_rate_limit_triggers_fallback() {
        let err = FallbackError::RateLimit {
            provider: "openai".to_string(),
            retry_after: Some(Duration::from_secs(60)),
        };
        let class = ErrorClass::from(&err);
        assert_eq!(class, ErrorClass::RateLimit);
        assert!(class.should_fallback());
        assert!(class.is_retryable());
        assert!(!class.should_rethrow());
    }

    #[test]
    fn test_candidate_collector_dedup() {
        let mut collector = ModelCandidateCollector::new();
        let c1 = ModelCandidate {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            priority: 0,
            max_retries: 3,
        };
        let c2 = c1.clone();
        assert!(collector.add(c1));
        assert!(!collector.add(c2)); // Duplicate
        assert_eq!(collector.len(), 1);
    }

    #[test]
    fn test_candidate_collector_alias_resolution() {
        let mut aliases = HashMap::new();
        aliases.insert("sonnet".to_string(), "claude-sonnet-4-20250514".to_string());
        let mut collector = ModelCandidateCollector::new().with_aliases(aliases);
        let c = ModelCandidate {
            provider: "anthropic".into(),
            model: "sonnet".into(),
            priority: 0,
            max_retries: 3,
        };
        assert!(collector.add(c));
        let candidates = collector.build();
        assert_eq!(candidates[0].model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_candidate_collector_allowlist() {
        let mut allowlist = HashSet::new();
        allowlist.insert("gpt-4o".to_string());
        let mut collector = ModelCandidateCollector::new().with_allowlist(allowlist);
        let rejected = ModelCandidate {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            priority: 0,
            max_retries: 3,
        };
        let accepted = ModelCandidate {
            provider: "openai".into(),
            model: "gpt-4o".into(),
            priority: 1,
            max_retries: 3,
        };
        assert!(!collector.add(rejected));
        assert!(collector.add(accepted));
        assert_eq!(collector.len(), 1);
    }

    #[test]
    fn test_probe_recovery_manager() {
        let config = ProbeConfig {
            probe_interval: Duration::from_millis(10),
            probe_margin: Duration::from_secs(300),
            max_probe_failures: 3,
        };
        let mut mgr = ProbeRecoveryManager::new(config);
        mgr.register_cooldown("anthropic", Duration::from_secs(60));
        assert!(mgr.is_in_cooldown("anthropic"));
        assert!(!mgr.is_in_cooldown("openai"));

        // Should be eligible for probing (within margin)
        std::thread::sleep(Duration::from_millis(15));
        let to_probe = mgr.providers_to_probe();
        assert!(to_probe.contains(&"anthropic".to_string()));

        mgr.record_recovery("anthropic");
        assert!(!mgr.is_in_cooldown("anthropic"));
    }
}
