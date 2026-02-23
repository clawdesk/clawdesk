//! Inbound message debounce with sliding window renewal.
//!
//! Prevents rapid-fire user edits from spawning parallel LLM calls.
//! Uses a sliding window: each new message in the window *renews* the timer,
//! and only the last message in each burst triggers processing.
//!
//! ## Algorithm
//!
//! - Messages arrive with a key (e.g., `chat_id` or `user_id + channel`).
//! - Each key has a pending slot. New messages *replace* the pending content
//!   and reset the timer. When the timer fires without renewal, the pending
//!   message is emitted for processing.
//! - Timer: `tokio::time::sleep` in a `tokio::select!` loop per key.
//!
//! ## Complexity
//! - Insertion: O(1) amortised (HashMap lookup + instant comparison)
//! - Memory: O(K) where K = number of active debounce keys

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Mutex};

/// Configuration for the debounce window.
#[derive(Debug, Clone)]
pub struct DebounceConfig {
    /// How long to wait after the last message before emitting.
    pub window: Duration,
    /// Maximum time a burst can be extended before forcing emission.
    pub max_burst: Duration,
}

impl Default for DebounceConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_millis(800),
            max_burst: Duration::from_secs(5),
        }
    }
}

/// A debounced message ready for processing.
#[derive(Debug, Clone)]
pub struct DebouncedMessage {
    /// The debounce key (e.g., chat_id).
    pub key: String,
    /// The final message content after debounce.
    pub content: String,
    /// Number of messages that were coalesced in this burst.
    pub coalesced_count: usize,
    /// Time from first message in burst to emission.
    pub burst_duration: Duration,
}

/// Internal state for a pending debounce slot.
struct PendingSlot {
    content: String,
    count: usize,
    first_seen: Instant,
    last_seen: Instant,
}

/// Inbound message debouncer.
///
/// Feed messages via `submit()` and receive debounced results via
/// the `rx` channel returned from `start()`.
pub struct MessageDebouncer {
    config: DebounceConfig,
    pending: Arc<Mutex<HashMap<String, PendingSlot>>>,
}

