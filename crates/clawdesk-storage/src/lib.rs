//! # clawdesk-storage
//!
//! Storage port (trait definitions) for ClawDesk.
//!
//! This crate defines the **contracts** that any storage backend must implement.
//! It contains no implementations — those live in adapter crates like `clawdesk-sochdb`.
//!
//! The trait hierarchy follows the hexagonal architecture pattern:
//! - `SessionStore`: CRUD operations on session state
//! - `ConversationStore`: Append-only conversation history with vector search
//! - `ConfigStore`: Configuration storage with versioning and hot-reload
//! - `VectorStore`: Vector similarity search for memory/RAG
//! - `GraphStore`: Graph overlay for relationship tracking

pub mod config_store;
pub mod conversation_store;
pub mod graph_store;
pub mod memory_backend;
pub mod replay_store;
pub mod session_store;
pub mod vector_store;

pub use config_store::ConfigStore;
pub use conversation_store::ConversationStore;
pub use graph_store::GraphStore;
pub use memory_backend::MemoryBackend;
pub use memory_backend::{
    // Atomic write types
    AtomicWriteResult, MemoryWriteOp,
    // Batch write types (A7)
    BatchWriteResult,
    // Memory schema types (A4)
    Episode, EpisodeType, Event, EventRole, EventMetrics, Entity, EntityKind, EntityFacts,
    // Context query types (A1)
    ContextQueryResult, ContextSection, ContextFormat, TruncationStrategy,
    // Task queue types (A8)
    BackgroundTask, TaskClaimResult, TaskQueueStats,
    // Cost model types (A9)
    SearchProfile, QueryCostSummary,
    // Filter pushdown types (A12)
    FilterPredicate, FilteredSearchOptions,
    // Multi-vector types (A11)
    AggregationMethod, MultiVectorDocument, DocumentSearchResult,
    // Path query types (A6)
    PathQueryRow,
    // Existing types
    GraphNeighborInfo, MemoryTraceSpan, PolicyCheckResult, TemporalEdgeInfo,
};
pub use replay_store::ChatReplayStore;
pub use session_store::SessionStore;
pub use vector_store::VectorStore;
