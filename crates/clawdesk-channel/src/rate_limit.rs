//! Channel rate limiting — per-channel token-bucket rate limiting.
//!
//! ## Lock-free fast path
//!
//! Each channel bucket packs `(tokens_x1000, timestamp_ms)` into an `AtomicU64`.
//! The `check()` hot path uses CAS without acquiring any locks. The outer
//! `DashMap` (or `HashMap + RwLock`) guards bucket creation only (cold path).

use clawdesk_types::channel::ChannelId;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Per-channel rate limit configuration.
#[derive(Debug, Clone)]
pub struct ChannelRateLimit {
    /// Maximum burst size (messages).
    pub capacity: u32,
    /// Messages refilled per second.
    pub refill_per_sec: f64,
}

impl Default for ChannelRateLimit {
    fn default() -> Self {
        Self {
            capacity: 30,
            refill_per_sec: 5.0,
        }
    }
}

/// Atomic token bucket — packs state into a single u64.
///
/// High 32 bits: tokens × 1000 (fixed-point, 3 decimal places).
/// Low 32 bits: timestamp in ms since the rate limiter's epoch.
struct AtomicBucket {
    state: AtomicU64,
}

impl AtomicBucket {
    fn new(tokens: f64, epoch_ms: u32) -> Self {
        let packed = Self::pack(tokens, epoch_ms);
        Self {
            state: AtomicU64::new(packed),
        }
    }

    fn pack(tokens: f64, ts_ms: u32) -> u64 {
        let tok = (tokens * 1000.0) as u32;
        ((tok as u64) << 32) | (ts_ms as u64)
    }

    fn unpack(val: u64) -> (f64, u32) {
        let tok = (val >> 32) as u32;
        let ts = val as u32;
        (tok as f64 / 1000.0, ts)
    }

    /// Try to consume one token using CAS. Returns true if allowed.
    fn try_consume(&self, capacity: f64, refill_per_sec: f64, now_ms: u32) -> bool {
        loop {
            let current = self.state.load(Ordering::Relaxed);
            let (mut tokens, last_ms) = Self::unpack(current);

            // Refill based on elapsed time.
            let elapsed_ms = now_ms.wrapping_sub(last_ms);
            let elapsed_secs = elapsed_ms as f64 / 1000.0;
            tokens = (tokens + elapsed_secs * refill_per_sec).min(capacity);

            if tokens < 1.0 {
                return false;
            }

            let new_tokens = tokens - 1.0;
            let new_packed = Self::pack(new_tokens, now_ms);

            if self
                .state
                .compare_exchange_weak(current, new_packed, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
            // CAS failed — retry (another thread modified concurrently).
        }
    }
}

/// Rate limiter that enforces per-channel send limits.
///
/// Hot-path `check()` is lock-free (atomic CAS on per-channel bucket).
/// Bucket creation is rare and guarded by a `RwLock`.
pub struct ChannelRateLimiter {
    limits: HashMap<ChannelId, ChannelRateLimit>,
    default_limit: ChannelRateLimit,
    buckets: DashMap<ChannelId, AtomicBucket>,
    epoch: Instant,
}

impl ChannelRateLimiter {
    /// Create a rate limiter with channel-specific overrides.
    pub fn new(
        default_limit: ChannelRateLimit,
        overrides: HashMap<ChannelId, ChannelRateLimit>,
    ) -> Self {
        Self {
            limits: overrides,
            default_limit,
            buckets: DashMap::new(),
            epoch: Instant::now(),
        }
    }

    /// Milliseconds since the rate limiter was created.
    fn now_ms(&self) -> u32 {
        self.epoch.elapsed().as_millis() as u32
    }

    /// Check if a message can be sent to this channel. Returns `true` if allowed.
    /// Lock-free on the hot path (atomic CAS on the bucket).
    pub async fn check(&self, channel_id: ChannelId) -> bool {
        let limit = self.limits.get(&channel_id).unwrap_or(&self.default_limit);
        let capacity = limit.capacity as f64;
        let refill = limit.refill_per_sec;
        let now_ms = self.now_ms();

        // Fast path: check existing bucket.
        if let Some(bucket) = self.buckets.get(&channel_id) {
            return bucket.try_consume(capacity, refill, now_ms);
        }

        // Slow path: create new bucket.
        let entry = self.buckets.entry(channel_id).or_insert_with(|| {
            AtomicBucket::new(capacity, now_ms)
        });
        entry.try_consume(capacity, refill, now_ms)
    }

    /// Wait until a send is allowed, then consume a token.
    pub async fn wait_and_send(&self, channel_id: ChannelId) {
        loop {
            if self.check(channel_id).await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

impl Default for ChannelRateLimiter {
    fn default() -> Self {
        Self::new(ChannelRateLimit::default(), HashMap::new())
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allows_within_capacity() {
        let limiter = ChannelRateLimiter::new(
            ChannelRateLimit {
                capacity: 2,
                refill_per_sec: 0.0,
            },
            HashMap::new(),
        );
        assert!(limiter.check(ChannelId::Telegram).await);
        assert!(limiter.check(ChannelId::Telegram).await);
        assert!(!limiter.check(ChannelId::Telegram).await); // exhausted
    }

    #[tokio::test]
    async fn separate_channels_have_separate_buckets() {
        let limiter = ChannelRateLimiter::new(
            ChannelRateLimit {
                capacity: 1,
                refill_per_sec: 0.0,
            },
            HashMap::new(),
        );
        assert!(limiter.check(ChannelId::Telegram).await);
        assert!(limiter.check(ChannelId::Discord).await);
        assert!(!limiter.check(ChannelId::Telegram).await);
    }

    #[tokio::test]
    async fn per_channel_overrides() {
        let mut overrides = HashMap::new();
        overrides.insert(
            ChannelId::WhatsApp,
            ChannelRateLimit {
                capacity: 5,
                refill_per_sec: 0.0,
            },
        );
        let limiter = ChannelRateLimiter::new(
            ChannelRateLimit {
                capacity: 1,
                refill_per_sec: 0.0,
            },
            overrides,
        );
        // WhatsApp gets 5 tokens
        for _ in 0..5 {
            assert!(limiter.check(ChannelId::WhatsApp).await);
        }
        assert!(!limiter.check(ChannelId::WhatsApp).await);

        // Others get 1 token
        assert!(limiter.check(ChannelId::Telegram).await);
        assert!(!limiter.check(ChannelId::Telegram).await);
    }
}
