//! Context window guard — predictive compaction with running token estimation.
//!
//! Maintains running token estimate T_est.
//! Triggers compaction when T_est > α × C (α = 0.80, C = context limit).
//! Tiered compaction: Level 1 (drop metadata, ~15% savings) →
//! Level 2 (summarize old turns, ~40% savings) →
//! Level 3 (aggressive truncation to last n turns).
//! Circuit breaker: closed → open (after 3 failures in 60s) → half-open.
//!
//! ## T12: Adaptive Context Guard
//!
//! Multi-source context window resolution: the effective context limit is
//! `min(model_limit, provider_limit, agent_override)`. Thresholds adapt
//! dynamically based on message-role token distribution — conversations with
//! heavy tool output trigger earlier DropMetadata compaction. ForceTruncate
//! and CircuitBroken recovery now use token-budget retention instead of a
//! fixed message count, preventing pathological cases where 10 large messages
//! already exceed the budget.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Resolve the effective context limit from multiple sources.
///
/// Takes the minimum of all provided limits (model, provider, agent), so the
/// guard never exceeds the most restrictive constraint.
///
/// ```
/// use clawdesk_domain::context_guard::ContextLimitResolver;
/// let limit = ContextLimitResolver::new()
///     .model_limit(200_000)
///     .provider_limit(128_000)
///     .agent_override(100_000)
///     .resolve();
/// assert_eq!(limit, 100_000);
/// ```
#[derive(Debug, Clone)]
pub struct ContextLimitResolver {
    model: Option<usize>,
    provider: Option<usize>,
    agent: Option<usize>,
    fallback: usize,
}

impl ContextLimitResolver {
    pub fn new() -> Self {
        Self {
            model: None,
            provider: None,
            agent: None,
            fallback: 128_000,
        }
    }

    pub fn model_limit(mut self, limit: usize) -> Self {
        self.model = Some(limit);
        self
    }

    pub fn provider_limit(mut self, limit: usize) -> Self {
        self.provider = Some(limit);
        self
    }

    pub fn agent_override(mut self, limit: usize) -> Self {
        self.agent = Some(limit);
        self
    }

    pub fn fallback(mut self, limit: usize) -> Self {
        self.fallback = limit;
        self
    }

    /// Resolve to the effective limit — min of all specified sources.
    pub fn resolve(&self) -> usize {
        let mut effective = self.fallback;
        if let Some(m) = self.model {
            effective = effective.min(m);
        }
        if let Some(p) = self.provider {
            effective = effective.min(p);
        }
        if let Some(a) = self.agent {
            effective = effective.min(a);
        }
        // Sanity floor: never go below 4096 tokens
        effective.max(4096)
    }
}

impl Default for ContextLimitResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for the context window guard.
#[derive(Debug, Clone)]
pub struct ContextGuardConfig {
    /// Total context window size in tokens.
    pub context_limit: usize,
    /// Trigger compaction at this fraction of the limit.
    /// Set to 0.0 to use adaptive thresholds (T12).
    pub trigger_threshold: f64, // α = 0.80
    /// Reserve tokens for the model's response.
    pub response_reserve: usize,
    /// Maximum compaction failures before circuit breaks.
    pub circuit_breaker_threshold: u32,
    /// Circuit breaker cooldown.
    pub circuit_breaker_cooldown: Duration,
    /// Enable adaptive thresholds based on message distribution (T12).
    /// When true, the trigger threshold shifts earlier if tool/assistant
    /// messages dominate the context (more compressible content).
    pub adaptive_thresholds: bool,
    /// Share of context budget to retain during force-truncation (T12).
    /// E.g. 0.5 means keep messages up to 50% of the effective limit.
    /// Replaces the old fixed `keep_last_n: 10`.
    pub force_truncate_retain_share: f64,
}

impl Default for ContextGuardConfig {
    fn default() -> Self {
        Self {
            context_limit: 128_000,
            trigger_threshold: 0.80,
            response_reserve: 8_192,
            circuit_breaker_threshold: 3,
            circuit_breaker_cooldown: Duration::from_secs(60),
            adaptive_thresholds: true,
            force_truncate_retain_share: 0.50,
        }
    }
}

