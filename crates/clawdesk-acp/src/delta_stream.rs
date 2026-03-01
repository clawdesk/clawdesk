//! Streaming delta protocol — incremental text updates replacing full snapshots.
//!
//! ## Streaming Delta (P3)
//!
//! The existing `BackpressureStream` (streaming.rs) sends `TextDelta` events
//! with raw delta strings. But there's no protocol for:
//! - **Offset tracking**: knowing _where_ in the response each delta applies.
//! - **Reassembly**: reconstructing the full response from a delta stream.
//! - **Resumption**: reconnecting mid-stream and catching up from an offset.
//! - **Verification**: confirming the client's assembled text matches the server's.
//!
//! This module adds `DeltaStream` — a protocol layer on top of `BackpressureStream`
//! that tracks byte offsets, supports checkpoint-based resumption, and uses
//! a polynomial rolling hash for O(1)-per-delta integrity verification.
//!
//! ## Rolling Hash
//!
//! Previous implementation used FNV-1a, which rehashed the entire assembled
//! string on every delta — O(N²) over the stream. Now uses a composable
//! polynomial hash mod Mersenne prime (2⁶¹ − 1):
//!
//! H(S ∥ C) = H(S) · p^|C| + H(C) mod M
//!
//! Each push only processes the incoming bytes: O(|chunk|) per delta.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tracing::debug;

/// A single delta in the incremental stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDelta {
    /// Sequence number (monotonically increasing per stream).
    pub seq: u64,
    /// Byte offset in the assembled text where this delta starts.
    pub offset: usize,
    /// The delta text to insert at `offset`.
    pub text: String,
    /// Whether this is the final delta (stream complete).
    pub done: bool,
    /// FNV-1a hash of the full assembled text after applying this delta.
    /// Clients can verify their local assembly matches.
    pub hash: u64,
}

/// Checkpoint for stream resumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamCheckpoint {
    /// Last sequence number successfully processed.
    pub last_seq: u64,
    /// Byte offset of the assembled text at this checkpoint.
    pub offset: usize,
    /// Hash of the assembled text at this checkpoint.
    pub hash: u64,
}

/// Server-side delta stream encoder.
///
/// Accumulates the response text and emits deltas with offset tracking.
/// Uses a polynomial rolling hash for O(|chunk|) integrity updates.
pub struct DeltaEncoder {
    /// Task/response identifier.
    task_id: String,
    /// Full assembled text so far.
    assembled: String,
    /// Current sequence number.
    seq: u64,
    /// Checkpoint interval (emit checkpoint every N deltas).
    checkpoint_interval: u64,
    /// Recent checkpoints for resumption support.
    checkpoints: VecDeque<StreamCheckpoint>,
    /// Maximum checkpoints to retain.
    max_checkpoints: usize,
    /// Rolling hash state — updated incrementally on each push.
    rolling_hash: RollingHash,
}

impl DeltaEncoder {
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            assembled: String::new(),
            seq: 0,
            checkpoint_interval: 10,
            checkpoints: VecDeque::new(),
            max_checkpoints: 50,
            rolling_hash: RollingHash::new(),
        }
    }

    pub fn with_checkpoint_interval(mut self, interval: u64) -> Self {
        self.checkpoint_interval = interval;
        self
    }

    /// Append a text chunk and produce a delta.
    ///
    /// Hash update is O(|text|) via rolling polynomial hash,
    /// not O(|assembled|) as with a full rehash.
    pub fn push(&mut self, text: &str) -> TextDelta {
        let offset = self.assembled.len();
        self.assembled.push_str(text);
        self.rolling_hash.append(text.as_bytes());
        let hash = self.rolling_hash.value();
        let seq = self.seq;
        self.seq += 1;

        // Create checkpoint if needed
        if seq > 0 && seq % self.checkpoint_interval == 0 {
            let cp = StreamCheckpoint {
                last_seq: seq,
                offset: self.assembled.len(),
                hash,
            };
            self.checkpoints.push_back(cp);
            if self.checkpoints.len() > self.max_checkpoints {
                self.checkpoints.pop_front();
            }
        }

        debug!(
            task_id = %self.task_id,
            seq,
            offset,
            delta_len = text.len(),
            total_len = self.assembled.len(),
            "delta emitted"
        );

        TextDelta {
            seq,
            offset,
            text: text.to_string(),
            done: false,
            hash,
        }
    }

    /// Emit the final (done) delta.
    pub fn finish(&mut self) -> TextDelta {
        let hash = self.rolling_hash.value();
        let seq = self.seq;
        self.seq += 1;

        TextDelta {
            seq,
            offset: self.assembled.len(),
            text: String::new(),
            done: true,
            hash,
        }
    }

    /// Get the full assembled text.
    pub fn assembled(&self) -> &str {
        &self.assembled
    }

    /// Current sequence number (next delta will have this seq).
    pub fn current_seq(&self) -> u64 {
        self.seq
    }

    /// Get the nearest checkpoint at or before the given sequence.
    /// Returns `None` if no checkpoints exist before `seq`.
    pub fn checkpoint_at_or_before(&self, seq: u64) -> Option<&StreamCheckpoint> {
        self.checkpoints
            .iter()
            .rev()
            .find(|cp| cp.last_seq <= seq)
    }

    /// Get all available checkpoints.
    pub fn checkpoints(&self) -> &VecDeque<StreamCheckpoint> {
        &self.checkpoints
    }
}

