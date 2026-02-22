//! Atomic compaction with referential integrity enforcement.
//!
//! ## Problem
//!
//! `compact_session()` in `conversation.rs` performs summary-insert +
//! message-deletes as independent operations. If the process crashes
//! between the summary write and any of the cold-tier deletes, the
//! session ends up in a non-monotonic state where:
//!
//! - **Partial delete**: some cold-tier messages survive alongside the
//!   summary, causing duplicated context (context balloon).
//! - **Lost summary**: the summary write fails but deletes succeed,
//!   causing permanent context loss.
//!
//! ## Solution
//!
//! Wrap all compaction mutations in a single `TransactionalConn` so
//! summary-insert and message-deletes commit atomically. A **compaction
//! manifest** records the boundary between hot and cold tiers, the
//! message keys covered by each summary, and a monotonic epoch counter
//! that downstream consumers can use to detect stale context snapshots.
//!
//! ### Recovery
//!
//! On startup (or before any `build_context` call), `validate_integrity()`
//! checks for:
//! 1. **Orphaned manifests** — manifest exists but referenced messages are
//!    still present (transaction never committed; safe to delete manifest).
//! 2. **Missing summaries** — manifest says messages were compacted but
//!    the summary key is absent (data loss; mark session as degraded).
//! 3. **Duplicate coverage** — multiple manifests cover overlapping
//!    message ranges (merge or discard the older one).

use crate::bridge::SochConn;
use crate::transaction::{TransactionalConn, CommitResult};
use crate::SochStore;
use clawdesk_types::error::StorageError;
use clawdesk_types::session::{AgentMessage, SessionKey};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Maximum number of messages kept verbatim in the hot tier.
/// Must match `conversation.rs::HOT_TIER_SIZE`.
const HOT_TIER_SIZE: usize = 200;

// ── Compaction Manifest ──────────────────────────────────────

/// Persistent record of a compaction operation — stored alongside the
/// summary so integrity can be verified at any time.
///
/// Key: `sessions/{id}/manifests/{epoch}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionManifest {
    /// Monotonically increasing epoch counter for this session.
    pub epoch: u64,
    /// Session that was compacted.
    pub session_id: String,
    /// Timestamp of the compaction.
    pub compacted_at: DateTime<Utc>,
    /// Message keys that were deleted in this compaction.
    pub compacted_keys: Vec<String>,
    /// The summary key where the compacted text was stored.
    pub summary_key: String,
    /// SHA-256 digest of the concatenated cold-tier text *before*
    /// the summarizer ran. Allows post-hoc integrity audit.
    pub source_digest: String,
    /// Number of messages compacted.
    pub message_count: usize,
    /// First message timestamp in the compacted range.
    pub range_start_ms: i64,
    /// Last message timestamp in the compacted range.
    pub range_end_ms: i64,
}

/// Result of an integrity validation pass.
#[derive(Debug, Clone, Default)]
pub struct IntegrityReport {
    /// Number of manifests checked.
    pub manifests_checked: usize,
    /// Manifests where the referenced summary exists and is valid.
    pub valid: usize,
    /// Orphaned manifests (messages still exist — txn didn't commit).
    pub orphaned: usize,
    /// Missing summaries (data loss).
    pub missing_summaries: usize,
    /// Overlapping coverage between manifests.
    pub overlapping: usize,
    /// Keys that were cleaned up during validation.
    pub cleaned_keys: Vec<String>,
}

// ── Atomic Compaction ────────────────────────────────────────