/// Running token counter for the context window.
#[derive(Clone)]
pub struct ContextGuard {
    config: ContextGuardConfig,
    /// Current estimated token count.
    estimated_tokens: usize,
    /// Circuit breaker state.
    breaker: CircuitBreaker,
    /// T12: Per-role token distribution for adaptive thresholds.
    role_tokens: RoleTokenDistribution,
}

/// Circuit breaker states for compaction failures.
#[derive(Debug, Clone)]
enum CircuitBreakerState {
    Closed,
    Open { opened_at: Instant },
    HalfOpen,
}

#[derive(Clone)]
struct CircuitBreaker {
    state: CircuitBreakerState,
    failure_count: u32,
    last_failure: Option<Instant>,
    threshold: u32,
    cooldown: Duration,
}

impl CircuitBreaker {
    fn new(threshold: u32, cooldown: Duration) -> Self {
        Self {
            state: CircuitBreakerState::Closed,
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
            self.state = CircuitBreakerState::Open { opened_at: now };
        }
    }

    fn record_success(&mut self) {
        self.failure_count = 0;
        self.state = CircuitBreakerState::Closed;
    }

    fn is_allowed(&mut self) -> bool {
        match &self.state {
            CircuitBreakerState::Closed => true,
            CircuitBreakerState::Open { opened_at } => {
                if Instant::now().duration_since(*opened_at) >= self.cooldown {
                    self.state = CircuitBreakerState::HalfOpen;
                    true // Allow one attempt
                } else {
                    false
                }
            }
            CircuitBreakerState::HalfOpen => true,
        }
    }
}

/// Compaction level — progressively more aggressive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CompactionLevel {
    /// Drop tool-result metadata, keep text summaries. Saves ~15%.
    DropMetadata = 1,
    /// Summarize turns older than k. Saves ~40%.
    SummarizeOld = 2,
    /// Aggressive truncation to last n turns.
    Truncate = 3,
}

/// T12: Per-role token distribution for adaptive threshold calculation.
///
/// Tracks how many tokens belong to each message role so the guard can
/// adjust compaction thresholds dynamically. Conversations with heavy
/// tool-result content trigger earlier DropMetadata compaction (since
/// tool results are highly compressible), while user-heavy conversations
/// maintain the default thresholds.
#[derive(Debug, Clone, Default)]
pub struct RoleTokenDistribution {
    pub system_tokens: usize,
    pub user_tokens: usize,
    pub assistant_tokens: usize,
    pub tool_tokens: usize,
}

impl RoleTokenDistribution {
    pub fn total(&self) -> usize {
        self.system_tokens + self.user_tokens + self.assistant_tokens + self.tool_tokens
    }

    /// Fraction of tokens from tool results (0.0–1.0).
    pub fn tool_share(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        self.tool_tokens as f64 / total as f64
    }

    /// Record tokens for a given role.
    pub fn record(&mut self, role: &str, tokens: usize) {
        match role {
            "system" => self.system_tokens += tokens,
            "user" => self.user_tokens += tokens,
            "assistant" => self.assistant_tokens += tokens,
            "tool" | "tool_result" | "function" => self.tool_tokens += tokens,
            _ => self.assistant_tokens += tokens,
        }
    }

    /// Reset all counters (e.g., after re-counting from scratch).
    pub fn reset(&mut self) {
        self.system_tokens = 0;
        self.user_tokens = 0;
        self.assistant_tokens = 0;
        self.tool_tokens = 0;
    }
}

/// Result of a compaction operation.
#[derive(Debug)]
pub struct CompactionResult {
    pub level: CompactionLevel,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub turns_removed: usize,
    pub turns_summarized: usize,
}

/// Action recommended by the context guard.
#[derive(Debug, PartialEq)]
pub enum GuardAction {
    /// Context is within budget, proceed normally.
    Ok,
    /// Compaction needed at specified level.
    Compact(CompactionLevel),
    /// T12: Context is critically over budget, retain messages up to this
    /// token budget (replaces fixed `keep_last_n`).
    ForceTruncate { retain_tokens: usize },
    /// T12: Circuit breaker is open. Retain messages up to this token budget
    /// as a safe fallback (replaces hardcoded 10-message truncation).
    CircuitBroken { retain_tokens: usize },
}

