//! # clawdesk-acp
//!
//! Agent-to-Agent protocol (A2A/ACP) for ClawDesk.
//!
//! Implements the Agent Client Protocol — a standard for inter-agent communication,
//! task delegation, and capability discovery. Inspired by Google's A2A protocol
//! and OpenClaw's ACP implementation.
//!
//! ## Core abstractions
//!
//! - **Agent Card** — Capability advertisement (what an agent can do, its endpoint, auth).
//!   This is the agent's "business card" in the network.
//!
//! - **Task** — A unit of work delegated between agents. Follows a finite state machine:
//!   `Submitted → Working → (InputRequired | Completed | Failed | Canceled)`
//!
//! - **Message** — Typed communication between agents within a task context.
//!
//! - **Router** — Discovers agents and routes requests to the best-matching agent
//!   based on capability intersection and availability.
//!
//! ## Protocol design
//!
//! The protocol models inter-agent communication as a **typed message-passing system**
//! over HTTP. Each agent is a **service** that advertises capabilities via an Agent Card
//! served at `/.well-known/agent.json`. Tasks are the unit of delegation:
//!
//! ```text
//! Agent A                          Agent B
//!    │                                │
//!    │── POST /a2a/tasks/send ───────▶│  (create task)
//!    │◀── 200 { status: "working" } ──│
//!    │                                │
//!    │── GET /a2a/tasks/{id} ────────▶│  (poll status)
//!    │◀── 200 { status: "completed" }─│
//! ```
//!
//! ## Mathematical model
//!
//! Agent discovery is a **bipartite matching** problem:
//! - Set A: task requirements (capabilities needed)
//! - Set B: available agents (capabilities offered)
//! - Edge weight: capability overlap score ∈ [0, 1]
//!
//! Routing selects argmax_b Σ_c w(c) · 𝟙[c ∈ caps(b)] for each task,
//! where w(c) is the importance weight of capability c.

pub mod agent_card;
pub mod announce;
pub mod capability;
pub mod content_router;
pub mod delta_stream;
pub mod discovery;
pub mod error;
pub mod heartbeat;
pub mod message;
pub mod policy;
pub mod router;
pub mod server;
pub mod session_router;
pub mod skill_wiring;
pub mod streaming;
pub mod task;

pub use agent_card::{AgentCard, AgentCapability, AgentEndpoint, AgentSkill};
pub use announce::{AnnounceRouter, Announcement, AnnouncePayload, DeliveryTarget, DeliveryResult, RetryPolicy};
pub use capability::{CapSet, CapabilityId};
pub use error::{AcpError, AcpErrorKind, AcpResult, Retryability, Severity};
pub use heartbeat::{HeartbeatConfig, HeartbeatMonitor, HeartbeatCallback, PingPayload, PongPayload, PingResult};
pub use message::{A2AMessage, A2AMessageKind, Artifact};
pub use router::{AgentDirectory, AgentRouter, RoutingDecision};
pub use server::A2AHandler;
pub use session_router::{AgentSource, AgentSummary, CircuitBreaker, SessionRouter};
pub use task::{Task, TaskId, TaskState, TaskEvent};