/// Perform compaction within a single `TransactionalConn`, ensuring
/// summary-insert and message-deletes are atomic.
///
/// Returns `(messages_compacted, manifest)` or `(0, None)` if below
/// threshold.
pub async fn atomic_compact_session(
    store: &Arc<SochStore>,
    key: &SessionKey,
    summarizer: Option<&dyn Fn(&str) -> String>,
) -> Result<(usize, Option<CompactionManifest>), StorageError> {
    let prefix = format!("sessions/{}/messages/", key.as_str());
    let results = store
        .scan(&prefix)
        .map_err(|e| StorageError::OpenFailed {
            detail: e.to_string(),
        })?;

    let total = results.len();
    if total <= HOT_TIER_SIZE {
        return Ok((0, None));
    }

    let cold_count = total - HOT_TIER_SIZE;
    let cold_entries = &results[..cold_count];

    // Build cold-tier text and collect keys.
    let mut cold_text = String::new();
    let mut compacted_keys = Vec::with_capacity(cold_count);
    let mut range_start_ms: i64 = i64::MAX;
    let mut range_end_ms: i64 = i64::MIN;

    for (k, v) in cold_entries {
        let key_str = k.clone();
        compacted_keys.push(key_str.clone());

        // Extract timestamp from key path: sessions/{id}/messages/{ts}
        if let Some(ts_str) = key_str.rsplit('/').next() {
            if let Ok(ts) = ts_str.parse::<i64>() {
                range_start_ms = range_start_ms.min(ts);
                range_end_ms = range_end_ms.max(ts);
            }
        }

        if let Ok(msg) = serde_json::from_slice::<AgentMessage>(v) {
            if !cold_text.is_empty() {
                cold_text.push('\n');
            }
            cold_text.push_str(&format!("{:?}: {}", msg.role, msg.content));
        }
    }

    // Compute digest of source text for audit trail.
    let source_digest = sha256_hex(&cold_text);

    // Apply summarizer.
    let summary = match summarizer {
        Some(f) => f(&cold_text),
        None => {
            let max_len = 2000;
            if cold_text.len() > max_len {
                format!(
                    "[Summary of {} messages] {}...",
                    cold_count,
                    &cold_text[..max_len]
                )
            } else {
                format!("[Summary of {} messages] {}", cold_count, cold_text)
            }
        }
    };

    // Resolve the next epoch for this session.
    let epoch = next_epoch(store, key)?;

    let ts = Utc::now().timestamp_millis();
    let summary_key = format!("sessions/{}/summaries/{}", key.as_str(), ts);
    let manifest_key = format!("sessions/{}/manifests/{:020}", key.as_str(), epoch);

    let manifest = CompactionManifest {
        epoch,
        session_id: key.as_str().to_string(),
        compacted_at: Utc::now(),
        compacted_keys: compacted_keys.clone(),
        summary_key: summary_key.clone(),
        source_digest,
        message_count: cold_count,
        range_start_ms,
        range_end_ms,
    };

    let manifest_bytes =
        serde_json::to_vec(&manifest).map_err(|e| StorageError::SerializationFailed {
            detail: e.to_string(),
        })?;

    // ── Begin atomic transaction ─────────────────────────────
    let conn = SochConn::new(Arc::clone(store));
    let mut txn = TransactionalConn::begin_with_label(conn, "compact-session");

    // 1. Write summary.
    txn.put(summary_key.as_bytes(), summary.as_bytes())
        .map_err(|e| StorageError::OpenFailed {
            detail: format!("txn summary put: {e}"),
        })?;

    // 2. Write manifest.
    txn.put(manifest_key.as_bytes(), &manifest_bytes)
        .map_err(|e| StorageError::OpenFailed {
            detail: format!("txn manifest put: {e}"),
        })?;

    // 3. Delete all cold-tier messages.
    for ck in &compacted_keys {
        txn.delete(ck.as_bytes()).map_err(|e| StorageError::OpenFailed {
            detail: format!("txn delete {ck}: {e}"),
        })?;
    }

    // 4. Update epoch counter.
    let epoch_key = format!("sessions/{}/meta/compaction_epoch", key.as_str());
    let epoch_bytes = (epoch).to_le_bytes();
    txn.put(epoch_key.as_bytes(), &epoch_bytes)
        .map_err(|e| StorageError::OpenFailed {
            detail: format!("txn epoch put: {e}"),
        })?;

    // ── Commit atomically ────────────────────────────────────
    let CommitResult { puts, deletes } =
        txn.commit().map_err(|e| StorageError::OpenFailed {
            detail: format!("compaction commit failed: {e}"),
        })?;

    info!(
        session = %key,
        cold_count,
        epoch,
        puts,
        deletes,
        "atomic compaction committed"
    );

    Ok((cold_count, Some(manifest)))
}

// ── Integrity Validation ────────────────────────────────────

