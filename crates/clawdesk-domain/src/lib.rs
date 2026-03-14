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

//! - **Contact graph**: Hawkes-process relationship health with entity resolution
//! - **Identity model**: Bayesian personality evolution with channel conditioning
//! - **Digest compiler**: Tumbling window event aggregation with Kahan summation
//! - **Approval queue**: Item-level human gates with quorum policies

pub mod approval;
pub mod auth;
pub mod compaction;
pub mod contacts;
pub mod context_guard;
pub mod digest;
pub mod fallback;
pub mod identity;
pub mod lineage;
pub mod migration;
pub mod model_catalog;
pub mod prompt_builder;
pub mod routing;
pub mod send_policy;
pub mod system_prompt;
pub mod prompt_trace;
pub mod proactive_compaction;
pub mod policy_dsl;
pub mod workflow_templates;
pub mod system_events;