impl MessageDebouncer {
    pub fn new(config: DebounceConfig) -> Self {
        Self {
            config,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Submit a message for debouncing.
    ///
    /// Returns `true` if this is a *new* burst (first message for this key),
    /// `false` if it renewed an existing burst.
    pub async fn submit(&self, key: String, content: String) -> bool {
        let mut pending = self.pending.lock().await;
        let now = Instant::now();

        if let Some(slot) = pending.get_mut(&key) {
            slot.content = content;
            slot.count += 1;
            slot.last_seen = now;
            false
        } else {
            pending.insert(
                key,
                PendingSlot {
                    content,
                    count: 1,
                    first_seen: now,
                    last_seen: now,
                },
            );
            true
        }
    }

    /// Try to harvest a debounced message for a given key.
    ///
    /// Returns `Some(DebouncedMessage)` if the debounce window has expired
    /// or the max burst duration has been exceeded.
    pub async fn try_harvest(&self, key: &str) -> Option<DebouncedMessage> {
        let mut pending = self.pending.lock().await;
        let now = Instant::now();

        let should_emit = pending.get(key).map_or(false, |slot| {
            let window_expired = now.duration_since(slot.last_seen) >= self.config.window;
            let burst_exceeded = now.duration_since(slot.first_seen) >= self.config.max_burst;
            window_expired || burst_exceeded
        });

        if should_emit {
            let slot = pending.remove(key)?;
            Some(DebouncedMessage {
                key: key.to_string(),
                content: slot.content,
                coalesced_count: slot.count,
                burst_duration: now.duration_since(slot.first_seen),
            })
        } else {
            None
        }
    }

    /// Check if a key has a pending message.
    pub async fn has_pending(&self, key: &str) -> bool {
        self.pending.lock().await.contains_key(key)
    }

    /// Get the number of active debounce keys.
    pub async fn active_keys(&self) -> usize {
        self.pending.lock().await.len()
    }

    /// Clear all pending messages without emitting.
    pub async fn clear(&self) {
        self.pending.lock().await.clear();
    }

    /// Remove a specific key without emitting.
    pub async fn cancel(&self, key: &str) -> bool {
        self.pending.lock().await.remove(key).is_some()
    }
}

/// Run a debounce loop for a specific key.
///
/// This spawns a task that polls the debouncer and sends the result
/// when the window expires. Useful for integrating with channel adapters.
pub async fn debounce_key(
    debouncer: Arc<MessageDebouncer>,
    key: String,
    tx: mpsc::Sender<DebouncedMessage>,
) {
    let poll_interval = debouncer.config.window / 4;
    let max_wait = debouncer.config.max_burst + Duration::from_millis(100);
    let start = Instant::now();

    loop {
        tokio::time::sleep(poll_interval).await;

        if let Some(msg) = debouncer.try_harvest(&key).await {
            let _ = tx.send(msg).await;
            return;
        }

        // Safety: don't loop forever
        if start.elapsed() > max_wait {
            // Force harvest
            if let Some(msg) = debouncer.try_harvest(&key).await {
                let _ = tx.send(msg).await;
            }
            return;
        }

        // If key was cancelled externally
        if !debouncer.has_pending(&key).await {
            return;
        }
    }
}

/// Convenience: per-channel debounce window defaults.
pub fn debounce_config_for_channel(channel: &str) -> DebounceConfig {
    match channel.to_lowercase().as_str() {
        "telegram" => DebounceConfig {
            window: Duration::from_millis(1000),
            max_burst: Duration::from_secs(5),
        },
        "discord" => DebounceConfig {
            window: Duration::from_millis(800),
            max_burst: Duration::from_secs(5),
        },
        "slack" => DebounceConfig {
            window: Duration::from_millis(600),
            max_burst: Duration::from_secs(4),
        },
        "irc" => DebounceConfig {
            window: Duration::from_millis(500),
            max_burst: Duration::from_secs(3),
        },
        _ => DebounceConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_single_message_no_coalesce() {
        let debouncer = MessageDebouncer::new(DebounceConfig {
            window: Duration::from_millis(50),
            max_burst: Duration::from_secs(1),
        });

        let is_new = debouncer.submit("chat-1".into(), "hello".into()).await;
        assert!(is_new);

        // Wait for window to expire
        tokio::time::sleep(Duration::from_millis(60)).await;

        let msg = debouncer.try_harvest("chat-1").await;
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.coalesced_count, 1);
    }

    #[tokio::test]
    async fn test_burst_coalesces() {
        let debouncer = MessageDebouncer::new(DebounceConfig {
            window: Duration::from_millis(100),
            max_burst: Duration::from_secs(2),
        });

        debouncer.submit("chat-1".into(), "hello".into()).await;
        tokio::time::sleep(Duration::from_millis(30)).await;

        let is_new = debouncer.submit("chat-1".into(), "hello world".into()).await;
        assert!(!is_new); // renewal, not new

        tokio::time::sleep(Duration::from_millis(30)).await;
        debouncer
            .submit("chat-1".into(), "hello world!".into())
            .await;

        // Window hasn't expired yet from last message
        tokio::time::sleep(Duration::from_millis(110)).await;

        let msg = debouncer.try_harvest("chat-1").await.unwrap();
        assert_eq!(msg.content, "hello world!"); // last message wins
        assert_eq!(msg.coalesced_count, 3);
    }

    #[tokio::test]
    async fn test_different_keys_independent() {
        let debouncer = MessageDebouncer::new(DebounceConfig {
            window: Duration::from_millis(50),
            max_burst: Duration::from_secs(1),
        });

        debouncer.submit("chat-1".into(), "msg A".into()).await;
        debouncer.submit("chat-2".into(), "msg B".into()).await;

        tokio::time::sleep(Duration::from_millis(60)).await;

        let a = debouncer.try_harvest("chat-1").await.unwrap();
        let b = debouncer.try_harvest("chat-2").await.unwrap();
        assert_eq!(a.content, "msg A");
        assert_eq!(b.content, "msg B");
    }

    #[tokio::test]
    async fn test_cancel_prevents_emission() {
        let debouncer = MessageDebouncer::new(DebounceConfig {
            window: Duration::from_millis(50),
            max_burst: Duration::from_secs(1),
        });

        debouncer.submit("chat-1".into(), "hello".into()).await;
        let cancelled = debouncer.cancel("chat-1").await;
        assert!(cancelled);

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(debouncer.try_harvest("chat-1").await.is_none());
    }

    #[test]
    fn test_channel_configs() {
        let tg = debounce_config_for_channel("telegram");
        assert_eq!(tg.window, Duration::from_millis(1000));

        let dc = debounce_config_for_channel("discord");
        assert_eq!(dc.window, Duration::from_millis(800));
    }
}