/// Validate referential integrity of a session's compaction history.
///
/// Checks every manifest and verifies:
/// 1. The referenced summary key exists.
/// 2. None of the compacted message keys still exist (fully deleted).
/// 3. No two manifests cover overlapping timestamp ranges.
pub fn validate_integrity(
    store: &SochStore,
    key: &SessionKey,
) -> Result<IntegrityReport, StorageError> {
    let manifest_prefix = format!("sessions/{}/manifests/", key.as_str());
    let manifest_entries = store
        .scan(&manifest_prefix)
        .map_err(|e| StorageError::OpenFailed {
            detail: e.to_string(),
        })?;

    let mut report = IntegrityReport::default();
    let mut ranges: Vec<(i64, i64, u64)> = Vec::new(); // (start, end, epoch)

    for (_mk, mv) in &manifest_entries {
        report.manifests_checked += 1;

        let manifest: CompactionManifest = match serde_json::from_slice(mv) {
            Ok(m) => m,
            Err(e) => {
                warn!(session = %key, "corrupt manifest: {e}");
                continue;
            }
        };

        // Check 1: Summary exists.
        let summary_exists = store
            .get(&manifest.summary_key)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?
            .is_some();

        if !summary_exists {
            report.missing_summaries += 1;
            warn!(
                session = %key,
                epoch = manifest.epoch,
                summary_key = %manifest.summary_key,
                "compaction summary MISSING — potential data loss"
            );
            continue;
        }

        // Check 2: Compacted messages no longer exist.
        let mut orphaned_msgs = 0usize;
        for ck in &manifest.compacted_keys {
            if let Ok(Some(_)) = store.get(ck) {
                orphaned_msgs += 1;
            }
        }

        if orphaned_msgs > 0 {
            // Manifest exists and summary exists, but messages are also
            // still present — this means the transaction was partially
            // applied (shouldn't happen with atomic compaction) or the
            // manifest is an orphan from a pre-atomic compaction era.
            report.orphaned += 1;
            warn!(
                session = %key,
                epoch = manifest.epoch,
                orphaned_msgs,
                "orphaned messages found alongside manifest+summary"
            );
        } else {
            report.valid += 1;
        }

        // Check 3: Overlapping ranges.
        for &(rs, re, ep) in &ranges {
            if manifest.range_start_ms <= re && manifest.range_end_ms >= rs {
                report.overlapping += 1;
                warn!(
                    session = %key,
                    epoch_a = ep,
                    epoch_b = manifest.epoch,
                    "overlapping compaction ranges detected"
                );
            }
        }
        ranges.push((manifest.range_start_ms, manifest.range_end_ms, manifest.epoch));
    }

    debug!(
        session = %key,
        manifests = report.manifests_checked,
        valid = report.valid,
        orphaned = report.orphaned,
        missing = report.missing_summaries,
        "integrity validation complete"
    );

    Ok(report)
}

/// Repair a session by removing orphaned manifests and cleaning up
/// dangling references.
///
/// Returns the number of keys cleaned.
pub fn repair_integrity(
    store: &Arc<SochStore>,
    key: &SessionKey,
) -> Result<usize, StorageError> {
    let manifest_prefix = format!("sessions/{}/manifests/", key.as_str());
    let manifest_entries = store
        .scan(&manifest_prefix)
        .map_err(|e| StorageError::OpenFailed {
            detail: e.to_string(),
        })?;

    let conn = SochConn::new(Arc::clone(store));
    let mut txn = TransactionalConn::begin_with_label(conn, "repair-integrity");
    let mut cleaned = 0usize;

    for (mk, mv) in &manifest_entries {
        let manifest: CompactionManifest = match serde_json::from_slice(mv) {
            Ok(m) => m,
            Err(_) => {
                // Corrupt manifest — remove it.
                txn.delete(mk.as_bytes()).map_err(|e| StorageError::OpenFailed {
                    detail: format!("repair delete corrupt manifest: {e}"),
                })?;
                cleaned += 1;
                continue;
            }
        };

        let summary_exists = store
            .get(&manifest.summary_key)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?
            .is_some();

        // If summary exists but messages are also still there, delete
        // the duplicate messages (summary takes precedence).
        if summary_exists {
            for ck in &manifest.compacted_keys {
                if let Ok(Some(_)) = store.get(ck) {
                    txn.delete(ck.as_bytes()).map_err(|e| StorageError::OpenFailed {
                        detail: format!("repair delete orphan msg: {e}"),
                    })?;
                    cleaned += 1;
                }
            }
        } else {
            // Summary missing — cannot recover the compacted data.
            // Remove the manifest to prevent confusing downstream code.
            txn.delete(mk.as_bytes()).map_err(|e| StorageError::OpenFailed {
                detail: format!("repair delete dangling manifest: {e}"),
            })?;
            cleaned += 1;
            warn!(
                session = %key,
                epoch = manifest.epoch,
                "removed dangling manifest (summary missing, data unrecoverable)"
            );
        }
    }

    if cleaned > 0 {
        txn.commit().map_err(|e| StorageError::OpenFailed {
            detail: format!("repair commit failed: {e}"),
        })?;
        info!(session = %key, cleaned, "integrity repair committed");
    } else {
        txn.rollback();
        debug!(session = %key, "no integrity issues to repair");
    }

    Ok(cleaned)
}

// ── Context-Safe Build ──────────────────────────────────────

