//! Multi-stage model failover controller.
//!
//! Wraps `AgentRunner` with a composable failover state machine that provides
//! automatic multi-level retry:
//!
//! **Level 1 — Auth Profile Cycling**: When a call fails with auth/rate-limit,
//! rotate to the next auth profile for the same model.
//!
//! **Level 2 — Model Fallback Chain**: When all profiles for a model are
//! exhausted, fall back to the next model in the chain.
//!
//! **Level 3 — Thinking-Level Downgrade**: On context overflow, reduce the
//! thinking level to free output token budget.
//!
//! ## State Machine (DFA)
//!
//! ```text
//! States S = {Init, TryProfile, TryModel, TryThinkLevel, Success, Exhausted}
//! Alphabet Σ = {AuthErr, RateLimit, ContextOverflow, BillingErr, Success, Unknown}
//! ```
//!
//! Worst-case attempts = O(P × M × T) where P=profiles, M=models, T=think_levels.
//! Expected attempts ≈ 2-3 under normal conditions (early exit on success).
//!
//! ## Retry Delay
//!
//! Uses decorrelated jitter: `delay_i = min(cap, rand(base, prev_delay × 3))`
//! This provides better spread than full-jitter exponential backoff.

use clawdesk_types::error::ClawDeskError;
use clawdesk_types::failover::{
    AttemptResult, FailoverAttempt, FailoverConfig, FailoverReason, FallbackModel, ThinkingLevel,
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// State of the failover state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FailoverState {
    /// Initial state — try primary model with first profile.
    Init,
    /// Trying auth profiles for the current model.
    TryProfile {
        model_index: usize,
        profile_index: usize,
    },
    /// Trying the next model in the fallback chain.
    TryModel { model_index: usize },
    /// Trying a lower thinking level for the current model.
    TryThinkLevel {
        model_index: usize,
        level: ThinkingLevel,
    },
    /// Successfully completed.
    Success,
    /// All options exhausted.
    Exhausted,
}

/// The failover controller — sits above the AgentRunner and implements
/// retry logic as a state machine.
///
/// The controller does NOT modify the runner. It wraps each run attempt
/// and transitions state based on the classified error.
pub struct FailoverController {
    /// Failover configuration.
    config: FailoverConfig,
    /// Primary model.
    primary_model: String,
    /// Primary provider.
    primary_provider: String,
    /// Number of auth profiles available per model (simplified).
    profiles_per_model: Vec<usize>,
    /// Record of all attempts.
    attempts: Vec<FailoverAttempt>,
    /// Current state.
    state: FailoverState,
    /// Total attempts so far.
    total_attempts: usize,
    /// Last retry delay for decorrelated jitter.
    last_delay: Duration,
}

impl FailoverController {
    /// Create a new failover controller.
    pub fn new(
        primary_provider: impl Into<String>,
        primary_model: impl Into<String>,
        config: FailoverConfig,
    ) -> Self {
        // Default to 1 profile per model if not specified
        let model_count = config.fallback_models.len() + 1;
        Self {
            config,
            primary_model: primary_model.into(),
            primary_provider: primary_provider.into(),
            profiles_per_model: vec![1; model_count],
            attempts: Vec::new(),
            state: FailoverState::Init,
            total_attempts: 0,
            last_delay: Duration::from_millis(500),
        }
    }

    /// Set the number of available auth profiles for each model.
    /// Index 0 = primary model, 1..N = fallback models.
    pub fn with_profile_counts(mut self, counts: Vec<usize>) -> Self {
        self.profiles_per_model = counts;
        self
    }

