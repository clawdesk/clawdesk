//! Memory Federation — unified query layer across SochDB and legacy stores.
//!
//! ## Memory Federation Layer
//!
//! ClawDesk uses SochDB (embedded ACID KV + HNSW vector search), while
//! The legacy system uses separate SQLite files. This module provides a **federated
//! query interface** that transparently routes memory operations to the
//! correct backend based on the data's source.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │              MemoryFederation                     │
//! │  ┌─────────────┐  ┌──────────────┐  ┌──────────┐ │
//! │  │ SochDB      │  │ Legacy       │  │  Policy  │ │
//! │  │ (native)    │  │ (remote)     │  │  Router  │ │
//! │  └──────┬──────┘  └──────┬───────┘  └────┬─────┘ │
//! │         │                │               │       │
//! │  ┌──────▼────────────────▼───────────────▼─────┐ │
//! │  │          Federated Query Engine              │ │
//! │  │  • scatter-gather across backends            │ │
//! │  │  • dedup by content hash                     │ │
//! │  │  • rank-merge by relevance score             │ │
//! │  └─────────────────────────────────────────────┘ │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! ## Query routing
//!
//! Each memory item has a `MemorySource` tag. Queries can be scoped to
//! a specific source or federated across all backends.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

/// Source backend for a memory item.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    /// Native SochDB (ClawDesk's embedded store).
    SochDb,
    /// the backend (SQLite or remote API).
    OpenClaw,
    /// External source (e.g., imported data).
    External(String),
}

/// A single memory item (conversation turn, knowledge snippet, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    /// Unique item ID.
    pub id: String,
    /// Content text.
    pub content: String,
    /// Source backend that owns this item.
    pub source: MemorySource,
    /// Session or conversation ID this item belongs to.
    pub session_id: Option<String>,
    /// Relevance score (for query results), in [0.0, 1.0].
    pub relevance: f64,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Content hash for deduplication (FNV-1a 64-bit).
    pub content_hash: u64,
    /// Optional metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl MemoryItem {
    /// Create a new memory item with auto-computed content hash.
    pub fn new(
        id: impl Into<String>,
        content: impl Into<String>,
        source: MemorySource,
    ) -> Self {
        let content = content.into();
        let content_hash = fnv1a_hash(content.as_bytes());
        Self {
            id: id.into(),
            content,
            source,
            session_id: None,
            relevance: 0.0,
            created_at: Utc::now(),
            content_hash,
            metadata: serde_json::Value::Null,
        }
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_relevance(mut self, relevance: f64) -> Self {
        self.relevance = relevance;
        self
    }
}

/// Query parameters for federated memory search.
#[derive(Debug, Clone)]
pub struct MemoryQuery {
    /// Text to search for (semantic or keyword).
    pub query: String,
    /// Maximum number of results.
    pub limit: usize,
    /// Optional session scope.
    pub session_id: Option<String>,
    /// Which backends to query (None = all).
    pub sources: Option<Vec<MemorySource>>,
    /// Minimum relevance threshold.
    pub min_relevance: f64,
    /// Whether to deduplicate results by content hash.
    pub dedup: bool,
}

impl Default for MemoryQuery {
    fn default() -> Self {
        Self {
            query: String::new(),
            limit: 20,
            session_id: None,
            sources: None,
            min_relevance: 0.0,
            dedup: true,
        }
    }
}

impl MemoryQuery {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            ..Default::default()
        }
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn sources(mut self, sources: Vec<MemorySource>) -> Self {
        self.sources = Some(sources);
        self
    }

    pub fn min_relevance(mut self, threshold: f64) -> Self {
        self.min_relevance = threshold;
        self
    }
}

/// Result of a federated memory query.
#[derive(Debug, Clone)]
pub struct FederatedResult {
    /// Merged and ranked results.
    pub items: Vec<MemoryItem>,
    /// Per-source result counts (before dedup/filtering).
    pub source_counts: HashMap<MemorySource, usize>,
    /// Number of items deduplicated.
    pub dedup_count: usize,
    /// Total query time across all backends (ms).
    pub total_query_ms: u64,
}

// ═══════════════════════════════════════════════════════════════════════════
// Backend trait
// ═══════════════════════════════════════════════════════════════════════════

/// A memory backend that can be queried for items.
#[async_trait]
pub trait MemoryBackend: Send + Sync + 'static {
    /// Source identifier for this backend.
    fn source(&self) -> MemorySource;

    /// Search for memory items matching the query.
    async fn search(&self, query: &MemoryQuery) -> Result<Vec<MemoryItem>, MemoryError>;

    /// Store a memory item.
    async fn store(&self, item: &MemoryItem) -> Result<(), MemoryError>;

    /// Delete a memory item by ID.
    async fn delete(&self, id: &str) -> Result<bool, MemoryError>;
}

