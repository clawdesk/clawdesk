//! SochDB session store implementation.
//!
//! Uses SochDB's path API for O(1) session lookup and MVCC transactions
//! with SSI conflict detection for concurrent session updates.

use async_trait::async_trait;
use clawdesk_storage::SessionStore;
use clawdesk_types::{
    error::StorageError,
    session::{Session, SessionFilter, SessionKey, SessionSummary},
};
use tracing::{debug, warn};

use crate::SochStore;

#[async_trait]
impl SessionStore for SochStore {
    async fn load_session(&self, key: &SessionKey) -> Result<Option<Session>, StorageError> {
        let path = format!("sessions/{}/state", key.as_str());
        debug!(%path, "loading session");

        match self.get(&path) {
            Ok(Some(bytes)) => {
                let session: Session =
                    serde_json::from_slice(&bytes).map_err(|e| StorageError::SerializationFailed {
                        detail: e.to_string(),
                    })?;
                Ok(Some(session))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn save_session(&self, key: &SessionKey, session: &Session) -> Result<(), StorageError> {
        let path = format!("sessions/{}/state", key.as_str());
        let bytes = serde_json::to_vec(session).map_err(|e| StorageError::SerializationFailed {
            detail: e.to_string(),
        })?;

        // GAP-01: Use put_durable() to guarantee crash-safe persistence for
        // session state transitions. put() relies on group-commit batching
        // with a ~10ms window — a crash within that window loses the write.
        self.put_durable(&path, &bytes)?;

        debug!(%path, "session saved (durable)");
        Ok(())
    }

    async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, StorageError> {
        // GAP-06: Use secondary index for O(log N + k) session listing instead
        // of scanning all keys under sessions/ (which includes messages, summaries,
        // and other sub-keys — O(S×M) for S sessions with M messages each).
        //
        // Strategy:
        // 1. Try idx/sessions/by_activity/ (pre-sorted by timestamp, most recent last)
        // 2. Resolve each session_id → sessions/{id}/state (point lookup)
        // 3. Apply filter in memory (state filter, since filter)
        //
        // If the index is empty (not yet populated), fall back to full scan.

        let limit = filter.limit.unwrap_or(usize::MAX);

        // Choose index prefix based on filter
        let index_prefix = if let Some(ref ch) = filter.channel {
            format!("idx/sessions/by_channel/{}/", ch)
        } else {
            "idx/sessions/by_activity/".to_string()
        };

        let index_entries = self.scan(&index_prefix)?;

        if !index_entries.is_empty() {
            // Index path: resolve session IDs from index, then point-lookup state
            let mut summaries = Vec::new();
            // Iterate in reverse for most-recent-first ordering
            for (key, _) in index_entries.iter().rev() {
                if summaries.len() >= limit {
                    break;
                }
                // Extract session_id from: idx/sessions/by_*/{...}/{session_id}
                let session_id = match key.rsplit('/').next() {
                    Some(id) => id,
                    None => continue,
                };

                let state_key = format!("sessions/{}/state", session_id);
                if let Ok(Some(bytes)) = self.get(&state_key) {
                    if let Ok(session) = serde_json::from_slice::<Session>(&bytes) {
                        // Apply remaining filters
                        if let Some(ref state) = filter.state {
                            if &session.state != state {
                                continue;
                            }
                        }
                        if let Some(since) = filter.since {
                            if session.last_activity < since {
                                continue;
                            }
                        }
                        summaries.push(SessionSummary {
                            key: session.key.clone(),
                            channel: session.channel,
                            state: session.state,
                            last_activity: session.last_activity,
                            message_count: session.message_count,
                            model: session.model.clone(),
                        });
                    }
                }
            }
            return Ok(summaries);
        }

        // Fallback: full scan (for backwards compatibility when index not populated)
        debug!("Session index empty — falling back to full scan");
        let results = self.scan("sessions/")?;

        let mut summaries = Vec::new();
        for (key, value) in &results {
            if !key.ends_with("/state") {
                continue;
            }
            if let Ok(session) = serde_json::from_slice::<Session>(value) {
                // Apply filters
                if let Some(ref ch) = filter.channel {
                    if &session.channel != ch {
                        continue;
                    }
                }
                if let Some(ref state) = filter.state {
                    if &session.state != state {
                        continue;
                    }
                }
                if let Some(since) = filter.since {
                    if session.last_activity < since {
                        continue;
                    }
                }
                summaries.push(SessionSummary {
                    key: session.key.clone(),
                    channel: session.channel,
                    state: session.state,
                    last_activity: session.last_activity,
                    message_count: session.message_count,
                    model: session.model.clone(),
                });
            }
        }

        // Sort by last_activity descending for consistent ordering
        summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

        if let Some(limit) = filter.limit {
            summaries.truncate(limit);
        }

        Ok(summaries)
    }

    async fn delete_session(&self, key: &SessionKey) -> Result<bool, StorageError> {
        let path = format!("sessions/{}/state", key.as_str());
        match self.delete(&path) {
            Ok(()) => Ok(true),
            Err(e) => {
                warn!(%path, error = %e, "failed to delete session");
                Ok(false)
            }
        }
    }

    async fn update_session<F>(
        &self,
        key: &SessionKey,
        updater: F,
    ) -> Result<Session, StorageError>
    where
        F: FnOnce(&mut Session) -> Result<(), StorageError> + Send + 'static,
    {
        // Load current state
        let mut session = self
            .load_session(key)
            .await?
            .unwrap_or_else(|| Session::new(key.clone(), clawdesk_types::channel::ChannelId::Internal));

        // Apply mutation
        updater(&mut session)?;

        // Save — SochDB's SSI will detect conflicts
        self.save_session(key, &session).await?;
        Ok(session)
    }
}