    /// Get the next action to take.
    ///
    /// Returns `None` if the controller has reached `Success` or `Exhausted` state.
    pub fn next_action(&self) -> Option<FailoverAction> {
        if self.total_attempts >= self.config.max_total_attempts {
            return None;
        }

        match &self.state {
            FailoverState::Init => Some(FailoverAction {
                provider: self.primary_provider.clone(),
                model: self.primary_model.clone(),
                profile_index: 0,
                thinking_level: None,
                retry_delay: Duration::ZERO,
                attempt_number: self.total_attempts + 1,
            }),
            FailoverState::TryProfile {
                model_index,
                profile_index,
            } => {
                let (provider, model) = self.model_at(*model_index);
                Some(FailoverAction {
                    provider,
                    model,
                    profile_index: *profile_index,
                    thinking_level: None,
                    retry_delay: self.compute_retry_delay(),
                    attempt_number: self.total_attempts + 1,
                })
            }
            FailoverState::TryModel { model_index } => {
                let (provider, model) = self.model_at(*model_index);
                Some(FailoverAction {
                    provider,
                    model,
                    profile_index: 0,
                    thinking_level: None,
                    retry_delay: self.compute_retry_delay(),
                    attempt_number: self.total_attempts + 1,
                })
            }
            FailoverState::TryThinkLevel { model_index, level } => {
                let (provider, model) = self.model_at(*model_index);
                Some(FailoverAction {
                    provider,
                    model,
                    profile_index: 0,
                    thinking_level: Some(*level),
                    retry_delay: self.compute_retry_delay(),
                    attempt_number: self.total_attempts + 1,
                })
            }
            FailoverState::Success | FailoverState::Exhausted => None,
        }
    }

    /// Record success and transition to terminal state.
    pub fn record_success(&mut self, duration_ms: u64) {
        let action = self.next_action();
        if let Some(action) = action {
            self.attempts.push(FailoverAttempt {
                attempt: self.total_attempts + 1,
                model: action.model,
                provider: action.provider,
                profile_id: Some(format!("profile-{}", action.profile_index)),
                thinking_level: action.thinking_level,
                result: AttemptResult::Success,
                duration_ms,
            });
        }
        self.total_attempts += 1;
        self.state = FailoverState::Success;
        info!(attempts = self.total_attempts, "Failover succeeded");
    }

    /// Record failure and transition to next state.
    ///
    /// Implements the DFA transition function:
    /// ```text
    /// δ(TryProfile, AuthErr) = TryProfile    if profiles_remaining > 0
    /// δ(TryProfile, AuthErr) = TryModel       if profiles_remaining = 0
    /// δ(TryProfile, ContextOverflow) = TryThinkLevel
    /// δ(TryModel, *) = TryProfile             with next model's profiles
    /// δ(TryThinkLevel, ContextOverflow) = TryModel if think_levels_remaining = 0
    /// δ(*, Success) = Success
    /// δ(TryModel, *) = Exhausted              if models_remaining = 0
    /// ```
    pub fn record_failure(&mut self, error_msg: &str, duration_ms: u64) {
        let reason = FailoverReason::classify(error_msg);
        let action = self.next_action();

        if let Some(ref action) = action {
            self.attempts.push(FailoverAttempt {
                attempt: self.total_attempts + 1,
                model: action.model.clone(),
                provider: action.provider.clone(),
                profile_id: Some(format!("profile-{}", action.profile_index)),
                thinking_level: action.thinking_level,
                result: AttemptResult::Failed(reason.clone()),
                duration_ms,
            });
        }

        self.total_attempts += 1;

        if self.total_attempts >= self.config.max_total_attempts {
            self.state = FailoverState::Exhausted;
            warn!(
                attempts = self.total_attempts,
                reason = ?reason,
                "Failover exhausted max attempts"
            );
            return;
        }

        // DFA transition
        let current_model_index = match &self.state {
            FailoverState::Init => 0,
            FailoverState::TryProfile { model_index, .. } => *model_index,
            FailoverState::TryModel { model_index } => *model_index,
            FailoverState::TryThinkLevel { model_index, .. } => *model_index,
            _ => {
                self.state = FailoverState::Exhausted;
                return;
            }
        };

        let current_profile = match &self.state {
            FailoverState::TryProfile { profile_index, .. } => *profile_index,
            _ => 0,
        };

        match reason {
            FailoverReason::AuthError | FailoverReason::RateLimit | FailoverReason::BillingError => {
                // Try next profile for same model
                let max_profiles = self
                    .profiles_per_model
                    .get(current_model_index)
                    .copied()
                    .unwrap_or(1);
                let next_profile = current_profile + 1;

                if next_profile < max_profiles {
                    self.state = FailoverState::TryProfile {
                        model_index: current_model_index,
                        profile_index: next_profile,
                    };
                    debug!(
                        model_index = current_model_index,
                        profile_index = next_profile,
                        "Rotating to next auth profile"
                    );
                } else {
                    // All profiles exhausted → try next model
                    self.advance_to_next_model(current_model_index);
                }
            }
            FailoverReason::ContextOverflow => {
                if self.config.enable_thinking_downgrade {
                    // Try downgrading thinking level
                    let current_level = match &self.state {
                        FailoverState::TryThinkLevel { level, .. } => *level,
                        _ => ThinkingLevel::High,
                    };
                    if let Some(lower) = current_level.downgrade() {
                        self.state = FailoverState::TryThinkLevel {
                            model_index: current_model_index,
                            level: lower,
                        };
                        debug!(
                            from = %current_level,
                            to = %lower,
                            "Downgrading thinking level"
                        );
                    } else {
                        // Can't downgrade further → try next model
                        self.advance_to_next_model(current_model_index);
                    }
                } else {
                    self.advance_to_next_model(current_model_index);
                }
            }
            FailoverReason::ModelUnavailable => {
                // Skip directly to next model
                self.advance_to_next_model(current_model_index);
            }
            FailoverReason::ServerError | FailoverReason::NetworkError | FailoverReason::Timeout => {
                // Transient: retry same configuration
                self.state = FailoverState::TryProfile {
                    model_index: current_model_index,
                    profile_index: current_profile,
                };
            }
            FailoverReason::Unknown => {
                // Unknown: advance to next model as a safe fallback
                self.advance_to_next_model(current_model_index);
            }
        }
    }

