//! # clawdesk-agents
//!
//! Agent execution engine with structured concurrency, tool orchestration,
//! and context window management.
//!
//! ## Architecture
//! The agent runner uses a composable middleware pipeline:
//! `AuthResolve → HistorySanitize → ContextGuard → ToolSplit → Execute → FailoverDecide`
//!
//! ## Features
//! - Parallel tool execution via `JoinSet`
//! - Cooperative cancellation via `CancellationToken`
//! - Predictive context window guard with circuit breaker
//! - Tool policy engine with allowlists, approval, and capability gating
//! - Lazy tool loading for O(A) startup time
//! - Event streaming for real-time monitoring

pub mod context;
pub mod harness;
pub mod pipeline;
pub mod prompt_isolation;
pub mod runner;
pub mod subagent;
pub mod task_router;
pub mod tool_policy;
pub mod tools;
pub mod trace;
pub mod workspace;
pub mod workspace_context;

pub use context::ContextAssembler;
pub use harness::{
    Harness, HarnessCapabilities, HarnessError, HarnessEvent, HarnessKind, HarnessPriority,
    HarnessSession, HarnessSessionState, HarnessSpawnConfig,
};
pub use pipeline::{AgentPipeline, PipelineBuilder, PipelineStep};
pub use prompt_isolation::{IsolatedPrompt, PromptIsolator, PromptNamespace};
pub use runner::{AgentConfig, AgentEvent, AgentResponse, AgentRunner};
pub use task_router::{ExecutionPath, RoutingCandidate, RoutingDecision, RoutingWeights, TaskFeatures, TaskRouter};
pub use tools::{Tool, ToolPolicy, ToolRegistry, ToolResult, ToolSchema};
pub use trace::{AgentTrace, TraceCollector, TraceEvent};
pub use workspace::{WorkspaceGuard, ConfinementResult};
