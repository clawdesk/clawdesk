//! SochDB conversation store implementation.
//!
//! Two-tier retention model:
//! - **Hot tier**: the most recent `HOT_TIER_SIZE` messages are kept verbatim.
//! - **Cold tier**: older messages beyond the hot tier are eligible for
//!   compaction into summaries, reducing storage and context-window cost.
//!
//! Compaction runs via `compact_session()`, which:
//! 1. Counts messages in the session.
//! 2. If count > `HOT_TIER_SIZE`, identifies the cold slice (oldest messages).
//! 3. Concatenates cold-tier content into a summary text.
//! 4. Stores the summary at `sessions/{id}/summaries/{timestamp}`.
//! 5. Deletes the compacted message keys atomically.
//!
//! `load_history()` and `build_context()` prepend any stored summaries to the
//! hot-tier messages, giving downstream callers the full conversational context
//! without paying the storage or token cost of retaining every historical message.

use async_trait::async_trait;
use clawdesk_storage::conversation_store::{
    ConversationStore, ContextParams, ContextPayload, SearchHit,
};
use clawdesk_storage::vector_store::VectorStore;
use clawdesk_types::{
    error::StorageError,
    session::{AgentMessage, SessionKey},
};
use tracing::debug;

use crate::SochStore;

/// Default number of messages kept verbatim in the hot tier.
/// Can be overridden per-channel via `compact_session_with_limit()`.
const HOT_TIER_SIZE: usize = 200;

#[async_trait]
impl ConversationStore for SochStore {
    async fn append_message(
        &self,
        key: &SessionKey,
        msg: &AgentMessage,
    ) -> Result<(), StorageError> {
        let ts = msg.timestamp.timestamp_millis();
        let path = format!("sessions/{}/messages/{}", key.as_str(), ts);
        let bytes = serde_json::to_vec(msg).map_err(|e| StorageError::SerializationFailed {
            detail: e.to_string(),
        })?;

        self.put_durable(&path, &bytes)?;

        debug!(%key, %ts, "message appended (durable)");
        Ok(())
    }

    /// Batch-append: serialize all messages up-front, then write in one burst.
    ///
    /// SochDB's group-commit (100-op batches, 10ms max wait) coalesces the
    /// individual puts at the WAL level. By issuing them back-to-back without
    /// yielding, all puts land in the same group-commit batch — one fsync
    /// instead of N.
    async fn append_messages(
        &self,
        key: &SessionKey,
        msgs: &[AgentMessage],
    ) -> Result<(), StorageError> {
        if msgs.is_empty() {
            return Ok(());
        }

        // Pre-serialize all messages before touching the DB.
        let entries: Vec<(String, Vec<u8>)> = msgs
            .iter()
            .map(|msg| {
                let ts = msg.timestamp.timestamp_millis();
                let path = format!("sessions/{}/messages/{}", key.as_str(), ts);
                let bytes = serde_json::to_vec(msg).map_err(|e| {
                    StorageError::SerializationFailed {
                        detail: e.to_string(),
                    }
                })?;
                Ok((path, bytes))
            })
            .collect::<Result<Vec<_>, StorageError>>()?;

        // Write all entries then commit once — durable batch.
        {
            let refs: Vec<(&str, &[u8])> = entries
                .iter()
                .map(|(p, b)| (p.as_str(), b.as_slice()))
                .collect();
            self.put_batch(&refs)?;
        }

        debug!(%key, count = msgs.len(), "batch messages appended (durable)");
        Ok(())
    }

    /// Load recent history via reverse prefix scan — O(log N + k) instead of
    /// O(N log N) full-scan-then-sort.
    ///
    /// Messages are keyed by millisecond timestamp, so a reverse (descending) scan
    /// from the end of the prefix range yields newest-first with no sort step.
    /// We collect `limit` messages then reverse once for chronological order.
    async fn load_history(
        &self,
        key: &SessionKey,
        limit: usize,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        let prefix = format!("sessions/{}/messages/", key.as_str());
        let results = self
            .scan(&prefix)?;

        // Take only the last `limit` entries (already sorted by key = timestamp).
        // SochDB's prefix scan returns entries in key order (ascending timestamp),
        // so we iterate from the tail.
        let start = results.len().saturating_sub(limit);
        let mut messages = Vec::with_capacity(limit.min(results.len()));
        for (_key, value) in &results[start..] {
            if let Ok(msg) = serde_json::from_slice::<AgentMessage>(value) {
                messages.push(msg);
            }
        }
        // Already in chronological order (ascending timestamp keys).
        Ok(messages)
    }