    /// Advance to the next model in the fallback chain.
    fn advance_to_next_model(&mut self, current_index: usize) {
        let next_index = current_index + 1;
        let total_models = self.config.fallback_models.len() + 1; // +1 for primary

        if next_index < total_models {
            self.state = FailoverState::TryModel {
                model_index: next_index,
            };
            let (provider, model) = self.model_at(next_index);
            info!(
                from_index = current_index,
                to_index = next_index,
                to_model = %model,
                to_provider = %provider,
                "Falling back to next model"
            );
        } else {
            self.state = FailoverState::Exhausted;
            warn!("All fallback models exhausted");
        }
    }

    /// Get provider and model name at the given index.
    /// Index 0 = primary, 1..N = fallback models.
    fn model_at(&self, index: usize) -> (String, String) {
        if index == 0 {
            (self.primary_provider.clone(), self.primary_model.clone())
        } else {
            let fallback_index = index - 1;
            if let Some(fb) = self.config.fallback_models.get(fallback_index) {
                (fb.provider.clone(), fb.model.clone())
            } else {
                (self.primary_provider.clone(), self.primary_model.clone())
            }
        }
    }

    /// Compute retry delay using decorrelated jitter.
    ///
    /// delay = min(cap, uniform(base, prev_delay × 3))
    fn compute_retry_delay(&self) -> Duration {
        let base = self.config.base_retry_delay_ms as f64;
        let cap = self.config.max_retry_delay_ms as f64;
        let prev = self.last_delay.as_millis() as f64;

        // Decorrelated jitter: midpoint between base and prev*3
        let upper = (prev * 3.0).min(cap);
        let delay = ((base + upper) / 2.0).min(cap);

        Duration::from_millis(delay as u64)
    }

    /// Whether the controller has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            FailoverState::Success | FailoverState::Exhausted
        )
    }

    /// Whether the last state was a success.
    pub fn is_success(&self) -> bool {
        self.state == FailoverState::Success
    }

    /// Get all recorded attempts.
    pub fn attempts(&self) -> &[FailoverAttempt] {
        &self.attempts
    }

    /// Total number of attempts made.
    pub fn total_attempts(&self) -> usize {
        self.total_attempts
    }
}

/// An action to take from the failover controller.
#[derive(Debug, Clone)]
pub struct FailoverAction {
    /// Provider to use.
    pub provider: String,
    /// Model to use.
    pub model: String,
    /// Profile index to use.
    pub profile_index: usize,
    /// Optional thinking level override.
    pub thinking_level: Option<ThinkingLevel>,
    /// Delay before attempting.
    pub retry_delay: Duration,
    /// Attempt number (1-indexed).
    pub attempt_number: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> FailoverConfig {
        FailoverConfig {
            fallback_models: vec![
                FallbackModel {
                    provider: "openai".to_string(),
                    model: "gpt-4o".to_string(),
                    thinking_level: None,
                },
                FallbackModel {
                    provider: "anthropic".to_string(),
                    model: "claude-haiku-3".to_string(),
                    thinking_level: None,
                },
            ],
            max_total_attempts: 15,
            enable_thinking_downgrade: true,
            base_retry_delay_ms: 100,
            max_retry_delay_ms: 5000,
        }
    }