/// Client-side delta stream decoder.
///
/// Applies incoming deltas to reconstruct the full response text.
/// Uses rolling hash for O(|chunk|) verification on appends.
pub struct DeltaDecoder {
    /// Assembled text buffer.
    assembled: String,
    /// Last applied sequence number.
    last_seq: Option<u64>,
    /// Number of deltas applied.
    deltas_applied: u64,
    /// Whether the stream is complete.
    done: bool,
    /// Hash mismatches detected.
    hash_mismatches: u64,
    /// Rolling hash state (invalidated on non-append operations).
    rolling_hash: RollingHash,
    /// Whether the rolling hash is in sync with `assembled`.
    /// Set to `false` on insert/replace operations; rehashed lazily.
    hash_valid: bool,
}

impl DeltaDecoder {
    pub fn new() -> Self {
        Self {
            assembled: String::new(),
            last_seq: None,
            deltas_applied: 0,
            done: false,
            hash_mismatches: 0,
            rolling_hash: RollingHash::new(),
            hash_valid: true,
        }
    }

    /// Resume from a checkpoint (for reconnection).
    ///
    /// Computes rolling hash from the provided text (one-time O(|text|) cost).
    pub fn from_checkpoint(checkpoint: &StreamCheckpoint, assembled_so_far: String) -> Self {
        let rolling_hash = RollingHash::from_data(assembled_so_far.as_bytes());
        Self {
            assembled: assembled_so_far,
            last_seq: Some(checkpoint.last_seq),
            deltas_applied: 0,
            done: false,
            hash_mismatches: 0,
            rolling_hash,
            hash_valid: true,
        }
    }

    /// Apply a delta to the assembled text.
    ///
    /// Returns `true` if the hash matches (integrity verified),
    /// `false` if there's a mismatch (possible missed delta).
    pub fn apply(&mut self, delta: &TextDelta) -> bool {
        // Check sequence ordering
        if let Some(last) = self.last_seq {
            if delta.seq <= last {
                // Duplicate or out-of-order — skip
                return true;
            }
        }

        if delta.done {
            self.done = true;
            self.last_seq = Some(delta.seq);
            // Verify final hash — ensure rolling hash is current
            let local_hash = self.current_hash();
            if local_hash != delta.hash {
                self.hash_mismatches += 1;
                return false;
            }
            return true;
        }

        // Apply delta at offset
        if delta.offset == self.assembled.len() {
            // Append (common case) — O(|text|) rolling hash update
            self.assembled.push_str(&delta.text);
            if self.hash_valid {
                self.rolling_hash.append(delta.text.as_bytes());
            }
        } else if delta.offset < self.assembled.len() {
            // Insert/replace at offset — recompute rolling hash
            self.assembled
                .replace_range(delta.offset..delta.offset, &delta.text);
            self.rolling_hash = RollingHash::from_data(self.assembled.as_bytes());
        } else {
            // Gap — pad with spaces (shouldn't happen in normal operation)
            let padding = delta.offset - self.assembled.len();
            self.assembled.extend(std::iter::repeat(' ').take(padding));
            self.assembled.push_str(&delta.text);
            self.rolling_hash = RollingHash::from_data(self.assembled.as_bytes());
        }

        self.last_seq = Some(delta.seq);
        self.deltas_applied += 1;

        // Verify hash — O(|chunk|) in common append case
        let local_hash = self.current_hash();
        if local_hash != delta.hash {
            self.hash_mismatches += 1;
            return false;
        }

        true
    }

