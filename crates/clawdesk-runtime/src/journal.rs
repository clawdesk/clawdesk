//! Activity journal — append-only log of durable side-effects.
//!
//! Every externally-observable operation (LLM call, tool execution, compaction,
//! gate decision) is journaled to SochDB **before** its effects are consumed.
//! On replay, completed journal entries are served from cache.
//!
//! ## Storage
//!
//! ```text
//! runtime:runs:{run_id}:journal:{seq:010}  →  JournalEntry (JSON)
//! ```
//!
//! The sequence number is zero-padded to 10 digits so that SochDB's prefix
//! scan returns entries in insertion order (lexicographic = numeric for
//! zero-padded decimal).

use crate::types::{JournalEntry, RunId, RuntimeError};
use clawdesk_sochdb::SochStore;
use clawdesk_types::error::StorageError;
use std::sync::Arc;
use tracing::{debug, warn};

/// Append-only activity journal backed by SochDB.
pub struct ActivityJournal {
    store: Arc<SochStore>,
}

impl ActivityJournal {
    /// Create a new journal backed by the given SochDB store.
    pub fn new(store: Arc<SochStore>) -> Self {
        Self { store }
    }

    /// Append a journal entry for a run.
    ///
    /// The entry is persisted to SochDB immediately. With group-commit
    /// enabled (100-op batches, 10ms max wait), back-to-back appends in
    /// the same round coalesce into a single fsync.
    pub async fn append(
        &self,
        run_id: &RunId,
        entry: &JournalEntry,
    ) -> Result<(), RuntimeError> {
        let seq = entry.seq();
        let key = Self::journal_key(run_id, seq);
        let bytes = serde_json::to_vec(entry).map_err(|e| StorageError::SerializationFailed {
            detail: e.to_string(),
        })?;

        self.store
            .db()
            .put(key.as_bytes(), &bytes)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        debug!(%run_id, seq, kind = entry.kind_label(), "journal entry appended");
        Ok(())
    }

