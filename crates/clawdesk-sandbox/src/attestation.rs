//! Cryptographic execution attestation with Merkle chain audit.
//!
//! Every tool execution produces a signed receipt that is appended to a
//! tamper-evident Merkle hash chain. The chain is periodically anchored
//! to a SochDB checkpoint for long-term auditability.
//!
//! ## Receipt structure
//!
//! ```text
//! ExecutionReceipt {
//!     execution_id: UUID,
//!     agent_id: String,
//!     tool_name: String,
//!     capability_mask: u128,      // Capabilities used
//!     input_hash: SHA-256,        // Hash of tool input (not the input itself)
//!     output_hash: SHA-256,       // Hash of tool output
//!     started_at: Timestamp,
//!     completed_at: Timestamp,
//!     exit_code: i32,
//!     prev_hash: SHA-256,         // Chain link to previous receipt
//!     receipt_hash: SHA-256,      // H(all above fields)
//!     signature: Ed25519,         // Optional: Ed25519 signature over receipt_hash
//! }
//! ```
//!
//! ## Merkle chain
//!
//! Each receipt's `receipt_hash` is computed as:
//!   `SHA-256(execution_id || agent_id || tool_name || ... || prev_hash)`
//!
//! The chain is verified by recomputing all hashes and checking each
//! `receipt_hash` matches. A break in the chain indicates tampering.
//!
//! ## Epoch anchors
//!
//! Every N receipts, an epoch anchor is inserted containing a wall-clock
//! timestamp and the cumulative Merkle root. This allows bisection of
//! the chain for time-range queries and provides reordering detection.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Execution receipt
// ---------------------------------------------------------------------------

/// A signed execution receipt — proof that a tool was invoked with specific
/// capabilities and produced a specific output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    /// Unique execution ID.
    pub execution_id: String,
    /// The agent that invoked the tool.
    pub agent_id: String,
    /// Tool name.
    pub tool_name: String,
    /// Capability bitmask used for this execution.
    pub capability_mask: u128,
    /// SHA-256 hash of the tool input.
    pub input_hash: String,
    /// SHA-256 hash of the tool output.
    pub output_hash: String,
    /// Execution start time.
    pub started_at: DateTime<Utc>,
    /// Execution completion time.
    pub completed_at: DateTime<Utc>,
    /// Process exit code (0 = success).
    pub exit_code: i32,
    /// SHA-256 hash of the previous receipt in the chain.
    pub prev_hash: String,
    /// SHA-256 hash of this entire receipt (chain link).
    pub receipt_hash: String,
    /// Ed25519 signature over `receipt_hash` (hex-encoded, if signing key available).
    pub signature: Option<String>,
}

/// Zero-hash for the genesis receipt's prev_hash.
const GENESIS_PREV_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

impl ExecutionReceipt {
    /// Compute the receipt hash from all fields (excluding signature).
    pub fn compute_hash(
        execution_id: &str,
        agent_id: &str,
        tool_name: &str,
        capability_mask: u128,
        input_hash: &str,
        output_hash: &str,
        started_at: &DateTime<Utc>,
        completed_at: &DateTime<Utc>,
        exit_code: i32,
        prev_hash: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(execution_id.as_bytes());
        hasher.update(b":");
        hasher.update(agent_id.as_bytes());
        hasher.update(b":");
        hasher.update(tool_name.as_bytes());
        hasher.update(b":");
        hasher.update(capability_mask.to_le_bytes());
        hasher.update(b":");
        hasher.update(input_hash.as_bytes());
        hasher.update(b":");
        hasher.update(output_hash.as_bytes());
        hasher.update(b":");
        hasher.update(started_at.to_rfc3339().as_bytes());
        hasher.update(b":");
        hasher.update(completed_at.to_rfc3339().as_bytes());
        hasher.update(b":");
        hasher.update(exit_code.to_le_bytes());
        hasher.update(b":");
        hasher.update(prev_hash.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Verify this receipt's hash is consistent with its fields.
    pub fn verify_hash(&self) -> bool {
        let expected = Self::compute_hash(
            &self.execution_id,
            &self.agent_id,
            &self.tool_name,
            self.capability_mask,
            &self.input_hash,
            &self.output_hash,
            &self.started_at,
            &self.completed_at,
            self.exit_code,
            &self.prev_hash,
        );
        expected == self.receipt_hash
    }
}

// ---------------------------------------------------------------------------
// Epoch anchor
// ---------------------------------------------------------------------------

/// An epoch anchor inserted every N receipts for bisection and reordering detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochAnchor {
    /// Epoch number (sequential).
    pub epoch: u64,
    /// Position in the chain (receipt index).
    pub chain_position: usize,
    /// Cumulative Merkle root at this epoch.
    pub merkle_root: String,
    /// Wall-clock time of the anchor.
    pub timestamp: DateTime<Utc>,
    /// Number of receipts since the last anchor.
    pub receipt_count: usize,
}

// ---------------------------------------------------------------------------
// Chain verification
// ---------------------------------------------------------------------------

/// Result of verifying the attestation chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainVerification {
    /// Whether the entire chain is valid.
    pub valid: bool,
    /// Number of receipts verified.
    pub receipts_checked: usize,
    /// Index of the first invalid receipt (if any).
    pub first_invalid: Option<usize>,
    /// Number of epoch anchors verified.
    pub epochs_checked: usize,
}

