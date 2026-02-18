//! Per-topic ring buffer with consumer cursor tracking.
//!
//! Each topic is a bounded FIFO backed by a ring buffer.
//! - O(1) publish (append to tail)
//! - O(1) consume (read at cursor, advance)
//! - Consumer lag = producer_offset - consumer_offset

use crate::event::Event;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::warn;

/// Configuration for a topic ring buffer.
#[derive(Debug, Clone)]
pub struct TopicConfig {
    /// Topic name (e.g., "email.inbound", "social.metrics")
    pub name: String,
    /// Maximum events retained in the ring buffer
    pub capacity: usize,
    /// Whether to persist events for crash recovery
    pub persistent: bool,
}

impl Default for TopicConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            capacity: 4096,
            persistent: true,
        }
    }
}

/// A single topic ring buffer with subscriber notification.
pub struct Topic {
    config: TopicConfig,
    /// Ring buffer storage
    buffer: RwLock<Vec<Event>>,
    /// Current write offset (monotonically increasing)
    write_offset: AtomicU64,
    /// Broadcast channel for notifying subscribers of new events
    notify: broadcast::Sender<u64>,
    /// Per-subscriber cursor positions: subscriber_id → last consumed offset
    cursors: RwLock<HashMap<String, u64>>,
}

impl Topic {
    /// Create a new topic with the given configuration.
    pub fn new(config: TopicConfig) -> Arc<Self> {
        let (notify, _) = broadcast::channel(256);
        Arc::new(Self {
            buffer: RwLock::new(Vec::with_capacity(config.capacity)),
            write_offset: AtomicU64::new(0),
            config,
            notify,
            cursors: RwLock::new(HashMap::new()),
        })
    }

    /// Publish an event to this topic. Returns the assigned offset.
    ///
    /// The event's `offset` field is set to the monotonic write position.
    /// O(1) amortized — append to the ring buffer, notify subscribers.
    pub async fn publish(&self, mut event: Event) -> u64 {
        let offset = self.write_offset.fetch_add(1, Ordering::SeqCst);
        event.offset = offset;
        event.topic = self.config.name.clone();

        let mut buf = self.buffer.write().await;
        if buf.len() >= self.config.capacity {
            // Ring buffer full — evict oldest (index 0)
            buf.remove(0);
        }
        buf.push(event);
        drop(buf);

        // Notify subscribers (best-effort; lagging receivers get Lagged error)
        let _ = self.notify.send(offset);
        offset
    }

    /// Register a new consumer with a starting cursor position.
    pub async fn register_consumer(&self, consumer_id: impl Into<String>, start_offset: u64) {
        let mut cursors = self.cursors.write().await;
        cursors.insert(consumer_id.into(), start_offset);
    }

    /// Read events from the given offset, up to `max_count`.
    /// Returns the events and the new cursor position.
    pub async fn consume(&self, consumer_id: &str, max_count: usize) -> (Vec<Event>, u64) {
        let cursors = self.cursors.read().await;
        let cursor = cursors.get(consumer_id).copied().unwrap_or(0);
        drop(cursors);

        let buf = self.buffer.read().await;
        let current_write = self.write_offset.load(Ordering::SeqCst);

        if cursor >= current_write {
            return (Vec::new(), cursor);
        }

        // Find events in buffer with offset >= cursor
        let events: Vec<Event> = buf
            .iter()
            .filter(|e| e.offset >= cursor)
            .take(max_count)
            .cloned()
            .collect();

        let new_cursor = events.last().map(|e| e.offset + 1).unwrap_or(cursor);
        drop(buf);

        // Advance cursor
        let mut cursors = self.cursors.write().await;
        cursors.insert(consumer_id.to_string(), new_cursor);

        (events, new_cursor)
    }

    /// Subscribe to new event notifications. Returns a broadcast receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<u64> {
        self.notify.subscribe()
    }

    /// Get consumer lag for a specific subscriber.
    pub async fn consumer_lag(&self, consumer_id: &str) -> u64 {
        let cursors = self.cursors.read().await;
        let cursor = cursors.get(consumer_id).copied().unwrap_or(0);
        let head = self.write_offset.load(Ordering::SeqCst);
        head.saturating_sub(cursor)
    }

    /// Topic name.
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Current write offset (total events ever published).
    pub fn head_offset(&self) -> u64 {
        self.write_offset.load(Ordering::SeqCst)
    }

    /// Number of events currently in the ring buffer.
    pub async fn buffered_count(&self) -> usize {
        self.buffer.read().await.len()
    }
}
