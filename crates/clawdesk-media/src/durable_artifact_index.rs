//! # Durable Artifact Index — Persistent artifact metadata backed by SochDB.
//!
//! Replaces the in-memory `RwLock<HashMap<ArtifactId, ArtifactRef>>` in
//! `ArtifactPipeline` with a durable, queryable index. Binary content
//! remains in `MediaCache` (content-addressed filesystem); metadata gains
//! restart-safe persistence with searchable provenance.
//!
//! ## Key Layout
//!
//! ```text
//! artifacts/meta/{artifact_id}                → ArtifactMeta JSON
//! artifacts/by_session/{session_id}/{id}      → artifact_id (index)
//! artifacts/by_tag/{tag}/{id}                 → artifact_id (index)
//! ```
//!
//! ## Deduplication
//!
//! Content-addressed hash-based dedup is preserved: the content hash acts
//! as the cache key in MediaCache. The durable index tracks metadata
//! separately, enabling multiple `ArtifactMeta` records to reference the
//! same content hash (different names/tags pointing to identical bytes).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ═══════════════════════════════════════════════════════════════════════════
// Durable artifact metadata
// ═══════════════════════════════════════════════════════════════════════════

/// Persistent artifact metadata that survives process restarts.
///
/// This replaces the in-memory `ArtifactRef` for metadata tracking while
/// preserving the same fields plus lineage-relevant additions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactMeta {
    /// Unique artifact identifier.
    pub id: String,
    /// Human-readable name (e.g., "report.pdf", "screenshot.png").
    pub name: String,
    /// MIME type (e.g., "application/pdf", "image/png").
    pub mime_type: String,
    /// Size of the binary content in bytes.
    pub size_bytes: u64,
    /// Content-addressed hash (SHA-256) for dedup with MediaCache.
    pub content_hash: String,
    /// When the artifact was created.
    pub created_at: DateTime<Utc>,
    /// Session that owns this artifact.
    pub owner_session: Option<String>,
    /// Searchable tags.
    pub tags: Vec<String>,
    /// Channels that have accessed this artifact (cross-channel tracking).
    pub accessed_from: HashSet<String>,
    /// TTL in seconds (None = no expiry).
    pub ttl_secs: Option<u64>,
    /// Lineage node ID (ties to execution lineage graph).
    pub lineage_node_id: Option<String>,
    /// Source agent that produced this artifact.
    pub source_agent: Option<String>,
    /// Source tool that produced this artifact.
    pub source_tool: Option<String>,
}

impl ArtifactMeta {
    /// Whether this artifact has expired based on its TTL.
    pub fn is_expired(&self) -> bool {
        if let Some(ttl) = self.ttl_secs {
            let expires_at = self.created_at + chrono::Duration::seconds(ttl as i64);
            Utc::now() > expires_at
        } else {
            false
        }
    }

    /// Record an access from a channel.
    pub fn record_access(&mut self, channel: &str) {
        self.accessed_from.insert(channel.to_string());
    }

    /// Whether this artifact has been accessed from multiple channels.
    pub fn is_cross_channel(&self) -> bool {
        self.accessed_from.len() > 1
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Artifact store trait — pluggable persistence backend
// ═══════════════════════════════════════════════════════════════════════════

/// Trait for durable artifact metadata storage.
#[async_trait::async_trait]
pub trait ArtifactMetaStore: Send + Sync + 'static {
    /// Store artifact metadata.
    async fn put(&self, meta: &ArtifactMeta) -> Result<(), ArtifactStoreError>;

    /// Retrieve artifact metadata by ID.
    async fn get(&self, artifact_id: &str) -> Result<Option<ArtifactMeta>, ArtifactStoreError>;

    /// Delete artifact metadata.
    async fn delete(&self, artifact_id: &str) -> Result<(), ArtifactStoreError>;

    /// List artifacts by owning session.
    async fn list_by_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<ArtifactMeta>, ArtifactStoreError>;

    /// List artifacts by tag.
    async fn list_by_tag(&self, tag: &str) -> Result<Vec<ArtifactMeta>, ArtifactStoreError>;

    /// List artifacts created within a time range.
    async fn list_by_time_range(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, ArtifactStoreError>;

    /// Remove expired artifacts. Returns count removed.
    async fn evict_expired(&self) -> Result<usize, ArtifactStoreError>;

    /// Total artifact count for monitoring.
    async fn count(&self) -> Result<usize, ArtifactStoreError>;

    /// Record a channel access for cross-channel tracking.
    async fn record_access(
        &self,
        artifact_id: &str,
        channel: &str,
    ) -> Result<(), ArtifactStoreError>;
}

/// Artifact store error.
#[derive(Debug)]
pub enum ArtifactStoreError {
    Storage(String),
    NotFound(String),
    Serialization(String),
}

impl std::fmt::Display for ArtifactStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(e) => write!(f, "artifact store error: {}", e),
            Self::NotFound(id) => write!(f, "artifact not found: {}", id),
            Self::Serialization(e) => write!(f, "serialization error: {}", e),
        }
    }
}

impl std::error::Error for ArtifactStoreError {}

// ═══════════════════════════════════════════════════════════════════════════
// In-memory implementation (for testing)
// ═══════════════════════════════════════════════════════════════════════════

/// In-memory artifact metadata store for testing.
pub struct InMemoryArtifactMetaStore {
    entries: tokio::sync::RwLock<std::collections::HashMap<String, ArtifactMeta>>,
}

impl InMemoryArtifactMetaStore {
    pub fn new() -> Self {
        Self {
            entries: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }
}

impl Default for InMemoryArtifactMetaStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ArtifactMetaStore for InMemoryArtifactMetaStore {
    async fn put(&self, meta: &ArtifactMeta) -> Result<(), ArtifactStoreError> {
        self.entries
            .write()
            .await
            .insert(meta.id.clone(), meta.clone());
        Ok(())
    }

