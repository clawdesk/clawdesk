//! Per-channel retry policies with server-directed backoff.
//!
//! Each platform has specific rate-limiting semantics:
//! - **Telegram**: Parses `retry_after` from `parameters.retry_after` (seconds).
//!   Matches retryable errors: 429, timeout, connect, reset, closed, unavailable.
//! - **Discord**: Uses `RateLimitError.retryAfter` from SDK responses.
//! - **Slack**: Respects `Retry-After` header on 429 responses.
//! - **Generic**: Configurable via `channels.<name>.retry` in config.
//!
//! All policies compose with the unified retry infrastructure in `clawdesk-infra`.

use std::time::Duration;

/// Per-channel retry policy configuration.
#[derive(Debug, Clone)]
pub struct ChannelRetryPolicy {
    /// Channel name (for logging).
    pub channel: String,
    /// Maximum number of retry attempts.
    pub max_retries: u32,
    /// Base delay for first retry.
    pub base_delay: Duration,
    /// Maximum delay cap.
    pub max_delay: Duration,
    /// Regex patterns for retryable error messages.
    pub retryable_patterns: Vec<String>,
}

impl ChannelRetryPolicy {
    /// Check if an error message matches any retryable pattern.
    pub fn is_retryable_message(&self, error_msg: &str) -> bool {
        let lower = error_msg.to_lowercase();
        self.retryable_patterns
            .iter()
            .any(|pat| lower.contains(&pat.to_lowercase()))
    }
}

/// Telegram retry policy.
///
/// Parses `retry_after` from nested error responses:
/// `parameters.retry_after` or `response.parameters.retry_after`.
/// Conversion: `(retry_after * 1000.0).ceil() as u64` to preserve sub-second precision.
pub fn telegram_policy() -> ChannelRetryPolicy {
    ChannelRetryPolicy {
        channel: "telegram".to_string(),
        max_retries: 5,
        base_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(60),
        retryable_patterns: vec![
            "429".to_string(),
            "timeout".to_string(),
            "connect".to_string(),
            "reset".to_string(),
            "closed".to_string(),
            "unavailable".to_string(),
            "temporarily".to_string(),
            "too many requests".to_string(),
        ],
    }
}

/// Discord retry policy.
///
/// Uses `RateLimitError.retryAfter` when available.
/// Only retries `RateLimitError` instances by default.
pub fn discord_policy() -> ChannelRetryPolicy {
    ChannelRetryPolicy {
        channel: "discord".to_string(),
        max_retries: 5,
        base_delay: Duration::from_millis(500),
        max_delay: Duration::from_secs(30),
        retryable_patterns: vec![
            "rate limit".to_string(),
            "429".to_string(),
            "too many requests".to_string(),
            "you are being rate limited".to_string(),
        ],
    }
}

/// Slack retry policy.
///
/// Respects `Retry-After` header on 429 responses.
pub fn slack_policy() -> ChannelRetryPolicy {
    ChannelRetryPolicy {
        channel: "slack".to_string(),
        max_retries: 3,
        base_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(30),
        retryable_patterns: vec![
            "429".to_string(),
            "rate_limited".to_string(),
            "timeout".to_string(),
            "service_unavailable".to_string(),
        ],
    }
}

/// Matrix retry policy.
pub fn matrix_policy() -> ChannelRetryPolicy {
    ChannelRetryPolicy {
        channel: "matrix".to_string(),
        max_retries: 3,
        base_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(30),
        retryable_patterns: vec![
            "429".to_string(),
            "limit exceeded".to_string(),
            "timeout".to_string(),
            "M_LIMIT_EXCEEDED".to_string(),
        ],
    }
}

/// Generic retry policy for channels without specific handling.
pub fn generic_policy(channel: &str) -> ChannelRetryPolicy {
    ChannelRetryPolicy {
        channel: channel.to_string(),
        max_retries: 3,
        base_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(30),
        retryable_patterns: vec![
            "429".to_string(),
            "timeout".to_string(),
            "rate limit".to_string(),
        ],
    }
}

/// Get the retry policy for a channel by name.
pub fn policy_for_channel(channel: &str) -> ChannelRetryPolicy {
    match channel.to_lowercase().as_str() {
        "telegram" => telegram_policy(),
        "discord" => discord_policy(),
        "slack" => slack_policy(),
        "matrix" => matrix_policy(),
        other => generic_policy(other),
    }
}

/// Parse Telegram retry_after from an error response body.
///
/// Tries nested paths: `parameters.retry_after` and `retry_after`.
/// Returns duration in milliseconds using `(secs * 1000.0).ceil()`.
pub fn parse_telegram_retry_after(body: &str) -> Option<Duration> {
    let val: serde_json::Value = serde_json::from_str(body).ok()?;

    // Try parameters.retry_after
    let retry_after = val
        .get("parameters")
        .and_then(|p| p.get("retry_after"))
        .and_then(|v| v.as_f64())
        .or_else(|| val.get("retry_after").and_then(|v| v.as_f64()));

    retry_after.map(|secs| {
        let ms = (secs * 1000.0).ceil() as u64;
        Duration::from_millis(ms)
    })
}

/// Parse Discord rate limit retry_after from response headers/body.
pub fn parse_discord_retry_after(body: &str) -> Option<Duration> {
    let val: serde_json::Value = serde_json::from_str(body).ok()?;
    let retry_after = val.get("retry_after").and_then(|v| v.as_f64())?;
    let ms = (retry_after * 1000.0).ceil() as u64;
    Some(Duration::from_millis(ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telegram_policy_retryable() {
        let policy = telegram_policy();
        assert!(policy.is_retryable_message("HTTP 429 Too Many Requests"));
        assert!(policy.is_retryable_message("connection timeout after 30s"));
        assert!(policy.is_retryable_message("connection reset by peer"));
        assert!(!policy.is_retryable_message("HTTP 401 Unauthorized"));
    }

    #[test]
    fn test_parse_telegram_retry_after() {
        let body = r#"{"ok": false, "error_code": 429, "description": "Too Many Requests", "parameters": {"retry_after": 5.5}}"#;
        let duration = parse_telegram_retry_after(body).unwrap();
        assert_eq!(duration, Duration::from_millis(5500));
    }

    #[test]
    fn test_parse_discord_retry_after() {
        let body = r#"{"retry_after": 1.234, "global": false}"#;
        let duration = parse_discord_retry_after(body).unwrap();
        assert_eq!(duration, Duration::from_millis(1234));
    }

    #[test]
    fn test_policy_for_channel() {
        let tg = policy_for_channel("Telegram");
        assert_eq!(tg.channel, "telegram");
        assert_eq!(tg.max_retries, 5);

        let dc = policy_for_channel("discord");
        assert_eq!(dc.channel, "discord");

        let custom = policy_for_channel("custom_webhook");
        assert_eq!(custom.channel, "custom_webhook");
        assert_eq!(custom.max_retries, 3);
    }
}
