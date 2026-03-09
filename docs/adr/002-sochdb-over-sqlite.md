# ADR-002: SochDB over SQLite for Primary Storage

## Status
Accepted

## Context
ClawDesk needs a storage engine that supports:
1. ACID transactions for conversation history
2. Vector similarity search for RAG and memory retrieval
3. Knowledge graph queries for entity relationships
4. Embedded operation (no external database process)
5. MVCC for concurrent read/write access

## Decision
Use SochDB as the primary storage engine. SochDB provides embedded ACID transactions, HNSW vector indexing, and graph storage in a single database, eliminating the need for separate databases for structured data, vectors, and graphs.

## Consequences

### Positive
- Single database for all storage needs (conversations, vectors, graphs, sessions)
- Embedded — no external process to manage, backup is a file copy
- HNSW index provides O(log n) approximate nearest neighbor queries
- MVCC enables lock-free concurrent reads
- Smaller deployment footprint than PostgreSQL + pgvector + Neo4j

### Negative
- Less mature than SQLite/PostgreSQL — fewer tools, smaller community
- Custom query language learning curve
- No native replication (federation must be application-level)

### Neutral
- The `clawdesk-storage` trait layer means we can add SQLite/PostgreSQL backends later without changing business logic

## Alternatives Considered

**SQLite + sqlite-vss:** Mature and widely deployed, but vector search is a third-party extension with limited HNSW support. No native graph queries.

**PostgreSQL + pgvector:** Production-grade but requires an external process, making single-binary deployment impossible without embedding.

**Custom storage:** Too much engineering effort for uncertain quality gains over SochDB.
