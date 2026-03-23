//! Tool rate limiter — token-bucket rate limiting for agent tool invocations.
//!
//! Prevents agents from firing tools too quickly. Uses a token-bucket algorithm
//! with per-tool and global rate limits.
//!
//! ## GAP 5 Fix
//!
//! The agent eval loop has `max_tool_rounds` per turn but no rate limiter on
//! how fast tools fire. An LLM in a tight loop could fire 25 shell commands
//! in under a second. This module adds:
//!
//! - Global ceiling: max N tool calls per second
//! - Per-tool cooldown: minimum interval between same-tool invocations
//! - Burst allowance: token bucket permits short bursts

use std::time::{Duration, Instant};
use rustc_hash::FxHashMap;

/// Configuration for the tool rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum tool calls per second (global ceiling).
    pub max_calls_per_second: f64,
    /// Minimum interval between same-tool calls.
    pub per_tool_cooldown: Duration,
    /// Burst allowance (initial tokens in bucket).
    pub burst_size: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_calls_per_second: 10.0,
            per_tool_cooldown: Duration::from_millis(100),
            burst_size: 5,
        }
    }
}

/// Token-bucket rate limiter for tool invocations.
pub struct ToolRateLimiter {
    config: RateLimitConfig,
    /// Global token bucket state.
    tokens: f64,
    last_refill: Instant,
    /// Per-tool last-invocation timestamp.
    per_tool_last: FxHashMap<String, Instant>,
}

impl ToolRateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            tokens: config.burst_size as f64,
            last_refill: Instant::now(),
            per_tool_last: FxHashMap::default(),
            config,
        }
    }

    /// Check if a tool invocation is rate-limited.
    ///
    /// Returns `None` if allowed, or `Some(wait_duration)` if rate-limited.
    /// Consumes one token if allowed.
    pub fn check(&mut self, tool_name: &str) -> Option<Duration> {
        let now = Instant::now();

        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(self.last_refill);
        self.tokens += elapsed.as_secs_f64() * self.config.max_calls_per_second;
        self.tokens = self.tokens.min(self.config.burst_size as f64);
        self.last_refill = now;

        // Check global rate
        if self.tokens < 1.0 {
            let wait = Duration::from_secs_f64(1.0 / self.config.max_calls_per_second);
            return Some(wait);
        }

        // Check per-tool cooldown
        if let Some(last) = self.per_tool_last.get(tool_name) {
            let since_last = now.duration_since(*last);
            if since_last < self.config.per_tool_cooldown {
                return Some(self.config.per_tool_cooldown - since_last);
            }
        }

        // Allowed — consume token and record invocation
        self.tokens -= 1.0;
        self.per_tool_last.insert(tool_name.to_string(), now);
        None
    }

    /// Wait if rate-limited, then proceed.
    pub async fn acquire(&mut self, tool_name: &str) {
        if let Some(wait) = self.check(tool_name) {
            tokio::time::sleep(wait).await;
            // After sleeping, the bucket should have refilled
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(self.last_refill);
            self.tokens += elapsed.as_secs_f64() * self.config.max_calls_per_second;
            self.tokens = self.tokens.min(self.config.burst_size as f64);
            self.last_refill = now;
            self.tokens -= 1.0;
            self.per_tool_last.insert(tool_name.to_string(), now);
        }
    }
}

impl Default for ToolRateLimiter {
    fn default() -> Self {
        Self::new(RateLimitConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_burst() {
        let mut limiter = ToolRateLimiter::new(RateLimitConfig {
            max_calls_per_second: 10.0,
            per_tool_cooldown: Duration::ZERO,
            burst_size: 3,
        });

        // Should allow 3 calls (burst)
        assert!(limiter.check("shell_exec").is_none());
        assert!(limiter.check("shell_exec").is_none());
        assert!(limiter.check("shell_exec").is_none());
        // 4th should be rate-limited
        assert!(limiter.check("shell_exec").is_some());
    }

    #[test]
    fn per_tool_cooldown() {
        let mut limiter = ToolRateLimiter::new(RateLimitConfig {
            max_calls_per_second: 100.0, // high global limit
            per_tool_cooldown: Duration::from_millis(500),
            burst_size: 100,
        });

        assert!(limiter.check("shell_exec").is_none()); // first call OK
        assert!(limiter.check("shell_exec").is_some());  // too soon for same tool
        assert!(limiter.check("file_read").is_none());   // different tool OK
    }
}
