//! Per-topic ring buffer with consumer cursor tracking.
//!
//! Each topic is a bounded FIFO backed by a VecDeque ring buffer.
//! - O(1) publish (push_back + pop_front at capacity)
//! - O(k) consume (read k events at cursor, advance)
//! - Consumer lag = producer_offset - consumer_offset

use crate::event::Event;
use dashmap::DashMap;
use std::collections::VecDeque;
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
    /// Ring buffer storage — VecDeque provides O(1) push_back + pop_front.
    buffer: RwLock<VecDeque<Event>>,
    /// Current write offset (monotonically increasing)
    write_offset: AtomicU64,
    /// Broadcast channel for notifying subscribers of new events
    notify: broadcast::Sender<u64>,
    /// Per-subscriber cursor positions: subscriber_id → last consumed offset
    cursors: DashMap<String, u64>,
    /// Offset of the oldest event currently in the buffer.
    /// Used for O(1) index computation in consume().
    base_offset: AtomicU64,
}

impl Topic {
    /// Create a new topic with the given configuration.
    pub fn new(config: TopicConfig) -> Arc<Self> {
        let (notify, _) = broadcast::channel(256);
        Arc::new(Self {
            buffer: RwLock::new(VecDeque::with_capacity(config.capacity)),
            write_offset: AtomicU64::new(0),
            base_offset: AtomicU64::new(0),
            config,
            notify,
            cursors: DashMap::new(),
        })
    }

    /// Publish an event to this topic. Returns the assigned offset.
    ///
    /// O(1) amortized — VecDeque::push_back + pop_front at capacity.
    /// Uses Release ordering on the write offset store to synchronize
    /// with Acquire-loading consumers (happens-before, no MFENCE needed).
    pub async fn publish(&self, mut event: Event) -> u64 {
        let offset = self.write_offset.fetch_add(1, Ordering::Release);
        event.offset = offset;
        event.topic = self.config.name.clone();

        let mut buf = self.buffer.write().await;
        if buf.len() >= self.config.capacity {
            // O(1) eviction — VecDeque::pop_front vs Vec::remove(0) which was O(N).
            buf.pop_front();
            self.base_offset.store(offset.saturating_sub(self.config.capacity as u64 - 1), Ordering::Release);
        }
        buf.push_back(event);
        drop(buf);

        // Notify subscribers (best-effort; lagging receivers get Lagged error)
        let _ = self.notify.send(offset);
        offset
    }

    /// Register a new consumer with a starting cursor position.
    pub async fn register_consumer(&self, consumer_id: impl Into<String>, start_offset: u64) {
        self.cursors.insert(consumer_id.into(), start_offset);
    }

    /// Read events from the given offset, up to `max_count`.
    /// Returns the events and the new cursor position.
    ///
    /// O(k) where k = events returned. Uses base_offset for direct index
    /// computation instead of O(N) linear filter.
    pub async fn consume(&self, consumer_id: &str, max_count: usize) -> (Vec<Event>, u64) {
        let cursor = self.cursors.get(consumer_id).map(|v| *v).unwrap_or(0);

        let buf = self.buffer.read().await;
        let current_write = self.write_offset.load(Ordering::Acquire);

        if cursor >= current_write || buf.is_empty() {
            return (Vec::new(), cursor);
        }

        // Direct index computation: the oldest event's offset is base_offset.
        let base = self.base_offset.load(Ordering::Acquire);
        let effective_cursor = cursor.max(base);
        let start_idx = (effective_cursor - base) as usize;

        let events: Vec<Event> = buf
            .iter()
            .skip(start_idx)
            .take(max_count)
            .cloned()
            .collect();

        let new_cursor = events.last().map(|e| e.offset + 1).unwrap_or(cursor);
        drop(buf);

        // Advance cursor
        self.cursors.insert(consumer_id.to_string(), new_cursor);

        (events, new_cursor)
    }

    /// Subscribe to new event notifications. Returns a broadcast receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<u64> {
        self.notify.subscribe()
    }

    /// Get consumer lag for a specific subscriber.
    pub async fn consumer_lag(&self, consumer_id: &str) -> u64 {
        let cursor = self.cursors.get(consumer_id).map(|v| *v).unwrap_or(0);
        let head = self.write_offset.load(Ordering::SeqCst);
        head.saturating_sub(cursor)
    }

    /// Topic name.
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Current write offset (total events ever published).
    pub fn head_offset(&self) -> u64 {
        self.write_offset.load(Ordering::Acquire)
    }

    /// Number of events currently in the ring buffer.
    pub async fn buffered_count(&self) -> usize {
        self.buffer.read().await.len()
    }
}
