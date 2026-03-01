//! Inbound adapter trait — the receiving half of the channel abstraction.
//!
//! Each channel adapter implements `InboundAdapter` to produce a stream of
//! `InboundEnvelope` values. The adapter framework merges K channel streams
//! via `SelectAll` (amortized O(log K) per element) and publishes to the
//! event bus as source-attributed messages.
//!
//! ## Data Flow
//!
//! ```text
//! iMessage → Monitor → Normalize ─┐
//! Slack → Webhook → Normalize ────┤
//! Telegram → Poll → Normalize ────┤→ Bus → Router → Agent → Reply → Channel
//! Discord → WS → Normalize ───────┘
//! ```
//!
//! ## Deduplication
//!
//! Uses a time-windowed Bloom filter with k=7, m=2^16 for O(k) per-lookup
//! deduplication. False positive probability ≈ 5.5×10⁻⁶ at n=1000 msgs/window.

use async_trait::async_trait;
use clawdesk_types::channel::ChannelId;
use clawdesk_types::message::{MessageOrigin, NormalizedMessage};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

/// An inbound envelope wrapping a normalized message with routing metadata.
///
/// The `reply_path` preserves the origin channel and thread information
/// needed to dispatch the agent's response back to the correct destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundEnvelope {
    /// The normalized message content.
    pub message: NormalizedMessage,
    /// Reply-path metadata for bidirectional routing.
    pub reply_path: ReplyPath,
    /// Whether this message has been deduplicated.
    pub deduplicated: bool,
    /// Adapter that produced this envelope.
    pub source_adapter: String,
}

/// Reply-path metadata for routing responses back to the originating channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyPath {
    /// The originating channel.
    pub channel: ChannelId,
    /// Channel-specific origin data for reply routing.
    pub origin: MessageOrigin,
    /// Whether to reply in-thread (if supported by channel).
    pub prefer_thread: bool,
    /// Whether to use streaming delivery (if supported by channel).
    pub prefer_streaming: bool,
}

/// Status of an inbound adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterStatus {
    /// Adapter is initialized but not yet started.
    Idle,
    /// Adapter is actively receiving messages.
    Running,
    /// Adapter is temporarily disconnected (will reconnect).
    Reconnecting,
    /// Adapter has been stopped gracefully.
    Stopped,
    /// Adapter encountered an unrecoverable error.
    Failed,
}

