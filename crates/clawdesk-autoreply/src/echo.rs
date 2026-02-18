//! Echo suppression and self-message deduplication.
//!
//! Prevents the bot from responding to its own messages or creating
//! echo loops in group chats. Uses a sliding-window bloom filter for
//! efficient O(k) per-check deduplication.
//!
//! ## Problem
//!
//! When a bot sends a message to a channel, the channel's event stream
//! often echoes that message back. Without suppression, the bot sees its
//! own message as new input and generates another response — creating an
//! infinite loop.
//!
//! ## Approach
//!
//! 1. **Self-message detection**: Compare sender ID against known bot IDs.
//! 2. **Content dedup**: Hash (channel, content) and check against a
//!    sliding-window bloom filter. Catches near-duplicates even when
//!    sender ID detection fails.
//! 3. **Cooldown**: Per-channel rate limiting prevents rapid-fire responses.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use clawdesk_types::channel::ChannelId;

/// Configuration for echo suppression.
#[derive(Debug, Clone)]
pub struct EchoSuppressionConfig {
    /// Known bot user IDs across channels.
    pub bot_ids: HashSet<String>,
    /// Window size for content deduplication (number of messages to remember).
    pub dedup_window: usize,
    /// Minimum cooldown between responses on the same channel+thread.
    pub cooldown: Duration,
    /// Number of hash functions for the bloom filter.
    pub bloom_k: usize,
    /// Bloom filter bit-array size.
    pub bloom_m: usize,
}

impl Default for EchoSuppressionConfig {
    fn default() -> Self {
        Self {
            bot_ids: HashSet::new(),
            dedup_window: 1000,
            cooldown: Duration::from_millis(500),
            bloom_k: 7,
            // ~12KB for <1% false positive at 10K messages
            bloom_m: 96_000,
        }
    }
}

/// Reason a message was suppressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuppressionReason {
    /// Message was sent by the bot itself.
    SelfMessage,
    /// Message content was recently seen (likely echo).
    DuplicateContent,
    /// Channel is in cooldown after a recent response.
    Cooldown,
}

/// Simple bloom filter for content deduplication.
#[derive(Debug)]
struct BloomFilter {
    bits: Vec<bool>,
    k: usize,
    m: usize,
}

impl BloomFilter {
    fn new(m: usize, k: usize) -> Self {
        Self {
            bits: vec![false; m],
            k,
            m,
        }
    }

    fn hash_indexes(&self, data: &[u8]) -> Vec<usize> {
        let mut indexes = Vec::with_capacity(self.k);
        for i in 0..self.k {
            let mut hasher = DefaultHasher::new();
            i.hash(&mut hasher);
            data.hash(&mut hasher);
            let h = hasher.finish() as usize;
            indexes.push(h % self.m);
        }
        indexes
    }

    fn insert(&mut self, data: &[u8]) {
        for idx in self.hash_indexes(data) {
            self.bits[idx] = true;
        }
    }

    fn might_contain(&self, data: &[u8]) -> bool {
        self.hash_indexes(data).iter().all(|&idx| self.bits[idx])
    }

    fn clear(&mut self) {
        self.bits.fill(false);
    }
}

/// Echo suppression engine.
pub struct EchoSuppressor {
    config: EchoSuppressionConfig,
    /// Bloom filter for fast content deduplication.
    bloom: BloomFilter,
    /// Sliding window of recent message hashes (for resetting bloom periodically).
    recent_hashes: VecDeque<u64>,
    /// Last response time per (channel, thread/conversation) key.
    last_response: HashMap<String, Instant>,
    /// Count of messages inserted since last bloom reset.
    insert_count: usize,
}

impl EchoSuppressor {
    /// Create a new suppressor with the given config.
    pub fn new(config: EchoSuppressionConfig) -> Self {
        let bloom = BloomFilter::new(config.bloom_m, config.bloom_k);
        Self {
            config,
            bloom,
            recent_hashes: VecDeque::new(),
            last_response: HashMap::new(),
            insert_count: 0,
        }
    }

    /// Check if a message should be suppressed.
    ///
    /// Returns `None` if the message should be processed, or
    /// `Some(reason)` if it should be dropped.
    pub fn check(
        &mut self,
        sender_id: &str,
        channel: ChannelId,
        thread_key: Option<&str>,
        content: &str,
    ) -> Option<SuppressionReason> {
        // 1. Self-message detection
        if self.config.bot_ids.contains(sender_id) {
            return Some(SuppressionReason::SelfMessage);
        }

        // 2. Content deduplication via bloom filter
        let content_key = format!("{}:{}:{}", channel, thread_key.unwrap_or(""), content);
        let content_bytes = content_key.as_bytes();

        if self.bloom.might_contain(content_bytes) {
            return Some(SuppressionReason::DuplicateContent);
        }

        // 3. Cooldown check
        let cooldown_key = format!("{}:{}", channel, thread_key.unwrap_or(""));
        if let Some(last) = self.last_response.get(&cooldown_key) {
            if last.elapsed() < self.config.cooldown {
                return Some(SuppressionReason::Cooldown);
            }
        }

        // Message is allowed — record it
        self.record_message(content_bytes);

        None
    }

    /// Record that the bot sent a response (for cooldown tracking).
    pub fn record_response(&mut self, channel: ChannelId, thread_key: Option<&str>) {
        let key = format!("{}:{}", channel, thread_key.unwrap_or(""));
        self.last_response.insert(key, Instant::now());
    }

