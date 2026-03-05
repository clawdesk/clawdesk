//! SochDB implementation of `ChatReplayStore` — durable paired turn storage.
//!
//! ## Storage Layout
//!
//! ```text
//! turns/{session_key}/seq/{sequence:020}  →  JSON(ChatTurn)
//! turns/{session_key}/id/{turn_id}        →  sequence (for ID lookup)
//! turns/{session_key}/meta/count          →  u64 (turn count)
//! ```
//!
//! Sequence numbers are zero-padded to 20 digits for lexicographic ordering,
//! enabling efficient prefix-scan-based sequential replay.

use async_trait::async_trait;
use clawdesk_storage::replay_store::{ChatReplayStore, ChatTurn, TurnId, TurnStats};
use clawdesk_types::error::StorageError;
use clawdesk_types::session::SessionKey;
use tracing::debug;

use crate::SochStore;

/// Format a sequence number for lexicographic key ordering.
fn seq_key(session: &str, seq: u64) -> String {
    format!("turns/{}/seq/{:020}", session, seq)
}

/// Format an ID-lookup key.
fn id_key(session: &str, turn_id: &str) -> String {
    format!("turns/{}/id/{}", session, turn_id)
}

/// Format the count key.
fn count_key(session: &str) -> String {
    format!("turns/{}/meta/count", session)
}

#[async_trait]
impl ChatReplayStore for SochStore {
    async fn store_turn(&self, turn: &ChatTurn) -> Result<(), StorageError> {
        let session_str = turn.session_id.clone();
        let bytes = serde_json::to_vec(turn).map_err(|e| StorageError::SerializationFailed {
            detail: e.to_string(),
        })?;

        // GAP-01: Use put_batch() to atomically write all three related keys
        // (turn data, ID→seq index, count) in a single commit+fsync.
        // Individual put() calls risk partial writes on crash: e.g. count
        // updated but turn data missing, or index entry without turn data.
        let skey = seq_key(&session_str, turn.sequence);
        let ikey = id_key(&session_str, turn.id.as_str());
        let seq_bytes = turn.sequence.to_le_bytes();
        let ckey = count_key(&session_str);
        let new_count = turn.sequence + 1;
        let count_bytes = new_count.to_le_bytes();

        self.put_batch(&[
            (&skey, bytes.as_slice()),
            (&ikey, &seq_bytes),
            (&ckey, &count_bytes),
        ])?;

        debug!(
            session = %session_str,
            sequence = turn.sequence,
            tokens = turn.total_tokens(),
            "chat turn stored (batch durable)"
        );

        Ok(())
    }

    async fn load_turns(
        &self,
        session_key: &SessionKey,
        limit: usize,
    ) -> Result<Vec<ChatTurn>, StorageError> {
        let session_str = session_key.as_str();
        let prefix = format!("turns/{}/seq/", session_str);
        let results = self
            .scan(&prefix)?;

        let mut turns = Vec::with_capacity(limit.min(results.len()));
        for (_key, value) in results.iter().take(limit) {
            if let Ok(turn) = serde_json::from_slice::<ChatTurn>(value) {
                turns.push(turn);
            }
        }

        debug!(
            session = %session_str,
            loaded = turns.len(),
            "chat turns loaded"
        );
        Ok(turns)
    }

    async fn get_turn(&self, turn_id: &TurnId) -> Result<Option<ChatTurn>, StorageError> {
        // Extract session from turn_id (format: "session:turn:N")
        let parts: Vec<&str> = turn_id.as_str().rsplitn(3, ":turn:").collect();
        if parts.len() < 2 {
            return Ok(None);
        }
        let session_str = parts[1];
        let ikey = id_key(session_str, turn_id.as_str());

        // Look up sequence from ID index
        let seq_bytes = self
            .get(&ikey)?;

        let seq_bytes = match seq_bytes {
            Some(b) => b,
            None => return Ok(None),
        };

        if seq_bytes.len() < 8 {
            return Ok(None);
        }
        let seq = u64::from_le_bytes(seq_bytes[..8].try_into().unwrap());

        // Load the turn by sequence
        let skey = seq_key(session_str, seq);
        let data = self
            .get(&skey)?;

        match data {
            Some(bytes) => {
                let turn = serde_json::from_slice::<ChatTurn>(&bytes).map_err(|e| {
                    StorageError::SerializationFailed {
                        detail: e.to_string(),
                    }
                })?;
                Ok(Some(turn))
            }
            None => Ok(None),
        }
    }