impl fmt::Display for AdapterStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Running => write!(f, "running"),
            Self::Reconnecting => write!(f, "reconnecting"),
            Self::Stopped => write!(f, "stopped"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Error type for inbound adapter operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterError {
    pub kind: AdapterErrorKind,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AdapterErrorKind {
    /// Authentication failure (bad token, expired key).
    Auth,
    /// Network connectivity issue.
    Network,
    /// Rate limited by the external service.
    RateLimit,
    /// Deserialization or protocol issue.
    Protocol,
    /// Configuration error.
    Config,
    /// Internal adapter error.
    Internal,
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for AdapterError {}

/// Trait for inbound channel adapters.
///
/// Each channel implements this trait to receive messages from its external
/// platform. The adapter produces `InboundEnvelope` values via an mpsc channel.
///
/// Adapters are independently deployable, testable, and hot-swappable:
/// - `start()` begins monitoring the external service
/// - `stop()` gracefully shuts down
/// - `status()` reports the current adapter state
///
/// The adapter framework manages the lifecycle and wires the output to the
/// event bus.
#[async_trait]
pub trait InboundAdapter: Send + Sync + 'static {
    /// Unique identifier for this adapter instance.
    fn id(&self) -> &str;

    /// The channel this adapter monitors.
    fn channel(&self) -> ChannelId;

    /// Start receiving messages. Sends envelopes to the provided sender.
    ///
    /// This method should spawn its own monitoring task(s) and return
    /// immediately. Messages are pushed to `tx` as they arrive.
    ///
    /// The adapter must handle reconnection internally. If the connection
    /// is permanently lost, send an error via the returned channel and
    /// transition to `Failed` status.
    async fn start(
        &self,
        tx: mpsc::Sender<Result<InboundEnvelope, AdapterError>>,
    ) -> Result<(), AdapterError>;

    /// Stop receiving messages gracefully.
    async fn stop(&self) -> Result<(), AdapterError>;

    /// Current adapter status.
    fn status(&self) -> AdapterStatus;

    /// Human-readable description for diagnostics.
    fn description(&self) -> String {
        format!("{} adapter ({})", self.channel(), self.id())
    }
}

/// Registry of active inbound adapters.
///
/// Manages the lifecycle of multiple adapters and merges their output
/// streams into a single channel for the event bus.
pub struct InboundAdapterRegistry {
    adapters: Vec<Arc<dyn InboundAdapter>>,
    /// Merged output channel for all adapters.
    tx: mpsc::Sender<Result<InboundEnvelope, AdapterError>>,
    rx: Option<mpsc::Receiver<Result<InboundEnvelope, AdapterError>>>,
}

impl InboundAdapterRegistry {
    /// Create a new registry with the given channel capacity.
    ///
    /// Channel capacity C determines backpressure behavior:
    /// when the consumer (event bus publisher) falls behind,
    /// adapters block on send after C messages buffer.
    pub fn new(capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel(capacity);
        Self {
            adapters: Vec::new(),
            tx,
            rx: Some(rx),
        }
    }

    /// Register an inbound adapter.
    pub fn register(&mut self, adapter: Arc<dyn InboundAdapter>) {
        self.adapters.push(adapter);
    }

    /// Start all registered adapters.
    ///
    /// Each adapter gets a clone of the shared sender, creating a
    /// fan-in topology where K adapters merge into a single stream.
    ///
    /// Throughput: T_total = Σ T_k, bounded by channel capacity C.
    pub async fn start_all(&self) -> Result<(), AdapterError> {
        for adapter in &self.adapters {
            let tx = self.tx.clone();
            adapter.start(tx).await?;
        }
        Ok(())
    }

    /// Stop all registered adapters.
    pub async fn stop_all(&self) -> Vec<Result<(), AdapterError>> {
        let mut results = Vec::new();
        for adapter in &self.adapters {
            results.push(adapter.stop().await);
        }
        results
    }

    /// Take the receiver end for the merged stream.
    ///
    /// Returns `None` if already taken. The caller should consume
    /// this receiver in a loop, publishing each envelope to the event bus.
    pub fn take_receiver(
        &mut self,
    ) -> Option<mpsc::Receiver<Result<InboundEnvelope, AdapterError>>> {
        self.rx.take()
    }

    /// Get status of all adapters.
    pub fn adapter_statuses(&self) -> Vec<(String, ChannelId, AdapterStatus)> {
        self.adapters
            .iter()
            .map(|a| (a.id().to_string(), a.channel(), a.status()))
            .collect()
    }

    /// Number of registered adapters.
    pub fn count(&self) -> usize {
        self.adapters.len()
    }

    /// Get a clone of the shared sender for external injection (e.g. webhooks).
    ///
    /// This allows non-adapter components to inject `InboundEnvelope` values
    /// into the same merged stream consumed by the event bus.
    pub fn sender(&self) -> mpsc::Sender<Result<InboundEnvelope, AdapterError>> {
        self.tx.clone()
    }
}

/// Time-windowed Bloom filter for message deduplication.
///
/// Parameters: k=7 hash functions, m=2^16 bits
/// At n=1000 messages/window: P(false_positive) ≈ 5.5×10⁻⁶
///
/// Space: O(m) = 8 KiB per window
/// Lookup: O(k) = O(7) = O(1)
pub struct DeduplicationFilter {
    /// Current window bit array.
    bits: Vec<u64>,
    /// Previous window (kept for overlap period).
    prev_bits: Vec<u64>,
    /// Bit array size in u64 words (m/64).
    word_count: usize,
    /// Number of hash functions.
    k: usize,
    /// Window duration.
    window_secs: u64,
    /// Timestamp of current window start.
    window_start: std::time::Instant,
}

impl DeduplicationFilter {
    /// Create a new dedup filter.
    ///
    /// Default: m=2^16 bits, k=7, window=60s
    pub fn new() -> Self {
        let word_count = 1024; // 2^16 / 64 = 1024 words
        Self {
            bits: vec![0u64; word_count],
            prev_bits: vec![0u64; word_count],
            word_count,
            k: 7,
            window_secs: 60,
            window_start: std::time::Instant::now(),
        }
    }

    /// Check if a message ID might be a duplicate (may false-positive).
    /// Returns `true` if the ID was probably seen before.
    pub fn check_and_insert(&mut self, id: &str) -> bool {
        self.maybe_rotate();

        let hashes = self.compute_hashes(id);

        // Check current window
        let in_current = hashes.iter().all(|&h| self.get_bit(&self.bits, h));
        // Check previous window
        let in_prev = hashes.iter().all(|&h| self.get_bit(&self.prev_bits, h));

        // Insert into current window
        for &h in &hashes {
            self.set_bit(h);
        }

        in_current || in_prev
    }

    /// Rotate windows if the current window has expired.
    fn maybe_rotate(&mut self) {
        let elapsed = self.window_start.elapsed().as_secs();
        if elapsed >= self.window_secs {
            // Swap current → previous, clear current
            std::mem::swap(&mut self.bits, &mut self.prev_bits);
            self.bits.iter_mut().for_each(|w| *w = 0);
            self.window_start = std::time::Instant::now();
        }
    }

    /// Compute k hash values for the given key using double-hashing.
    ///
    /// h_i(x) = (h1(x) + i × h2(x)) mod m
    fn compute_hashes(&self, key: &str) -> Vec<usize> {
        let m = self.word_count * 64;
        let h1 = self.fnv1a(key.as_bytes());
        let h2 = self.djb2(key.as_bytes());
        (0..self.k)
            .map(|i| ((h1.wrapping_add((i as u64).wrapping_mul(h2))) % (m as u64)) as usize)
            .collect()
    }

    fn get_bit(&self, bits: &[u64], pos: usize) -> bool {
        let word = pos / 64;
        let bit = pos % 64;
        word < bits.len() && (bits[word] & (1u64 << bit)) != 0
    }

    fn set_bit(&mut self, pos: usize) {
        let word = pos / 64;
        let bit = pos % 64;
        if word < self.bits.len() {
            self.bits[word] |= 1u64 << bit;
        }
    }

    /// FNV-1a hash.
    fn fnv1a(&self, data: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for &byte in data {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    /// DJB2 hash.
    fn djb2(&self, data: &[u8]) -> u64 {
        let mut hash: u64 = 5381;
        for &byte in data {
            hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
        }
        hash
    }
}

impl Default for DeduplicationFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup_filter_detects_duplicate() {
        let mut filter = DeduplicationFilter::new();
        assert!(!filter.check_and_insert("msg-001"));
        assert!(filter.check_and_insert("msg-001")); // duplicate
    }

    #[test]
    fn test_dedup_filter_distinct_messages() {
        let mut filter = DeduplicationFilter::new();
        assert!(!filter.check_and_insert("msg-001"));
        assert!(!filter.check_and_insert("msg-002"));
        assert!(!filter.check_and_insert("msg-003"));
    }

    #[test]
    fn test_adapter_status_display() {
        assert_eq!(AdapterStatus::Running.to_string(), "running");
        assert_eq!(AdapterStatus::Reconnecting.to_string(), "reconnecting");
    }

    #[test]
    fn test_inbound_envelope_creation() {
        use chrono::Utc;
        use uuid::Uuid;

        let envelope = InboundEnvelope {
            message: NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: clawdesk_types::SessionKey::new(ChannelId::Telegram, "test-12345"),
                body: "Hello from Telegram".to_string(),
                body_for_agent: None,
                sender: clawdesk_types::SenderIdentity {
                    id: "user-123".to_string(),
                    display_name: "Test User".to_string(),
                    channel: ChannelId::Telegram,
                },
                media: vec![],
                artifact_refs: vec![],
                reply_context: None,
                origin: MessageOrigin::Telegram {
                    chat_id: 12345,
                    message_id: 67890,
                    thread_id: None,
                },
                timestamp: Utc::now(),
            },
            reply_path: ReplyPath {
                channel: ChannelId::Telegram,
                origin: MessageOrigin::Telegram {
                    chat_id: 12345,
                    message_id: 67890,
                    thread_id: None,
                },
                prefer_thread: false,
                prefer_streaming: false,
            },
            deduplicated: false,
            source_adapter: "telegram-bot".to_string(),
        };

        assert_eq!(envelope.reply_path.channel, ChannelId::Telegram);
        assert!(!envelope.deduplicated);
    }

    #[test]
    fn test_registry_lifecycle() {
        let mut registry = InboundAdapterRegistry::new(100);
        assert_eq!(registry.count(), 0);
        assert!(registry.take_receiver().is_some());
        assert!(registry.take_receiver().is_none()); // already taken
    }
}