    /// Load all journal entries for a run, in sequence order.
    pub async fn load_all(
        &self,
        run_id: &RunId,
    ) -> Result<Vec<JournalEntry>, RuntimeError> {
        let prefix = format!("runtime:runs:{}:journal:", run_id);
        let entries = self
            .store
            .db()
            .scan(prefix.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        let mut journal = Vec::with_capacity(entries.len());
        for (key_bytes, value) in &entries {
            match serde_json::from_slice::<JournalEntry>(value) {
                Ok(entry) => journal.push(entry),
                Err(e) => {
                    let key_str = String::from_utf8_lossy(key_bytes);
                    warn!(%run_id, key = %key_str, error = %e, "skipping corrupted journal entry");
                }
            }
        }

        Ok(journal)
    }

    /// Find a specific journal entry by round and kind.
    ///
    /// Used during replay to check if an LLM call or tool execution
    /// was already completed in a previous attempt.
    pub async fn find_llm_call(
        &self,
        run_id: &RunId,
        round: usize,
    ) -> Result<Option<JournalEntry>, RuntimeError> {
        let journal = self.load_all(run_id).await?;
        Ok(journal.into_iter().find(|e| {
            matches!(e, JournalEntry::LlmCall { round: r, .. } if *r == round)
        }))
    }

    /// Find a tool execution journal entry by tool_call_id.
    pub async fn find_tool_execution(
        &self,
        run_id: &RunId,
        tool_call_id: &str,
    ) -> Result<Option<JournalEntry>, RuntimeError> {
        let journal = self.load_all(run_id).await?;
        Ok(journal.into_iter().find(|e| {
            matches!(e, JournalEntry::ToolExecution { tool_call_id: id, .. } if id == tool_call_id)
        }))
    }

    /// Count journal entries for a run.
    pub async fn count(&self, run_id: &RunId) -> Result<usize, RuntimeError> {
        let prefix = format!("runtime:runs:{}:journal:", run_id);
        let entries = self
            .store
            .db()
            .scan(prefix.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;
        Ok(entries.len())
    }

    /// Delete all journal entries for a run (cleanup after completion).
    pub async fn delete_all(&self, run_id: &RunId) -> Result<usize, RuntimeError> {
        let prefix = format!("runtime:runs:{}:journal:", run_id);
        let entries = self
            .store
            .db()
            .scan(prefix.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        let count = entries.len();
        for (key, _) in &entries {
            let _ = self.store.db().delete(key);
        }

        debug!(%run_id, count, "journal entries deleted");
        Ok(count)
    }

    /// Build the SochDB key for a journal entry.
    fn journal_key(run_id: &RunId, seq: u64) -> String {
        format!("runtime:runs:{}:journal:{:010}", run_id, seq)
    }
}

// ── JournalEntry helpers ─────────────────────────────────────

impl JournalEntry {
    /// Get the sequence number of this entry.
    pub fn seq(&self) -> u64 {
        match self {
            Self::LlmCall { seq, .. } => *seq,
            Self::ToolExecution { seq, .. } => *seq,
            Self::Compaction { seq, .. } => *seq,
            Self::GateDecision { seq, .. } => *seq,
        }
    }

    /// Get a short label for logging.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::LlmCall { .. } => "llm_call",
            Self::ToolExecution { .. } => "tool_execution",
            Self::Compaction { .. } => "compaction",
            Self::GateDecision { .. } => "gate_decision",
        }
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LlmSnapshot;
    use chrono::Utc;
    use clawdesk_providers::{FinishReason, TokenUsage};

    fn test_store() -> Arc<SochStore> {
        Arc::new(SochStore::open_in_memory().expect("in-memory store"))
    }

    #[tokio::test]
    async fn append_and_load() {
        let store = test_store();
        let journal = ActivityJournal::new(store);
        let run_id = RunId::new();

        let entry = JournalEntry::LlmCall {
            seq: 0,
            round: 0,
            request_hash: 12345,
            response: LlmSnapshot {
                content: "Hello world".into(),
                model: "gpt-4".into(),
                tool_calls: vec![],
                usage: TokenUsage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                },
                finish_reason: FinishReason::Stop,
            },
            started_at: Utc::now(),
            completed_at: Utc::now(),
        };

        journal.append(&run_id, &entry).await.unwrap();

        let loaded = journal.load_all(&run_id).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].seq(), 0);
        assert_eq!(loaded[0].kind_label(), "llm_call");
    }

    #[tokio::test]
    async fn find_by_round() {
        let store = test_store();
        let journal = ActivityJournal::new(store);
        let run_id = RunId::new();

        let snapshot = LlmSnapshot {
            content: "response".into(),
            model: "gpt-4".into(),
            tool_calls: vec![],
            usage: TokenUsage::default(),
            finish_reason: FinishReason::Stop,
        };

        // Append entries for rounds 0, 1, 2.
        for round in 0..3 {
            let entry = JournalEntry::LlmCall {
                seq: round as u64,
                round,
                request_hash: round as u64 * 1000,
                response: snapshot.clone(),
                started_at: Utc::now(),
                completed_at: Utc::now(),
            };
            journal.append(&run_id, &entry).await.unwrap();
        }

        // Find round 1.
        let found = journal.find_llm_call(&run_id, 1).await.unwrap();
        assert!(found.is_some());
        if let Some(JournalEntry::LlmCall { round, .. }) = found {
            assert_eq!(round, 1);
        }

        // Round 5 doesn't exist.
        let not_found = journal.find_llm_call(&run_id, 5).await.unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn delete_all_entries() {
        let store = test_store();
        let journal = ActivityJournal::new(store);
        let run_id = RunId::new();

        let snapshot = LlmSnapshot {
            content: "x".into(),
            model: "m".into(),
            tool_calls: vec![],
            usage: TokenUsage::default(),
            finish_reason: FinishReason::Stop,
        };

        for i in 0..5 {
            let entry = JournalEntry::LlmCall {
                seq: i,
                round: i as usize,
                request_hash: i,
                response: snapshot.clone(),
                started_at: Utc::now(),
                completed_at: Utc::now(),
            };
            journal.append(&run_id, &entry).await.unwrap();
        }

        let deleted = journal.delete_all(&run_id).await.unwrap();
        assert_eq!(deleted, 5);

        let remaining = journal.load_all(&run_id).await.unwrap();
        assert!(remaining.is_empty());
    }
}