    async fn get(&self, artifact_id: &str) -> Result<Option<ArtifactMeta>, ArtifactStoreError> {
        Ok(self.entries.read().await.get(artifact_id).cloned())
    }

    async fn delete(&self, artifact_id: &str) -> Result<(), ArtifactStoreError> {
        self.entries.write().await.remove(artifact_id);
        Ok(())
    }

    async fn list_by_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<ArtifactMeta>, ArtifactStoreError> {
        Ok(self
            .entries
            .read()
            .await
            .values()
            .filter(|m| m.owner_session.as_deref() == Some(session_id))
            .cloned()
            .collect())
    }

    async fn list_by_tag(&self, tag: &str) -> Result<Vec<ArtifactMeta>, ArtifactStoreError> {
        Ok(self
            .entries
            .read()
            .await
            .values()
            .filter(|m| m.tags.iter().any(|t| t == tag))
            .cloned()
            .collect())
    }

    async fn list_by_time_range(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, ArtifactStoreError> {
        Ok(self
            .entries
            .read()
            .await
            .values()
            .filter(|m| m.created_at >= from && m.created_at <= to)
            .cloned()
            .collect())
    }

    async fn evict_expired(&self) -> Result<usize, ArtifactStoreError> {
        let mut entries = self.entries.write().await;
        let before = entries.len();
        entries.retain(|_, m| !m.is_expired());
        Ok(before - entries.len())
    }

    async fn count(&self) -> Result<usize, ArtifactStoreError> {
        Ok(self.entries.read().await.len())
    }

    async fn record_access(
        &self,
        artifact_id: &str,
        channel: &str,
    ) -> Result<(), ArtifactStoreError> {
        let mut entries = self.entries.write().await;
        if let Some(meta) = entries.get_mut(artifact_id) {
            meta.record_access(channel);
            Ok(())
        } else {
            Err(ArtifactStoreError::NotFound(artifact_id.to_string()))
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// SochDB key helpers
// ═══════════════════════════════════════════════════════════════════════════

/// SochDB key for artifact metadata.
pub fn artifact_meta_key(artifact_id: &str) -> String {
    format!("artifacts/meta/{}", artifact_id)
}

/// SochDB key prefix for session index.
pub fn artifact_session_prefix(session_id: &str) -> String {
    format!("artifacts/by_session/{}/", session_id)
}

/// SochDB key prefix for tag index.
pub fn artifact_tag_prefix(tag: &str) -> String {
    format!("artifacts/by_tag/{}/", tag)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn make_meta(id: &str, session: &str) -> ArtifactMeta {
        ArtifactMeta {
            id: id.to_string(),
            name: format!("{}.txt", id),
            mime_type: "text/plain".into(),
            size_bytes: 1024,
            content_hash: format!("sha256:{}", id),
            created_at: Utc::now(),
            owner_session: Some(session.to_string()),
            tags: vec!["test".into()],
            accessed_from: HashSet::new(),
            ttl_secs: None,
            lineage_node_id: None,
            source_agent: None,
            source_tool: None,
        }
    }

    #[tokio::test]
    async fn test_put_and_get() {
        let store = InMemoryArtifactMetaStore::new();
        let meta = make_meta("a1", "s1");

        store.put(&meta).await.unwrap();
        let got = store.get("a1").await.unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().name, "a1.txt");
    }

    #[tokio::test]
    async fn test_list_by_session() {
        let store = InMemoryArtifactMetaStore::new();
        store.put(&make_meta("a1", "s1")).await.unwrap();
        store.put(&make_meta("a2", "s1")).await.unwrap();
        store.put(&make_meta("a3", "s2")).await.unwrap();

        let s1 = store.list_by_session("s1").await.unwrap();
        assert_eq!(s1.len(), 2);
    }

    #[tokio::test]
    async fn test_list_by_tag() {
        let store = InMemoryArtifactMetaStore::new();
        store.put(&make_meta("a1", "s1")).await.unwrap();

        let tagged = store.list_by_tag("test").await.unwrap();
        assert_eq!(tagged.len(), 1);

        let untagged = store.list_by_tag("nonexistent").await.unwrap();
        assert!(untagged.is_empty());
    }

    #[tokio::test]
    async fn test_cross_channel_tracking() {
        let store = InMemoryArtifactMetaStore::new();
        store.put(&make_meta("a1", "s1")).await.unwrap();

        store.record_access("a1", "telegram").await.unwrap();
        store.record_access("a1", "discord").await.unwrap();

        let meta = store.get("a1").await.unwrap().unwrap();
        assert!(meta.is_cross_channel());
        assert_eq!(meta.accessed_from.len(), 2);
    }

    #[tokio::test]
    async fn test_evict_expired() {
        let store = InMemoryArtifactMetaStore::new();

        let mut expired = make_meta("a1", "s1");
        expired.ttl_secs = Some(1);
        expired.created_at = Utc::now() - chrono::Duration::seconds(10);

        let mut fresh = make_meta("a2", "s1");
        fresh.ttl_secs = Some(3600);

        store.put(&expired).await.unwrap();
        store.put(&fresh).await.unwrap();

        let removed = store.evict_expired().await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(store.count().await.unwrap(), 1);
    }

    #[test]
    fn test_sochdb_keys() {
        assert_eq!(artifact_meta_key("abc"), "artifacts/meta/abc");
        assert_eq!(artifact_session_prefix("s1"), "artifacts/by_session/s1/");
        assert_eq!(artifact_tag_prefix("report"), "artifacts/by_tag/report/");
    }
}