    /// Get the assembled text.
    pub fn assembled(&self) -> &str {
        &self.assembled
    }

    /// Whether the stream is complete.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Last applied sequence number.
    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    /// Number of hash mismatches detected.
    pub fn hash_mismatches(&self) -> u64 {
        self.hash_mismatches
    }

    /// Number of deltas applied.
    pub fn deltas_applied(&self) -> u64 {
        self.deltas_applied
    }

    /// Create a checkpoint of the current decoder state.
    pub fn checkpoint(&self) -> Option<StreamCheckpoint> {
        self.last_seq.map(|seq| StreamCheckpoint {
            last_seq: seq,
            offset: self.assembled.len(),
            hash: self.current_hash(),
        })
    }

    /// Get the current hash, rehashing from scratch if invalidated.
    fn current_hash(&self) -> u64 {
        if self.hash_valid {
            self.rolling_hash.value()
        } else {
            RollingHash::compute(self.assembled.as_bytes())
        }
    }
}

impl Default for DeltaDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Polynomial Rolling Hash — O(|chunk|) composable integrity verification
// ═══════════════════════════════════════════════════════════════════════════

/// Mersenne prime M = 2⁶¹ − 1, used as the hash modulus.
///
/// Chosen because:
/// - Large enough for negligible collision probability (~2⁻⁶¹ per pair)
/// - Allows efficient modular reduction via bit tricks
/// - Well-studied in Rabin-Karp fingerprinting literature
const MERSENNE_61: u64 = (1u64 << 61) - 1;

/// Hash base (a prime > 256 to avoid trivial collisions with byte values).
const HASH_BASE: u64 = 131;

/// Composable polynomial rolling hash.
///
/// H(s₀s₁…sₖ₋₁) = Σ sᵢ · p^(k−1−i)  mod M
///
/// Appending chunk C of length m:
///   H(S ∥ C) = H(S) · p^m + H(C)  mod M
///
/// Each append only processes the incoming bytes, keeping the operation
/// strictly O(|chunk|) and entirely within L1 cache.
#[derive(Debug, Clone)]
struct RollingHash {
    hash: u64,
}

impl RollingHash {
    fn new() -> Self {
        Self { hash: 0 }
    }

    /// Initialize from existing data (one-time O(|data|) cost).
    fn from_data(data: &[u8]) -> Self {
        let mut h = Self::new();
        h.append(data);
        h
    }

    /// Append bytes and update the hash in O(|data|).
    ///
    /// H(S ∥ C) = H(S) · p^|C| + H(C)  mod M
    fn append(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let p_pow = mod_pow(HASH_BASE, data.len() as u64);
        let chunk_hash = hash_bytes(data);
        self.hash = mod_add(mod_mul(self.hash, p_pow), chunk_hash);
    }

    /// Get the current hash value.
    fn value(&self) -> u64 {
        self.hash
    }

    /// Compute hash from scratch (for non-append operations / verification).
    fn compute(data: &[u8]) -> u64 {
        hash_bytes(data)
    }
}

/// Hash a byte slice: H = Σ data[i] · p^(len−1−i) mod M
#[inline]
fn hash_bytes(data: &[u8]) -> u64 {
    let mut h: u64 = 0;
    for &byte in data {
        h = mod_add(mod_mul(h, HASH_BASE), byte as u64);
    }
    h
}

/// Modular multiplication: (a × b) mod M, using u128 to avoid overflow.
#[inline]
fn mod_mul(a: u64, b: u64) -> u64 {
    ((a as u128 * b as u128) % MERSENNE_61 as u128) as u64
}

/// Modular addition: (a + b) mod M.
#[inline]
fn mod_add(a: u64, b: u64) -> u64 {
    let sum = a as u128 + b as u128;
    (sum % MERSENNE_61 as u128) as u64
}

