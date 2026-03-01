//! GAP-I: Lossless conversation cold tier.
//!
//! The existing compaction (`compact_session`) is **lossy** — it summarizes old
//! messages, stores the summary, and deletes the originals. This module adds a
//! lossless archive layer that preserves full message data before compaction
//! deletes it, enabling transparent retrieval of historical conversations.
//!
//! ## Key Layout
//!
//! ```text
//! archive/{session_key}/{epoch_millis}   → ArchiveChunk (JSON: metadata + messages)
//! ```
//!
//! Each chunk stores a batch of messages (typically the cold slice from one
//! compaction pass). Chunks are append-only and immutable once written.
//!
//! ## Integration
//!
//! The `archive_and_compact` method replaces the lossy `compact_session` workflow:
//!
//! 1. Identify cold-tier messages (same logic as `compact_session_with_limit`)
//! 2. Write full messages to `archive/{key}/{ts}` as an `ArchiveChunk`
//! 3. Produce summary (same as existing compaction)
//! 4. Delete compacted hot-tier message keys
//! 5. Return count + archive chunk ID
//!
//! Retrieval: `load_archive_chunks` and `load_full_history` provide transparent
//! access to archived data, merging it with hot-tier messages.

use chrono::{DateTime, Utc};
use clawdesk_types::{
    error::StorageError,
    session::{AgentMessage, SessionKey},
};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::SochStore;

/// A single archive chunk containing a batch of losslessly preserved messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveChunk {
    /// Unique chunk identifier (epoch millis when archived).
    pub chunk_id: i64,
    /// Session this chunk belongs to.
    pub session_key: String,
    /// When this chunk was archived.
    pub archived_at: DateTime<Utc>,
    /// Number of messages in this chunk.
    pub message_count: usize,
    /// Timestamp of the earliest message in the chunk.
    pub earliest_message: DateTime<Utc>,
    /// Timestamp of the latest message in the chunk.
    pub latest_message: DateTime<Utc>,
    /// The full, losslessly preserved messages.
    pub messages: Vec<AgentMessage>,
    /// Byte size of the serialized messages (for stats).
    pub uncompressed_bytes: usize,
}

/// Lightweight chunk metadata for fast index lookups.
///
/// Stored separately at `archive_index/{session}/{chunk_id}` so range
/// queries can skip irrelevant chunks without deserializing messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveChunkMeta {
    pub chunk_id: i64,
    pub earliest_message: DateTime<Utc>,
    pub latest_message: DateTime<Utc>,
    pub message_count: usize,
    pub uncompressed_bytes: usize,
}

/// Statistics about the cold-tier archive for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveStats {
    /// Total number of archive chunks.
    pub chunk_count: usize,
    /// Total archived messages across all chunks.
    pub total_messages: usize,
    /// Total bytes stored in archive.
    pub total_bytes: usize,
    /// Timestamp of oldest archived message.
    pub oldest_message: Option<DateTime<Utc>>,
    /// Timestamp of newest archived message.
    pub newest_message: Option<DateTime<Utc>>,
}

/// Result of an `archive_and_compact` operation.
#[derive(Debug, Clone)]
pub struct ArchiveCompactResult {
    /// Number of messages archived + compacted.
    pub messages_archived: usize,
    /// The archive chunk ID.
    pub chunk_id: i64,
    /// Number of messages remaining in hot tier.
    pub hot_tier_remaining: usize,
}

