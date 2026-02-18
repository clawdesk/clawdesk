//! # clawdesk-domain
//!
//! Pure business logic for ClawDesk — no I/O, no storage, no network.
//!
//! All functions are pure transformations over closed algebraic types:
//! - **Fallback FSM**: Deterministic finite state machine for LLM provider fallback
//! - **Compaction**: Semantically-aware conversation compaction with token budgeting
//! - **Auth ring**: Circular buffer auth profile rotation with cooldown semantics
//! - **Context guard**: Predictive context window guard with circuit breaker
//! - **Model catalog**: Capability-based model selection with cost/latency routing
//! - **Routing**: Message routing logic with allowlist matching
//! - **Send policy**: Token-bucket rate limiter with priority queuing and backpressure
//! - **System prompt**: Token-budgeted system prompt construction (greedy knapsack)

pub mod auth;
pub mod compaction;
pub mod context_guard;
pub mod fallback;
pub mod migration;
pub mod model_catalog;
pub mod prompt_builder;
pub mod routing;
pub mod send_policy;
pub mod system_prompt;
