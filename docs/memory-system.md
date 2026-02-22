# Memory System

ClawDesk's memory system provides persistent, searchable agent memory using embeddings, BM25 keyword search, and hybrid retrieval with Reciprocal Rank Fusion (RRF).

## Overview

```
┌──────────────────────────────────────────────────┐
│                  MemoryManager                    │
│                                                   │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐  │
│  │  remember() │  │  recall()  │  │  forget()  │  │
│  └──────┬─────┘  └──────┬─────┘  └──────┬─────┘  │
│         │               │               │         │
│         ▼               ▼               │         │
│  ┌────────────┐  ┌────────────────┐     │         │
│  │  Chunker   │  │ HybridSearcher │     │         │
│  │ (semantic  │  │ (RRF fusion)   │     │         │
│  │  boundary) │  │                │     │         │
│  └──────┬─────┘  └───┬──────┬────┘     │         │
│         │            │      │           │         │
│         ▼            ▼      ▼           ▼         │
│  ┌────────────┐  ┌──────┐ ┌─────┐  ┌────────┐    │
│  │ Embedding  │  │Vector│ │BM25 │  │ SochDB │    │
│  │  Provider  │  │Search│ │Index│  │ Delete │    │
│  │(tiered/    │  │      │ │     │  │        │    │
│  │ cached)    │  └──────┘ └─────┘  └────────┘    │
│  └────────────┘                                   │
│                                                   │
│  Backed by SochDB (VectorStore trait)             │
└──────────────────────────────────────────────────┘
```

## Storing Memories

### `remember(content, metadata)`

When a memory is stored:

1. **Chunking** — Content is split into chunks at semantic boundaries (paragraph, sentence, word)
   - UTF-8 safe splitting (never breaks multi-byte characters)
   - Configurable chunk size and overlap
   - Preserves semantic coherence

2. **Embedding** — Each chunk is embedded via the configured embedding provider
   - Embedding cache prevents redundant API calls
   - Batch pipeline uses AIMD-style rate adaptation
   - Tiered provider with circuit breaker fallback to BM25-only mode

3. **Indexing** — Embeddings are stored in SochDB's HNSW vector index
   - BM25 keyword index is updated in parallel
   - Metadata (source, timestamp, tags) stored alongside

4. **Batch Mode** — `remember_batch()` processes multiple memories in one call
   - Batched embedding requests for better throughput
   - Single atomic commit to SochDB

### Memory Sources

Memories can come from:
- **User explicit** — "Remember that..." commands
- **Session indexing** — Automatic indexing of conversation turns
- **Corpus ingestion** — Bulk import from documents

## Retrieving Memories

### `recall(query, limit)`

Memory recall uses hybrid search for best-of-both-worlds retrieval:

1. **Vector Search** — Semantic similarity via HNSW
   - Cosine similarity scoring
   - Approximate nearest neighbors for speed

2. **BM25 Search** — Keyword matching
   - TF-IDF scoring with BM25 formula (k1=1.2, b=0.75)
   - Persistent BM25 index for instant keyword lookups

3. **Reciprocal Rank Fusion (RRF)** — Combines both result sets
   ```
   RRF_score = Σ 1 / (k + rank_i)
   ```
   where k=60 (standard RRF constant)

4. **Re-ranking** — Optional post-retrieval refinement
   - Lexical re-ranking for precision
   - Configurable re-ranking strategy

5. **MMR Diversity** — Maximal Marginal Relevance ensures result diversity
   ```
   MMR = λ · sim(d, q) - (1-λ) · max(sim(d, d_i))
   ```
   Prevents returning near-duplicate memories

6. **Temporal Decay** — Exponential half-life decay for recency bias
   ```
   decay = exp(-λt)  where λ = ln(2) / half_life
   ```
   Newer memories score higher, old memories fade (but never disappear)

## Embedding Providers

### Provider Trait

```rust
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, MemoryError>;
    fn dimension(&self) -> usize;
    fn model_name(&self) -> &str;
}
```

### Available Providers

| Provider | Dimensions | Model |
|----------|-----------|-------|
| **OpenAI** | 1536 / 3072 | `text-embedding-3-small/large` |
| **Cohere** | 1024 | `embed-english-v3.0` |
| **Voyage** | 1024 | `voyage-2` |
| **Ollama** | varies | Local embedding models |
| **HuggingFace** | varies | Any HF embedding model |
| **Mock** | configurable | For testing |

### Tiered Provider

`TieredEmbeddingProvider` wraps any provider with resilience:

```
Tier 1: Primary provider (e.g., OpenAI)
    │
    │ circuit break on N failures
    ▼
Tier 2: Fallback provider (e.g., Ollama local)
    │
    │ circuit break
    ▼
Tier 3: BM25-only mode (no embeddings)
```