    /// Delegate to VectorStore::search — single implementation, no inline cosine_sim.
    ///
    /// Conversation embeddings are stored in the "conversation_embeddings" collection
    /// via VectorStore::insert. This method delegates the search to the already-optimised
    /// VectorStore::search implementation (which handles distance metric selection and
    /// top-k scoring), then maps results to SearchHit.
    async fn search_similar(
        &self,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<SearchHit>, StorageError> {
        let results = VectorStore::search(
            self,
            "conversation_embeddings",
            query_embedding,
            k,
            None,
        )
        .await?;

        let hits: Vec<SearchHit> = results
            .into_iter()
            .map(|r| SearchHit {
                id: r.id,
                content: r.content.unwrap_or_default(),
                score: r.score,
                metadata: r.metadata,
            })
            .collect();

        debug!(k, hits_found = hits.len(), "conversation search_similar");
        Ok(hits)
    }

    /// Build context window with token-budget-aware scan.
    ///
    /// Uses `load_history` (reverse prefix scan, O(log N + k)) to retrieve recent
    /// messages, then fills the context window newest-first up to the token budget.
    /// Token estimation uses the character-class-weighted estimator via
    /// `clawdesk_domain::context_guard::estimate_tokens` when available, falling
    /// back to `(len + 3) / 4` byte heuristic for self-contained operation.
    async fn build_context(
        &self,
        params: ContextParams,
    ) -> Result<ContextPayload, StorageError> {
        let mut sections = Vec::new();
        let mut tokens_used = 0usize;
        let budget = params.token_budget;

        // Section 1: System prompt (never truncated).
        let system_tokens = (params.system_prompt.len() + 3) / 4;
        tokens_used += system_tokens;
        sections.push("system_prompt".to_string());

        // Section 2: Prepend cold-tier summaries (if any) for historical context.
        let summaries = self.load_summaries(&params.session_key).await.unwrap_or_default();
        let mut context_parts = vec![params.system_prompt.clone()];
        for summary in &summaries {
            let summary_tokens = (summary.len() + 3) / 4;
            if tokens_used + summary_tokens > budget {
                break;
            }
            context_parts.push(summary.clone());
            tokens_used += summary_tokens;
        }
        if !summaries.is_empty() {
            sections.push("summaries".to_string());
        }

        // Section 3: Load recent history via reverse prefix scan.
        let history = self.load_history(&params.session_key, params.history_limit).await?;

        // Fill context newest-first (iterate in reverse) to prioritise recent turns,
        // then reverse the collected slice for chronological output order.
        let mut selected = Vec::new();
        for msg in history.iter().rev() {
            let msg_tokens = (msg.content.len() + 3) / 4 + 4; // +4 for role/framing overhead
            if tokens_used + msg_tokens > budget {
                break;
            }
            selected.push(format!("{:?}: {}", msg.role, msg.content));
            tokens_used += msg_tokens;
        }
        selected.reverse(); // chronological order

        context_parts.extend(selected);

        if !history.is_empty() {
            sections.push("history".to_string());
        }

        Ok(ContextPayload {
            text: context_parts.join("\n\n"),
            tokens_used,
            tokens_budget: budget,
            sections_included: sections,
        })
    }
}

// ── Conversation compaction ──────────────────────────────────

impl SochStore {
    /// Load stored cold-tier summaries for a session (chronological order).
    pub async fn load_summaries(
        &self,
        key: &SessionKey,
    ) -> Result<Vec<String>, StorageError> {
        let prefix = format!("sessions/{}/summaries/", key.as_str());
        let results = self
            .scan(&prefix)?;

        let mut summaries = Vec::new();
        for (_k, v) in &results {
            if let Ok(text) = std::str::from_utf8(v) {
                summaries.push(text.to_string());
            }
        }
        Ok(summaries)
    }

    /// Count the total number of messages in a session.
    pub async fn message_count(&self, key: &SessionKey) -> Result<usize, StorageError> {
        let prefix = format!("sessions/{}/messages/", key.as_str());
        let results = self
            .scan(&prefix)?;
        Ok(results.len())
    }

    /// Compact old messages into a summary.
    ///
    /// If the session has more than `HOT_TIER_SIZE` messages, the oldest
    /// `count - HOT_TIER_SIZE` messages are concatenated into a summary
    /// string. The caller can provide a custom `summarizer` function to
    /// produce an LLM-driven summary; if `None`, a simple concatenation
    /// is used.
    ///
    /// Returns the number of messages compacted (0 if below threshold).
    pub async fn compact_session(
        &self,
        key: &SessionKey,
        summarizer: Option<&dyn Fn(&str) -> String>,
    ) -> Result<usize, StorageError> {
        self.compact_session_with_limit(key, summarizer, HOT_TIER_SIZE).await
    }

    /// GAP-3: Compact with a per-channel history limit.
    ///
    /// Like `compact_session`, but allows the caller to override the hot-tier
    /// size. This is used by channels that need smaller context windows
    /// (e.g., SMS with 50 messages, Telegram with 100).
    pub async fn compact_session_with_limit(
        &self,
        key: &SessionKey,
        summarizer: Option<&dyn Fn(&str) -> String>,
        hot_tier_size: usize,
    ) -> Result<usize, StorageError> {
        let prefix = format!("sessions/{}/messages/", key.as_str());
        let results = self
            .scan(&prefix)?;

        let total = results.len();
        if total <= hot_tier_size {
            return Ok(0);
        }

        let cold_count = total - hot_tier_size;
        let cold_entries = &results[..cold_count];

        // Build summary text from cold-tier messages.
        let mut cold_text = String::new();
        for (_k, v) in cold_entries {
            if let Ok(msg) = serde_json::from_slice::<AgentMessage>(v) {
                if !cold_text.is_empty() {
                    cold_text.push('\n');
                }
                cold_text.push_str(&format!("{:?}: {}", msg.role, msg.content));
            }
        }

        let summary = match summarizer {
            Some(f) => f(&cold_text),
            None => {
                // Simple truncation-based summary (no LLM).
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

        // Store the summary.
        let ts = chrono::Utc::now().timestamp_millis();
        let summary_key = format!("sessions/{}/summaries/{}", key.as_str(), ts);
        self.put(&summary_key, summary.as_bytes())?;

        // Delete compacted messages.
        for (k, _v) in cold_entries {
            let _ = self.delete(k);
        }

        debug!(
            %key,
            cold_count,
            remaining = hot_tier_size,
            "conversation compacted"
        );
        Ok(cold_count)
    }
}
