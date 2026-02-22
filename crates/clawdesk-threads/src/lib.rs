//! # clawdesk-threads
//!
//! Namespaced chat-thread persistence on SochDB.
//!
//! Every chat thread gets its own **namespace** (key-prefix partition) inside a
//! single SochDB database. This mirrors how `agentreplay-storage` isolates
//! traces by `tenant_id/project_id` — except the primary dimension here is
//! `thread_id`.
//!
//! ## Key Schema
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │ Primary Data                                                           │
//! │                                                                        │
//! │   threads/{thread_id:032x}                      → ThreadMeta (JSON)    │
//! │   msgs/{thread_id:032x}/{timestamp:020}/{msg_id:032x}  → Message      │
//! │   attachments/{msg_id:032x}                     → binary blob (zstd)   │
//! │                                                                        │
//! │ Secondary Indexes                                                      │
//! │                                                                        │
//! │   idx/agent/{agent_id}/{updated:020}/{thread_id:032x}  → []           │
//! │   idx/thread_agent/{thread_id:032x}             → agent_id string     │
//! │                                                                        │
//! │ Metadata                                                               │
//! │                                                                        │
//! │   meta/thread_count                             → u64 LE              │
//! │   meta/msg_count                                → u64 LE              │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Design Principles (borrowed from agentreplay-storage)
//!
//! 1. **Single database, namespace via key prefix** — no multi-DB management.
//! 2. **Zero-padded timestamps** (`{:020}`) for lexicographic = chronological order.
//! 3. **Write-time secondary indexes** (empty values for existence-only indexes).
//! 4. **Cascading deletes** — deleting a thread removes all messages + indexes.
//! 5. **Group-commit** for write throughput (100-op batches, 10ms max wait).
//! 6. **Periodic checkpoint** — caller drives `checkpoint_and_gc()`.

pub mod error;
pub mod keys;
pub mod store;
pub mod types;

pub use error::ThreadStoreError;
pub use store::ThreadStore;
pub use types::{
    Message, MessageRole, ThreadMeta, ThreadSummary, ThreadQuery, SortOrder,
};
