//! Bridge layer — `SochConn` newtype implementing `sochdb::ConnectionTrait`.
//!
//! All advanced SochDB modules (`SemanticCache`, `TraceStore`, `GraphOverlay`,
//! `PolicyEngine`, `AtomicMemoryWriter`, etc.) are generic over `C: ConnectionTrait`.
//! `SochConn` wraps `Arc<SochStore>` to satisfy this bound while sharing the
//! same underlying `Database` (DurableConnection) across all modules.
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
/// `SochStore::db()` (= `DurableConnection`). Safe to clone — all clones
/// share the same ACID database via `Arc`.
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
        self.0.db().put(key, value)
    }

    fn get(&self, key: &[u8]) -> sochdb::error::Result<Option<Vec<u8>>> {
        self.0.db().get(key)
    }

    fn delete(&self, key: &[u8]) -> sochdb::error::Result<()> {
        self.0.db().delete(key)
    }

    fn scan(&self, prefix: &[u8]) -> sochdb::error::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.0.db().scan(prefix)
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