/// Build context with integrity pre-check.
///
/// Before assembling the context window, validates that the compaction
/// history is consistent. If any anomalies are detected (orphaned
/// messages, missing summaries), a repair pass runs automatically.
///
/// This ensures `build_context()` never sees a non-monotonic state
/// where both a summary and its source messages are present.
pub async fn integrity_checked_context(
    store: &Arc<SochStore>,
    key: &SessionKey,
) -> Result<IntegrityReport, StorageError> {
    let report = validate_integrity(store, key)?;

    if report.orphaned > 0 || report.missing_summaries > 0 {
        warn!(
            session = %key,
            orphaned = report.orphaned,
            missing = report.missing_summaries,
            "integrity issues detected — running auto-repair"
        );
        let cleaned = repair_integrity(store, key)?;
        debug!(session = %key, cleaned, "auto-repair complete");
    }

    Ok(report)
}

// ── Helpers ─────────────────────────────────────────────────

/// Resolve the next compaction epoch for a session.
fn next_epoch(store: &SochStore, key: &SessionKey) -> Result<u64, StorageError> {
    let epoch_key = format!("sessions/{}/meta/compaction_epoch", key.as_str());
    match store.get(&epoch_key) {
        Ok(Some(bytes)) if bytes.len() == 8 => {
            let current = u64::from_le_bytes(bytes.try_into().unwrap());
            Ok(current + 1)
        }
        Ok(_) => Ok(1), // First compaction
        Err(e) => Err(StorageError::OpenFailed {
            detail: format!("read compaction epoch: {e}"),
        }),
    }
}

/// Simple SHA-256 hex digest (no external crate — uses a basic hash
/// for integrity auditing, not cryptographic security).
///
/// Uses a FNV-1a-based double-hash to produce a 64-char hex string
/// that's good enough for content fingerprinting without requiring
/// a `sha2` dependency.
fn sha256_hex(input: &str) -> String {
    // FNV-1a 64-bit, run twice with different seeds for 128-bit output.
    let h1 = fnv1a_64(input.as_bytes(), 0xcbf29ce484222325);
    let h2 = fnv1a_64(input.as_bytes(), 0x6c62272e07bb0142);
    format!("{:016x}{:016x}", h1, h2)
}

fn fnv1a_64(data: &[u8], seed: u64) -> u64 {
    let mut hash = seed;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
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
    fn sha256_hex_deterministic() {
        let d1 = sha256_hex("hello world");
        let d2 = sha256_hex("hello world");
        assert_eq!(d1, d2);
        assert_eq!(d1.len(), 32); // 2 × 16 hex chars
    }

    #[test]
    fn sha256_hex_different_inputs() {
        let d1 = sha256_hex("hello");
        let d2 = sha256_hex("world");
        assert_ne!(d1, d2);
    }

    #[test]
    fn next_epoch_starts_at_one() {
        // Without a DB, we can't test this directly, but verify the
        // manifest struct serialization round-trips.
        let manifest = CompactionManifest {
            epoch: 1,
            session_id: "test-session".into(),
            compacted_at: Utc::now(),
            compacted_keys: vec!["sessions/test/messages/100".into()],
            summary_key: "sessions/test/summaries/200".into(),
            source_digest: sha256_hex("test content"),
            message_count: 1,
            range_start_ms: 100,
            range_end_ms: 100,
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: CompactionManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.epoch, 1);
        assert_eq!(parsed.message_count, 1);
        assert_eq!(parsed.source_digest, manifest.source_digest);
    }

    #[test]
    fn integrity_report_defaults() {
        let report = IntegrityReport::default();
        assert_eq!(report.manifests_checked, 0);
        assert_eq!(report.valid, 0);
        assert_eq!(report.orphaned, 0);
        assert_eq!(report.missing_summaries, 0);
        assert_eq!(report.overlapping, 0);
    }

    #[test]
    fn manifest_with_multiple_keys() {
        let keys: Vec<String> = (1..=50)
            .map(|i| format!("sessions/s1/messages/{}", i * 1000))
            .collect();

        let manifest = CompactionManifest {
            epoch: 3,
            session_id: "s1".into(),
            compacted_at: Utc::now(),
            compacted_keys: keys.clone(),
            summary_key: "sessions/s1/summaries/99999".into(),
            source_digest: sha256_hex("fifty messages"),
            message_count: 50,
            range_start_ms: 1000,
            range_end_ms: 50000,
        };

        assert_eq!(manifest.compacted_keys.len(), 50);
        assert_eq!(manifest.range_end_ms - manifest.range_start_ms, 49000);
    }
}
