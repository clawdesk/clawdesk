//! # clawdesk-agent-config
//!
//! Declarative agent configuration via TOML files. Enables the full agent lifecycle
//! (define, deploy, update, delete) without any Rust code changes or recompilation.
//!
//! ## Architecture
//!
//! ```text
//! agents/analyst.toml  ──→  AgentConfig (parsed)
//!                            │
//!                            ├─→ DashMap<AgentId, Arc<AgentConfig>> (registry)
//!                            │
//!                            └─→ File watcher (kqueue/inotify) for hot-reload
//! ```
//!
//! ## Quick Start
//!
//! ```toml
//! [agent]
//! name = "analyst"
//! description = "Data analyst with strong statistical reasoning"
//! version = "1.0.0"
//!
//! [model]
//! provider = "anthropic"
//! model = "claude-sonnet-4-20250514"
//! fallback = ["openai:gpt-4o", "gemini:gemini-2.5-pro"]
//! temperature = 0.3
//! max_tokens = 8192
//!
//! [system_prompt]
//! content = """
//! You are a data analyst. Distinguish correlation from causation.
//! Always show your reasoning with statistical evidence.
//! """
//!
//! [capabilities]
//! tools = ["read_file", "web_search", "python_exec"]
//! network = ["*"]
//! memory_write = ["self.*"]
//!
//! [resources]
//! max_tokens_per_hour = 100000
//! max_tool_iterations = 15
//! timeout_seconds = 300
//! ```
//!
//! See [`crate::schema`] for the full configuration schema.
//! See [ADR-006](../docs/adr/006-declarative-agents.md) for design rationale.

mod schema;
mod registry;
mod loader;
mod watcher;
mod error;

pub use schema::*;
pub use registry::AgentRegistry;
pub use loader::AgentLoader;
pub use watcher::AgentWatcher;
pub use error::AgentConfigError;