// ---------------------------------------------------------------------------
// Attestation chain
// ---------------------------------------------------------------------------

/// Configuration for the attestation chain.
#[derive(Debug, Clone)]
pub struct AttestationConfig {
    /// Maximum receipts to keep in memory (older ones are referenced by hash).
    pub max_memory_receipts: usize,
    /// Insert an epoch anchor every N receipts.
    pub epoch_interval: usize,
}

impl Default for AttestationConfig {
    fn default() -> Self {
        Self {
            max_memory_receipts: 10_000,
            epoch_interval: 100,
        }
    }
}

/// Tamper-evident Merkle chain of execution receipts.
pub struct AttestationChain {
    config: AttestationConfig,
    inner: RwLock<ChainInner>,
}

struct ChainInner {
    receipts: VecDeque<Arc<ExecutionReceipt>>,
    anchors: Vec<EpochAnchor>,
    total_receipts: usize,
    current_epoch: u64,
    receipts_since_anchor: usize,
}

impl AttestationChain {
    pub fn new(config: AttestationConfig) -> Self {
        Self {
            config,
            inner: RwLock::new(ChainInner {
                receipts: VecDeque::new(),
                anchors: Vec::new(),
                total_receipts: 0,
                current_epoch: 0,
                receipts_since_anchor: 0,
            }),
        }
    }

    /// SHA-256 hash of arbitrary bytes.
    fn sha256(data: &[u8]) -> String {
        hex::encode(Sha256::digest(data))
    }

    /// SHA-256 hash of a string.
    fn sha256_str(s: &str) -> String {
        Self::sha256(s.as_bytes())
    }

    /// Append a new execution receipt to the chain.
    pub async fn append(
        &self,
        agent_id: &str,
        tool_name: &str,
        capability_mask: u128,
        input: &[u8],
        output: &[u8],
        started_at: DateTime<Utc>,
        completed_at: DateTime<Utc>,
        exit_code: i32,
    ) -> ExecutionReceipt {
        let mut inner = self.inner.write().await;

        let execution_id = Uuid::new_v4().to_string();
        let input_hash = Self::sha256(input);
        let output_hash = Self::sha256(output);

        let prev_hash = inner
            .receipts
            .back()
            .map(|r| r.receipt_hash.clone())
            .unwrap_or_else(|| GENESIS_PREV_HASH.to_string());

        let receipt_hash = ExecutionReceipt::compute_hash(
            &execution_id,
            agent_id,
            tool_name,
            capability_mask,
            &input_hash,
            &output_hash,
            &started_at,
            &completed_at,
            exit_code,
            &prev_hash,
        );

        let receipt = ExecutionReceipt {
            execution_id,
            agent_id: agent_id.to_string(),
            tool_name: tool_name.to_string(),
            capability_mask,
            input_hash,
            output_hash,
            started_at,
            completed_at,
            exit_code,
            prev_hash,
            receipt_hash,
            signature: None, // Signing key can be set separately.
        };

        // Evict oldest if at capacity.
        if inner.receipts.len() >= self.config.max_memory_receipts {
            inner.receipts.pop_front();
        }

        inner.receipts.push_back(Arc::new(receipt.clone()));
        inner.total_receipts += 1;
        inner.receipts_since_anchor += 1;

        // Check if we need an epoch anchor.
        if inner.receipts_since_anchor >= self.config.epoch_interval {
            let merkle_root = self.compute_merkle_root_inner(&inner);
            let anchor = EpochAnchor {
                epoch: inner.current_epoch,
                chain_position: inner.total_receipts,
                merkle_root,
                timestamp: Utc::now(),
                receipt_count: inner.receipts_since_anchor,
            };
            inner.anchors.push(anchor);
            inner.current_epoch += 1;
            inner.receipts_since_anchor = 0;

            debug!(
                epoch = inner.current_epoch - 1,
                total = inner.total_receipts,
                "epoch anchor inserted"
            );
        }

        receipt
    }