/// Memory federation error.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("backend '{backend:?}' failed: {detail}")]
    BackendFailed {
        backend: MemorySource,
        detail: String,
    },
    #[error("query timeout")]
    Timeout,
    #[error("no backends registered")]
    NoBackends,
}

// ═══════════════════════════════════════════════════════════════════════════
// Federation engine
// ═══════════════════════════════════════════════════════════════════════════

/// Federated memory query engine.
///
/// Scatter-gathers queries across registered backends, deduplicates
/// results by content hash, and rank-merges by relevance score.
pub struct MemoryFederation {
    backends: Vec<Arc<dyn MemoryBackend>>,
}

impl MemoryFederation {
    pub fn new() -> Self {
        Self {
            backends: Vec::new(),
        }
    }

    /// Register a memory backend.
    pub fn add_backend(&mut self, backend: Arc<dyn MemoryBackend>) {
        self.backends.push(backend);
    }

    /// Federated search across all (or scoped) backends.
    ///
    /// ## Algorithm
    /// 1. Scatter query to each eligible backend concurrently.
    /// 2. Collect results (tolerating individual backend failures).
    /// 3. Deduplicate by content hash.
    /// 4. Sort by relevance descending.
    /// 5. Truncate to limit.
    pub async fn search(&self, query: &MemoryQuery) -> Result<FederatedResult, MemoryError> {
        if self.backends.is_empty() {
            return Err(MemoryError::NoBackends);
        }

        // Filter to requested sources
        let eligible: Vec<&Arc<dyn MemoryBackend>> = self
            .backends
            .iter()
            .filter(|b| {
                query
                    .sources
                    .as_ref()
                    .map_or(true, |sources| sources.contains(&b.source()))
            })
            .collect();

        if eligible.is_empty() {
            return Ok(FederatedResult {
                items: vec![],
                source_counts: HashMap::new(),
                dedup_count: 0,
                total_query_ms: 0,
            });
        }

        let start = std::time::Instant::now();

        // Scatter queries concurrently
        let mut handles = Vec::with_capacity(eligible.len());
        for backend in &eligible {
            let backend = Arc::clone(backend);
            let query = query.clone();
            handles.push(tokio::spawn(async move {
                let source = backend.source();
                match backend.search(&query).await {
                    Ok(items) => (source, Ok(items)),
                    Err(e) => (source, Err(e)),
                }
            }));
        }

        // Gather results
        let mut all_items: Vec<MemoryItem> = Vec::new();
        let mut source_counts: HashMap<MemorySource, usize> = HashMap::new();

        for handle in handles {
            match handle.await {
                Ok((source, Ok(items))) => {
                    source_counts.insert(source, items.len());
                    all_items.extend(items);
                }
                Ok((source, Err(e))) => {
                    warn!(source = ?source, error = %e, "memory backend query failed");
                    source_counts.insert(source, 0);
                }
                Err(e) => {
                    warn!(error = %e, "memory backend task panicked");
                }
            }
        }

        // Deduplicate by content hash
        let dedup_count = if query.dedup {
            let before = all_items.len();
            let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
            all_items.retain(|item| seen.insert(item.content_hash));
            before - all_items.len()
        } else {
            0
        };

        // Filter by minimum relevance
        if query.min_relevance > 0.0 {
            all_items.retain(|item| item.relevance >= query.min_relevance);
        }

        // Sort by relevance descending
        all_items.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Truncate to limit
        all_items.truncate(query.limit);

        let total_query_ms = start.elapsed().as_millis() as u64;
        debug!(
            results = all_items.len(),
            dedup = dedup_count,
            backends = eligible.len(),
            query_ms = total_query_ms,
            "federated search complete"
        );

        Ok(FederatedResult {
            items: all_items,
            source_counts,
            dedup_count,
            total_query_ms,
        })
    }

    /// Store a memory item to the appropriate backend.
    pub async fn store(&self, item: &MemoryItem) -> Result<(), MemoryError> {
        let backend = self
            .backends
            .iter()
            .find(|b| b.source() == item.source)
            .ok_or(MemoryError::BackendFailed {
                backend: item.source.clone(),
                detail: "no backend registered for source".into(),
            })?;

        backend.store(item).await
    }
}