impl ContextGuard {
    pub fn new(config: ContextGuardConfig) -> Self {
        let breaker = CircuitBreaker::new(
            config.circuit_breaker_threshold,
            config.circuit_breaker_cooldown,
        );
        Self {
            config,
            estimated_tokens: 0,
            breaker,
            role_tokens: RoleTokenDistribution::default(),
        }
    }

    /// Create a guard with a resolved context limit from multiple sources.
    ///
    /// ```rust,ignore
    /// let limit = ContextLimitResolver::new()
    ///     .model_limit(200_000)
    ///     .provider_limit(128_000)
    ///     .resolve();
    /// let guard = ContextGuard::with_resolved_limit(limit, 0.80, 8_192);
    /// ```
    pub fn with_resolved_limit(
        context_limit: usize,
        trigger_threshold: f64,
        response_reserve: usize,
    ) -> Self {
        Self::new(ContextGuardConfig {
            context_limit,
            trigger_threshold,
            response_reserve,
            adaptive_thresholds: true,
            ..Default::default()
        })
    }

    /// Update the token estimate after appending a message.
    /// Token estimation: ~4 chars per token (fast O(1) estimate).
    pub fn record_tokens(&mut self, text: &str) {
        self.estimated_tokens += estimate_tokens(text);
    }

    /// Record tokens with role annotation (T12 adaptive distribution).
    pub fn record_tokens_for_role(&mut self, text: &str, role: &str) {
        let tokens = estimate_tokens(text);
        self.estimated_tokens += tokens;
        self.role_tokens.record(role, tokens);
    }

    /// Set the token count directly (e.g., from a tokenizer).
    pub fn set_token_count(&mut self, count: usize) {
        self.estimated_tokens = count;
    }

    /// Subtract tokens after compaction.
    pub fn subtract_tokens(&mut self, count: usize) {
        self.estimated_tokens = self.estimated_tokens.saturating_sub(count);
    }

    /// Get current estimated token count.
    pub fn current_tokens(&self) -> usize {
        self.estimated_tokens
    }

    /// Available token budget for the next response.
    pub fn available_budget(&self) -> usize {
        let used = self.estimated_tokens + self.config.response_reserve;
        self.config.context_limit.saturating_sub(used)
    }

    /// Check what action should be taken given current token usage.
    ///
    /// T12: When `adaptive_thresholds` is enabled, the trigger threshold
    /// shifts down by up to 10 percentage points when tool-result content
    /// dominates (>40% of tokens). This is safe because tool output is
    /// highly compressible — DropMetadata alone saves ~15%.
    ///
    /// ForceTruncate now returns a token budget (`retain_tokens`) instead of
    /// a fixed message count. The caller keeps the newest messages that fit
    /// within this budget.
    pub fn check(&mut self) -> GuardAction {
        let effective_limit = self.config.context_limit.saturating_sub(self.config.response_reserve);
        if effective_limit == 0 {
            return GuardAction::Ok;
        }

        // T12: Adaptive threshold — shift trigger earlier when tool output
        // dominates, since it's highly compressible.
        let base_threshold = self.config.trigger_threshold;
        let adaptive_alpha = if self.config.adaptive_thresholds {
            let tool_share = self.role_tokens.tool_share();
            // Shift threshold down by up to 0.10 (10pp) when tool_share > 0.4.
            // Formula: α' = α - 0.10 × clamp((tool_share - 0.2) / 0.4, 0, 1)
            // At tool_share=0.2 → no shift. At tool_share=0.6+ → full 10pp shift.
            let shift = 0.10 * ((tool_share - 0.2) / 0.4).clamp(0.0, 1.0);
            (base_threshold - shift).max(0.50) // Floor at 50%
        } else {
            base_threshold
        };

        let threshold = (effective_limit as f64 * adaptive_alpha) as usize;
        if self.estimated_tokens <= threshold {
            return GuardAction::Ok;
        }

        // T12: Budget-based retention for circuit breaker recovery.
        // Retain up to force_truncate_retain_share of the effective limit.
        let retain_budget = (effective_limit as f64 * self.config.force_truncate_retain_share) as usize;

        if !self.breaker.is_allowed() {
            return GuardAction::CircuitBroken {
                retain_tokens: retain_budget,
            };
        }

        // Determine compaction level based on how far over we are
        let ratio = self.estimated_tokens as f64 / effective_limit as f64;
        if ratio > 0.95 {
            GuardAction::ForceTruncate { retain_tokens: retain_budget }
        } else if ratio > 0.90 {
            GuardAction::Compact(CompactionLevel::SummarizeOld)
        } else {
            GuardAction::Compact(CompactionLevel::DropMetadata)
        }
    }