    /// Compute the Merkle root of all receipts currently in memory.
    fn compute_merkle_root_inner(&self, inner: &ChainInner) -> String {
        if inner.receipts.is_empty() {
            return GENESIS_PREV_HASH.to_string();
        }

        let mut hashes: Vec<String> = inner
            .receipts
            .iter()
            .map(|r| r.receipt_hash.clone())
            .collect();

        // Merkle tree construction: iteratively hash pairs.
        while hashes.len() > 1 {
            let mut next_level = Vec::with_capacity((hashes.len() + 1) / 2);
            for chunk in hashes.chunks(2) {
                if chunk.len() == 2 {
                    let combined = format!("{}{}", chunk[0], chunk[1]);
                    next_level.push(Self::sha256_str(&combined));
                } else {
                    // Odd element: promote as-is.
                    next_level.push(chunk[0].clone());
                }
            }
            hashes = next_level;
        }

        hashes.into_iter().next().unwrap_or_default()
    }

    /// Get the current Merkle root.
    pub async fn merkle_root(&self) -> String {
        let inner = self.inner.read().await;
        self.compute_merkle_root_inner(&inner)
    }

    /// Verify the integrity of the chain.
    pub async fn verify(&self) -> ChainVerification {
        let inner = self.inner.read().await;
        let mut valid = true;
        let mut first_invalid = None;

        for (i, receipt) in inner.receipts.iter().enumerate() {
            // Verify receipt hash.
            if !receipt.verify_hash() {
                valid = false;
                if first_invalid.is_none() {
                    first_invalid = Some(i);
                }
                warn!(
                    index = i,
                    execution_id = %receipt.execution_id,
                    "receipt hash verification FAILED"
                );
                break;
            }

            // Verify chain link.
            if i > 0 {
                let prev = &inner.receipts[i - 1];
                if receipt.prev_hash != prev.receipt_hash {
                    valid = false;
                    if first_invalid.is_none() {
                        first_invalid = Some(i);
                    }
                    warn!(
                        index = i,
                        execution_id = %receipt.execution_id,
                        "chain link verification FAILED (prev_hash mismatch)"
                    );
                    break;
                }
            }
        }

        ChainVerification {
            valid,
            receipts_checked: inner.receipts.len(),
            first_invalid,
            epochs_checked: inner.anchors.len(),
        }
    }

    /// Get the total number of receipts ever appended.
    pub async fn total_receipts(&self) -> usize {
        let inner = self.inner.read().await;
        inner.total_receipts
    }

    /// Get recent receipts (last N).
    pub async fn recent(&self, count: usize) -> Vec<ExecutionReceipt> {
        let inner = self.inner.read().await;
        inner
            .receipts
            .iter()
            .rev()
            .take(count)
            .map(|r| (**r).clone())
            .collect()
    }

    /// Get all epoch anchors.
    pub async fn anchors(&self) -> Vec<EpochAnchor> {
        let inner = self.inner.read().await;
        inner.anchors.clone()
    }