impl SochStore {
    /// Archive cold-tier messages losslessly, then compact (summarize + delete).
    ///
    /// This is the lossless equivalent of `compact_session_with_limit`:
    /// 1. Identifies messages beyond `hot_tier_size`
    /// 2. Writes them to an `ArchiveChunk` in the archive keyspace
    /// 3. Creates a summary (same as existing compaction)
    /// 4. Deletes the compacted message keys
    ///
    /// Returns `None` if below threshold (nothing to compact).
    pub async fn archive_and_compact(
        &self,
        key: &SessionKey,
        summarizer: Option<&dyn Fn(&str) -> String>,
        hot_tier_size: usize,
    ) -> Result<Option<ArchiveCompactResult>, StorageError> {
        let prefix = format!("sessions/{}/messages/", key.as_str());
        let results = self.scan(&prefix)?;

        let total = results.len();
        if total <= hot_tier_size {
            return Ok(None);
        }

        let cold_count = total - hot_tier_size;
        let cold_entries = &results[..cold_count];

        // Deserialize cold-tier messages for archival.
        let mut cold_messages = Vec::with_capacity(cold_count);
        let mut cold_text = String::new();
        let mut raw_bytes = 0usize;

        for (_k, v) in cold_entries {
            raw_bytes += v.len();
            if let Ok(msg) = serde_json::from_slice::<AgentMessage>(v) {
                if !cold_text.is_empty() {
                    cold_text.push('\n');
                }
                cold_text.push_str(&format!("{:?}: {}", msg.role, msg.content));
                cold_messages.push(msg);
            }
        }

        if cold_messages.is_empty() {
            return Ok(None);
        }

        let now = Utc::now();
        let chunk_id = now.timestamp_millis();

        // Build archive chunk.
        let earliest = cold_messages
            .first()
            .map(|m| m.timestamp)
            .unwrap_or(now);
        let latest = cold_messages
            .last()
            .map(|m| m.timestamp)
            .unwrap_or(now);

        let chunk = ArchiveChunk {
            chunk_id,
            session_key: key.as_str().to_string(),
            archived_at: now,
            message_count: cold_messages.len(),
            earliest_message: earliest,
            latest_message: latest,
            uncompressed_bytes: raw_bytes,
            messages: cold_messages,
        };

        // Write archive chunk.
        let archive_key = format!("archive/{}/{}", key.as_str(), chunk_id);
        let archive_bytes = serde_json::to_vec(&chunk).map_err(|e| {
            StorageError::SerializationFailed {
                detail: e.to_string(),
            }
        })?;
        self.put_durable(&archive_key, &archive_bytes)?;

        // Write lightweight chunk index entry.
        let index_key = format!("archive_index/{}/{}", key.as_str(), chunk_id);
        let meta = ArchiveChunkMeta {
            chunk_id,
            earliest_message: earliest,
            latest_message: latest,
            message_count: chunk.message_count,
            uncompressed_bytes: raw_bytes,
        };
        let index_bytes = serde_json::to_vec(&meta).map_err(|e| {
            StorageError::SerializationFailed {
                detail: e.to_string(),
            }
        })?;
        self.put(&index_key, &index_bytes)?;

        // Create summary (same logic as compact_session_with_limit).
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

        let summary_key = format!("sessions/{}/summaries/{}", key.as_str(), chunk_id);
        self.put(&summary_key, summary.as_bytes())?;

        // Delete compacted messages from hot tier.
        for (k, _v) in cold_entries {
            let _ = self.delete(k);
        }

        debug!(
            %key,
            cold_count,
            chunk_id,
            archive_bytes = archive_bytes.len(),
            remaining = hot_tier_size,
            "conversation archived + compacted (lossless)"
        );

        Ok(Some(ArchiveCompactResult {
            messages_archived: cold_count,
            chunk_id,
            hot_tier_remaining: hot_tier_size,
        }))
    }

    /// Load all archive chunks for a session, in chronological order.
    pub async fn load_archive_chunks(
        &self,
        key: &SessionKey,
    ) -> Result<Vec<ArchiveChunk>, StorageError> {
        let prefix = format!("archive/{}/", key.as_str());
        let results = self.scan(&prefix)?;

        let mut chunks = Vec::with_capacity(results.len());
        for (_k, v) in &results {
            match serde_json::from_slice::<ArchiveChunk>(v) {
                Ok(chunk) => chunks.push(chunk),
                Err(e) => {
                    debug!(error = %e, "skipping corrupt archive chunk");
                }
            }
        }

        // Already sorted by chunk_id (timestamp) via key ordering.
        Ok(chunks)
    }

