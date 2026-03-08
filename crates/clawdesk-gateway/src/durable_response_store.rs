//! # Durable Response Store — Persistent response objects keyed by lineage ID.
//!
//! Replaces the in-memory `BoundedResponseStore` with a durable, append-only
//! log backed by SochDB. Response objects are keyed by lineage ID, enabling
//! correlation with sessions, artifacts, bus events, and announce deliveries.
//!
//! ## Key Layout
//!
//! ```text
//! responses/{response_id}           → ResponseRecord JSON
//! responses/by_session/{session_id}/{response_id} → (index)
//! responses/by_lineage/{lineage_id}/{response_id} → (index)
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A durable response record that can be correlated with the lineage graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRecord {
    /// Response object ID (matches OpenAI-compatible responses API).
    pub id: String,
    /// Session that owns this response.
    pub session_id: String,
    /// Agent that produced this response.
    pub agent_id: String,
    /// Lineage run ID for provenance tracking.
    pub lineage_run_id: Option<String>,
    /// Model that generated the response.
    pub model: String,
    /// The response content.
    pub content: String,
    /// Status: created, in_progress, completed, failed.
    pub status: String,
    /// When the response was created.
    pub created_at: DateTime<Utc>,
    /// When the response was completed.
    pub completed_at: Option<DateTime<Utc>>,
    /// Token usage.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Tool rounds used.
    pub total_rounds: usize,
    /// Associated artifact IDs.
    pub artifact_ids: Vec<String>,
    /// Custom metadata.
    pub metadata: serde_json::Value,
}

/// Trait for durable response storage.
#[async_trait::async_trait]
pub trait ResponseRecordStore: Send + Sync + 'static {
    /// Store a response record.
    async fn put(&self, record: &ResponseRecord) -> Result<(), ResponseStoreError>;

    /// Retrieve a response record by ID.
    async fn get(&self, response_id: &str) -> Result<Option<ResponseRecord>, ResponseStoreError>;

    /// List responses for a session.
    async fn list_by_session(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<ResponseRecord>, ResponseStoreError>;

    /// List responses for a lineage run.
    async fn list_by_lineage(
        &self,
        lineage_run_id: &str,
    ) -> Result<Vec<ResponseRecord>, ResponseStoreError>;
}

/// Response store error.
#[derive(Debug)]
pub enum ResponseStoreError {
    Storage(String),
    NotFound(String),
    Serialization(String),
}

impl std::fmt::Display for ResponseStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(e) => write!(f, "response store error: {}", e),
            Self::NotFound(id) => write!(f, "response not found: {}", id),
            Self::Serialization(e) => write!(f, "serialization error: {}", e),
        }
    }
}

impl std::error::Error for ResponseStoreError {}

/// In-memory durable response store for testing.
pub struct InMemoryResponseRecordStore {
    records: tokio::sync::RwLock<std::collections::HashMap<String, ResponseRecord>>,
}

impl InMemoryResponseRecordStore {
    pub fn new() -> Self {
        Self {
            records: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }
}

impl Default for InMemoryResponseRecordStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ResponseRecordStore for InMemoryResponseRecordStore {
    async fn put(&self, record: &ResponseRecord) -> Result<(), ResponseStoreError> {
        self.records
            .write()
            .await
            .insert(record.id.clone(), record.clone());
        Ok(())
    }

    async fn get(&self, response_id: &str) -> Result<Option<ResponseRecord>, ResponseStoreError> {
        Ok(self.records.read().await.get(response_id).cloned())
    }

    async fn list_by_session(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<ResponseRecord>, ResponseStoreError> {
        let mut results: Vec<ResponseRecord> = self
            .records
            .read()
            .await
            .values()
            .filter(|r| r.session_id == session_id)
            .cloned()
            .collect();
        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        results.truncate(limit);
        Ok(results)
    }

    async fn list_by_lineage(
        &self,
        lineage_run_id: &str,
    ) -> Result<Vec<ResponseRecord>, ResponseStoreError> {
        Ok(self
            .records
            .read()
            .await
            .values()
            .filter(|r| r.lineage_run_id.as_deref() == Some(lineage_run_id))
            .cloned()
            .collect())
    }
}

/// SochDB key for a response record.
pub fn response_key(response_id: &str) -> String {
    format!("responses/{}", response_id)
}

/// SochDB key prefix for session index.
pub fn response_session_prefix(session_id: &str) -> String {
    format!("responses/by_session/{}/", session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(id: &str, session: &str) -> ResponseRecord {
        ResponseRecord {
            id: id.to_string(),
            session_id: session.to_string(),
            agent_id: "agent-1".into(),
            lineage_run_id: Some("run-1".into()),
            model: "claude-sonnet-4-20250514".into(),
            content: "Hello world".into(),
            status: "completed".into(),
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
            input_tokens: 100,
            output_tokens: 50,
            total_rounds: 1,
            artifact_ids: Vec::new(),
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn test_put_and_get() {
        let store = InMemoryResponseRecordStore::new();
        store.put(&make_record("r1", "s1")).await.unwrap();

        let got = store.get("r1").await.unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().content, "Hello world");
    }

    #[tokio::test]
    async fn test_list_by_session() {
        let store = InMemoryResponseRecordStore::new();
        store.put(&make_record("r1", "s1")).await.unwrap();
        store.put(&make_record("r2", "s1")).await.unwrap();
        store.put(&make_record("r3", "s2")).await.unwrap();

        let s1 = store.list_by_session("s1", 10).await.unwrap();
        assert_eq!(s1.len(), 2);
    }

    #[tokio::test]
    async fn test_list_by_lineage() {
        let store = InMemoryResponseRecordStore::new();
        store.put(&make_record("r1", "s1")).await.unwrap();

        let lineage = store.list_by_lineage("run-1").await.unwrap();
        assert_eq!(lineage.len(), 1);
    }
}
