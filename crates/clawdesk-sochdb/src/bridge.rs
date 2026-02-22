//! Bridge layer — `SochConn` newtype implementing `sochdb::ConnectionTrait`.
//!
//! All advanced SochDB modules (`SemanticCache`, `TraceStore`, `GraphOverlay`,
//! `PolicyEngine`, `AtomicMemoryWriter`, etc.) are generic over `C: ConnectionTrait`.
//! `SochConn` wraps `Arc<SochStore>` to satisfy this bound while sharing the
//! same underlying `EmbeddedConnection` across all modules.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use clawdesk_sochdb::{SochStore, SochConn};
//! use sochdb::semantic_cache::SemanticCache;
//!
//! let store = Arc::new(SochStore::open("./data")?);
//! let conn = SochConn::new(store.clone());
//! let cache = SemanticCache::new(conn);
//! ```

use crate::SochStore;
use sochdb::ConnectionTrait;
use std::sync::Arc;

/// Cloneable connection wrapper for SochDB advanced modules.
///
/// Implements `sochdb::ConnectionTrait` by delegating to the underlying
/// `SochStore::connection()` (= `EmbeddedConnection`). Safe to clone — all
/// clones share the same ACID database via `Arc`.
///
/// Note: `ConnectionTrait` uses `&[u8]` keys, so we convert to `&str` since
/// all ClawDesk keys are valid UTF-8 strings.
#[derive(Clone, Debug)]
pub struct SochConn(Arc<SochStore>);

impl SochConn {
    /// Create a new bridge connection from an existing store.
    pub fn new(store: Arc<SochStore>) -> Self {
        Self(store)
    }

    /// Get a reference to the underlying store.
    pub fn store(&self) -> &SochStore {
        &self.0
    }
}

impl ConnectionTrait for SochConn {
    fn put(&self, key: &[u8], value: &[u8]) -> sochdb::error::Result<()> {
        let path = std::str::from_utf8(key)
            .map_err(|e| sochdb::error::ClientError::Storage(format!("Invalid UTF-8 key: {e}")))?;
        self.0.put(path, value)
            .map_err(|e| sochdb::error::ClientError::Storage(format!("bridge put: {e}")))
    }

    fn get(&self, key: &[u8]) -> sochdb::error::Result<Option<Vec<u8>>> {
        let path = std::str::from_utf8(key)
            .map_err(|e| sochdb::error::ClientError::Storage(format!("Invalid UTF-8 key: {e}")))?;
        self.0.get(path)
            .map_err(|e| sochdb::error::ClientError::Storage(format!("bridge get: {e}")))
    }

    fn delete(&self, key: &[u8]) -> sochdb::error::Result<()> {
        let path = std::str::from_utf8(key)
            .map_err(|e| sochdb::error::ClientError::Storage(format!("Invalid UTF-8 key: {e}")))?;
        self.0.delete(path)
            .map_err(|e| sochdb::error::ClientError::Storage(format!("bridge delete: {e}")))
    }

    fn scan(&self, prefix: &[u8]) -> sochdb::error::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let path = std::str::from_utf8(prefix)
            .map_err(|e| sochdb::error::ClientError::Storage(format!("Invalid UTF-8 prefix: {e}")))?;
        // SochStore::scan returns Vec<(String, Vec<u8>)>,
        // but ConnectionTrait expects Vec<(Vec<u8>, Vec<u8>)>.
        let results = self.0.scan(path)
            .map_err(|e| sochdb::error::ClientError::Storage(format!("bridge scan: {e}")))?;
        Ok(results.into_iter().map(|(k, v)| (k.into_bytes(), v)).collect())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Convenience type aliases — hide the generic parameter for downstream use
// ═══════════════════════════════════════════════════════════════════════════

/// Semantic cache backed by SochDB — caches LLM responses, avoids redundant API calls.
pub type SochSemanticCache = sochdb::semantic_cache::SemanticCache<SochConn>;

/// OpenTelemetry-compatible trace store for agent observability.
pub type SochTraceStore = sochdb::trace::TraceStore<SochConn>;

/// Durable workflow checkpoint store — resume multi-step agent tasks after crash.
pub type SochCheckpointStore = sochdb::checkpoint::DefaultCheckpointStore<SochConn>;

/// Lightweight knowledge graph overlay on KV storage.
pub type SochGraphOverlay = sochdb::graph::GraphOverlay<SochConn>;

/// Temporal graph with time-bounded edges and point-in-time queries.
pub type SochTemporalGraph = sochdb::temporal_graph::TemporalGraphOverlay<SochConn>;

/// Policy engine for access control, rate limiting, PII redaction.
pub type SochPolicyEngine = sochdb::policy::PolicyEngine<SochConn>;

/// Atomic all-or-nothing writes across KV + vector + graph indexes.
pub type SochAtomicWriter = sochdb::atomic_memory::AtomicMemoryWriter<SochConn>;

/// Agent capability registry for multi-agent routing.
pub type SochAgentRegistry = sochdb::routing::AgentRegistry<SochConn>;

/// Tool router — dispatches tool calls to capable agents.
pub type SochToolRouter = sochdb::routing::ToolRouter<SochConn>;
