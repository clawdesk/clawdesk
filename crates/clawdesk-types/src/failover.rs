//! Failover types — error classification and failover state model.
//!
//! Defines the error classification taxonomy and state machine types
//! used by the failover controller in `clawdesk-agents`.

use serde::{Deserialize, Serialize};

/// Classified reason for a provider failure.
///
/// Error classification uses pattern matching on provider error messages
/// to route failures to the correct failover path:
/// - AuthErr/RateLimit → rotate auth profile
/// - ContextOverflow → downgrade thinking level
/// - BillingErr → rotate profile, then model
/// - ServerErr → retry with backoff
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FailoverReason {
    /// Authentication failure (invalid key, expired token).
    AuthError,
    /// API rate limit exceeded (429).
    RateLimit,
    /// Context window exceeded (input too large).
    ContextOverflow,
    /// Billing/quota exhausted.
    BillingError,
    /// Server error (5xx).
    ServerError,
    /// Network connectivity failure.
    NetworkError,
    /// Model not available or deprecated.
    ModelUnavailable,
    /// Request timeout.
    Timeout,
    /// Content was filtered by the provider (empty response).
    ContentFilter,
    /// Unknown/unclassified error.
    Unknown,
}

impl FailoverReason {
    /// Classify an error message into a `FailoverReason`.
    ///
    /// Uses substring matching for O(n) classification where n = message length.
    /// Patterns are checked in priority order (most specific first).
    pub fn classify(error_msg: &str) -> Self {
        let lower = error_msg.to_lowercase();

        // Auth errors
        if lower.contains("invalid api key")
            || lower.contains("invalid x-api-key")
            || lower.contains("authentication")
            || lower.contains("unauthorized")
            || lower.contains("invalid_api_key")
            || lower.contains("401")
        {
            return Self::AuthError;
        }

        // Rate limits
        if lower.contains("rate limit")
            || lower.contains("rate_limit")
            || lower.contains("too many requests")
            || lower.contains("429")
            || lower.contains("overloaded")
        {
            return Self::RateLimit;
        }

        // Context overflow
        if lower.contains("context length")
            || lower.contains("context window")
            || lower.contains("maximum context")
            || lower.contains("too many tokens")
            || lower.contains("token limit")
            || lower.contains("max_tokens")
            || lower.contains("input too long")
        {
            return Self::ContextOverflow;
        }

        // Billing
        if lower.contains("billing")
            || lower.contains("quota")
            || lower.contains("insufficient_quota")
            || lower.contains("payment required")
            || lower.contains("402")
        {
            return Self::BillingError;
        }

        // Server errors
        if lower.contains("internal server error")
            || lower.contains("500")
            || lower.contains("502")
            || lower.contains("503")
            || lower.contains("service unavailable")
        {
            return Self::ServerError;
        }

        // Network errors
        if lower.contains("connection refused")
            || lower.contains("dns")
            || lower.contains("network")
            || lower.contains("timed out")
            || lower.contains("timeout")
        {
            return Self::NetworkError;
        }

        // Model unavailable
        if lower.contains("model not found")
            || lower.contains("model_not_found")
            || lower.contains("deprecated")
            || lower.contains("not available")
        {
            return Self::ModelUnavailable;
        }

        // Content filter (empty response from provider)
        if lower.contains("content filter")
            || lower.contains("content_filter")
            || lower.contains("empty response")
        {
            return Self::ContentFilter;
        }

        Self::Unknown
    }

    /// Whether this failure type should trigger profile rotation.
    pub fn rotates_profile(&self) -> bool {
        matches!(
            self,
            Self::AuthError | Self::RateLimit | Self::BillingError
        )
    }

    /// Whether this failure type should trigger model fallback.
    pub fn rotates_model(&self) -> bool {
        matches!(
            self,
            Self::ModelUnavailable | Self::BillingError | Self::ContextOverflow
        )
    }

    /// Whether this failure type should trigger thinking-level downgrade.
    pub fn downgrades_thinking(&self) -> bool {
        matches!(self, Self::ContextOverflow)
    }

    /// Whether this failure is likely transient and worth retrying.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::RateLimit | Self::ServerError | Self::NetworkError | Self::Timeout
        )
    }
}

/// Thinking level for models that support variable reasoning depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThinkingLevel {
    High,
    Medium,
    Low,
    Off,
}