    /// Report compaction success.
    pub fn compaction_succeeded(&mut self, result: &CompactionResult) {
        self.estimated_tokens = result.tokens_after;
        self.breaker.record_success();
        // Reset role distribution since message shapes may have changed
        self.role_tokens.reset();
    }

    /// Report compaction failure.
    pub fn compaction_failed(&mut self) {
        self.breaker.record_failure();
    }

    /// Get the current role token distribution (T12).
    pub fn role_distribution(&self) -> &RoleTokenDistribution {
        &self.role_tokens
    }

    /// Reset the role token distribution (e.g., after re-scanning messages).
    pub fn reset_role_distribution(&mut self) {
        self.role_tokens.reset();
    }

    /// Utilization as a fraction (0.0 - 1.0).
    pub fn utilization(&self) -> f64 {
        let effective = self.config.context_limit.saturating_sub(self.config.response_reserve);
        if effective == 0 {
            return 1.0;
        }
        self.estimated_tokens as f64 / effective as f64
    }

    /// Get the effective context limit (after response reserve).
    pub fn effective_limit(&self) -> usize {
        self.config.context_limit.saturating_sub(self.config.response_reserve)
    }

    /// Get the configured context limit.
    pub fn context_limit(&self) -> usize {
        self.config.context_limit
    }
}