    /// Load full conversation history: archived + hot tier, merged chronologically.
    ///
    /// This is the **transparent retrieval** method: it reconstructs the complete
    /// conversation from archive chunks and current hot-tier messages.
    pub async fn load_full_history(
        &self,
        key: &SessionKey,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        // 1. Load archived messages.
        let chunks = self.load_archive_chunks(key).await?;
        let mut all_messages: Vec<AgentMessage> = chunks
            .into_iter()
            .flat_map(|c| c.messages)
            .collect();

        // 2. Load hot-tier messages (no limit — get everything).
        let prefix = format!("sessions/{}/messages/", key.as_str());
        let results = self.scan(&prefix)?;
        for (_k, v) in &results {
            if let Ok(msg) = serde_json::from_slice::<AgentMessage>(v) {
                all_messages.push(msg);
            }
        }

        // 3. Sort by timestamp for chronological order.
        all_messages.sort_by_key(|m| m.timestamp);

        Ok(all_messages)
    }

    /// Load the lightweight chunk index for a session.
    ///
    /// Returns chunk metadata in chronological order without deserializing
    /// any message payloads. O(n×meta_size) where meta is ~100 bytes.
    pub async fn load_chunk_index(
        &self,
        key: &SessionKey,
    ) -> Result<Vec<ArchiveChunkMeta>, StorageError> {
        let prefix = format!("archive_index/{}/", key.as_str());
        let results = self.scan(&prefix)?;

        let mut metas = Vec::with_capacity(results.len());
        for (_k, v) in &results {
            if let Ok(meta) = serde_json::from_slice::<ArchiveChunkMeta>(v) {
                metas.push(meta);
            }
        }
        Ok(metas)
    }

    /// Load archived messages within a time range.
    ///
    /// Uses the chunk index to skip irrelevant chunks, avoiding
    /// full deserialization of out-of-range data.
    pub async fn load_archive_range(
        &self,
        key: &SessionKey,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        let index = self.load_chunk_index(key).await?;

        let mut messages = Vec::new();
        for meta in &index {
            // Skip chunks entirely outside the range via index.
            if meta.latest_message < from || meta.earliest_message > to {
                continue;
            }
            // Only deserialize chunks that overlap the requested range.
            let chunk_key = format!("archive/{}/{}", key.as_str(), meta.chunk_id);
            if let Ok(Some(data)) = self.get(&chunk_key) {
                if let Ok(chunk) = serde_json::from_slice::<ArchiveChunk>(&data) {
                    for msg in chunk.messages {
                        if msg.timestamp >= from && msg.timestamp <= to {
                            messages.push(msg);
                        }
                    }
                }
            }
        }

        messages.sort_by_key(|m| m.timestamp);
        Ok(messages)
    }

    /// Get archive statistics for a session.
    pub async fn archive_stats(
        &self,
        key: &SessionKey,
    ) -> Result<ArchiveStats, StorageError> {
        let chunks = self.load_archive_chunks(key).await?;

        let mut stats = ArchiveStats {
            chunk_count: chunks.len(),
            total_messages: 0,
            total_bytes: 0,
            oldest_message: None,
            newest_message: None,
        };

        for chunk in &chunks {
            stats.total_messages += chunk.message_count;
            stats.total_bytes += chunk.uncompressed_bytes;

            match stats.oldest_message {
                None => stats.oldest_message = Some(chunk.earliest_message),
                Some(ref oldest) if chunk.earliest_message < *oldest => {
                    stats.oldest_message = Some(chunk.earliest_message);
                }
                _ => {}
            }

            match stats.newest_message {
                None => stats.newest_message = Some(chunk.latest_message),
                Some(ref newest) if chunk.latest_message > *newest => {
                    stats.newest_message = Some(chunk.latest_message);
                }
                _ => {}
            }
        }

        Ok(stats)
    }