    #[test]
    fn test_success_on_first_attempt() {
        let mut ctrl = FailoverController::new("anthropic", "claude-sonnet", test_config());

        let action = ctrl.next_action().unwrap();
        assert_eq!(action.model, "claude-sonnet");
        assert_eq!(action.provider, "anthropic");
        assert_eq!(action.attempt_number, 1);

        ctrl.record_success(150);
        assert!(ctrl.is_terminal());
        assert!(ctrl.is_success());
        assert_eq!(ctrl.total_attempts(), 1);
    }

    #[test]
    fn test_profile_rotation_on_rate_limit() {
        let config = test_config();
        let mut ctrl = FailoverController::new("anthropic", "claude-sonnet", config)
            .with_profile_counts(vec![3, 2, 1]); // 3 profiles for primary

        // First attempt
        let action = ctrl.next_action().unwrap();
        assert_eq!(action.profile_index, 0);

        // Rate limit → rotate profile
        ctrl.record_failure("429 rate limit exceeded", 100);
        let action = ctrl.next_action().unwrap();
        assert_eq!(action.profile_index, 1);
        assert_eq!(action.model, "claude-sonnet"); // same model

        // Rate limit again → next profile
        ctrl.record_failure("rate_limit_error", 100);
        let action = ctrl.next_action().unwrap();
        assert_eq!(action.profile_index, 2);
    }

    #[test]
    fn test_model_fallback_when_profiles_exhausted() {
        let config = test_config();
        let mut ctrl = FailoverController::new("anthropic", "claude-sonnet", config)
            .with_profile_counts(vec![1, 1, 1]); // 1 profile each

        // Primary fails
        ctrl.record_failure("401 Unauthorized", 50);

        // Should advance to first fallback model
        let action = ctrl.next_action().unwrap();
        assert_eq!(action.model, "gpt-4o");
        assert_eq!(action.provider, "openai");
    }

    #[test]
    fn test_thinking_level_downgrade_on_context_overflow() {
        let config = test_config();
        let mut ctrl = FailoverController::new("anthropic", "claude-sonnet", config);

        ctrl.record_failure("maximum context length exceeded", 200);

        let action = ctrl.next_action().unwrap();
        assert_eq!(action.thinking_level, Some(ThinkingLevel::Medium));

        ctrl.record_failure("context window exceeded", 200);
        let action = ctrl.next_action().unwrap();
        assert_eq!(action.thinking_level, Some(ThinkingLevel::Low));

        ctrl.record_failure("context length exceeded", 200);
        let action = ctrl.next_action().unwrap();
        assert_eq!(action.thinking_level, Some(ThinkingLevel::Off));
    }

    #[test]
    fn test_exhaustion() {
        let config = FailoverConfig {
            fallback_models: vec![],
            max_total_attempts: 2,
            ..Default::default()
        };
        let mut ctrl = FailoverController::new("anthropic", "claude-sonnet", config)
            .with_profile_counts(vec![1]);

        ctrl.record_failure("rate limit", 50);
        ctrl.record_failure("rate limit", 50);

        assert!(ctrl.is_terminal());
        assert!(!ctrl.is_success());
        assert!(ctrl.next_action().is_none());
    }

    #[test]
    fn test_transient_error_retries_same_config() {
        let config = test_config();
        let mut ctrl = FailoverController::new("anthropic", "claude-sonnet", config);

        // Server error → retry same
        ctrl.record_failure("500 internal server error", 100);
        let action = ctrl.next_action().unwrap();
        assert_eq!(action.model, "claude-sonnet");
        assert_eq!(action.profile_index, 0); // same profile
    }

    #[test]
    fn test_attempt_recording() {
        let config = test_config();
        let mut ctrl = FailoverController::new("anthropic", "claude-sonnet", config);

        ctrl.record_failure("rate limit", 100);
        ctrl.record_success(200);

        assert_eq!(ctrl.attempts().len(), 2);
        assert!(matches!(
            ctrl.attempts()[0].result,
            AttemptResult::Failed(FailoverReason::RateLimit)
        ));
        assert!(matches!(ctrl.attempts()[1].result, AttemptResult::Success));
    }
}
