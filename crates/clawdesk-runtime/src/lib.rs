//! # clawdesk-runtime — Durable Agent Execution Runtime
//!
//! Turns ClawDesk's ephemeral agent execution into crash-recoverable workflows.
//!
//! ## Design
//!
//! Every side-effect (LLM call, tool execution, human gate) is an **Activity**
//! journaled to SochDB before execution and marked complete after. On crash
//! recovery, the runtime replays the journal, skipping completed activities
//! and resuming from the first incomplete one.
//!
//! This is **checkpoint-and-resume**, not Temporal-style deterministic replay.
//! The agent loop itself is not replayed — only its accumulated state (messages,
//! token counts, round number) is restored from the last checkpoint.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │                  clawdesk-runtime                    │
//! │                                                     │
//! │  WorkflowEngine ── ActivityJournal ── DagExecutor   │
//! │       │                 │                 │          │
//! │  LeaseManager    CheckpointStore    DeadLetterQueue  │
//! │                                                     │
//! │  ┌────────────────────────────────────────────────┐ │
//! │  │           RecoveryManager                      │ │
//! │  │  scan_incomplete() → resume_or_reassign()      │ │
//! │  └────────────────────────────────────────────────┘ │
//! └─────────────────────┬───────────────────────────────┘
//!                       │ uses
//!        ┌──────────────┼──────────────┐
//!        ▼              ▼              ▼
//!   AgentRunner    AgentPipeline    SochDB
//! ```
//!
//! ## Storage Schema (SochDB)
//!
//! ```text
//! runtime:runs:{run_id}              → WorkflowRun (JSON)
//! runtime:runs:{run_id}:journal:{seq}→ JournalEntry
//! runtime:runs:{run_id}:checkpoint   → Checkpoint (latest only)
//! runtime:leases:{run_id}            → Lease
//! runtime:dlq:{run_id}               → DeadLetterEntry
//! runtime:index:state:{state}        → secondary index
//! runtime:index:worker:{worker_id}   → worker's active runs
//! ```

pub mod checkpoint;
pub mod dag;
pub mod dead_letter;
pub mod durable_runner;
pub mod journal;
pub mod lease;
pub mod recovery;
pub mod types;
pub mod supervisor;
pub mod kill_tree;
pub mod pty;
pub mod session_mux;
pub mod writer;

// Re-exports for ergonomic use.
pub use checkpoint::CheckpointStore;
pub use dag::DagExecutor;
pub use dead_letter::DeadLetterQueue;
pub use durable_runner::DurableAgentRunner;
pub use journal::ActivityJournal;
pub use kill_tree::{kill_tree, kill_process_group, KillSignal, KillTreeResult};
pub use lease::LeaseManager;
pub use pty::{PtySession, PtyConfig, PtyEvent, PtyPool};
pub use session_mux::{
    SessionEvent, SessionEventKind, SessionMode, SessionMux, SessionMuxConfig, SessionMuxError,
    SessionPriority, SessionSnapshot, SessionSpawnRequest, SessionStatus,
};
pub use recovery::RecoveryManager;
pub use types::*;
pub use supervisor::{ProcessSupervisor, ProcessInfo, SpawnConfig, ProcessState};
pub use writer::DurableMessageWriter;