    /// Delete archive chunks older than a given date.
    ///
    /// This is the final cleanup — use when you're certain archived data
    /// is no longer needed (e.g., exported to external backup).
    pub async fn purge_archive_before(
        &self,
        key: &SessionKey,
        before: DateTime<Utc>,
    ) -> Result<usize, StorageError> {
        let prefix = format!("archive/{}/", key.as_str());
        let results = self.scan(&prefix)?;

        let mut purged = 0;
        for (k, v) in &results {
            if let Ok(chunk) = serde_json::from_slice::<ArchiveChunk>(v) {
                if chunk.latest_message < before {
                    let _ = self.delete(k);
                    purged += chunk.message_count;
                }
            }
        }

        if purged > 0 {
            debug!(%key, purged, %before, "archive chunks purged");
        }

        Ok(purged)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_storage::conversation_store::ConversationStore;
    use clawdesk_types::session::{AgentMessage, Role, SessionKey};
    use clawdesk_types::channel::ChannelId;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Monotonic counter to generate unique session keys across tests.
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_messages(count: usize) -> Vec<AgentMessage> {
        let base = Utc::now();
        (0..count)
            .map(|i| AgentMessage {
                role: if i % 2 == 0 { Role::User } else { Role::Assistant },
                content: format!("Message #{}", i),
                timestamp: base + chrono::Duration::seconds(i as i64),
                model: None,
                token_count: Some(10),
                tool_call_id: None,
                tool_name: None,
            })
            .collect()
    }

    fn test_store() -> SochStore {
        SochStore::open_ephemeral_quiet().unwrap()
    }

    fn unique_key() -> SessionKey {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        SessionKey::new(ChannelId::Internal, &format!("cold-tier-test-{}", n))
    }

    #[tokio::test]
    async fn test_archive_and_compact_below_threshold() {
        let store = test_store();
        let key = unique_key();

        // Add 5 messages (below any reasonable threshold)
        let msgs = make_messages(5);
        for msg in &msgs {
            store.append_message(&key, msg).await.unwrap();
        }

        let result = store.archive_and_compact(&key, None, 10).await.unwrap();
        assert!(result.is_none(), "should not compact below threshold");
    }

    #[tokio::test]
    async fn test_archive_and_compact_above_threshold() {
        let store = test_store();
        let key = unique_key();

        let msgs = make_messages(15);
        for msg in &msgs {
            store.append_message(&key, msg).await.unwrap();
        }

        let result = store
            .archive_and_compact(&key, None, 10)
            .await
            .unwrap()
            .expect("should compact");

        assert_eq!(result.messages_archived, 5);
        assert_eq!(result.hot_tier_remaining, 10);

        // Verify hot tier has exactly 10 messages.
        let hot = store.load_history(&key, 100).await.unwrap();
        assert_eq!(hot.len(), 10);

        // Verify archive has the 5 oldest messages.
        let chunks = store.load_archive_chunks(&key).await.unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].message_count, 5);
        assert_eq!(chunks[0].messages[0].content, "Message #0");
        assert_eq!(chunks[0].messages[4].content, "Message #4");
    }

    #[tokio::test]
    async fn test_load_full_history_merges_archive_and_hot() {
        let store = test_store();
        let key = unique_key();

        let msgs = make_messages(15);
        for msg in &msgs {
            store.append_message(&key, msg).await.unwrap();
        }

        // Archive first 5
        store.archive_and_compact(&key, None, 10).await.unwrap();

        // Full history should have all 15
        let full = store.load_full_history(&key).await.unwrap();
        assert_eq!(full.len(), 15);
        assert_eq!(full[0].content, "Message #0");
        assert_eq!(full[14].content, "Message #14");
    }

    #[tokio::test]
    async fn test_multiple_compaction_passes() {
        let store = test_store();
        let key = unique_key();

        // Pass 1: 20 messages, compact to 10
        let msgs = make_messages(20);
        for msg in &msgs {
            store.append_message(&key, msg).await.unwrap();
        }
        let r1 = store
            .archive_and_compact(&key, None, 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r1.messages_archived, 10);

        // Pass 2: Add 10 more, compact again
        let more_msgs: Vec<AgentMessage> = (20..30)
            .map(|i| AgentMessage {
                role: Role::User,
                content: format!("Message #{}", i),
                timestamp: Utc::now() + chrono::Duration::seconds(i as i64 + 100),
                model: None,
                token_count: Some(10),
                tool_call_id: None,
                tool_name: None,
            })
            .collect();
        for msg in &more_msgs {
            store.append_message(&key, msg).await.unwrap();
        }
        let r2 = store
            .archive_and_compact(&key, None, 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r2.messages_archived, 10);

        // Should have 2 archive chunks
        let chunks = store.load_archive_chunks(&key).await.unwrap();
        assert_eq!(chunks.len(), 2);

        // Full history: 30 messages
        let full = store.load_full_history(&key).await.unwrap();
        assert_eq!(full.len(), 30);
    }

    #[tokio::test]
    async fn test_archive_stats() {
        let store = test_store();
        let key = unique_key();

        let msgs = make_messages(25);
        for msg in &msgs {
            store.append_message(&key, msg).await.unwrap();
        }

        store.archive_and_compact(&key, None, 10).await.unwrap();

        let stats = store.archive_stats(&key).await.unwrap();
        assert_eq!(stats.chunk_count, 1);
        assert_eq!(stats.total_messages, 15);
        assert!(stats.oldest_message.is_some());
        assert!(stats.newest_message.is_some());
    }

    #[tokio::test]
    async fn test_archive_range_query() {
        let store = test_store();
        let key = unique_key();

        let msgs = make_messages(20);
        let mid_time = msgs[10].timestamp;
        for msg in &msgs {
            store.append_message(&key, msg).await.unwrap();
        }

        store.archive_and_compact(&key, None, 5).await.unwrap();

        // Query a range that includes messages 5-14 (archived portion starts at 0)
        let range = store
            .load_archive_range(
                &key,
                msgs[5].timestamp,
                mid_time,
            )
            .await
            .unwrap();

        // Should get messages 5 through 10 from the archived 15 messages
        assert!(!range.is_empty());
        for msg in &range {
            assert!(msg.timestamp >= msgs[5].timestamp);
            assert!(msg.timestamp <= mid_time);
        }
    }

    #[tokio::test]
    async fn test_purge_archive() {
        let store = test_store();
        let key = unique_key();

        let msgs = make_messages(20);
        for msg in &msgs {
            store.append_message(&key, msg).await.unwrap();
        }
        store.archive_and_compact(&key, None, 5).await.unwrap();

        // Purge everything archived before far in the future
        let far_future = Utc::now() + chrono::Duration::days(365);
        let purged = store.purge_archive_before(&key, far_future).await.unwrap();
        assert_eq!(purged, 15);

        // Archive should now be empty
        let chunks = store.load_archive_chunks(&key).await.unwrap();
        assert!(chunks.is_empty());

        // But hot tier is untouched
        let hot = store.load_history(&key, 100).await.unwrap();
        assert_eq!(hot.len(), 5);
    }

    #[tokio::test]
    async fn test_custom_summarizer_with_archive() {
        let store = test_store();
        let key = unique_key();

        let msgs = make_messages(15);
        for msg in &msgs {
            store.append_message(&key, msg).await.unwrap();
        }

        let summarizer = |text: &str| format!("[CUSTOM] {} chars of history", text.len());
        let result = store
            .archive_and_compact(&key, Some(&summarizer), 10)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(result.messages_archived, 5);

        // Verify the custom summary was stored
        let summaries = store.load_summaries(&key).await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].starts_with("[CUSTOM]"));
    }
}