    /// Record a message in the bloom filter and sliding window.
    fn record_message(&mut self, content_bytes: &[u8]) {
        self.bloom.insert(content_bytes);
        self.insert_count += 1;

        // Hash for the sliding window
        let mut hasher = DefaultHasher::new();
        content_bytes.hash(&mut hasher);
        self.recent_hashes.push_back(hasher.finish());

        // Evict old entries and reset bloom if window overflows
        if self.insert_count > self.config.dedup_window {
            // Reset bloom and re-insert recent window
            self.bloom.clear();
            self.insert_count = 0;

            // Keep only the last dedup_window/2 entries
            let keep = self.config.dedup_window / 2;
            while self.recent_hashes.len() > keep {
                self.recent_hashes.pop_front();
            }

            // Note: We clear bloom completely here for simplicity.
            // Re-inserting would require storing original content.
            // The brief window of no dedup is acceptable.
        }
    }

    /// Add a bot user ID to the known set.
    pub fn add_bot_id(&mut self, id: impl Into<String>) {
        self.config.bot_ids.insert(id.into());
    }

    /// Clear all cooldowns (useful for testing or reset).
    pub fn clear_cooldowns(&mut self) {
        self.last_response.clear();
    }

    /// Number of messages tracked.
    pub fn tracked_count(&self) -> usize {
        self.insert_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> EchoSuppressionConfig {
        EchoSuppressionConfig {
            bot_ids: ["bot-123".to_string()].into_iter().collect(),
            dedup_window: 100,
            cooldown: Duration::from_millis(100),
            bloom_k: 3,
            bloom_m: 1000,
        }
    }

    #[test]
    fn test_self_message_suppressed() {
        let mut s = EchoSuppressor::new(test_config());
        let result = s.check("bot-123", ChannelId::Discord, None, "hello");
        assert_eq!(result, Some(SuppressionReason::SelfMessage));
    }

    #[test]
    fn test_normal_message_allowed() {
        let mut s = EchoSuppressor::new(test_config());
        let result = s.check("user-456", ChannelId::Discord, None, "hello");
        assert_eq!(result, None);
    }

    #[test]
    fn test_duplicate_content_suppressed() {
        let mut s = EchoSuppressor::new(test_config());

        // First occurrence — allowed
        let r1 = s.check("user-456", ChannelId::Slack, Some("thread-1"), "same message");
        assert_eq!(r1, None);

        // Second occurrence — suppressed
        let r2 = s.check("user-789", ChannelId::Slack, Some("thread-1"), "same message");
        assert_eq!(r2, Some(SuppressionReason::DuplicateContent));
    }

    #[test]
    fn test_different_content_allowed() {
        let mut s = EchoSuppressor::new(test_config());
        let r1 = s.check("user-456", ChannelId::Slack, None, "message one");
        assert_eq!(r1, None);
        let r2 = s.check("user-456", ChannelId::Slack, None, "message two");
        assert_eq!(r2, None);
    }

    #[test]
    fn test_cooldown_suppression() {
        let mut s = EchoSuppressor::new(test_config());

        // Record a response
        s.record_response(ChannelId::Telegram, Some("thread-1"));

        // Check immediately — should be suppressed
        let r = s.check("user-456", ChannelId::Telegram, Some("thread-1"), "new msg");
        assert_eq!(r, Some(SuppressionReason::Cooldown));
    }

    #[test]
    fn test_cooldown_expires() {
        let config = EchoSuppressionConfig {
            cooldown: Duration::from_millis(1), // Very short for testing
            ..test_config()
        };
        let mut s = EchoSuppressor::new(config);

        s.record_response(ChannelId::Telegram, None);
        std::thread::sleep(Duration::from_millis(5));

        let r = s.check("user-456", ChannelId::Telegram, None, "new msg after cooldown");
        assert_eq!(r, None);
    }

    #[test]
    fn test_add_bot_id() {
        let mut s = EchoSuppressor::new(test_config());
        let r1 = s.check("new-bot", ChannelId::Discord, None, "hello");
        assert_eq!(r1, None); // Not yet known as bot

        s.add_bot_id("new-bot");
        let r2 = s.check("new-bot", ChannelId::Discord, None, "hello2");
        assert_eq!(r2, Some(SuppressionReason::SelfMessage));
    }

    #[test]
    fn test_bloom_filter_basic() {
        let mut bf = BloomFilter::new(1000, 3);
        let data = b"test data";
        assert!(!bf.might_contain(data));
        bf.insert(data);
        assert!(bf.might_contain(data));
        assert!(!bf.might_contain(b"other data"));
    }

    #[test]
    fn test_bloom_filter_clear() {
        let mut bf = BloomFilter::new(1000, 3);
        bf.insert(b"test");
        assert!(bf.might_contain(b"test"));
        bf.clear();
        assert!(!bf.might_contain(b"test"));
    }

    #[test]
    fn test_different_channels_not_deduped() {
        let mut s = EchoSuppressor::new(test_config());
        let r1 = s.check("user", ChannelId::Slack, None, "same msg");
        assert_eq!(r1, None);
        // Same content but different channel — allowed
        let r2 = s.check("user", ChannelId::Discord, None, "same msg");
        assert_eq!(r2, None);
    }
}
