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

pub mod agent_backend_bridge;
pub mod bootstrap;
pub mod builtin_tools;
pub mod compaction;
pub mod context;
pub mod context_window;
pub mod dynamic_prompt;
pub mod failover;
pub mod harness;
pub mod pipeline;
pub mod pipeline_executor;
pub mod pipeline_router;
pub mod prompt_budget;
pub mod prompt_isolation;
pub mod provenance;
pub mod runner;
pub mod session_coord;
pub mod session_lane;
pub mod subagent;
pub mod outbox;
pub mod task_router;
pub mod tool_policy;
pub mod tools;
pub mod trace;
pub mod transactional_lane;
pub mod transcript_repair;
pub mod turn_capture;
pub mod workspace;
pub mod workspace_context;

pub use agent_backend_bridge::{RunnerBackend, PipelineAgentConfig};
pub use context::ContextAssembler;
pub use harness::{
    Harness, HarnessCapabilities, HarnessError, HarnessEvent, HarnessKind, HarnessPriority,
    HarnessSession, HarnessSessionState, HarnessSpawnConfig,
};
pub use pipeline::{AgentPipeline, PipelineBuilder, PipelineStep};
pub use pipeline_executor::{AgentBackend, PipelineError, PipelineEvent, PipelineExecutor};
pub use prompt_isolation::{IsolatedPrompt, PromptIsolator, PromptNamespace};
pub use runner::{AgentConfig, AgentEvent, AgentResponse, AgentRunner, ApprovalGate, ChannelContext, ResponseSegment, SandboxGate, SkillInjection, SkillProvider};
pub use failover::{FailoverAction, FailoverController};
pub use session_lane::{SessionGuard, SessionLaneManager, SessionLaneError};
pub use task_router::{ExecutionPath, RoutingCandidate, RoutingDecision, RoutingWeights, TaskFeatures, TaskRouter};
pub use builtin_tools::{MessageSendTool, MessagingToolSend, MessagingToolTracker, SessionsSendTool, DynamicSpawnTool, DynamicSpawnRequest, ToolAccess};
pub use tools::{Tool, ToolPolicy, ToolRegistry, ToolResult, ToolSchema};
pub use trace::{AgentTrace, TraceCollector, TraceEvent};
pub use workspace::{WorkspaceGuard, ConfinementResult};
