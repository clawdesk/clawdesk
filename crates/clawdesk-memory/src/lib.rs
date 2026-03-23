//! # clawdesk-memory
//!
//! Memory and embeddings system — embedding providers, batch pipeline,
//! hybrid search (RRF), and WAL-based sync.
//!
//! ## Architecture
//! - **EmbeddingProvider**: Trait for embedding text into vector space
//! - **TieredEmbeddingProvider**: Circuit-breaker wrapping any provider with FTS-only fallback
//! - **BatchPipeline**: AIMD-style batching for embedding requests
//! - **HybridSearch**: Reciprocal Rank Fusion combining vector + keyword search
//! - **MemoryManager**: Coordinates storage, embedding, and retrieval
//! - **Chunker**: UTF-8 safe, semantic-boundary-aware text chunking
//! - **TemporalDecay**: Exponential half-life decay for memory recency
//! - **MMR**: Maximal Marginal Relevance diversity re-ranking
//! - **WalSync**: Write-ahead log for crash-safe embedding sync

pub mod bm25;
pub mod bm25_store;
pub mod batch_runner;
pub mod chunker;
pub mod embedding;
pub mod embedding_cache;
pub mod embeddings_voyage;
pub mod file_memory;
pub mod fts_fallback;
pub mod generation;
pub mod hierarchical;
pub mod episodic_timeline;
pub mod hybrid;
pub mod ingest;
pub mod manager;
pub mod mmr;
pub mod multimodal;
pub mod pipeline;
pub mod reranker;
pub mod retrieval_stage;
pub mod session_indexer;
pub mod temporal_decay;
pub mod tiered;
pub mod transparent;

pub use bm25::Bm25Index;
pub use bm25_store::{PersistentBm25, Bm25Stats};
pub use chunker::{chunk_text, safe_truncate, safe_truncate_with_ellipsis, sha256_hex, ChunkerConfig, Chunk};
pub use embedding_cache::{PersistentCachedProvider, PersistentCacheConfig, EmbeddingCacheStore};
pub use generation::MemoryGeneration;
pub use embedding::{
    EmbeddingProvider, EmbeddingResult, BatchEmbeddingResult, EmbeddingCacheConfig,
    OpenAiEmbeddingProvider, CohereEmbeddingProvider, VoyageEmbeddingProvider,
    OllamaEmbeddingProvider, HuggingFaceEmbeddingProvider, MockEmbeddingProvider,
    CachedEmbeddingProvider,
};
pub use hybrid::{HybridSearcher, SearchStrategy};
pub use ingest::IngestionResult;
pub use manager::{MemoryManager, MemoryConfig, MemorySource, MemoryStats};
pub use mmr::{mmr_rerank, MmrCandidate, MmrConfig, MmrResult};
pub use batch_runner::{BatchRunner, BatchConfig, BatchProgress, BatchPhase, BatchResult, CostEstimate, estimate_cost};
pub use temporal_decay::{
    DecayProfile, MemoryType, TypedDecayConfig,
    decay_factor_profile, typed_decay_factor, apply_typed_temporal_decay,
};
// Re-export MemoryBackend trait from storage so consumers can refer to it via clawdesk-memory
pub use clawdesk_storage::memory_backend::{
    MemoryBackend, MemoryWriteOp, AtomicWriteResult, PolicyCheckResult,
    // Memory Schema types (A4)
    Episode, EpisodeType, Event, EventRole, EventMetrics, Entity, EntityKind, EntityFacts,
    // Context Query types (A1)
    ContextQueryResult, ContextSection, ContextFormat, TruncationStrategy,
    // Task Queue types (A8)
    BackgroundTask, TaskClaimResult, TaskQueueStats,
    // Batch types (A7)
    BatchWriteResult,
    // Path Query types (A6)
    PathQueryRow,
};
pub use pipeline::BatchPipeline;
pub use reranker::{lexical_rerank, RerankerConfig, RerankerStrategy};
pub use session_indexer::{index_session, SessionIndexConfig, SessionMessage};
pub use temporal_decay::{TemporalDecayConfig, decay_factor, apply_temporal_decay};
pub use tiered::{TieredEmbeddingProvider, TieredConfig, EmbeddingTier, build_tiered_provider};
pub use file_memory::{FileMemoryStore, FileMemoryConfig, FileMemoryEntry, FileMemoryResult};
pub use hierarchical::{HierarchicalMemory, TieredMemoryEntry, MemoryTier, ConsolidationConfig, TierStats};
