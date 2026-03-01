//! GAP-E: Artifact store trait — content-addressed artifact persistence.
//!
//! Defines the contract for storing and retrieving artifacts across channels.
//! Implementations live in adapter crates (e.g., SochDB-backed, filesystem-backed).

use async_trait::async_trait;
use clawdesk_types::artifact::{ArtifactId, ArtifactRef};

/// Errors from artifact store operations.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactStoreError {
    #[error("artifact not found: {0}")]
    NotFound(ArtifactId),
    #[error("storage I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(String),
    #[error("storage backend error: {0}")]
    Backend(String),
    #[error("artifact too large: {size} bytes (max {max})")]
    TooLarge { size: u64, max: u64 },
}

/// Content-addressed artifact storage.
///
/// Artifacts are stored by their content-addressed ID and can be queried
/// by session, tag, or time range. The store is append-mostly — artifacts
/// are created once and rarely updated (only metadata like tags/access tracking).
#[async_trait]
pub trait ArtifactStore: Send + Sync + 'static {
    /// Store an artifact, returning its content-addressed ID.
    ///
    /// If an artifact with the same content hash already exists,
    /// this is a no-op and returns the existing ID.
    async fn put(&self, artifact: &ArtifactRef, data: Option<&[u8]>) -> Result<ArtifactId, ArtifactStoreError>;

    /// Retrieve an artifact reference by ID.
    async fn get(&self, id: &str) -> Result<Option<ArtifactRef>, ArtifactStoreError>;

    /// Retrieve the raw data for an artifact (if it was stored inline or in the content store).
    async fn get_data(&self, id: &str) -> Result<Option<Vec<u8>>, ArtifactStoreError>;

    /// Delete an artifact by ID.
    async fn delete(&self, id: &str) -> Result<bool, ArtifactStoreError>;

    /// List all artifacts for a session, ordered by creation time (newest first).
    async fn list_by_session(&self, session_id: &str) -> Result<Vec<ArtifactRef>, ArtifactStoreError>;

    /// List all artifacts matching a tag.
    async fn list_by_tag(&self, tag: &str) -> Result<Vec<ArtifactRef>, ArtifactStoreError>;

    /// List artifacts created within a time range.
    async fn list_by_time_range(
        &self,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ArtifactRef>, ArtifactStoreError>;

    /// Get aggregate stats.
    async fn stats(&self) -> Result<ArtifactStoreStats, ArtifactStoreError>;

    /// Evict expired artifacts (TTL-based). Returns count of evicted artifacts.
    async fn evict_expired(&self) -> Result<usize, ArtifactStoreError>;
}

/// Aggregate statistics for the artifact store.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ArtifactStoreStats {
    pub total_artifacts: usize,
    pub total_size_bytes: u64,
    pub sessions_with_artifacts: usize,
    pub oldest_artifact_age_secs: Option<u64>,
}
