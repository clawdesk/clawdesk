//! # ClawDesk Core — Transport-Agnostic Service Kernel
//!
//! This crate contains the business logic that was previously embedded in
//! `clawdesk-tauri/src/commands.rs`. By extracting it into a standalone
//! crate, all transports (Tauri desktop, CLI, Gateway HTTP, TMUX) share
//! the same code path.
//!
//! ## Architecture (First-Principles)
//!
//! ```text
//! ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
//! │  Tauri IPC   │  │   CLI stdin  │  │  HTTP/WS API │  │  TMUX panes  │
//! └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘
//!        │                 │                 │                 │
//!        └─────────────────┴────────┬────────┴─────────────────┘
//!                                   │
//!                          ┌────────▼────────┐
//!                          │   CoreService    │  ← THIS CRATE
//!                          │                  │
//!                          │  • ChatService   │  — chat lifecycle, messaging
//!                          │  • ProjectService│  — per-chat workspace isolation
//!                          │  • AgentService  │  — agent CRUD, tool registry
//!                          │  • SkillService  │  — skill activation/scoring
//!                          │  • EventSink     │  — trait for transport events
//!                          └────────┬────────┘
//!                                   │
//!                    ┌──────────────┴──────────────┐
//!                    │       Domain Crates          │
//!                    │  agents · providers · sochdb │
//!                    │  security · memory · skills  │
//!                    └──────────────────────────────┘
//! ```
//!
//! ## Zero-Copy Event Model
//!
//! Instead of Tauri's `AppHandle::emit()`, the core uses an `EventSink`
//! trait. Each transport implements the trait:
//! - **Tauri**: emits to frontend via IPC
//! - **CLI**: prints to stdout/stderr
//! - **Gateway**: pushes via WebSocket
//! - **TMUX**: writes to pane via `tmux send-keys`
//!
//! ## Parallelism (Rust-Native)
//!
//! The core is designed around three concurrency primitives:
//! 1. **Session lanes** — one agent run per chat (serialized)
//! 2. **LLM semaphore** — bounded concurrent LLM calls
//! 3. **JoinSet** — parallel tool execution within a turn
//!
//! These map directly to CPU/IO dynamics:
//! - LLM calls are IO-bound (network) → high concurrency
//! - Tool execution is CPU-bound (file I/O, shell) → bounded parallelism
//! - Session state is shared-mutable → serialized access

pub mod event;
pub mod project;
pub mod chat;
pub mod service;

pub use event::{CoreEvent, EventSink, NullEventSink};
pub use project::ProjectService;
pub use chat::ChatService;
pub use service::CoreService;