impl ThinkingLevel {
    /// Downgrade to the next lower level.
    pub fn downgrade(self) -> Option<Self> {
        match self {
            Self::High => Some(Self::Medium),
            Self::Medium => Some(Self::Low),
            Self::Low => Some(Self::Off),
            Self::Off => None,
        }
    }

    /// All levels in descending order.
    pub fn all_descending() -> &'static [ThinkingLevel] {
        &[Self::High, Self::Medium, Self::Low, Self::Off]
    }
}

impl std::fmt::Display for ThinkingLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::High => write!(f, "high"),
            Self::Medium => write!(f, "medium"),
            Self::Low => write!(f, "low"),
            Self::Off => write!(f, "off"),
        }
    }
}

/// A fallback model specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackModel {
    /// Provider name (e.g., "anthropic", "openai").
    pub provider: String,
    /// Model identifier (e.g., "claude-sonnet-4-20250514").
    pub model: String,
    /// Optional thinking level override.
    pub thinking_level: Option<ThinkingLevel>,
}

/// Configuration for the failover chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailoverConfig {
    /// Ordered list of fallback models (tried in order).
    pub fallback_models: Vec<FallbackModel>,
    /// Maximum total attempts across all levels.
    pub max_total_attempts: usize,
    /// Whether to attempt thinking-level downgrade on context overflow.
    pub enable_thinking_downgrade: bool,
    /// Base retry delay for decorrelated jitter backoff (ms).
    pub base_retry_delay_ms: u64,
    /// Maximum retry delay cap (ms).
    pub max_retry_delay_ms: u64,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            fallback_models: Vec::new(),
            max_total_attempts: 15,
            enable_thinking_downgrade: true,
            base_retry_delay_ms: 500,
            max_retry_delay_ms: 30_000,
        }
    }
}

/// A failover attempt record for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailoverAttempt {
    /// Attempt number (1-indexed).
    pub attempt: usize,
    /// Model used for this attempt.
    pub model: String,
    /// Provider used for this attempt.
    pub provider: String,
    /// Profile ID used (if applicable).
    pub profile_id: Option<String>,
    /// Thinking level used (if applicable).
    pub thinking_level: Option<ThinkingLevel>,
    /// Result of this attempt.
    pub result: AttemptResult,
    /// Duration of this attempt in milliseconds.
    pub duration_ms: u64,
}

/// Result of a single failover attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttemptResult {
    Success,
    Failed(FailoverReason),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_rate_limit() {
        assert_eq!(
            FailoverReason::classify("rate limit exceeded"),
            FailoverReason::RateLimit
        );
        assert_eq!(
            FailoverReason::classify("Error: 429 Too Many Requests"),
            FailoverReason::RateLimit
        );
    }

    #[test]
    fn test_classify_auth() {
        assert_eq!(
            FailoverReason::classify("Invalid API key provided"),
            FailoverReason::AuthError
        );
        assert_eq!(
            FailoverReason::classify("401 Unauthorized"),
            FailoverReason::AuthError
        );
    }

    #[test]
    fn test_classify_context_overflow() {
        assert_eq!(
            FailoverReason::classify("maximum context length exceeded"),
            FailoverReason::ContextOverflow
        );
    }

    #[test]
    fn test_classify_unknown() {
        assert_eq!(
            FailoverReason::classify("some random error"),
            FailoverReason::Unknown
        );
    }

    #[test]
    fn test_thinking_level_downgrade() {
        assert_eq!(ThinkingLevel::High.downgrade(), Some(ThinkingLevel::Medium));
        assert_eq!(ThinkingLevel::Medium.downgrade(), Some(ThinkingLevel::Low));
        assert_eq!(ThinkingLevel::Low.downgrade(), Some(ThinkingLevel::Off));
        assert_eq!(ThinkingLevel::Off.downgrade(), None);
    }

    #[test]
    fn test_failover_reason_routing() {
        assert!(FailoverReason::RateLimit.rotates_profile());
        assert!(!FailoverReason::RateLimit.rotates_model());
        assert!(FailoverReason::ContextOverflow.downgrades_thinking());
        assert!(FailoverReason::ServerError.is_transient());
        assert!(!FailoverReason::AuthError.is_transient());
    }
}