impl Default for MemoryFederation {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// FNV-1a 64-bit hash for content deduplication.
fn fnv1a_hash(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory mock backend for testing.
    struct MockBackend {
        source: MemorySource,
        items: Mutex<Vec<MemoryItem>>,
    }

    impl MockBackend {
        fn new(source: MemorySource, items: Vec<MemoryItem>) -> Self {
            Self {
                source,
                items: Mutex::new(items),
            }
        }
    }

    #[async_trait]
    impl MemoryBackend for MockBackend {
        fn source(&self) -> MemorySource {
            self.source.clone()
        }

        async fn search(&self, query: &MemoryQuery) -> Result<Vec<MemoryItem>, MemoryError> {
            let items = self.items.lock().unwrap();
            let results: Vec<MemoryItem> = items
                .iter()
                .filter(|i| {
                    i.content.to_lowercase().contains(&query.query.to_lowercase())
                })
                .cloned()
                .collect();
            Ok(results)
        }

        async fn store(&self, item: &MemoryItem) -> Result<(), MemoryError> {
            self.items.lock().unwrap().push(item.clone());
            Ok(())
        }

        async fn delete(&self, id: &str) -> Result<bool, MemoryError> {
            let mut items = self.items.lock().unwrap();
            let before = items.len();
            items.retain(|i| i.id != id);
            Ok(items.len() < before)
        }
    }

    #[tokio::test]
    async fn federated_search_across_backends() {
        let mut fed = MemoryFederation::new();

        fed.add_backend(Arc::new(MockBackend::new(
            MemorySource::SochDb,
            vec![
                MemoryItem::new("s1", "Rust is great", MemorySource::SochDb)
                    .with_relevance(0.9),
            ],
        )));
        fed.add_backend(Arc::new(MockBackend::new(
            MemorySource::OpenClaw,
            vec![
                MemoryItem::new("o1", "Rust programming rocks", MemorySource::OpenClaw)
                    .with_relevance(0.8),
            ],
        )));

        let result = fed
            .search(&MemoryQuery::new("Rust").limit(10))
            .await
            .unwrap();

        assert_eq!(result.items.len(), 2);
        // Highest relevance first
        assert_eq!(result.items[0].id, "s1");
        assert_eq!(result.items[1].id, "o1");
    }

    #[tokio::test]
    async fn deduplication_by_content_hash() {
        let mut fed = MemoryFederation::new();

        let content = "duplicate content";
        fed.add_backend(Arc::new(MockBackend::new(
            MemorySource::SochDb,
            vec![MemoryItem::new("s1", content, MemorySource::SochDb).with_relevance(0.9)],
        )));
        fed.add_backend(Arc::new(MockBackend::new(
            MemorySource::OpenClaw,
            vec![MemoryItem::new("o1", content, MemorySource::OpenClaw).with_relevance(0.8)],
        )));

        let result = fed
            .search(&MemoryQuery::new("duplicate").limit(10))
            .await
            .unwrap();

        assert_eq!(result.items.len(), 1); // Deduplicated
        assert_eq!(result.dedup_count, 1);
    }

    #[tokio::test]
    async fn source_scoping() {
        let mut fed = MemoryFederation::new();

        fed.add_backend(Arc::new(MockBackend::new(
            MemorySource::SochDb,
            vec![MemoryItem::new("s1", "hello world", MemorySource::SochDb)],
        )));
        fed.add_backend(Arc::new(MockBackend::new(
            MemorySource::OpenClaw,
            vec![MemoryItem::new("o1", "hello world", MemorySource::OpenClaw)],
        )));

        // Query only SochDb
        let result = fed
            .search(
                &MemoryQuery::new("hello")
                    .sources(vec![MemorySource::SochDb])
                    .limit(10),
            )
            .await
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].source, MemorySource::SochDb);
    }

    #[tokio::test]
    async fn store_routes_to_correct_backend() {
        let mut fed = MemoryFederation::new();
        let sochdb = Arc::new(MockBackend::new(MemorySource::SochDb, vec![]));
        fed.add_backend(sochdb.clone());

        let item = MemoryItem::new("test-1", "new memory", MemorySource::SochDb);
        fed.store(&item).await.unwrap();

        let items = sochdb.items.lock().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "test-1");
    }

    #[tokio::test]
    async fn min_relevance_filter() {
        let mut fed = MemoryFederation::new();

        fed.add_backend(Arc::new(MockBackend::new(
            MemorySource::SochDb,
            vec![
                MemoryItem::new("s1", "data", MemorySource::SochDb).with_relevance(0.9),
                MemoryItem::new("s2", "data", MemorySource::SochDb).with_relevance(0.3),
            ],
        )));

        let result = fed
            .search(&MemoryQuery::new("data").min_relevance(0.5).limit(10))
            .await
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "s1");
    }
}