// Re-export the canonical tokenizer from clawdesk-types.
// This was previously defined inline here; now consolidated in one place.
pub use clawdesk_types::tokenizer::estimate_tokens;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guard_ok_when_under_threshold() {
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 1000,
            trigger_threshold: 0.80,
            response_reserve: 100,
            ..Default::default()
        });
        guard.set_token_count(500); // 500 / 900 = 55% < 80%
        assert_eq!(guard.check(), GuardAction::Ok);
    }

    #[test]
    fn test_guard_triggers_compaction() {
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 1000,
            trigger_threshold: 0.80,
            response_reserve: 100,
            ..Default::default()
        });
        guard.set_token_count(750); // 750 / 900 = 83% > 80%
        let action = guard.check();
        assert!(matches!(action, GuardAction::Compact(CompactionLevel::DropMetadata)));
    }

    #[test]
    fn test_guard_force_truncate() {
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 1000,
            trigger_threshold: 0.80,
            response_reserve: 100,
            ..Default::default()
        });
        guard.set_token_count(860); // 860 / 900 = 95.5% > 95%
        let action = guard.check();
        // T12: ForceTruncate now returns retain_tokens instead of keep_last_n
        match action {
            GuardAction::ForceTruncate { retain_tokens } => {
                // retain = 900 * 0.50 = 450
                assert_eq!(retain_tokens, 450);
            }
            other => panic!("expected ForceTruncate, got {:?}", other),
        }
    }

    #[test]
    fn test_token_estimation() {
        // Empty string
        assert_eq!(estimate_tokens(""), 0);
        // Single char (alnum: 1/4.2 ≈ 0.24, ceil = 1)
        assert_eq!(estimate_tokens("a"), 1);
        // "hello" = 5 alnum: 5/4.2 ≈ 1.19, ceil = 2
        assert_eq!(estimate_tokens("hello"), 2);
        // Punctuation-heavy JSON: {"a": 1} = 4 punct + 1 alnum + 2 ws
        let json_est = estimate_tokens("{\"a\": 1}");
        assert!(json_est >= 3, "JSON should estimate more tokens due to punctuation");
    }

    #[test]
    fn test_utilization() {
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 1000,
            trigger_threshold: 0.80,
            response_reserve: 0,
            ..Default::default()
        });
        guard.set_token_count(500);
        assert!((guard.utilization() - 0.5).abs() < 0.01);
    }

    // ── T12 tests ──

    #[test]
    fn test_context_limit_resolver_min_of_sources() {
        let limit = ContextLimitResolver::new()
            .model_limit(200_000)
            .provider_limit(128_000)
            .agent_override(100_000)
            .resolve();
        assert_eq!(limit, 100_000);
    }

    #[test]
    fn test_context_limit_resolver_fallback() {
        let limit = ContextLimitResolver::new().resolve();
        assert_eq!(limit, 128_000);
    }

    #[test]
    fn test_context_limit_resolver_floor() {
        let limit = ContextLimitResolver::new()
            .model_limit(1000) // below 4096 floor
            .resolve();
        assert_eq!(limit, 4096);
    }

    #[test]
    fn test_adaptive_threshold_shifts_earlier_for_tool_heavy() {
        // With 60% tool tokens, threshold should shift down ~10pp
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 10_000,
            trigger_threshold: 0.80,
            response_reserve: 0,
            adaptive_thresholds: true,
            ..Default::default()
        });
        // Simulate 60% tool tokens
        guard.role_tokens.tool_tokens = 4200;
        guard.role_tokens.user_tokens = 1400;
        guard.role_tokens.assistant_tokens = 1400;
        // α' ≈ 0.70, threshold = 7000. Set tokens above threshold.
        guard.set_token_count(7100); // 71% — over adaptive α'=0.70 but under base 0.80

        let action = guard.check();
        // With adaptive: α' ≈ 0.80 - 0.10 = 0.70, so 71% > 70% → compact
        assert!(matches!(action, GuardAction::Compact(CompactionLevel::DropMetadata)),
            "expected DropMetadata at 71% with tool-heavy adaptive, got {:?}", action);
    }

    #[test]
    fn test_no_adaptive_shift_for_user_heavy() {
        // With 10% tool tokens, threshold should NOT shift
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 10_000,
            trigger_threshold: 0.80,
            response_reserve: 0,
            adaptive_thresholds: true,
            ..Default::default()
        });
        guard.role_tokens.tool_tokens = 700;
        guard.role_tokens.user_tokens = 3500;
        guard.role_tokens.assistant_tokens = 2800;
        guard.set_token_count(7000); // 70% — under 80% threshold

        let action = guard.check();
        // Tool share = 10% < 20%, no shift. 70% < 80% → Ok
        assert_eq!(action, GuardAction::Ok);
    }

    #[test]
    fn test_circuit_broken_returns_budget() {
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: 1000,
            trigger_threshold: 0.80,
            response_reserve: 100,
            circuit_breaker_threshold: 2,
            circuit_breaker_cooldown: Duration::from_secs(300),
            ..Default::default()
        });
        guard.set_token_count(800);
        // Trip the circuit breaker
        guard.compaction_failed();
        guard.compaction_failed();

        let action = guard.check();
        match action {
            GuardAction::CircuitBroken { retain_tokens } => {
                // retain = 900 * 0.50 = 450
                assert_eq!(retain_tokens, 450);
            }
            other => panic!("expected CircuitBroken, got {:?}", other),
        }
    }

    #[test]
    fn test_role_token_distribution() {
        let mut dist = RoleTokenDistribution::default();
        dist.record("user", 100);
        dist.record("assistant", 200);
        dist.record("tool", 300);
        assert_eq!(dist.total(), 600);
        assert!((dist.tool_share() - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_with_resolved_limit() {
        let guard = ContextGuard::with_resolved_limit(50_000, 0.75, 4_096);
        assert_eq!(guard.context_limit(), 50_000);
        assert_eq!(guard.effective_limit(), 50_000 - 4_096);
    }
}