/// Modular exponentiation: base^exp mod M, via binary exponentiation.
#[inline]
fn mod_pow(mut base: u64, mut exp: u64) -> u64 {
    if exp == 0 {
        return 1;
    }
    base %= MERSENNE_61;
    let mut result: u64 = 1;
    while exp > 0 {
        if exp & 1 == 1 {
            result = mod_mul(result, base);
        }
        exp >>= 1;
        if exp > 0 {
            base = mod_mul(base, base);
        }
    }
    result
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_decoder_roundtrip() {
        let mut encoder = DeltaEncoder::new("task-1");
        let mut decoder = DeltaDecoder::new();

        let chunks = ["Hello, ", "world! ", "How are ", "you?"];
        for chunk in &chunks {
            let delta = encoder.push(chunk);
            assert!(decoder.apply(&delta));
        }

        let final_delta = encoder.finish();
        assert!(decoder.apply(&final_delta));
        assert!(decoder.is_done());
        assert_eq!(decoder.assembled(), "Hello, world! How are you?");
        assert_eq!(encoder.assembled(), decoder.assembled());
    }

    #[test]
    fn offset_tracking() {
        let mut encoder = DeltaEncoder::new("task-1");

        let d1 = encoder.push("Hello");
        assert_eq!(d1.offset, 0);
        assert_eq!(d1.seq, 0);

        let d2 = encoder.push(", world");
        assert_eq!(d2.offset, 5);
        assert_eq!(d2.seq, 1);

        let d3 = encoder.push("!");
        assert_eq!(d3.offset, 12);
        assert_eq!(d3.seq, 2);
    }

    #[test]
    fn hash_verification() {
        let mut encoder = DeltaEncoder::new("task-1");
        let mut decoder = DeltaDecoder::new();

        let delta = encoder.push("test");
        assert!(decoder.apply(&delta));
        assert_eq!(decoder.hash_mismatches(), 0);
    }

    #[test]
    fn duplicate_delta_ignored() {
        let mut encoder = DeltaEncoder::new("task-1");
        let mut decoder = DeltaDecoder::new();

        let delta = encoder.push("hello");
        assert!(decoder.apply(&delta));
        assert!(decoder.apply(&delta)); // duplicate — should be ignored
        assert_eq!(decoder.assembled(), "hello");
        assert_eq!(decoder.deltas_applied(), 1);
    }

    #[test]
    fn checkpoint_resumption() {
        let mut encoder = DeltaEncoder::new("task-1")
            .with_checkpoint_interval(2);
        let mut decoder = DeltaDecoder::new();

        // Apply first 4 deltas
        for i in 0..4 {
            let delta = encoder.push(&format!("chunk{} ", i));
            decoder.apply(&delta);
        }

        // Simulate disconnect: save checkpoint from decoder
        let cp = decoder.checkpoint().unwrap();
        let saved_text = decoder.assembled().to_string();

        // New decoder from checkpoint
        let mut decoder2 = DeltaDecoder::from_checkpoint(&cp, saved_text);

        // Continue from where we left off
        for i in 4..6 {
            let delta = encoder.push(&format!("chunk{} ", i));
            decoder2.apply(&delta);
        }

        let final_delta = encoder.finish();
        decoder2.apply(&final_delta);

        assert_eq!(encoder.assembled(), decoder2.assembled());
    }

    #[test]
    fn encoder_checkpoints() {
        let mut encoder = DeltaEncoder::new("task-1")
            .with_checkpoint_interval(3);

        for i in 0..10 {
            encoder.push(&format!("{}", i));
        }

        // Should have checkpoints at seq 3, 6, 9
        assert_eq!(encoder.checkpoints().len(), 3);

        let cp = encoder.checkpoint_at_or_before(7).unwrap();
        assert_eq!(cp.last_seq, 6);
    }

    #[test]
    fn rolling_hash_consistency() {
        let h1 = RollingHash::compute(b"hello");
        let h2 = RollingHash::compute(b"hello");
        let h3 = RollingHash::compute(b"world");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn rolling_hash_composability() {
        // H("hello world") computed at once must equal
        // H("hello ") then append("world")
        let full = RollingHash::compute(b"hello world");

        let mut incremental = RollingHash::new();
        incremental.append(b"hello ");
        incremental.append(b"world");
        assert_eq!(full, incremental.value());

        // Also test 3-way split
        let mut three_way = RollingHash::new();
        three_way.append(b"hel");
        three_way.append(b"lo ");
        three_way.append(b"world");
        assert_eq!(full, three_way.value());
    }

    #[test]
    fn delta_serialization() {
        let delta = TextDelta {
            seq: 42,
            offset: 100,
            text: "test delta".into(),
            done: false,
            hash: 12345,
        };
        let json = serde_json::to_string(&delta).unwrap();
        let restored: TextDelta = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.seq, 42);
        assert_eq!(restored.text, "test delta");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// DeltaPublisher — producer-side bridge between DeltaEncoder and BackpressureStream
// ═══════════════════════════════════════════════════════════════════════════

use crate::streaming::{BackpressureStream, StreamPayload, StreamError};

/// Producer-side bridge: encodes text chunks into rich deltas and publishes
/// them through a `BackpressureStream` with overflow protection.
///
/// ## Usage
///
/// ```text
/// let stream = BackpressureStream::new(config);
/// let mut publisher = DeltaPublisher::new("task-42", &stream);
///
/// // As LLM yields chunks:
/// publisher.push("Hello ").await?;
/// publisher.push("world").await?;
/// publisher.finish().await?;
/// ```
///
/// Each `push()` call:
/// 1. Encodes via `DeltaEncoder::push()` → `TextDelta` with seq, offset, hash
/// 2. Wraps in `StreamPayload::RichTextDelta`
/// 3. Publishes to `BackpressureStream` with backpressure handling
pub struct DeltaPublisher<'a> {
    encoder: DeltaEncoder,
    stream: &'a BackpressureStream,
    task_id: String,
}

impl<'a> DeltaPublisher<'a> {
    /// Create a new delta publisher for the given task.
    pub fn new(task_id: impl Into<String>, stream: &'a BackpressureStream) -> Self {
        let task_id = task_id.into();
        Self {
            encoder: DeltaEncoder::new(task_id.clone()),
            stream,
            task_id,
        }
    }

    /// Create with a custom checkpoint interval.
    pub fn with_checkpoint_interval(mut self, interval: u64) -> Self {
        self.encoder = self.encoder.with_checkpoint_interval(interval);
        self
    }

    /// Push a text chunk through the delta encoder and into the stream.
    ///
    /// Returns the sequence number of the published delta.
    pub async fn push(&mut self, text: &str) -> Result<u64, StreamError> {
        let delta = self.encoder.push(text);
        let seq = delta.seq;
        self.stream
            .publish(self.task_id.clone(), StreamPayload::RichTextDelta(delta))
            .await?;
        Ok(seq)
    }

    /// Finish the stream — publish the final done delta.
    pub async fn finish(&mut self) -> Result<u64, StreamError> {
        let delta = self.encoder.finish();
        let seq = delta.seq;
        self.stream
            .publish(self.task_id.clone(), StreamPayload::RichTextDelta(delta))
            .await?;
        Ok(seq)
    }

    /// Get the full assembled text so far.
    pub fn assembled(&self) -> &str {
        self.encoder.assembled()
    }

    /// Get the nearest checkpoint at or before a given sequence number.
    /// Used for reconnection: client sends its `last_seq`, server finds
    /// the checkpoint and replays from there.
    pub fn checkpoint_at_or_before(&self, seq: u64) -> Option<&StreamCheckpoint> {
        self.encoder.checkpoint_at_or_before(seq)
    }

    /// Current sequence number (next to be assigned).
    pub fn current_seq(&self) -> u64 {
        self.encoder.current_seq()
    }
}

/// Consumer-side bridge: applies `RichTextDelta` events from a `BackpressureStream`
/// through a `DeltaDecoder` with integrity verification.
///
/// ## Usage
///
/// ```text
/// let mut consumer = DeltaConsumer::new();
///
/// while let Some(event) = stream.consume().await {
///     if let Some(text_update) = consumer.apply_event(&event.payload) {
///         // text_update is the new chunk for the UI
///     }
/// }
///
/// println!("Final text: {}", consumer.assembled());
/// ```
pub struct DeltaConsumer {
    decoder: DeltaDecoder,
}

impl DeltaConsumer {
    /// Create a new consumer with a fresh decoder.
    pub fn new() -> Self {
        Self {
            decoder: DeltaDecoder::new(),
        }
    }

    /// Create a consumer that resumes from a checkpoint.
    pub fn from_checkpoint(checkpoint: StreamCheckpoint, assembled_text: &str) -> Self {
        Self {
            decoder: DeltaDecoder::from_checkpoint(&checkpoint, assembled_text.to_string()),
        }
    }

    /// Apply a stream event. Returns `Some(delta_text)` if a `RichTextDelta`
    /// was successfully applied, `None` for other payload types or integrity failures.
    pub fn apply_event(&mut self, payload: &StreamPayload) -> Option<String> {
        match payload {
            StreamPayload::RichTextDelta(delta) => {
                if self.decoder.apply(delta) {
                    Some(delta.text.clone())
                } else {
                    debug!(
                        seq = delta.seq,
                        hash_mismatches = self.decoder.hash_mismatches(),
                        "delta hash mismatch — integrity violation"
                    );
                    None
                }
            }
            _ => None,
        }
    }

    /// Full assembled text so far.
    pub fn assembled(&self) -> &str {
        self.decoder.assembled()
    }

    /// Whether the stream is complete (final done delta received).
    pub fn is_done(&self) -> bool {
        self.decoder.is_done()
    }

    /// Last applied sequence number.
    pub fn last_seq(&self) -> Option<u64> {
        self.decoder.last_seq()
    }

    /// Number of hash mismatches encountered.
    pub fn hash_mismatches(&self) -> u64 {
        self.decoder.hash_mismatches()
    }

    /// Create a checkpoint of the current consumer state for reconnection.
    pub fn checkpoint(&self) -> Option<StreamCheckpoint> {
        self.decoder.checkpoint()
    }
}

impl Default for DeltaConsumer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod bridge_tests {
    use super::*;
    use crate::streaming::{BackpressureStream, StreamConfig, StreamPayload};

    #[tokio::test]
    async fn delta_publisher_consumer_roundtrip() {
        let stream = BackpressureStream::new(StreamConfig::default());
        let mut publisher = DeltaPublisher::new("task-1", &stream);

        publisher.push("Hello ").await.unwrap();
        publisher.push("world").await.unwrap();
        publisher.finish().await.unwrap();

        assert_eq!(publisher.assembled(), "Hello world");

        let mut consumer = DeltaConsumer::new();
        let mut chunks = Vec::new();

        // Consume exactly 3 events (two pushes + finish).
        // BackpressureStream::consume() blocks when empty (no close signal),
        // so we use consume_timeout to avoid hanging.
        for _ in 0..3 {
            let event = stream
                .consume_timeout(std::time::Duration::from_secs(2))
                .await
                .expect("expected event within timeout");
            if let Some(text) = consumer.apply_event(&event.payload) {
                chunks.push(text);
            }
        }

        assert_eq!(chunks, vec!["Hello ", "world", ""]);
        assert_eq!(consumer.assembled(), "Hello world");
        assert!(consumer.is_done());
        assert_eq!(consumer.hash_mismatches(), 0);
    }

    #[tokio::test]
    async fn delta_publisher_checkpoint_resumption() {
        let stream = BackpressureStream::new(StreamConfig::default());
        let mut publisher = DeltaPublisher::new("task-2", &stream)
            .with_checkpoint_interval(2);

        publisher.push("aaa").await.unwrap();
        publisher.push("bbb").await.unwrap();
        publisher.push("ccc").await.unwrap();

        // Verify checkpoints are created.
        let cp = publisher.checkpoint_at_or_before(2);
        assert!(cp.is_some());

        // Simulate consumer resuming from checkpoint.
        let cp = cp.unwrap().clone();
        let assembled_so_far = &publisher.assembled()[..cp.offset];
        let mut consumer = DeltaConsumer::from_checkpoint(cp, assembled_so_far);

        // Consumer should be able to continue from where it left off.
        assert_eq!(consumer.last_seq(), Some(2));
    }
}
