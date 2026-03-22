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
pub mod agent_event_stream;
pub mod agent_message;
pub mod agent_registry;
pub mod aop_verifier;
pub mod bootstrap;
pub mod browser_tools;
pub mod btw;
pub mod builtin_tools;
pub mod causal_trace;
pub mod cli_runner;
pub mod cli_provider;
pub mod compaction;
pub mod context;
pub mod context_budget;
pub mod context_transform;
pub mod crdt;
pub mod context_window;
pub mod dynamic_orchestrator;
pub mod dynamic_prompt;
pub mod exec_policy;
pub mod failover;
pub mod handoff_summarizer;
pub mod harness;
pub mod isolated_agent;
pub mod loop_guard;
pub mod loop_stages;
pub mod marketplace;
pub mod persona_field;
pub mod pipeline;
pub mod pipeline_executor;
pub mod pipeline_router;
pub mod port;
pub mod process_manager;
pub mod prompt_assembler;
pub mod prompt_budget;
pub mod prompt_isolation;
pub mod provenance;
pub mod recursion_depth;
pub mod runner;
pub mod session_coord;
pub mod session_control;
pub mod session_lane;
pub mod shared_state;
pub mod steering;
pub mod speculative;
pub mod status_watcher;
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
pub mod unified_policy;
pub mod workspace;
pub mod workspace_pool;
pub mod workspace_context;
pub mod trait_system;
pub mod persona_algebra;
pub mod persona_ext;
pub mod email_tool;
pub mod subagent_profiles;
pub mod shell_hooks;
pub mod canvas_tools;
pub mod node_tools;
pub mod token_budget;
pub mod intent;
pub mod eval_loop;
pub mod coherence;
pub mod cli_orchestration;
pub mod a2ui;
pub mod auto_compose;
pub mod web_search;
pub mod shell_dispatch;

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
pub use canvas_tools::{CanvasPresentTool, CanvasHideTool, CanvasNavigateTool, CanvasEvalTool, CanvasSnapshotTool, A2uiPushTool, A2uiResetTool, DeviceInfoTool, LocationGetTool, create_canvas_tools};
pub use node_tools::{SmsSendTool, PhotosLatestTool, ContactsSearchTool, ContactsAddTool, CalendarEventsTool, CalendarAddTool, MotionActivityTool, TalkActivateTool, TalkDeactivateTool, TalkStatusTool, TalkModeBridge, create_node_device_tools, create_talk_mode_tools};
pub use aop_verifier::{AopVerifier, VerificationConfig, VerificationResult, Subtask as AopSubtask, AgentCapabilityProfile, SolvabilityResult, CompletenessResult, NonRedundancyResult};
pub use crdt::{Lattice, GCounter, PnCounter, LwwRegister, OrSet, Rga, RgaId, RgaElement};
pub use token_budget::{TokenBudgetManager, BudgetConfig, BudgetVerdict, AgentUsage};
pub use agent_registry::{AgentRegistry, RegistrySnapshot, StatusTransition};
pub use agent_event_stream::{AgentEventStream, AgentLoopEvent, AgentExecutionId, EventStreamCombiner};
pub use agent_message::{AgentMessage, ConvertConfig, PipelineEventType, NotificationSeverity, convert_to_llm};
pub use context_transform::{ContextTransformPipeline, ContextTransform, TransformContext, BudgetEnforcementTransform, MemoryInjectionTransform, SkillContextTransform, MemoryFragment};
pub use dynamic_orchestrator::{DynamicOrchestrator, HandleId, ManagedAgent, AgentStatus, WaitResult, SpawnError};
pub use handoff_summarizer::{HandoffSummarizer, LlmHandoffSummarizer, StaticHandoffSummarizer, build_kickoff_prompt};
pub use isolated_agent::{IsolatedAgentManager, IsolatedAgentHandle, IsolationConfig, IsolationTransport, IsolationError, ExitStatus};
pub use steering::{SteeringController, SteeringSender, FollowUpSender, SteeringMessage, FollowUpMessage, SteeringSource, FollowUpSource, DequeueMode, SteeringCheck, FollowUpCheck, SteeringError};
pub use status_watcher::{StatusWatcher, AgentStatusWatch, AgentStatusTransition};