    async fn load_turn_range(
        &self,
        session_key: &SessionKey,
        from_sequence: u64,
        to_sequence: u64,
    ) -> Result<Vec<ChatTurn>, StorageError> {
        let session_str = session_key.as_str();

        // GAP-09: Use scan_range() for O(R) bounded retrieval instead of
        // scanning ALL turns then filtering in-memory. Keys are zero-padded
        // to 20 digits precisely to enable lexicographic range scans.
        let from_key = seq_key(&session_str, from_sequence);
        let to_key = seq_key(&session_str, to_sequence);

        let results = self.scan_range(&from_key, &to_key)?;

        let mut turns = Vec::with_capacity(results.len());
        for (_key, value) in &results {
            if let Ok(turn) = serde_json::from_slice::<ChatTurn>(value) {
                turns.push(turn);
            }
        }

        Ok(turns)
    }

    async fn turn_count(&self, session_key: &SessionKey) -> Result<u64, StorageError> {
        let session_str = session_key.as_str();
        let ckey = count_key(&session_str);
        let data = self
            .get(&ckey)?;

        match data {
            Some(bytes) if bytes.len() >= 8 => {
                Ok(u64::from_le_bytes(bytes[..8].try_into().unwrap()))
            }
            _ => Ok(0),
        }
    }

    async fn delete_session_turns(
        &self,
        session_key: &SessionKey,
    ) -> Result<u64, StorageError> {
        let session_str = session_key.as_str();

        // Delete sequence entries
        let seq_prefix = format!("turns/{}/seq/", session_str);
        let seq_entries = self
            .scan(&seq_prefix)?;
        let count = seq_entries.len() as u64;
        for (key, _) in &seq_entries {
            let _ = self.delete(key);
        }

        // Delete ID index entries
        let id_prefix = format!("turns/{}/id/", session_str);
        let id_entries = self
            .scan(&id_prefix)?;
        for (key, _) in &id_entries {
            let _ = self.delete(key);
        }

        // Delete count
        let ckey = count_key(&session_str);
        let _ = self.delete(&ckey);

        debug!(session = %session_str, deleted = count, "session turns deleted");
        Ok(count)
    }

    async fn turn_stats(&self, session_key: &SessionKey) -> Result<TurnStats, StorageError> {
        let turns = self.load_turns(session_key, usize::MAX).await?;

        if turns.is_empty() {
            return Ok(TurnStats {
                total_turns: 0,
                total_input_tokens: 0,
                total_output_tokens: 0,
                total_tool_calls: 0,
                avg_duration_ms: 0.0,
                primary_model: None,
            });
        }

        let total_turns = turns.len() as u64;
        let total_input_tokens: u64 = turns.iter().map(|t| t.input_tokens).sum();
        let total_output_tokens: u64 = turns.iter().map(|t| t.output_tokens).sum();
        let total_tool_calls: u64 = turns
            .iter()
            .map(|t| t.tool_exchanges.len() as u64)
            .sum();
        let total_duration: u64 = turns.iter().map(|t| t.duration_ms).sum();
        let avg_duration_ms = total_duration as f64 / total_turns as f64;

        // Find most common model
        let mut model_counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for turn in &turns {
            if let Some(model) = &turn.model {
                *model_counts.entry(model.clone()).or_insert(0) += 1;
            }
        }
        let primary_model = model_counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(model, _)| model);

        Ok(TurnStats {
            total_turns,
            total_input_tokens,
            total_output_tokens,
            total_tool_calls,
            avg_duration_ms,
            primary_model,
        })
    }
}
