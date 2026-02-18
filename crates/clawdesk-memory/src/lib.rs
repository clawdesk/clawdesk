//! # clawdesk-memory
//!
//! Memory and embeddings system — embedding providers, batch pipeline,
//! hybrid search (RRF), and WAL-based sync.
//!
//! ## Architecture
//! - **EmbeddingProvider**: Trait for embedding text into vector space
//! - **BatchPipeline**: AIMD-style batching for embedding requests
//! - **HybridSearch**: Reciprocal Rank Fusion combining vector + keyword search
//! - **MemoryManager**: Coordinates storage, embedding, and retrieval
//! - **WalSync**: Write-ahead log for crash-safe embedding sync

pub mod bm25;
pub mod embedding;
pub mod hybrid;
pub mod ingest;
pub mod manager;
pub mod pipeline;
pub mod reranker;

pub use bm25::Bm25Index;
pub use embedding::{
    EmbeddingProvider, EmbeddingResult, BatchEmbeddingResult, EmbeddingCacheConfig,
    OpenAiEmbeddingProvider, CohereEmbeddingProvider, VoyageEmbeddingProvider,
    OllamaEmbeddingProvider, HuggingFaceEmbeddingProvider, MockEmbeddingProvider,
    CachedEmbeddingProvider,
};
pub use hybrid::{HybridSearcher, SearchStrategy};
pub use ingest::IngestionResult;
pub use manager::MemoryManager;
pub use pipeline::BatchPipeline;
pub use reranker::{lexical_rerank, RerankerConfig, RerankerStrategy};
