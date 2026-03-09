# ADR-005: Trait-Based Storage Abstraction

## Status
Accepted

## Context
ClawDesk operates across multiple deployment targets: desktop (persistent local storage), server (shared database), CI/testing (in-memory). The storage layer must support conversation threads, agent configs, embeddings, and key–value metadata with varying durability requirements.

## Decision
Define storage as a set of async Rust traits in `clawdesk-storage`:
- `ThreadStore` — CRUD for conversation threads and messages
- `MemoryStore` — agent memory persistence with TTL and scoping
- `EmbeddingStore` — vector insert/search via HNSW (delegated to SochDB)
- `KvStore` — generic key–value with optional expiry

Concrete implementations live in adapter crates. The default adapter uses SochDB for all four trait families. In-memory adapters exist for testing.

## Consequences

### Positive
- Swap storage backends without touching business logic
- In-memory adapter enables deterministic, fast tests
- Schema evolution isolated to adapter crates
- Port traits enforce interface stability across versions

### Negative
- Trait objects require `Send + Sync + 'static` bounds, complicating lifetime management
- Additional indirection vs direct SochDB calls
- Must maintain compatibility between trait evolution and persisted data

### Neutral
- Graph storage (SochDB's graph features) exposed as a separate `GraphStore` trait when needed

## Alternatives Considered

**Direct SochDB calls everywhere:** Simpler initially, but couples all crates to one storage engine. Ruled out for testability and portability.

**SQLite + sqlite-vss:** Mature but lacks native graph storage and MVCC. Would need separate libraries for vector search. See ADR-002.

**Abstract via generic parameters (monomorphization):** Avoids vtable overhead but explodes compile times in a 44-crate workspace. Trait objects with `Arc<dyn Store>` chosen for practical compile-time/runtime trade-off.
