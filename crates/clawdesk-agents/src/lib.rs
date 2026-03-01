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
pub mod browser_tools;
pub mod builtin_tools;
pub mod compaction;
pub mod context;
pub mod context_budget;
pub mod context_window;
pub mod dynamic_prompt;
pub mod exec_policy;
pub mod failover;
pub mod harness;
pub mod loop_guard;
pub mod loop_stages;
pub mod pipeline;
pub mod pipeline_executor;
pub mod pipeline_router;
pub mod process_manager;
pub mod prompt_assembler;
pub mod prompt_budget;
pub mod prompt_isolation;
pub mod provenance;
pub mod recursion_depth;
pub mod runner;
pub mod session_coord;
pub mod session_lane;
pub mod shared_state;
pub mod subagent;
pub mod outbox;
pub mod task_router;
pub mod tool_policy;
pub mod tools;
pub mod trace;
pub mod transactional_lane;
pub mod transcript_repair;
pub mod turn_capture;
pub mod turn_router;
pub mod workspace;
pub mod workspace_context;
pub mod trait_system;
pub mod persona_algebra;
pub mod email_tool;
pub mod subagent_profiles;
pub mod shell_hooks;

pub use agent_backend_bridge::{RunnerBackend, PipelineAgentConfig};
pub use context::ContextAssembler;
pub use harness::{
    Harness, HarnessCapabilities, HarnessError, HarnessEvent, HarnessKind, HarnessPriority,
    HarnessSession, HarnessSessionState, HarnessSpawnConfig,
};
pub use pipeline::{AgentPipeline, PipelineBuilder, PipelineStep};
pub use pipeline_executor::{AgentBackend, PipelineError, PipelineEvent, PipelineExecutor};
pub use prompt_isolation::{IsolatedPrompt, PromptIsolator, PromptNamespace};
pub use runner::{AgentConfig, AgentEvent, AgentResponse, AgentRunner, AgentRunnerBuilder, ApprovalDecision, ApprovalGate, ChannelContext, MemoryRecallFn, MemoryRecallResult, ResponseSegment, SandboxConfigured, SandboxGate, SandboxUnconfigured, SkillInjection, SkillProvider};
pub use failover::{FailoverAction, FailoverController};
pub use session_lane::{SessionGuard, SessionLaneManager, SessionLaneError};
pub use shared_state::{SharedAgentState, SharedStateBackend, SharedStateError, SharedStateManager, InMemorySharedState, StateEntry};
pub use task_router::{ExecutionPath, RoutingCandidate, RoutingDecision, RoutingWeights, TaskFeatures, TaskRouter};
pub use turn_router::{TurnRouter, TurnRoutingResult};
pub use builtin_tools::{MessageSendTool, MessagingToolSend, MessagingToolTracker, SessionsSendTool, DynamicSpawnTool, DynamicSpawnRequest, ToolAccess, BrowserActionTool, CronScheduleTool, CronListTool, CronRemoveTool, CronTriggerTool, DiscoverAgentsTool, SendNotificationTool, NotificationPriority, McpConnectTool, McpCallTool, PipelineComposeTool, WorkspaceSearchTool, WorkspaceGrepTool, DurableTaskTool};
pub use tools::{Tool, ToolPolicy, ToolRegistry, ToolResult, ToolSchema};
pub use trace::{AgentTrace, TraceCollector, TraceEvent};
pub use workspace::{WorkspaceGuard, ConfinementResult};
pub use browser_tools::{BrowserObserveTool, BrowserNavigateTool, BrowserClickTool, BrowserTypeTool, BrowserScreenshotTool, BrowserScrollTool, BrowserCloseTool, register_browser_tools};
pub use builtin_tools::{ProcessStartTool, ProcessPollTool, ProcessWriteTool, ProcessKillTool, ProcessListTool, McpBridgeToolInstance, McpDiscoveredTool, register_mcp_bridge_tools};
pub use trait_system::{AgentTrait, TraitCategory, TraitLibrary, CompositionResult, builtin_trait_library};
pub use persona_algebra::{PersonaVector, PersonaDimension, TraitVectorMap, builtin_trait_vectors};
pub use email_tool::{EmailSendTool, EmailSendParams, EmailSmtpConfig, register_email_tool, register_email_tool_dry_run};
pub use subagent_profiles::{SubagentProfile, ResolvedProfile, ModelTier, resolve_profile, build_subagent_policy_stack, attack_surface};
pub use shell_hooks::{ShellHook, ShellHookConfig, HooksConfig, HookPhase, register_shell_hooks};