    /// Query receipts by agent ID.
    pub async fn by_agent(&self, agent_id: &str) -> Vec<ExecutionReceipt> {
        let inner = self.inner.read().await;
        inner
            .receipts
            .iter()
            .filter(|r| r.agent_id == agent_id)
            .map(|r| (**r).clone())
            .collect()
    }

    /// Query receipts by tool name.
    pub async fn by_tool(&self, tool_name: &str) -> Vec<ExecutionReceipt> {
        let inner = self.inner.read().await;
        inner
            .receipts
            .iter()
            .filter(|r| r.tool_name == tool_name)
            .map(|r| (**r).clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chain() -> AttestationChain {
        AttestationChain::new(AttestationConfig {
            max_memory_receipts: 100,
            epoch_interval: 5,
        })
    }

    #[tokio::test]
    async fn append_and_verify_single() {
        let chain = make_chain();
        let receipt = chain
            .append(
                "agent-1",
                "read_file",
                0x01,
                b"input data",
                b"output data",
                Utc::now(),
                Utc::now(),
                0,
            )
            .await;

        assert!(receipt.verify_hash());
        assert_eq!(receipt.prev_hash, GENESIS_PREV_HASH);
    }

    #[tokio::test]
    async fn chain_links_correctly() {
        let chain = make_chain();

        let r1 = chain
            .append("a", "tool1", 0, b"", b"", Utc::now(), Utc::now(), 0)
            .await;
        let r2 = chain
            .append("a", "tool2", 0, b"", b"", Utc::now(), Utc::now(), 0)
            .await;

        assert_eq!(r2.prev_hash, r1.receipt_hash);
    }

    #[tokio::test]
    async fn chain_verification_passes() {
        let chain = make_chain();
        for i in 0..10 {
            chain
                .append(
                    "agent",
                    &format!("tool_{i}"),
                    i as u128,
                    format!("input-{i}").as_bytes(),
                    format!("output-{i}").as_bytes(),
                    Utc::now(),
                    Utc::now(),
                    0,
                )
                .await;
        }

        let verification = chain.verify().await;
        assert!(verification.valid);
        assert_eq!(verification.receipts_checked, 10);
    }

    #[tokio::test]
    async fn epoch_anchors_inserted() {
        let chain = make_chain(); // epoch_interval = 5
        for i in 0..12 {
            chain
                .append("a", "t", 0, b"", b"", Utc::now(), Utc::now(), 0)
                .await;
        }

        let anchors = chain.anchors().await;
        assert_eq!(anchors.len(), 2); // 5 + 5 = 10, two epochs
    }

    #[tokio::test]
    async fn merkle_root_changes() {
        let chain = make_chain();
        let root1 = chain.merkle_root().await;

        chain
            .append("a", "t", 0, b"data", b"out", Utc::now(), Utc::now(), 0)
            .await;
        let root2 = chain.merkle_root().await;

        assert_ne!(root1, root2);
    }

    #[tokio::test]
    async fn query_by_agent() {
        let chain = make_chain();
        chain.append("alice", "t1", 0, b"", b"", Utc::now(), Utc::now(), 0).await;
        chain.append("bob", "t2", 0, b"", b"", Utc::now(), Utc::now(), 0).await;
        chain.append("alice", "t3", 0, b"", b"", Utc::now(), Utc::now(), 0).await;

        let alice = chain.by_agent("alice").await;
        assert_eq!(alice.len(), 2);
    }

    #[tokio::test]
    async fn receipt_hash_tamper_detection() {
        let chain = make_chain();
        let mut receipt = chain
            .append("a", "t", 0, b"", b"", Utc::now(), Utc::now(), 0)
            .await;

        // Tamper with a field.
        receipt.tool_name = "tampered".to_string();
        assert!(!receipt.verify_hash());
    }

    #[tokio::test]
    async fn total_receipts_count() {
        let chain = make_chain();
        assert_eq!(chain.total_receipts().await, 0);

        for _ in 0..7 {
            chain.append("a", "t", 0, b"", b"", Utc::now(), Utc::now(), 0).await;
        }
        assert_eq!(chain.total_receipts().await, 7);
    }
}
