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
//! FNV-1a hashes for integrity verification.

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
        }
    }

    pub fn with_checkpoint_interval(mut self, interval: u64) -> Self {
        self.checkpoint_interval = interval;
        self
    }

    /// Append a text chunk and produce a delta.
    pub fn push(&mut self, text: &str) -> TextDelta {
        let offset = self.assembled.len();
        self.assembled.push_str(text);
        let hash = fnv1a_hash(self.assembled.as_bytes());
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
        let hash = fnv1a_hash(self.assembled.as_bytes());
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
}

impl DeltaDecoder {
    pub fn new() -> Self {
        Self {
            assembled: String::new(),
            last_seq: None,
            deltas_applied: 0,
            done: false,
            hash_mismatches: 0,
        }
    }

    /// Resume from a checkpoint (for reconnection).
    pub fn from_checkpoint(checkpoint: &StreamCheckpoint, assembled_so_far: String) -> Self {
        Self {
            assembled: assembled_so_far,
            last_seq: Some(checkpoint.last_seq),
            deltas_applied: 0,
            done: false,
            hash_mismatches: 0,
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
            // Verify final hash
            let local_hash = fnv1a_hash(self.assembled.as_bytes());
            if local_hash != delta.hash {
                self.hash_mismatches += 1;
                return false;
            }
            return true;
        }

        // Apply delta at offset
        if delta.offset == self.assembled.len() {
            // Append (common case)
            self.assembled.push_str(&delta.text);
        } else if delta.offset < self.assembled.len() {
            // Insert/replace at offset
            self.assembled
                .replace_range(delta.offset..delta.offset, &delta.text);
        } else {
            // Gap — pad with spaces (shouldn't happen in normal operation)
            let padding = delta.offset - self.assembled.len();
            self.assembled.extend(std::iter::repeat(' ').take(padding));
            self.assembled.push_str(&delta.text);
        }

        self.last_seq = Some(delta.seq);
        self.deltas_applied += 1;

        // Verify hash
        let local_hash = fnv1a_hash(self.assembled.as_bytes());
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
            hash: fnv1a_hash(self.assembled.as_bytes()),
        })
    }
}

impl Default for DeltaDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// FNV-1a 64-bit hash — fast non-cryptographic hash for integrity checking.
///
/// Chosen over CRC32 for better distribution, and over SHA-256 for speed.
/// Collision probability ≈ 2⁻⁶⁴ per pair — sufficient for delta verification.
fn fnv1a_hash(data: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001B3;

    let mut hash = FNV_OFFSET;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
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
    fn fnv1a_consistency() {
        let h1 = fnv1a_hash(b"hello");
        let h2 = fnv1a_hash(b"hello");
        let h3 = fnv1a_hash(b"world");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
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