Features:
- Configurable failure thresholds per tier
- Automatic recovery when upstream comes back
- Latency tracking for adaptive tier selection

### Embedding Cache

`PersistentCachedProvider` caches embeddings to avoid redundant API calls:
- Content-addressed cache (hash of input text → embedding)
- Persistent via `EmbeddingCacheStore`
- Cache hit rates typically > 60% in conversational use

## BM25 Index

`PersistentBm25` provides fast keyword search:

- **TF-IDF** with BM25 scoring (k1=1.2, b=0.75)
- **Persistent storage** — survives restarts
- **Incremental updates** — new documents added without full re-index
- **Statistics tracking** — document count, average length, term frequencies

## Hybrid Search Strategies

`HybridSearcher` supports multiple fusion strategies:

| Strategy | Description |
|----------|-------------|
| `VectorOnly` | Pure semantic search |
| `Bm25Only` | Pure keyword search |
| `Hybrid(α)` | Weighted RRF fusion (α = vector weight) |
| `Adaptive` | Automatically selects based on query characteristics |

## Batch Pipeline

`BatchPipeline` manages embedding request throughput:

- **AIMD rate adaptation** — Additive Increase, Multiplicative Decrease
  - On success: batch size += 1
  - On failure: batch size = batch size / 2
- **Bounded queue** — Prevents unbounded memory growth
- **Retry with backoff** — Failed batches are retried

## Session Indexing

`SessionIndexer` automatically indexes conversation turns:

```rust
let indexer = SessionIndexer::new(SessionIndexConfig {
    min_message_length: 50,     // Skip very short messages
    index_user_messages: true,  // Index user messages
    index_assistant_messages: true,
    batch_size: 10,
    delay_seconds: 30,          // Delay before indexing (debounce)
});
```

Each session turn is:
1. Filtered by minimum length
2. Chunked if longer than chunk size
3. Embedded and stored with session metadata
4. Available for recall in future conversations

## Memory Generation

`MemoryGeneration` creates synthesized memories:
- Summarizes long conversations into key facts
- Extracts action items and decisions
- Creates structured memory entries from unstructured conversation

## Memory Lifecycle

### Creation
```
User action or auto-indexing
    → Chunk
    → Embed (cached, tiered, batched)
    → Store in SochDB (vector + BM25 + metadata)
```

### Retrieval
```
Query
    → Embed query
    → Parallel: Vector search + BM25 search
    → RRF fusion
    → MMR diversity filter
    → Temporal decay scoring
    → Re-rank
    → Return top-K results
```

### Injection
```
Recall results
    → Score by relevance
    → Pack within memory_cap budget (4096 tokens default)
    → Inject as System message before user's last message
    → LLM sees memories with high attention (recency bias)
```

### Deletion
```
forget(memory_id)
    → Remove from vector index
    → Remove from BM25 index
    → Remove metadata from SochDB
```

## Memory in the Prompt

Memory fragments are injected into the prompt by `PromptBuilder`:

```
System Prompt Structure:
┌────────────────────────┐
│ Identity (persona)     │ ← 2,000 token cap
│ Skills (activated)     │ ← 4,096 token cap
│ Runtime context        │ ← 512 token cap
│ Safety instructions    │ ← 1,024 token cap
├────────────────────────┤
│ ... conversation ...   │
├────────────────────────┤
│ Memory injection       │ ← 4,096 token cap, pre-user-message
│ (as System message)    │   for recency attention bias
├────────────────────────┤
│ Latest user message    │
└────────────────────────┘
```

## IPC Commands

| Command | Parameters | Description |
|---------|-----------|-------------|
| `remember_memory` | `content, metadata` | Store a new memory |
| `remember_batch` | `entries[]` | Store multiple memories |
| `recall_memories` | `query, limit` | Search memories by query |
| `forget_memory` | `memory_id` | Delete a specific memory |
| `get_memory_stats` | — | Get index statistics (count, size, cache hit rate) |

## Configuration

Memory behavior is configured via `MemoryConfig`:

| Setting | Default | Description |
|---------|---------|-------------|
| `embedding_provider` | `openai` | Which embedding provider to use |
| `embedding_model` | `text-embedding-3-small` | Specific model |
| `chunk_size` | `512` | Characters per chunk |
| `chunk_overlap` | `64` | Overlap between chunks |
| `search_strategy` | `Hybrid(0.7)` | Retrieval strategy (0.7 = 70% vector weight) |
| `mmr_lambda` | `0.5` | MMR diversity parameter |
| `temporal_decay_half_life` | `7 days` | Memory recency half-life |
| `max_results` | `10` | Maximum recall results |
| `cache_enabled` | `true` | Enable embedding cache |
