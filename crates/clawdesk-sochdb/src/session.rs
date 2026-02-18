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

        match self.db.get(path.as_bytes()) {
            Ok(Some(bytes)) => {
                let session: Session =
                    serde_json::from_slice(&bytes).map_err(|e| StorageError::SerializationFailed {
                        detail: e.to_string(),
                    })?;
                Ok(Some(session))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(StorageError::OpenFailed {
                detail: e.to_string(),
            }),
        }
    }

    async fn save_session(&self, key: &SessionKey, session: &Session) -> Result<(), StorageError> {
        let path = format!("sessions/{}/state", key.as_str());
        let bytes = serde_json::to_vec(session).map_err(|e| StorageError::SerializationFailed {
            detail: e.to_string(),
        })?;

        self.db
            .put(path.as_bytes(), &bytes)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        debug!(%path, "session saved");
        Ok(())
    }

    async fn list_sessions(
        &self,
        _filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, StorageError> {
        // Scan all session keys — only process `/state` entries to avoid
        // deserializing messages, summaries, and other session sub-keys.
        let results = self
            .db
            .scan(b"sessions/")
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        let mut summaries = Vec::new();
        for (key, value) in &results {
            // Only process session state entries (sessions/{id}/state).
            let key_str = String::from_utf8_lossy(key);
            if !key_str.ends_with("/state") {
                continue;
            }
            if let Ok(session) = serde_json::from_slice::<Session>(value) {
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

        Ok(summaries)
    }

    async fn delete_session(&self, key: &SessionKey) -> Result<bool, StorageError> {
        let path = format!("sessions/{}/state", key.as_str());
        match self.db.delete(path.as_bytes()) {
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
