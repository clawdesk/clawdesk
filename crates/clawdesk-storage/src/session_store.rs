//! Session storage trait — CRUD operations on session state.

use async_trait::async_trait;
use clawdesk_types::{
    error::StorageError,
    session::{Session, SessionFilter, SessionKey, SessionSummary},
};

/// Port: session storage operations.
///
/// Implementations must provide ACID guarantees:
/// - **Atomicity**: Session writes are all-or-nothing
/// - **Consistency**: Invariants checked at commit time
/// - **Isolation**: MVCC + SSI for concurrent access
/// - **Durability**: WAL-backed persistence
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    /// Load a session by key. Returns None if not found.
    async fn load_session(
        &self,
        key: &SessionKey,
    ) -> Result<Option<Session>, StorageError>;

    /// Save (upsert) a session. Uses SSI conflict detection.
    async fn save_session(
        &self,
        key: &SessionKey,
        session: &Session,
    ) -> Result<(), StorageError>;

    /// List sessions matching the given filter.
    async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, StorageError>;

    /// Delete a session. Returns true if the session existed.
    async fn delete_session(&self, key: &SessionKey) -> Result<bool, StorageError>;

    /// Update a session transactionally with automatic retry on conflict.
    ///
    /// The `updater` closure receives a mutable reference to the session.
    /// If two concurrent updates conflict, one is retried (up to `max_retries`).
    async fn update_session<F>(
        &self,
        key: &SessionKey,
        updater: F,
    ) -> Result<Session, StorageError>
    where
        F: FnOnce(&mut Session) -> Result<(), StorageError> + Send + 'static;
}
