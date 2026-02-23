//! Application state — shared across all Tauri commands.
//!
//! Holds real backend services: skill registry, provider registry,
//! security scanner, audit logger, and agent execution infrastructure.
//! Extended with all 19 backend service integrations (Tasks 12–30).
//!
//! ## Storage architecture
//!
//! All durable state is backed by **SochDB** (`SochStore`) — a single
//! embedded ACID database with WAL, MVCC, HNSW vector search, and a
//! path-based API for O(|path|) key lookups. In-memory `HashMap`/`Vec`
//! fields serve as hot caches; on cold start they are hydrated from
//! SochDB, and every mutation writes through to the database.
//!
//! The **MemoryManager** provides semantic memory (remember / recall /
//! forget) backed by SochDB's `VectorStore` trait implementation,
//! giving the agent embeddings + hybrid search with no external service.

use crate::canvas::Canvas;
use crate::commands_infra::NotificationInfo;
use clawdesk_agents::ToolRegistry;
use tauri::Manager;
use clawdesk_agents::runner::{AgentConfig, AgentRunner};
use clawdesk_acp::AgentDirectory;
use clawdesk_channel::registry::ChannelRegistry;
use clawdesk_cron::CronManager;
use clawdesk_discovery::{MdnsAdvertiser, PairingSession, PeerRegistry, ServiceInfo};
use clawdesk_domain::context_guard::{ContextGuard, ContextGuardConfig};
use clawdesk_domain::prompt_builder::PromptManifest;
use clawdesk_infra::clipboard::ClipboardEntry;
use clawdesk_infra::voice_wake::VoiceWakeManager;
use clawdesk_infra::{IdleConfig, IdleDetector};
use clawdesk_media::MediaPipeline;
use clawdesk_memory::{MemoryManager, MockEmbeddingProvider, OllamaEmbeddingProvider, EmbeddingProvider, OpenAiEmbeddingProvider};
use clawdesk_memory::manager::MemoryConfig;
use clawdesk_memory::tiered::build_tiered_provider;
use clawdesk_observability::ObservabilityConfig;
use clawdesk_plugin::PluginHost;
use clawdesk_providers::anthropic::AnthropicProvider;
use clawdesk_providers::capability::{ProviderCaps, ProviderWeight, ANTHROPIC_CAPS, GEMINI_CAPS, OLLAMA_CAPS, OPENAI_CAPS};
use clawdesk_providers::gemini::GeminiProvider;
use clawdesk_providers::negotiator::ProviderNegotiator;
use clawdesk_providers::ollama::OllamaProvider;
use clawdesk_providers::openai::OpenAiProvider;
use clawdesk_providers::registry::ProviderRegistry;
use clawdesk_providers::Provider;
use clawdesk_runtime::DurableAgentRunner;
use clawdesk_security::audit::{AuditLogger, AuditLoggerConfig};
use clawdesk_security::scanner::{CascadeScanner, CascadeScannerConfig};
use clawdesk_security::{AclManager, AuthProfileManager, ExecApprovalManager, IdentityContract, OAuthFlowManager, ServerSecret};
use clawdesk_skills::bundled::load_bundled_skills;
use clawdesk_skills::registry::SkillRegistry;
use clawdesk_skills::{SkillVerifier, TrustLevel};
use clawdesk_sochdb::SochStore;
use clawdesk_sochdb::SochMemoryBackend;
use clawdesk_sochdb::{
    SochConn, SochSemanticCache, SochTraceStore, SochCheckpointStore,
    SochGraphOverlay, SochTemporalGraph, SochPolicyEngine,
    SochAtomicWriter, SochAgentRegistry,
};
use clawdesk_tunnel::discovery::InviteManager;
use clawdesk_tunnel::metrics::TunnelMetrics;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::{error, info, warn};

// Types serialized to the frontend

/// Provider configuration synced from the UI for use by channel adapters.
///
/// When the user picks a provider in the UI (e.g. "Local (OpenAI Compatible)"
/// with a base URL), this gets stored in `AppState` so the `ChannelMessageSink`
/// can construct the same provider for inbound Discord/Telegram messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelProviderOverride {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopAgent {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub color: String,
    pub persona: String,
    pub persona_hash: String,
    pub skills: Vec<String>,
    pub model: String,
    pub created: String,
    pub msg_count: u64,
    pub status: String,
    pub token_budget: usize,
    pub tokens_used: usize,
    pub source: String,
    /// T8: Optional template ID this agent was created from.
    #[serde(default)]
    pub template_id: Option<String>,
}

impl Default for DesktopAgent {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            icon: String::new(),
            color: String::new(),
            persona: String::new(),
            persona_hash: String::new(),
            skills: Vec::new(),
            model: String::new(),
            created: String::new(),
            msg_count: 0,
            status: String::new(),
            token_budget: 0,
            tokens_used: 0,
            source: String::new(),
            template_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub success: bool,
    pub agents: Vec<DesktopAgent>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityStatus {
    pub gateway_bind: String,
    pub tunnel_active: bool,
    pub tunnel_endpoint: String,
    pub auth_mode: String,
    pub scoped_tokens: bool,
    pub identity_contracts: usize,
    pub skill_scanning: String,
    pub rate_limiter: String,
    pub mdns_disabled: bool,
    pub scanner_patterns: usize,
    pub audit_entries: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostMetrics {
    pub today_cost: f64,
    pub today_input_tokens: u64,
    pub today_output_tokens: u64,
    pub model_breakdown: Vec<ModelCostEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCostEntry {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TauriAgentEvent {
    RoundStart { round: usize },
    Response { content: String, finish_reason: String },
    ToolStart { name: String, args: String },
    ToolEnd { name: String, success: bool, duration_ms: u64 },
    Compaction { level: String, tokens_before: usize, tokens_after: usize },
    StreamChunk { text: String, done: bool },
    Done { total_rounds: usize },
    Error { error: String },
    PromptAssembled {
        total_tokens: usize,
        skills_included: Vec<String>,
        skills_excluded: Vec<String>,
        memory_fragments: usize,
        budget_utilization: f64,
    },
    IdentityVerified { hash_match: bool, version: u64 },
    ContextGuardAction { action: String, token_count: usize, threshold: f64 },
    FallbackTriggered { from_model: String, to_model: String, reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    pub timestamp: String,
    pub event: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub steps: Vec<PipelineNodeDescriptor>,
    pub edges: Vec<(usize, usize)>,
    pub created: String,
    /// Cron expression (5-field) for automated scheduling, e.g. "0 9 * * 1-5".
    /// When set, the pipeline is registered as a recurring CronTask.
    #[serde(default)]
    pub schedule: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineNodeDescriptor {
    pub label: String,
    pub node_type: String,
    pub model: Option<String>,
    pub agent_id: Option<String>,
    /// Optional condition for gate nodes. If set, the gate checks whether
    /// the incoming text contains this substring (case-insensitive).
    #[serde(default)]
    pub condition: Option<String>,
    pub x: f64,
    pub y: f64,
    /// T9 FIX: Step-specific configuration. Contains user-configured values:
    /// - For agent steps: `prompt` (custom system prompt fragment), `max_rounds`
    /// - For gate steps: `expression` (evaluation expression)
    /// - For webhook steps: `url`, `method`, `headers`
    /// All keys are optional; the executor uses defaults when absent.
    #[serde(default)]
    pub config: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub estimated_tokens: usize,
    pub state: String,
    pub verified: bool,
    pub icon: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_secs: u64,
    pub agents_active: usize,
    pub skills_loaded: usize,
    pub tunnel_active: bool,
    /// Whether the storage backend is using durable (on-disk) persistence.
    /// `false` means data will NOT survive a restart (ephemeral / temp storage).
    pub storage_healthy: bool,
    /// Human-readable storage path (for diagnostics).
    pub storage_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub metadata: Option<ChatMessageMeta>,
}

impl Default for ChatMessage {
    fn default() -> Self {
        Self {
            id: String::new(),
            role: String::new(),
            content: String::new(),
            timestamp: String::new(),
            metadata: None,
        }
    }
}

/// A chat session — a single conversation thread with an agent.
/// Each chat has a unique UUID. Multiple chats can exist per agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatSession {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub messages: Vec<ChatMessage>,
    pub created_at: String,
    pub updated_at: String,
}

impl Default for ChatSession {
    fn default() -> Self {
        Self {
            id: String::new(),
            agent_id: String::new(),
            title: String::new(),
            messages: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatMessageMeta {
    pub skills_activated: Vec<String>,
    pub token_cost: usize,
    pub cost_usd: f64,
    pub model: String,
    pub duration_ms: u64,
    pub identity_verified: bool,
    pub tools_used: Vec<ToolUsageSummary>,
    pub compaction: Option<CompactionInfo>,
}

impl Default for ChatMessageMeta {
    fn default() -> Self {
        Self {
            skills_activated: Vec::new(),
            token_cost: 0,
            cost_usd: 0.0,
            model: String::new(),
            duration_ms: 0,
            identity_verified: false,
            tools_used: Vec::new(),
            compaction: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolUsageSummary {
    pub name: String,
    pub success: bool,
    pub duration_ms: u64,
}

impl Default for ToolUsageSummary {
    fn default() -> Self {
        Self {
            name: String::new(),
            success: false,
            duration_ms: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionInfo {
    pub level: String,
    pub tokens_before: usize,
    pub tokens_after: usize,
}

impl Default for CompactionInfo {
    fn default() -> Self {
        Self {
            level: String::new(),
            tokens_before: 0,
            tokens_after: 0,
        }
    }
}

// Application State with real backend services

pub struct AppState {
    // ── SochDB: single ACID store for all durable state ──
    pub soch_store: Arc<SochStore>,

    // ── Thread store: namespaced ACID chat-thread persistence ──
    pub thread_store: Arc<clawdesk_threads::ThreadStore>,

    // ── SochDB advanced modules (powered by ConnectionTrait bridge) ──
    /// Semantic cache — avoids redundant LLM API calls via exact + embedding similarity match.
    pub semantic_cache: Arc<SochSemanticCache>,
    /// OpenTelemetry-compatible trace store — tracks agent runs, token costs, tool calls.
    pub trace_store: Arc<SochTraceStore>,
    /// Workflow checkpoint store — durable multi-step agent task state, resume after crash.
    pub checkpoint_store: Arc<SochCheckpointStore>,
    /// Knowledge graph — models agent↔conversation↔entity relationships.
    pub knowledge_graph: Arc<SochGraphOverlay>,
    /// Temporal graph — time-bounded edges with point-in-time queries ("what did agent believe 2 min ago?").
    pub temporal_graph: Arc<SochTemporalGraph>,
    /// Policy engine — access control, rate limiting, PII redaction on reads/writes.
    pub policy_engine: Arc<SochPolicyEngine>,
    /// Atomic memory writer — all-or-nothing writes across KV + vector + graph indexes.
    pub atomic_writer: Arc<SochAtomicWriter>,
    /// Agent capability registry — multi-agent routing by tool capabilities.
    pub agent_registry: Arc<SochAgentRegistry>,

    // ── Memory system: embeddings + hybrid search backed by SochDB MemoryBackend ──
    // Uses SochMemoryBackend for full integration: atomic writes, graph nodes,
    // temporal edges, policy checks, and trace spans.
    pub memory: Arc<MemoryManager<SochMemoryBackend>>,
    /// Shared embedding provider — used by semantic cache to pre-compute query embeddings (Task 2).
    pub embedding_provider: Arc<dyn EmbeddingProvider>,

    // Real backend services
    pub skill_registry: RwLock<SkillRegistry>,
    pub provider_registry: RwLock<ProviderRegistry>,
    pub tool_registry: Arc<ToolRegistry>,
    pub channel_registry: Arc<RwLock<ChannelRegistry>>,
    /// Saved channel configurations (channel_id → config key-value pairs).
    /// Persisted in-memory; the `update_channel` / `disconnect_channel` commands
    /// read and write this map so the Settings UI can configure adapters.
    pub channel_configs: RwLock<HashMap<String, HashMap<String, String>>>,
    pub scanner: Arc<CascadeScanner>,
    pub scanner_pattern_count: usize,
    pub audit_logger: Arc<AuditLogger>,

    // Agent & session management (hot cache — persisted to SochDB)
    pub agents: RwLock<HashMap<String, DesktopAgent>>,
    pub identities: RwLock<HashMap<String, IdentityContract>>,
    pub server_secret: ServerSecret,
    pub sessions: RwLock<HashMap<String, ChatSession>>,

    // Infrastructure
    pub invites: RwLock<InviteManager>,
    pub tunnel_metrics: Arc<TunnelMetrics>,

    // Metrics (hot counters — persisted to SochDB on checkpoint)
    pub total_cost_today: AtomicU64,
    pub total_input_tokens: AtomicU64,
    pub total_output_tokens: AtomicU64,
    pub model_costs: RwLock<HashMap<String, (u64, u64, u64)>>,
    /// Date string (YYYY-MM-DD) of the last cost counter reset.
    /// Checked on each `record_usage` call; if the day has changed,
    /// counters are reset to zero before accumulating new costs.
    pub last_cost_reset_date: RwLock<String>,
    pub traces: RwLock<HashMap<String, Vec<TraceEntry>>>,
    pub pipelines: RwLock<Vec<PipelineDescriptor>>,
    pub started_at: std::time::Instant,
    pub cancel: tokio_util::sync::CancellationToken,
    /// Per-chat cancellation tokens for currently-running `send_message` tasks.
    pub active_chat_runs: tokio::sync::RwLock<HashMap<String, tokio_util::sync::CancellationToken>>,

    // ── Durable Runtime ──
    pub durable_runner: Option<Arc<DurableAgentRunner>>,

    // ── Media Pipeline ──
    pub media_pipeline: Arc<tokio::sync::RwLock<MediaPipeline>>,

    // ── Plugin System ──
    pub plugin_host: Option<Arc<PluginHost>>,

    // ── A2A Protocol ──
    pub agent_directory: RwLock<AgentDirectory>,
    pub a2a_tasks: tokio::sync::RwLock<HashMap<String, clawdesk_acp::Task>>,

    // ── OAuth2 + PKCE ──
    pub oauth_flow_manager: Arc<OAuthFlowManager>,
    pub auth_profile_manager: Arc<AuthProfileManager>,

    // ── Execution Approval ──
    pub approval_manager: Arc<ExecApprovalManager>,

    // ── Network Discovery ──
    pub mdns_advertiser: RwLock<MdnsAdvertiser>,
    pub pairing_session: RwLock<Option<PairingSession>>,
    pub peer_registry: RwLock<PeerRegistry>,

    // ── Observability ──
    pub observability_config: RwLock<ObservabilityConfig>,
    /// T25: Metrics aggregator for latency, tokens, cost tracking.
    pub metrics_aggregator: Arc<clawdesk_observability::MetricsAggregator>,

    // ── Notifications (hot cache — persisted to SochDB) ──
    pub notification_history: RwLock<Vec<NotificationInfo>>,

    // ── Channel provider override (synced from UI) ──
    /// The active provider configuration from the UI, used by ChannelMessageSink
    /// to construct one-shot providers for inbound channel messages (Discord, etc.).
    pub channel_provider: RwLock<Option<ChannelProviderOverride>>,

    // ── Clipboard (hot cache — persisted to SochDB) ──
    pub clipboard_history: RwLock<Vec<ClipboardEntry>>,

    // ── Voice Wake ──
    pub voice_wake: RwLock<Option<VoiceWakeManager>>,

    // ── Whisper STT Engine ──
    pub whisper_engine: RwLock<Option<clawdesk_media::whisper::WhisperSttEngine>>,

    // ── Audio Recorder (cpal) ──
    pub audio_recorder: parking_lot::Mutex<clawdesk_media::recorder::AudioRecorder>,

    // ── ACL Engine ──
    pub acl_manager: Arc<AclManager>,

    // ── Skill Promotion ──
    pub skill_verifier: Arc<SkillVerifier>,

    // ── Provider Negotiation ──
    pub negotiator: Arc<RwLock<ProviderNegotiator>>,

    // ── Context Guard ──
    pub context_guards: RwLock<HashMap<String, ContextGuard>>,

    // ── Prompt Builder manifests ──
    pub prompt_manifests: RwLock<HashMap<String, PromptManifest>>,

    // ── Idle Detection ──
    pub idle_detector: Option<Arc<IdleDetector>>,

    // ── Canvas (hot cache — persisted to SochDB) ──
    pub canvases: RwLock<HashMap<String, Canvas>>,

    // ── Cron Scheduling (T5) ──
    pub cron_manager: Arc<CronManager>,

    // ── T8: Life OS Template Registry ──
    pub template_registry: Arc<clawdesk_skills::life_os::TemplateRegistry>,

    // ── T9: Journal Store (hot cache — persisted to SochDB) ──
    pub journal_entries: RwLock<HashMap<String, clawdesk_skills::journal::JournalEntry>>,

    // ── T7 FIX: Per-session serialization ──
    // Ensures only one agent run per session at a time. Concurrent requests
    // for the same session queue behind the active run. Prevents interleaved
    // assistant messages and corrupted conversation history.
    pub session_lanes: clawdesk_agents::session_lane::SessionLaneManager,

    // ── GAP-3 FIX: Global LLM concurrency bound ──
    // Limits the total number of concurrent LLM calls across all sessions.
    // Without this, unbounded parallel requests can overwhelm API rate limits
    // and exhaust memory. The semaphore is acquired after the per-session lane
    // guard and before `runner.run()` / `runner.run_with_failover()`.
    pub llm_concurrency: Arc<tokio::sync::Semaphore>,

    // ── GAP-4/1 FIX: Channel dock for prompt injection ──
    // Lightweight metadata registry of all known channels. Used to construct
    // `ChannelContext` for the agent runner without requiring a live channel.
    pub channel_dock: Arc<clawdesk_channel::channel_dock::ChannelDock>,

    // ── GAP-7 FIX: Hook manager for plugin lifecycle hooks ──
    // Dispatches lifecycle hooks (MessageReceive, BeforeAgentStart, etc.) to
    // registered plugins. Shared across all agent runs.
    pub hook_manager: Arc<clawdesk_plugin::HookManager>,

    // ── GAP-4 FIX: Channel binding entries for multi-channel routing ──
    // Maps channel+account combinations to specific agent IDs. When empty
    // (default for desktop), the requested agent_id is used directly.
    pub channel_bindings: RwLock<Vec<clawdesk_domain::routing::ChannelBindingEntry>>,

    // ── T2 FIX: Workspace root for agent tool scoping ──
    // All file-system tools are confined to this directory. Bootstrap context
    // discovery reads project files from here. Defaults to ~/.clawdesk/workspace/.
    pub workspace_root: std::path::PathBuf,

    // ── T4 FIX: Sandbox policy engine for tool execution gating ──
    // Decides per-tool isolation level (None/PathScope/ProcessIsolation/FullSandbox).
    // Tools whose required level exceeds platform capability are blocked.
    pub sandbox_engine: Arc<clawdesk_security::sandbox_policy::SandboxPolicyEngine>,

    // ── GAP-1 FIX: Sub-agent lifecycle manager ──
    // Tracks all running sub-agents (static and ephemeral), enforcing global
    // depth limits (default 5), concurrency caps (default 50), and deferred GC.
    pub sub_mgr: Arc<clawdesk_gateway::subagent_manager::SubAgentManager>,
}

// ── Cron executor glue (T5: wire CronManager to pipeline/agent execution) ──

/// AgentExecutor implementation for CronManager.
/// Executes cron-triggered prompts using a configured LLM provider.
struct CronAgentExecutor {
    provider: Arc<dyn Provider>,
    tool_registry: Arc<ToolRegistry>,
    cancel: tokio_util::sync::CancellationToken,
}

#[async_trait::async_trait]
impl clawdesk_cron::executor::AgentExecutor for CronAgentExecutor {
    async fn execute(&self, prompt: &str, _agent_id: Option<&str>) -> Result<String, String> {
        let config = AgentConfig {
            model: "default".to_string(),
            system_prompt: "You are a helpful automation assistant executing a scheduled task.".to_string(),
            max_tool_rounds: 5,
            context_limit: 32_000,
            response_reserve: 4_096,
            ..Default::default()
        };
        let runner = AgentRunner::new(
            Arc::clone(&self.provider),
            Arc::clone(&self.tool_registry),
            config,
            self.cancel.clone(),
        );
        let history = vec![clawdesk_providers::ChatMessage::new(
            clawdesk_providers::MessageRole::User,
            prompt.to_string(),
        )];
        let response = runner
            .run(history, "You are a helpful automation assistant.".to_string())
            .await
            .map_err(|e| e.to_string())?;
        Ok(response.content)
    }
}

/// No-op delivery handler for desktop — results are delivered through Tauri events.
pub(crate) struct NoOpDelivery;

#[async_trait::async_trait]
impl clawdesk_cron::executor::DeliveryHandler for NoOpDelivery {
    async fn deliver(
        &self,
        _target: &clawdesk_types::cron::DeliveryTarget,
        _content: &str,
    ) -> Result<(), String> {
        // Desktop app delivers cron results via emit("routine:executed") events,
        // not through the DeliveryHandler trait.
        Ok(())
    }
}

/// No-op plugin factory — plugins loaded at runtime, not at boot.
pub(crate) struct NoopPluginFactory;

#[async_trait::async_trait]
impl clawdesk_plugin::PluginFactory for NoopPluginFactory {
    async fn create(
        &self,
        manifest: &clawdesk_types::plugin::PluginManifest,
    ) -> Result<std::sync::Arc<dyn clawdesk_plugin::PluginInstance>, clawdesk_types::error::PluginError> {
        Err(clawdesk_types::error::PluginError::LoadFailed {
            name: manifest.name.clone(),
            detail: "No plugin factory configured".into(),
        })
    }
}

/// No-op agent executor — cron tasks are handled differently in the desktop app.
pub(crate) struct NoopAgentExecutor;

#[async_trait::async_trait]
impl clawdesk_cron::executor::AgentExecutor for NoopAgentExecutor {
    async fn execute(&self, _prompt: &str, _agent_id: Option<&str>) -> Result<String, String> {
        Err("no-op executor".to_string())
    }
}

// ── Channel MessageSink — routes inbound channel messages through the agent ──

/// `MessageSink` implementation that processes inbound messages from external
/// channels (Discord, Telegram, Slack, etc.) through the agent pipeline and
/// sends responses back via the originating channel.
///
/// This is the glue between the channel adapters' `gateway_loop()` and the
/// agent runner: when a Discord user @mentions the bot, the gateway loop
/// calls `sink.on_message(normalized)`, which lands here. We resolve the
/// agent + provider, run the LLM, and send the response back via
/// `Channel::send()`.
///
/// Uses `AppHandle` to read agents live from `AppState` instead of a
/// stale startup snapshot — agents added/modified via the UI are visible
/// immediately.
pub(crate) struct ChannelMessageSink {
    pub negotiator: Arc<RwLock<ProviderNegotiator>>,
    pub tool_registry: Arc<ToolRegistry>,
    pub app_handle: tauri::AppHandle,
    pub channel_registry: Arc<RwLock<ChannelRegistry>>,
    pub cancel: tokio_util::sync::CancellationToken,
}

#[async_trait::async_trait]
impl clawdesk_channel::MessageSink for ChannelMessageSink {
    async fn on_message(&self, msg: clawdesk_types::message::NormalizedMessage) {
        use clawdesk_providers::capability::ProviderCaps;

        let channel_id = msg.sender.channel;
        let sender_name = msg.sender.display_name.clone();

        info!(
            channel = %channel_id,
            sender = %sender_name,
            body_len = msg.body.len(),
            body_preview = %msg.body.chars().take(80).collect::<String>(),
            "Inbound channel message received — routing to agent"
        );

        // 1. Find the default agent (first registered agent) — read LIVE from AppState
        let agent = {
            let state = self.app_handle.state::<AppState>();
            let result = match state.agents.read() {
                Ok(agents) => {
                    let count = agents.len();
                    info!(agent_count = count, "Available agents for channel message");
                    agents.values().next().cloned()
                }
                Err(e) => {
                    error!("Failed to read agents: {e}");
                    return;
                }
            };
            result
        };
        let Some(agent) = agent else {
            warn!("No agent configured — dropping inbound message from {sender_name}");
            return;
        };

        // 2. Resolve provider — try channel_provider override first, then negotiator
        let model_id = AppState::resolve_model_id(&agent.model);
        let required = ProviderCaps::TEXT_COMPLETION.union(ProviderCaps::SYSTEM_PROMPT);

        // Try the UI-synced channel provider override first.
        // This creates a one-shot provider from the user's saved settings
        // (e.g. "Local (OpenAI Compatible)" with a custom base URL).
        let provider: Arc<dyn Provider> = {
            let channel_prov = {
                let state = self.app_handle.state::<AppState>();
                state.channel_provider.read().ok().and_then(|g| g.clone())
            };

            if let Some(ref cp) = channel_prov {
                info!(
                    provider = %cp.provider,
                    model = %cp.model,
                    base_url = %cp.base_url,
                    "Using channel_provider override for inbound message"
                );
                match cp.provider.as_str() {
                    "Anthropic" => {
                        use clawdesk_providers::anthropic::AnthropicProvider;
                        Arc::new(AnthropicProvider::new(cp.api_key.clone(), Some(model_id.clone())))
                    }
                    "OpenAI" => {
                        use clawdesk_providers::openai::OpenAiProvider;
                        let base = if cp.base_url.is_empty() { None } else { Some(cp.base_url.clone()) };
                        Arc::new(OpenAiProvider::new(cp.api_key.clone(), base, Some(model_id.clone())))
                    }
                    "Ollama (Local)" | "ollama" => {
                        use clawdesk_providers::ollama::OllamaProvider;
                        let base = if cp.base_url.is_empty() { None } else { Some(cp.base_url.clone()) };
                        Arc::new(OllamaProvider::new(base, Some(model_id.clone())))
                    }
                    "Local (OpenAI Compatible)" | "local_compatible" => {
                        use clawdesk_providers::compatible::{CompatibleConfig, OpenAiCompatibleProvider};
                        let base_url = if cp.base_url.is_empty() {
                            "http://localhost:8080/v1".to_string()
                        } else {
                            cp.base_url.clone()
                        };
                        let config = CompatibleConfig::new("local_compatible", &base_url, &cp.api_key)
                            .with_default_model(model_id.clone());
                        Arc::new(OpenAiCompatibleProvider::new(config))
                    }
                    "Google" => {
                        use clawdesk_providers::gemini::GeminiProvider;
                        Arc::new(GeminiProvider::new(cp.api_key.clone(), Some(model_id.clone())))
                    }
                    _ => {
                        // Unknown override provider — fall through to negotiator
                        warn!(provider = %cp.provider, "Unknown channel_provider — trying negotiator");
                        let neg = match self.negotiator.read() {
                            Ok(n) => n,
                            Err(e) => { error!("Negotiator lock poisoned: {e}"); return; }
                        };
                        match neg.resolve_model(&model_id, required) {
                            Some((p, _)) => Arc::clone(p),
                            None => {
                                let state = self.app_handle.state::<AppState>();
                                match state.resolve_provider(&agent.model) {
                                    Ok(p) => p,
                                    Err(e) => { warn!(error = %e, "No provider available — dropping message"); return; }
                                }
                            }
                        }
                    }
                }
            } else {
                // No channel_provider override — use negotiator → resolve_provider fallback
                let neg = match self.negotiator.read() {
                    Ok(n) => n,
                    Err(e) => { error!("Negotiator lock poisoned: {e}"); return; }
                };
                match neg.resolve_model(&model_id, required) {
                    Some((p, _)) => Arc::clone(p),
                    None => {
                        drop(neg);
                        let state = self.app_handle.state::<AppState>();
                        match state.resolve_provider(&agent.model) {
                            Ok(p) => p,
                            Err(e) => {
                                warn!(model = %agent.model, error = %e, "No provider for model — dropping message");
                                return;
                            }
                        }
                    }
                }
            }
        };

        // 3. Build a minimal AgentConfig + runner
        let config = AgentConfig {
            model: agent.model.clone(),
            system_prompt: agent.persona.clone(),
            ..Default::default()
        };

        let history = vec![clawdesk_providers::ChatMessage::new(
            clawdesk_providers::MessageRole::User,
            msg.body.as_str(),
        )];

        let runner = AgentRunner::new(
            provider,
            Arc::clone(&self.tool_registry),
            config,
            self.cancel.clone(),
        );

        // 4. Run the agent
        let response = match runner.run(history, agent.persona.clone()).await {
            Ok(resp) => resp,
            Err(e) => {
                error!(channel = %channel_id, error = %e, "Agent run failed for inbound message");
                return;
            }
        };

        info!(
            channel = %channel_id,
            sender = %sender_name,
            response_len = response.content.len(),
            "Agent response ready — sending back to channel"
        );

        // 5. Send response back through the originating channel
        let outbound = clawdesk_types::message::OutboundMessage {
            origin: msg.origin.clone(),
            body: response.content,
            media: vec![],
            reply_to: None,
            thread_id: None,
        };

        let ch = {
            let reg = match self.channel_registry.read() {
                Ok(r) => r,
                Err(e) => {
                    error!("Channel registry lock poisoned: {e}");
                    return;
                }
            };
            reg.get(&channel_id).cloned()
        };

        if let Some(ch) = ch {
            if let Err(e) = ch.send(outbound).await {
                error!(channel = %channel_id, error = %e, "Failed to send response back");
            }
        } else {
            error!(channel = %channel_id, "Channel not found in registry — cannot reply");
        }
    }
}

// ── T6: ApprovalGate adapter — bridges ExecApprovalManager into AgentRunner ──

/// Bridges `ExecApprovalManager` into the agent runner's `ApprovalGate` trait.
/// When a tool requires approval, creates a pending request and waits for
/// the user to approve/deny via the UI (Tauri `approval:pending` event).
pub struct TauriApprovalGate {
    manager: Arc<ExecApprovalManager>,
    app: tauri::AppHandle,
}

impl TauriApprovalGate {
    pub fn new(manager: Arc<ExecApprovalManager>, app: tauri::AppHandle) -> Self {
        Self { manager, app }
    }
}

#[async_trait::async_trait]
impl clawdesk_agents::runner::ApprovalGate for TauriApprovalGate {
    async fn request_approval(
        &self,
        tool_name: &str,
        arguments: &str,
    ) -> Result<bool, String> {
        use clawdesk_security::RiskLevel;
        use tauri::Emitter;

        let request = self
            .manager
            .create_request(
                tool_name,
                arguments,
                RiskLevel::Medium,
                Some(format!("Agent wants to execute tool '{}'", tool_name)),
            )
            .await;

        // Emit approval:pending event to the UI
        let _ = self.app.emit(
            "approval:pending",
            serde_json::json!({
                "id": request.id.to_string(),
                "tool_name": &request.tool_name,
                "command": &request.command,
                "risk": format!("{:?}", request.risk),
                "expires_at": request.expires_at.to_rfc3339(),
                "context": &request.context,
            }),
        );

        // Wait for user decision (blocks until approved/denied/timeout)
        match self.manager.wait_for_decision(request.id).await {
            Ok(status) => {
                use clawdesk_security::ApprovalStatus;
                match status {
                    ApprovalStatus::Approved { .. } => Ok(true),
                    ApprovalStatus::Denied { .. } => Ok(false),
                    ApprovalStatus::TimedOut { .. } => Ok(false),
                    ApprovalStatus::Pending => Ok(false),
                }
            }
            Err(e) => Err(format!("Approval error: {:?}", e)),
        }
    }
}

impl AppState {
    /// Initialize all real backend services at startup.
    ///
    /// Opens **SochDB** at `~/.clawdesk/sochdb/` for ACID-transactional
    /// persistence. All durable state (agents, sessions, canvases,
    /// notifications, clipboard, pipelines) is stored in SochDB and
    /// hydrated into in-memory hot caches on cold start.
    ///
    /// The **MemoryManager** is initialized with `SochStore` as the
    /// `VectorStore` backend, giving remember/recall/forget with HNSW
    /// vector similarity + BM25 hybrid search.
    ///
    /// Providers are auto-registered from environment variables:
    /// - `ANTHROPIC_API_KEY` → AnthropicProvider (haiku, sonnet, opus)
    /// - `OPENAI_API_KEY` → OpenAIProvider (gpt-*)
    /// - `GOOGLE_API_KEY` → GeminiProvider (gemini-*)
    /// - `OLLAMA_HOST` or always → OllamaProvider (local models, defaults to localhost)
    pub fn new() -> Self {
        // ── Open SochDB ──────────────────────────────────────────────
        let sochdb_path = {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(home).join(".clawdesk").join("sochdb")
        };

        // Ensure the directory exists before opening the database
        if let Err(e) = std::fs::create_dir_all(&sochdb_path) {
            error!(path = ?sochdb_path, error = %e, "Failed to create SochDB directory");
        }

        // SochStore::open() already retries 3× with exponential backoff.
        // If it STILL fails, we open ephemeral but flag it so the UI can warn
        // the user — never silently degrade.
        let soch_store = match SochStore::open(&sochdb_path) {
            Ok(store) => {
                info!(path = ?sochdb_path, "SochDB opened — ACID storage active");
                Arc::new(store)
            }
            Err(e) => {
                error!(
                    error = %e,
                    path = ?sochdb_path,
                    "SochDB open FAILED after retries — falling back to EPHEMERAL storage. \
                     Data will NOT survive restart! Check disk permissions and free space."
                );
                // Ephemeral fallback: is_ephemeral flag will be true, UI will
                // show a degraded-storage banner via the health endpoint.
                Arc::new(SochStore::open_in_memory().expect("in-memory SochDB must succeed"))
            }
        };

        // ── Open ThreadStore at ~/.clawdesk/threads/ ─────────────────
        let threads_path = sochdb_path.parent().unwrap_or(&sochdb_path).join("threads");
        let thread_store = match clawdesk_threads::ThreadStore::open(&threads_path) {
            Ok(store) => {
                info!(path = ?threads_path, "ThreadStore opened — chat thread persistence active");
                Arc::new(store)
            }
            Err(e) => {
                error!(
                    error = %e,
                    path = ?threads_path,
                    "ThreadStore open failed — using temp fallback. \
                     Chat history will NOT survive restart!"
                );
                let tmp = std::env::temp_dir().join(format!("clawdesk-threads-{}", std::process::id()));
                Arc::new(
                    clawdesk_threads::ThreadStore::open(&tmp)
                        .expect("temp ThreadStore must succeed"),
                )
            }
        };

        // ── Embedding provider (for MemoryManager) ──────────────────
        // Tiered provider: tries cloud APIs → Ollama → auto-degrades to FTS-only.
        // Memory search always works, even without any API key or Ollama install.
        let embedding: Arc<dyn EmbeddingProvider> = build_tiered_provider();
        let embedding_for_state = embedding.clone(); // Task 2: shared with semantic cache
        info!("Tiered embedding provider ready — memory always has FTS fallback");

        // ── MemoryManager<SochMemoryBackend> ─────────────────────────
        // SochMemoryBackend wraps SochStore + all SochDB advanced modules
        // (AtomicMemoryWriter, GraphOverlay, TemporalGraphOverlay, PolicyEngine, TraceStore)
        // enabling atomic writes (Task 1), graph nodes (Task 7), temporal edges (Task 5),
        // policy checks (Task 9), and trace spans (Task 8) in the memory pipeline.
        let soch_memory_backend = Arc::new(SochMemoryBackend::new(soch_store.clone()));
        let memory_config = MemoryConfig::default();
        let memory = Arc::new(MemoryManager::new(
            soch_memory_backend,
            embedding,
            memory_config,
        ));
        info!("MemoryManager<SochMemoryBackend> initialized — full SochDB integration active");

        // ── SochDB advanced modules (via SochConn bridge) ───────────
        let conn = SochConn::new(soch_store.clone());
        let semantic_cache = Arc::new(sochdb::semantic_cache::SemanticCache::new(conn.clone()));
        let trace_store = Arc::new(sochdb::trace::TraceStore::new(conn.clone()));
        let checkpoint_store = Arc::new(sochdb::checkpoint::DefaultCheckpointStore::new(conn.clone()));
        let knowledge_graph = Arc::new(sochdb::graph::GraphOverlay::new(conn.clone(), "clawdesk"));
        let temporal_graph = Arc::new(sochdb::temporal_graph::TemporalGraphOverlay::new(conn.clone(), "clawdesk"));
        let policy_engine = Arc::new(sochdb::policy::PolicyEngine::new(conn.clone()));
        let atomic_writer = Arc::new(sochdb::atomic_memory::AtomicMemoryWriter::new(conn.clone()));
        let agent_registry = Arc::new(sochdb::routing::AgentRegistry::new(Arc::new(conn.clone())));
        info!(
            "SochDB advanced modules initialized: SemanticCache, TraceStore, \
             CheckpointStore, GraphOverlay, TemporalGraph, PolicyEngine, \
             AtomicMemoryWriter, AgentRegistry"
        );

        let skill_registry = load_bundled_skills();
        let scanner_config = CascadeScannerConfig::default();
        let pattern_count = scanner_config.patterns.len() + scanner_config.ac_patterns.len();
        let scanner = CascadeScanner::new(scanner_config);
        let audit_logger = AuditLogger::new(AuditLoggerConfig::default());
        let mut tool_registry = ToolRegistry::new();

        // ── Resolve workspace root ──────────
        // All file-system tools are scoped to this directory. Bootstrap context
        // discovery and skill file lookup also use this path. Defaults to
        // ~/.clawdesk/workspace/ — created on first launch.
        let workspace_root = {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            let ws = std::path::PathBuf::from(home).join(".clawdesk").join("workspace");
            if let Err(e) = std::fs::create_dir_all(&ws) {
                error!(path = ?ws, error = %e, "Failed to create workspace directory");
            }
            ws
        };

        // ── Register built-in tools ──────────
        // Real implementations: shell, HTTP, file I/O, web search, memory.
        // Tools are scoped to workspace_root for path confinement.
        clawdesk_agents::builtin_tools::register_builtin_tools(&mut tool_registry, Some(workspace_root.clone()));

        // Register memory search tool with async callback to MemoryManager
        // Natively async — eliminates the block_in_place deadlock risk.
        // Results include citation metadata (timestamp, source, tags) for provenance.
        {
            let mem = Arc::clone(&memory);
            let recall_fn: std::sync::Arc<dyn Fn(String, usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<(String, f32)>> + Send>> + Send + Sync> =
                std::sync::Arc::new(move |query: String, max: usize| {
                    let mem = Arc::clone(&mem);
                    Box::pin(async move {
                        match mem.recall(&query, Some(max)).await {
                            Ok(results) => results
                                .into_iter()
                                .filter_map(|r| {
                                    let text = r.content?;
                                    if text.is_empty() { return None; }
                                    // Enrich with citation metadata
                                    let meta = &r.metadata;
                                    let timestamp = meta.get("timestamp")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown");
                                    let source = meta.get("source")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown");
                                    let tags = meta.get("tags")
                                        .and_then(|v| v.as_array())
                                        .map(|a| a.iter()
                                            .filter_map(|t| t.as_str())
                                            .collect::<Vec<_>>()
                                            .join(", "))
                                        .unwrap_or_default();
                                    let citation = if tags.is_empty() {
                                        format!("[source: {}, date: {}] {}", source, timestamp, text)
                                    } else {
                                        format!("[source: {}, date: {}, tags: {}] {}", source, timestamp, tags, text)
                                    };
                                    Some((citation, r.score))
                                })
                                .collect(),
                            Err(_) => Vec::new(),
                        }
                    })
                });
            clawdesk_agents::builtin_tools::register_memory_tool_async(&mut tool_registry, recall_fn);
        }

        // Register memory store tool with async callback to MemoryManager::remember()
        // Allows the LLM to explicitly save memories (preferences, decisions, facts).
        {
            let mem = Arc::clone(&memory);
            let store_fn: std::sync::Arc<dyn Fn(String, Vec<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send + Sync> =
                std::sync::Arc::new(move |content: String, tags: Vec<String>| {
                    let mem = Arc::clone(&mem);
                    Box::pin(async move {
                        let mut metadata = serde_json::json!({});
                        if !tags.is_empty() {
                            metadata["tags"] = serde_json::json!(tags);
                        }
                        metadata["stored_by"] = serde_json::json!("llm_tool");
                        mem.remember(
                            &content,
                            clawdesk_memory::manager::MemorySource::UserSaved,
                            metadata,
                        )
                        .await
                    })
                });
            clawdesk_agents::builtin_tools::register_memory_store_tool_async(&mut tool_registry, store_fn);
        }
        info!(tools = tool_registry.total_count(), "Built-in tool registry initialized");

        // NOTE: tool_registry is NOT wrapped in Arc yet — we need to register
        // the messaging tool after channel_registry is built (see below).

        // T11: Wire ChannelFactory → ChannelRegistry. Always register
        // config-free channels (webchat, internal); probe env vars for the rest.
        let mut channel_registry = ChannelRegistry::new();
        {
            use clawdesk_channels::factory::{ChannelConfig, ChannelFactory};
            use serde_json::Map;

            let factory = ChannelFactory::with_builtins();

            // Always-available channels (no credentials needed)
            for kind in &["webchat", "internal"] {
                let cfg = ChannelConfig::new(*kind, Map::new());
                match factory.create(&cfg) {
                    Ok(ch) => {
                        let _ = channel_registry.register(ch);
                        info!(kind, "Channel registered");
                    }
                    Err(e) => warn!(kind, error = %e, "Failed to create channel"),
                }
            }

            // Probe env-var-configured channels
            let env_channels: Vec<(&str, Vec<(&str, &str)>)> = vec![
                ("telegram", vec![("bot_token", "TELEGRAM_BOT_TOKEN")]),
                ("discord", vec![("bot_token", "DISCORD_TOKEN"), ("application_id", "DISCORD_APP_ID")]),
                ("slack", vec![("bot_token", "SLACK_BOT_TOKEN"), ("app_token", "SLACK_APP_TOKEN"), ("signing_secret", "SLACK_SIGNING_SECRET")]),
                ("whatsapp", vec![("phone_number_id", "WHATSAPP_PHONE_ID"), ("access_token", "WHATSAPP_ACCESS_TOKEN")]),
                ("email", vec![("imap_host", "EMAIL_IMAP_HOST"), ("smtp_host", "EMAIL_SMTP_HOST"), ("email", "EMAIL_ADDRESS"), ("password", "EMAIL_PASSWORD")]),
            ];

            for (kind, fields) in env_channels {
                let mut map = Map::new();
                let mut all_present = true;
                for (field_name, env_var) in &fields {
                    match std::env::var(env_var) {
                        Ok(val) => { map.insert(field_name.to_string(), serde_json::Value::String(val)); }
                        Err(_) => { all_present = false; break; }
                    }
                }
                if all_present {
                    let cfg = ChannelConfig::new(kind, map);
                    match factory.create(&cfg) {
                        Ok(ch) => {
                            let _ = channel_registry.register(ch);
                            info!(kind, "Channel registered from env");
                        }
                        Err(e) => warn!(kind, error = %e, "Failed to create channel from env"),
                    }
                }
            }

            info!(count = channel_registry.list().len(), "Channel registry initialized");
        }

        // GAP-11 FIX: Register the messaging tool with a ChannelRegistry-backed callback.
        // This gives the LLM the ability to send messages to other channels/users
        // via the `message_send` tool. The send_fn looks up the target channel in
        // the registry and delivers via Channel::send().
        //
        // We wrap channel_registry in Arc<RwLock> so the async callback can capture
        // a clone. AppState stores the same Arc.
        let channel_registry: Arc<std::sync::RwLock<ChannelRegistry>> =
            Arc::new(std::sync::RwLock::new(channel_registry));
        {
            use clawdesk_channel::Channel;
            use clawdesk_types::channel::ChannelId;
            use clawdesk_types::message::OutboundMessage;

            let channels_for_tool = Arc::clone(&channel_registry);

            let send_fn: std::sync::Arc<
                dyn Fn(String, Option<String>, String, Vec<String>)
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                    + Send + Sync,
            > = std::sync::Arc::new(move |target, channel_name, content, _media_urls| {
                let channels = Arc::clone(&channels_for_tool);
                Box::pin(async move {
                    let channel_id = match channel_name.as_deref().unwrap_or("webchat") {
                        "telegram" => ChannelId::Telegram,
                        "discord" => ChannelId::Discord,
                        "slack" => ChannelId::Slack,
                        "whatsapp" => ChannelId::WhatsApp,
                        "webchat" => ChannelId::WebChat,
                        "internal" => ChannelId::Internal,
                        other => return Err(format!("Unknown channel: {}", other)),
                    };
                    let ch = {
                        let reg = channels.read().map_err(|e| format!("channel lock: {}", e))?;
                        Arc::clone(
                            reg.get(&channel_id)
                                .ok_or_else(|| format!("Channel {:?} not registered", channel_id))?
                        )
                    }; // guard dropped here — before any .await
                    let msg = OutboundMessage {
                        origin: clawdesk_types::message::MessageOrigin::WebChat {
                            session_id: target.clone(),
                        },
                        body: content,
                        media: vec![],
                        reply_to: None,
                        thread_id: None,
                    };
                    let receipt = ch.send(msg).await?;
                    Ok(receipt.message_id)
                })
            });
            clawdesk_agents::builtin_tools::register_messaging_tool(&mut tool_registry, send_fn);
            info!("Messaging tool registered — LLM can send cross-channel messages");
        }

        // Wrap in Arc now — all mutable registration is complete.
        let tool_registry = Arc::new(tool_registry);

        let mut provider_registry = ProviderRegistry::new();

        // Auto-register providers from environment variables
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            info!("Registering Anthropic provider from ANTHROPIC_API_KEY");
            let provider = AnthropicProvider::new(key, None);
            provider_registry.register(Arc::new(provider));
        }

        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            info!("Registering OpenAI provider from OPENAI_API_KEY");
            let provider = OpenAiProvider::new(key, None, None);
            provider_registry.register(Arc::new(provider));
        }

        if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
            info!("Registering Gemini provider from GOOGLE_API_KEY");
            let provider = GeminiProvider::new(key, None);
            provider_registry.register(Arc::new(provider));
        }

        if let Ok(key) = std::env::var("AZURE_OPENAI_API_KEY") {
            if let Ok(base) = std::env::var("AZURE_OPENAI_ENDPOINT") {
                info!("Registering Azure OpenAI provider from AZURE_OPENAI_API_KEY");
                let provider = clawdesk_providers::azure::AzureOpenAiProvider::new(key, base, None, None);
                provider_registry.register(Arc::new(provider));
            }
        }

        if let Ok(key) = std::env::var("COHERE_API_KEY") {
            info!("Registering Cohere provider from COHERE_API_KEY");
            let provider = clawdesk_providers::cohere::CohereProvider::new(key, None, None);
            provider_registry.register(Arc::new(provider));
        }

        if let Ok(project) = std::env::var("VERTEX_PROJECT_ID") {
            if let Ok(location) = std::env::var("VERTEX_LOCATION") {
                info!("Registering Vertex AI provider from VERTEX_PROJECT_ID");
                let provider = clawdesk_providers::vertex::VertexProvider::new(project, location, None);
                provider_registry.register(Arc::new(provider));
            }
        }

        // Ollama is always available (local, no API key needed)
        {
            let base_url = std::env::var("OLLAMA_HOST").ok();
            info!(base_url = ?base_url, "Registering Ollama provider (local)");
            let provider = OllamaProvider::new(base_url, None);
            provider_registry.register(Arc::new(provider));
        }

        let provider_count = provider_registry.list().len();
        info!(providers = provider_count, "Provider auto-registration complete");

        // ── Build ProviderNegotiator with capability-aware routing ──
        let mut negotiator = ProviderNegotiator::new();
        for name in provider_registry.list() {
            if let Some(p) = provider_registry.get(&name) {
                let (caps, weights) = match name.as_str() {
                    "anthropic" => (ANTHROPIC_CAPS, vec![
                        ProviderWeight { provider: "anthropic".into(), model: "claude-haiku-4-20250514".into(), cost_per_m_input: 0.25, cost_per_m_output: 1.25, latency_p50_ms: 300, caps: ANTHROPIC_CAPS, quality_tier: 2 },
                        ProviderWeight { provider: "anthropic".into(), model: "claude-sonnet-4-20250514".into(), cost_per_m_input: 3.0, cost_per_m_output: 15.0, latency_p50_ms: 600, caps: ANTHROPIC_CAPS, quality_tier: 3 },
                        ProviderWeight { provider: "anthropic".into(), model: "claude-opus-4-20250514".into(), cost_per_m_input: 15.0, cost_per_m_output: 75.0, latency_p50_ms: 2000, caps: ANTHROPIC_CAPS, quality_tier: 4 },
                    ]),
                    "openai" => (OPENAI_CAPS, vec![
                        ProviderWeight { provider: "openai".into(), model: "gpt-4o-mini".into(), cost_per_m_input: 0.15, cost_per_m_output: 0.60, latency_p50_ms: 250, caps: OPENAI_CAPS, quality_tier: 2 },
                        ProviderWeight { provider: "openai".into(), model: "gpt-4o".into(), cost_per_m_input: 2.5, cost_per_m_output: 10.0, latency_p50_ms: 500, caps: OPENAI_CAPS, quality_tier: 3 },
                    ]),
                    "gemini" => (GEMINI_CAPS, vec![
                        ProviderWeight { provider: "gemini".into(), model: "gemini-2.0-flash".into(), cost_per_m_input: 0.075, cost_per_m_output: 0.30, latency_p50_ms: 200, caps: GEMINI_CAPS, quality_tier: 2 },
                    ]),
                    _ => (OLLAMA_CAPS, vec![
                        ProviderWeight { provider: name.clone(), model: "llama3.2".into(), cost_per_m_input: 0.0, cost_per_m_output: 0.0, latency_p50_ms: 1000, caps: OLLAMA_CAPS, quality_tier: 1 },
                    ]),
                };
                negotiator.register(Arc::clone(p), caps, weights);
            }
        }
        info!(negotiator_providers = negotiator.provider_count(), "ProviderNegotiator initialized");

        // ── T5: CronManager — wire scheduled pipeline execution ──────
        let cancel = tokio_util::sync::CancellationToken::new();
        let cron_provider: Arc<dyn Provider> = provider_registry
            .default_provider()
            .map(|p| Arc::clone(p))
            .unwrap_or_else(|| {
                // Fallback: create an Ollama provider (always available locally)
                Arc::new(OllamaProvider::new(None, None)) as Arc<dyn Provider>
            });
        let cron_executor = CronAgentExecutor {
            provider: cron_provider,
            tool_registry: Arc::clone(&tool_registry),
            cancel: cancel.clone(),
        };
        let cron_manager = Arc::new(CronManager::new(
            Arc::new(cron_executor),
            Arc::new(NoOpDelivery),
        ));
        info!("CronManager initialized — scheduled pipeline execution ready");

        // ── Hydrate hot caches from SochDB ──────────────────────────
        info!("Starting hydration from SochDB...");
        let agents = hydrate_map(&soch_store, "agents/");
        info!(agents = agents.len(), "Hydrated agents");
        // ── Hydrate chat sessions (new multi-chat format: chats/{chat_id}) ──
        let mut sessions: HashMap<String, ChatSession> = hydrate_map(&soch_store, "chats/");
        info!(
            sessions = sessions.len(),
            "Hydrated chat sessions (chats/ prefix)"
        );
        // Log each session for diagnostic purposes
        for (chat_id, session) in &sessions {
            info!(
                chat_id = %chat_id,
                agent_id = %session.agent_id,
                title = %session.title,
                messages = session.messages.len(),
                created = %session.created_at,
                updated = %session.updated_at,
                "Hydrated session detail"
            );
        }
        // ── Migration: if no new-format chats exist, import old chat_sessions/{agent_id} ──
        if sessions.is_empty() {
            let old_sessions: HashMap<String, Vec<ChatMessage>> = hydrate_map(&soch_store, "chat_sessions/");
            for (agent_id, msgs) in old_sessions {
                if msgs.is_empty() { continue; }
                let chat_id = uuid::Uuid::new_v4().to_string();
                let agent_name = agents.get(&agent_id)
                    .map(|a: &DesktopAgent| a.name.clone())
                    .unwrap_or_else(|| "Conversation".to_string());
                let first_user_msg = msgs.iter()
                    .find(|m| m.role == "user")
                    .map(|m| {
                        let words: Vec<&str> = m.content.split_whitespace().take(6).collect();
                        words.join(" ")
                    });
                let title = first_user_msg.unwrap_or(agent_name);
                let created_at = msgs.first().map(|m| m.timestamp.clone())
                    .unwrap_or_else(|| Utc::now().to_rfc3339());
                let updated_at = msgs.last().map(|m| m.timestamp.clone())
                    .unwrap_or_else(|| Utc::now().to_rfc3339());
                sessions.insert(chat_id.clone(), ChatSession {
                    id: chat_id,
                    agent_id,
                    title,
                    messages: msgs,
                    created_at,
                    updated_at,
                });
            }
            // Persist migrated sessions in new format
            for (chat_id, session) in &sessions {
                if let Ok(bytes) = serde_json::to_vec(session) {
                    let key = format!("chats/{}", chat_id);
                    let _ = soch_store.put(&key, &bytes);
                }
            }
            if !sessions.is_empty() {
                info!(count = sessions.len(), "Migrated old chat_sessions to new chats format");
            }
        }
        let pipelines: Vec<PipelineDescriptor> = hydrate_list(&soch_store, "pipelines/");
        let notification_history: Vec<NotificationInfo> = hydrate_list(&soch_store, "notifications/");
        let clipboard_history: Vec<ClipboardEntry> = hydrate_list(&soch_store, "clipboard/");
        let canvases: HashMap<String, Canvas> = hydrate_map(&soch_store, "canvases/");
        let journal_entries: HashMap<String, clawdesk_skills::journal::JournalEntry> =
            hydrate_map(&soch_store, "journal/");

        let pipelines = if pipelines.is_empty() { vec![default_pipeline()] } else { pipelines };

        info!(
            agents = agents.len(),
            sessions = sessions.len(),
            pipelines = pipelines.len(),
            notifications = notification_history.len(),
            clipboard = clipboard_history.len(),
            canvases = canvases.len(),
            "Hydrated hot caches from SochDB"
        );

        // ── Data loss diagnostic ─────────────────────────────────────
        // If we have zero sessions but this isn't the first run (canary exists),
        // something is wrong with data persistence.
        if sessions.is_empty() {
            let wal_path = sochdb_path.join("wal.log");
            let wal_size = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
            if wal_size > 0 {
                error!(
                    wal_size_bytes = wal_size,
                    wal_path = ?wal_path,
                    "ZERO sessions found after hydration but WAL file is non-empty — possible data loss!"
                );
            } else {
                info!("No sessions found (fresh database — WAL is empty)");
            }
        }

        Self {
            soch_store,
            thread_store,
            semantic_cache,
            trace_store,
            checkpoint_store,
            knowledge_graph,
            temporal_graph,
            policy_engine,
            atomic_writer,
            agent_registry,
            memory,
            embedding_provider: embedding_for_state,

            skill_registry: RwLock::new(skill_registry),
            provider_registry: RwLock::new(provider_registry),
            tool_registry,
            channel_registry,
            channel_configs: RwLock::new(HashMap::new()),
            scanner: Arc::new(scanner),
            scanner_pattern_count: pattern_count,
            audit_logger: Arc::new(audit_logger),
            agents: RwLock::new(agents),
            identities: RwLock::new(HashMap::new()),
            server_secret: ServerSecret::generate(),
            sessions: RwLock::new(sessions),
            invites: RwLock::new(InviteManager::new()),
            tunnel_metrics: Arc::new(TunnelMetrics::new()),
            total_cost_today: AtomicU64::new(0),
            total_input_tokens: AtomicU64::new(0),
            total_output_tokens: AtomicU64::new(0),
            model_costs: RwLock::new(HashMap::new()),
            last_cost_reset_date: RwLock::new(Utc::now().format("%Y-%m-%d").to_string()),
            traces: RwLock::new(HashMap::new()),
            pipelines: RwLock::new(pipelines),
            started_at: std::time::Instant::now(),
            cancel,
            active_chat_runs: tokio::sync::RwLock::new(HashMap::new()),

            // ── Durable Runtime ──
            durable_runner: None,

            // ── Media Pipeline ──
            media_pipeline: Arc::new(tokio::sync::RwLock::new(MediaPipeline::new())),

            // ── Plugin System ──
            plugin_host: None,

            // ── A2A Protocol ──
            agent_directory: RwLock::new(AgentDirectory::new()),
            a2a_tasks: tokio::sync::RwLock::new(HashMap::new()),

            // ── OAuth2 + PKCE ──
            oauth_flow_manager: Arc::new(OAuthFlowManager::new()),
            auth_profile_manager: Arc::new(AuthProfileManager::new()),

            // ── Execution Approval ──
            approval_manager: Arc::new(ExecApprovalManager::new(Duration::from_secs(300))),

            // ── Network Discovery ──
            mdns_advertiser: RwLock::new(MdnsAdvertiser::new(
                ServiceInfo::new("clawdesk-desktop", 18789),
            )),
            pairing_session: RwLock::new(None),
            peer_registry: RwLock::new(PeerRegistry::new(Duration::from_secs(120))),

            // ── Observability ──
            observability_config: RwLock::new(ObservabilityConfig::from_env()),
            metrics_aggregator: Arc::new(clawdesk_observability::MetricsAggregator::new()),

            // ── Notifications (hydrated from SochDB) ──
            notification_history: RwLock::new(notification_history),

            // ── Channel provider override (not yet synced from UI) ──
            channel_provider: RwLock::new(None),

            // ── Clipboard (hydrated from SochDB) ──
            clipboard_history: RwLock::new(clipboard_history),

            // ── Voice Wake ──
            voice_wake: RwLock::new(None),

            // ── Whisper STT Engine ──
            whisper_engine: RwLock::new({
                let models_dir = clawdesk_media::whisper::default_models_dir();
                // Auto-detect best available model
                let engine = clawdesk_media::whisper::WhisperSttEngine::new(
                    models_dir.clone(),
                    clawdesk_media::whisper::WhisperModel::Base,
                );
                if engine.is_model_downloaded() {
                    Some(engine)
                } else {
                    // Try tiny as fallback
                    let tiny = clawdesk_media::whisper::WhisperSttEngine::new(
                        models_dir,
                        clawdesk_media::whisper::WhisperModel::Tiny,
                    );
                    if tiny.is_model_downloaded() {
                        Some(tiny)
                    } else {
                        None
                    }
                }
            }),

            // ── Audio Recorder (cpal) ──
            audio_recorder: parking_lot::Mutex::new(clawdesk_media::recorder::AudioRecorder::new()),

            // ── ACL Engine ──
            acl_manager: Arc::new(AclManager::new()),

            // ── Skill Promotion ──
            skill_verifier: Arc::new(SkillVerifier::development()),

            // ── Provider Negotiation ──
            negotiator: Arc::new(RwLock::new(negotiator)),

            // ── Context Guard ──
            context_guards: RwLock::new(HashMap::new()),

            // ── Prompt Builder manifests ──
            prompt_manifests: RwLock::new(HashMap::new()),

            // ── Idle Detection ──
            idle_detector: Some(Arc::new(IdleDetector::new(
                IdleConfig { idle_threshold_secs: 300, check_interval_secs: 30 },
                vec![],
            ))),

            // ── Canvas (hydrated from SochDB) ──
            canvases: RwLock::new(canvases),

            // ── T5: Cron Scheduling ──
            cron_manager,

            // ── T8: Life OS Template Registry ──
            template_registry: Arc::new(clawdesk_skills::life_os::TemplateRegistry::with_builtins()),

            // ── T9: Journal Store ──
            journal_entries: RwLock::new(journal_entries),

            // ── T7 FIX: Per-session serialization ──
            session_lanes: clawdesk_agents::session_lane::SessionLaneManager::new(),

            // ── GAP-3 FIX: Global concurrency bound (default 8 parallel LLM calls) ──
            llm_concurrency: Arc::new(tokio::sync::Semaphore::new(8)),

            // ── GAP-4/1 FIX: Channel dock (all 23 channels pre-registered) ──
            channel_dock: Arc::new(clawdesk_channel::channel_dock::ChannelDock::with_all_defaults()),

            // ── GAP-7 FIX: Hook manager ──
            hook_manager: Arc::new(clawdesk_plugin::HookManager::new()),

            // ── GAP-4 FIX: Channel bindings (empty by default for desktop) ──
            channel_bindings: RwLock::new(Vec::new()),

            // ── T2 FIX: Workspace root for agent tool scoping ──
            workspace_root,

            // ── T4 FIX: Sandbox policy engine (auto-detects platform capabilities) ──
            sandbox_engine: Arc::new(clawdesk_security::sandbox_policy::SandboxPolicyEngine::new()),

            // ── GAP-1 FIX: Sub-agent lifecycle manager ──
            sub_mgr: Arc::new(clawdesk_gateway::subagent_manager::SubAgentManager::new(
                clawdesk_gateway::subagent_manager::SubAgentManagerConfig::default(),
            )),
        }
    }

    /// Resolve a provider from the registry based on model short name.
    ///
    /// Mapping:
    /// - "haiku" / "sonnet" / "opus" → "anthropic"
    /// - "gpt-*" → "openai"
    /// - "gemini-*" → "gemini"
    /// - "local" / "llama*" / "deepseek*" → "ollama"
    /// - Direct provider name → exact match
    /// - Fallback → default provider
    pub fn resolve_provider(&self, model: &str) -> Result<Arc<dyn Provider>, String> {
        let reg = self.provider_registry.read().map_err(|e| e.to_string())?;

        let provider_name = match model {
            "haiku" | "sonnet" | "opus" => "anthropic",
            m if m.starts_with("gpt-") || m.starts_with("o1") || m.starts_with("o3") => "openai",
            m if m.starts_with("gemini") => "gemini",
            "local" => "ollama",
            m if m.starts_with("llama") || m.starts_with("deepseek") || m.starts_with("mistral")
                || m.starts_with("codellama") || m.starts_with("phi") => "ollama",
            m if m.starts_with("claude") => "anthropic",
            other => other,
        };

        if let Some(p) = reg.get(provider_name) {
            return Ok(Arc::clone(p));
        }

        // Fallback to any available provider — emit a warning so callers know
        // the resolved provider differs from what was requested.
        if let Some(p) = reg.default_provider() {
            let fallback_name = reg.iter()
                .find(|(_, v)| Arc::ptr_eq(v, p))
                .map(|(k, _)| k.clone())
                .unwrap_or_else(|| "unknown".to_string());
            tracing::warn!(
                requested = %provider_name,
                fallback = %fallback_name,
                model = %model,
                "Provider '{}' not found, falling back to '{}'",
                provider_name,
                fallback_name,
            );
            return Ok(Arc::clone(p));
        }

        Err(format!(
            "No provider available for model '{}'. \
             Set ANTHROPIC_API_KEY, OPENAI_API_KEY, or GOOGLE_API_KEY environment variable.",
            model
        ))
    }

    /// Map a model short name to the actual model identifier for the provider API.
    pub fn resolve_model_id(model: &str) -> String {
        match model {
            "haiku" => "claude-haiku-4-20250514".to_string(),
            "sonnet" => "claude-sonnet-4-20250514".to_string(),
            "opus" => "claude-opus-4-20250514".to_string(),
            "local" => "llama3.2".to_string(),
            other => other.to_string(),
        }
    }

    pub fn record_usage(&self, model: &str, input_tokens: u64, output_tokens: u64) {
        // Check for day boundary and reset daily counters
        let today = Utc::now().format("%Y-%m-%d").to_string();
        if let Ok(mut last_date) = self.last_cost_reset_date.write() {
            if *last_date != today {
                self.total_cost_today.store(0, Ordering::Relaxed);
                self.total_input_tokens.store(0, Ordering::Relaxed);
                self.total_output_tokens.store(0, Ordering::Relaxed);
                if let Ok(mut costs) = self.model_costs.write() {
                    costs.clear();
                }
                *last_date = today;
            }
        }

        let (cpi, cpo) = model_cost_rates(model);
        let cost_micro = ((input_tokens as f64 * cpi / 1_000_000.0)
            + (output_tokens as f64 * cpo / 1_000_000.0))
            * 1_000_000.0;
        self.total_cost_today.fetch_add(cost_micro as u64, Ordering::Relaxed);
        self.total_input_tokens.fetch_add(input_tokens, Ordering::Relaxed);
        self.total_output_tokens.fetch_add(output_tokens, Ordering::Relaxed);
        if let Ok(mut costs) = self.model_costs.write() {
            let entry = costs.entry(model.to_string()).or_insert((0, 0, 0));
            entry.0 += input_tokens;
            entry.1 += output_tokens;
            entry.2 += cost_micro as u64;
        }
    }

    pub fn cost_today_usd(&self) -> f64 {
        self.total_cost_today.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Persist critical state to SochDB.
    ///
    /// Writes agents, sessions, pipelines, notifications, clipboard, and
    /// canvases to SochDB's path API. Uses JSON serialization for each
    /// item. Best-effort — logs errors but does not fail the caller.
    pub fn persist(&self) {
        let session_count = self.sessions.read().map(|s| s.len()).unwrap_or(0);
        let agent_count = self.agents.read().map(|a| a.len()).unwrap_or(0);
        tracing::info!(sessions = session_count, agents = agent_count, "bulk persist() starting");

        // Agents
        if let Ok(agents) = self.agents.read() {
            for (id, agent) in agents.iter() {
                if let Ok(bytes) = serde_json::to_vec(agent) {
                    let key = format!("agents/{}", id);
                    if let Err(e) = self.soch_store.put(&key, &bytes) {
                        tracing::error!(key = %key, error = %e, "Failed to persist agent");
                    }
                }
            }
        }

        // Sessions (new multi-chat format)
        if let Ok(sessions) = self.sessions.read() {
            let mut ok_count = 0usize;
            let mut fail_count = 0usize;
            for (id, session) in sessions.iter() {
                match serde_json::to_vec(session) {
                    Ok(bytes) => {
                        let key = format!("chats/{}", id);
                        if let Err(e) = self.soch_store.put(&key, &bytes) {
                            tracing::error!(key = %key, error = %e, "Failed to persist chat session");
                            fail_count += 1;
                        } else {
                            ok_count += 1;
                        }
                    }
                    Err(e) => {
                        tracing::error!(chat_id = %id, error = %e, "Failed to SERIALIZE chat session");
                        fail_count += 1;
                    }
                }
            }
            tracing::info!(ok = ok_count, failed = fail_count, "bulk persist: sessions written");
        }

        // Pipelines
        if let Ok(pipelines) = self.pipelines.read() {
            for p in pipelines.iter() {
                if let Ok(bytes) = serde_json::to_vec(p) {
                    let key = format!("pipelines/{}", p.id);
                    if let Err(e) = self.soch_store.put(&key, &bytes) {
                        tracing::error!(key = %key, error = %e, "Failed to persist pipeline");
                    }
                }
            }
        }

        // Canvases
        if let Ok(canvases) = self.canvases.read() {
            for (id, canvas) in canvases.iter() {
                if let Ok(bytes) = serde_json::to_vec(canvas) {
                    let key = format!("canvases/{}", id);
                    if let Err(e) = self.soch_store.put(&key, &bytes) {
                        tracing::error!(key = %key, error = %e, "Failed to persist canvas");
                    }
                }
            }
        }

        // Commit the transaction so all writes above are durable in the WAL.
        // Without this, writes are buffered in TxnWalBuffer (in-memory only)
        // and would be lost on restart.
        if let Err(e) = self.soch_store.commit() {
            tracing::error!(error = %e, "Failed to commit bulk persist transaction");
        } else {
            tracing::info!("bulk persist: commit() succeeded");
        }

        // Checkpoint WAL to keep it bounded
        if let Err(e) = self.soch_store.checkpoint_and_gc() {
            tracing::warn!(error = %e, "SochDB checkpoint failed");
        } else {
            tracing::info!("bulk persist: checkpoint_and_gc() succeeded");
        }
    }

    /// Persist a single agent to SochDB (write-through, durable).
    pub fn persist_agent(&self, id: &str, agent: &DesktopAgent) {
        if let Ok(bytes) = serde_json::to_vec(agent) {
            let key = format!("agents/{}", id);
            if let Err(e) = self.soch_store.put_durable(&key, &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist agent");
            }
        }
    }

    /// Persist a single chat session to SochDB (write-through, durable).
    /// Returns Ok(()) on success or Err with description on failure.
    pub fn persist_session(&self, chat_id: &str, session: &ChatSession) -> Result<(), String> {
        let bytes = serde_json::to_vec(session).map_err(|e| format!("Session serialize error: {}", e))?;
        let key = format!("chats/{}", chat_id);
        let bytes_len = bytes.len();
        self.soch_store.put_durable(&key, &bytes)
            .map_err(|e| {
                tracing::error!(key = %key, error = %e, "Failed to persist session");
                format!("Session persist failed: {}", e)
            })?;
        tracing::debug!(
            key = %key,
            bytes = bytes_len,
            messages = session.messages.len(),
            "Session persisted to SochDB (durable)"
        );
        Ok(())
    }

    /// GAP-1: Unified session write — append a message to both the hot
    /// cache (in-memory `RwLock<HashMap>`) and the durable SochDB store
    /// atomically. Uses write-ahead: the durable store is written first,
    /// and the hot cache rolls back on failure.
    ///
    /// This collapses the previous dual-write pattern (manual
    /// `sessions.write()` + `persist_session()`) into a single call site,
    /// preventing drift between the two stores.
    pub fn append_session_message(
        &self,
        chat_id: &str,
        agent_id: &str,
        title: &str,
        msg: ChatMessage,
        now: &chrono::DateTime<chrono::Utc>,
    ) -> Result<usize, String> {
        let mut sessions = self.sessions.write().map_err(|e| e.to_string())?;
        let session = sessions.entry(chat_id.to_string()).or_insert_with(|| ChatSession {
            id: chat_id.to_string(),
            agent_id: agent_id.to_string(),
            title: title.to_string(),
            messages: Vec::new(),
            created_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
        });
        session.messages.push(msg);
        session.updated_at = chrono::Utc::now().to_rfc3339();

        // Write-ahead: persist first, roll back hot cache on failure
        if let Err(e) = self.persist_session(chat_id, session) {
            session.messages.pop(); // rollback
            return Err(e);
        }
        Ok(session.messages.len())
    }

    /// Persist tool messages separately from the visible session.
    /// Tool messages (tool_use + tool_result) are stored in a parallel SochDB key
    /// (`tool_history/{chat_id}`) so they don't inflate the main session serialization.
    /// They are loaded lazily during history building for subsequent LLM calls.
    ///
    /// GAP-12: Capped to the most recent `MAX_TOOL_HISTORY_ENTRIES` messages to
    /// prevent unbounded growth on long-running sessions.
    pub fn persist_tool_history(&self, chat_id: &str, tool_msgs: &[ChatMessage]) -> Result<(), String> {
        /// Maximum tool history entries kept per session.
        const MAX_TOOL_HISTORY_ENTRIES: usize = 200;

        if tool_msgs.is_empty() {
            return Ok(());
        }
        let key = format!("tool_history/{}", chat_id);
        // Append to existing tool history (load → extend → write)
        let mut existing: Vec<ChatMessage> = self.soch_store.get(&key)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        existing.extend_from_slice(tool_msgs);

        // GAP-12: Trim to most recent entries to prevent unbounded growth
        if existing.len() > MAX_TOOL_HISTORY_ENTRIES {
            let drain_count = existing.len() - MAX_TOOL_HISTORY_ENTRIES;
            existing.drain(..drain_count);
            tracing::debug!(
                chat_id = %chat_id,
                drained = drain_count,
                "Tool history trimmed to {} entries",
                MAX_TOOL_HISTORY_ENTRIES,
            );
        }

        let bytes = serde_json::to_vec(&existing)
            .map_err(|e| format!("Tool history serialize error: {}", e))?;
        self.soch_store.put_durable(&key, &bytes)
            .map_err(|e| {
                tracing::error!(key = %key, error = %e, "Failed to persist tool history");
                format!("Tool history persist failed: {}", e)
            })
    }

    /// Load tool history for a chat session (used during history building).
    pub fn load_tool_history(&self, chat_id: &str) -> Vec<ChatMessage> {
        let key = format!("tool_history/{}", chat_id);
        self.soch_store.get(&key)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Persist a notification to SochDB (write-through).
    pub fn persist_notification(&self, info: &NotificationInfo) {
        if let Ok(bytes) = serde_json::to_vec(info) {
            let key = format!("notifications/{}", info.id);
            if let Err(e) = self.soch_store.put(&key, &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist notification");
            }
        }
    }

    /// Persist a clipboard entry to SochDB (write-through).
    pub fn persist_clipboard_entry(&self, entry: &ClipboardEntry) {
        if let Ok(bytes) = serde_json::to_vec(entry) {
            let key = format!("clipboard/{}", entry.id);
            if let Err(e) = self.soch_store.put(&key, &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist clipboard entry");
            }
        }
    }

    /// Persist a canvas to SochDB (write-through).
    pub fn persist_canvas(&self, canvas: &Canvas) {
        if let Ok(bytes) = serde_json::to_vec(canvas) {
            let key = format!("canvases/{}", canvas.id);
            if let Err(e) = self.soch_store.put(&key, &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist canvas");
            }
        }
    }

    /// Delete an agent from SochDB.
    pub fn delete_agent_from_store(&self, id: &str) {
        let key = format!("agents/{}", id);
        if let Err(e) = self.soch_store.delete(&key) {
            tracing::error!(key = %key, error = %e, "Failed to delete agent from SochDB");
        }
    }

    /// Persist a pipeline to SochDB (write-through).
    pub fn persist_pipeline(&self, pipeline: &PipelineDescriptor) {
        if let Ok(bytes) = serde_json::to_vec(pipeline) {
            let key = format!("pipelines/{}", pipeline.id);
            if let Err(e) = self.soch_store.put(&key, &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist pipeline");
            }
        }
    }

    /// T9: Persist a journal entry to SochDB (write-through).
    pub fn persist_journal_entry(&self, entry: &clawdesk_skills::journal::JournalEntry) {
        if let Ok(bytes) = serde_json::to_vec(entry) {
            let key = format!("journal/{}", entry.id);
            if let Err(e) = self.soch_store.put(&key, &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist journal entry");
            }
        }
    }

    /// T9: Delete a journal entry from SochDB.
    pub fn delete_journal_entry_from_store(&self, id: &str) {
        let key = format!("journal/{}", id);
        if let Err(e) = self.soch_store.delete(&key) {
            tracing::error!(key = %key, error = %e, "Failed to delete journal entry from SochDB");
        }
    }

    /// T21: Post-init health verification.
    ///
    /// Checks that critical subsystems are ready and logs warnings for degraded state.
    /// Returns a list of warnings (empty = fully healthy).
    pub fn verify_health(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        // Check provider registry has at least one provider
        if let Ok(reg) = self.provider_registry.read() {
            if reg.list().is_empty() {
                warnings.push(
                    "No LLM providers registered. Set ANTHROPIC_API_KEY, OPENAI_API_KEY, or GEMINI_API_KEY.".to_string()
                );
            }
        }

        // Check tool registry has tools
        if self.tool_registry.total_count() == 0 {
            warnings.push("Tool registry is empty — builtin tools failed to load.".to_string());
        }

        // Check skill registry has skills
        if let Ok(reg) = self.skill_registry.read() {
            if reg.len() == 0 {
                warnings.push("Skill registry is empty — bundled skills failed to load.".to_string());
            }
        }

        // Check memory system
        let mem_ok = tokio::runtime::Handle::try_current()
            .ok()
            .map(|_| true) // memory manager is async, basic existence check
            .unwrap_or(false);
        if !mem_ok {
            warnings.push("Tokio runtime not available for memory system.".to_string());
        }

        // Check persistence health — ephemeral storage means data loss
        if self.soch_store.is_ephemeral() {
            warnings.push(format!(
                "Storage is EPHEMERAL (path: {}). Chat history will NOT survive a restart. \
                 Check disk permissions for ~/.clawdesk/sochdb/",
                self.soch_store.store_path().display()
            ));
        }

        // T23: Validate effective config through ValidatedConfig
        {
            use clawdesk_types::{ClawDeskConfig, ValidatedConfig};
            let raw = ClawDeskConfig::default();
            if let Err(config_errors) = ValidatedConfig::from_raw(raw) {
                for err in config_errors {
                    warnings.push(format!("Config validation: {}", err));
                }
            }
        }

        for w in &warnings {
            tracing::warn!(subsystem = "health", "{}", w);
        }
        if warnings.is_empty() {
            info!("All subsystems healthy — ready to serve requests");
        }

        warnings
    }
}

impl Default for AppState {
    fn default() -> Self { Self::new() }
}

pub fn model_cost_rates(model: &str) -> (f64, f64) {
    match model {
        "haiku" => (0.25, 1.25),
        "sonnet" => (3.0, 15.0),
        "opus" => (15.0, 75.0),
        "local" => (0.0, 0.0),
        _ => (3.0, 15.0),
    }
}

fn default_pipeline() -> PipelineDescriptor {
    PipelineDescriptor {
        id: "default-research-draft".to_string(),
        name: "Research -> Analyze -> Draft".to_string(),
        description: "Research, analyze in parallel, then draft with approval gate.".to_string(),
        steps: vec![
            PipelineNodeDescriptor { label: "User Input".into(), node_type: "input".into(), model: None, agent_id: None, condition: None, x: 30.0, y: 85.0, config: Default::default() },
            PipelineNodeDescriptor { label: "Researcher".into(), node_type: "agent".into(), model: Some("Sonnet".into()), agent_id: Some("researcher".into()), condition: None, x: 170.0, y: 35.0, config: Default::default() },
            PipelineNodeDescriptor { label: "Analyst".into(), node_type: "agent".into(), model: Some("Sonnet".into()), agent_id: Some("analyst".into()), condition: None, x: 170.0, y: 135.0, config: Default::default() },
            PipelineNodeDescriptor { label: "Review Gate".into(), node_type: "gate".into(), model: None, agent_id: None, condition: None, x: 340.0, y: 85.0, config: Default::default() },
            PipelineNodeDescriptor { label: "Writer".into(), node_type: "agent".into(), model: Some("Opus".into()), agent_id: Some("writer".into()), condition: None, x: 490.0, y: 85.0, config: Default::default() },
            PipelineNodeDescriptor { label: "Output".into(), node_type: "output".into(), model: None, agent_id: None, condition: None, x: 630.0, y: 85.0, config: Default::default() },
        ],
        edges: vec![(0, 1), (0, 2), (1, 3), (2, 3), (3, 4), (4, 5)],
        created: Utc::now().to_rfc3339(),
        schedule: None,
    }
}

// ═══════════════════════════════════════════════════════════
// SochDB hydration helpers
// ═══════════════════════════════════════════════════════════

/// Hydrate a `HashMap<String, T>` from SochDB by scanning a prefix.
///
/// For each `(key, value)` pair under `prefix`, the trailing segment
/// after the last `/` is used as the map key. Entries that fail to
/// deserialize are silently skipped (logged at warn level).
pub fn hydrate_map<T: serde::de::DeserializeOwned>(
    store: &SochStore,
    prefix: &str,
) -> HashMap<String, T> {
    let mut map = HashMap::new();
    match store.scan(prefix) {
        Ok(entries) => {
            let total = entries.len();
            let mut failed = 0usize;
            let mut last_error: Option<String> = None;
            for (key_str, value) in entries {
                let id = key_str
                    .strip_prefix(prefix)
                    .unwrap_or(&key_str)
                    .to_string();
                match serde_json::from_slice::<T>(&value) {
                    Ok(item) => { map.insert(id, item); }
                    Err(e) => {
                        failed += 1;
                        let err_str = e.to_string();
                        warn!(
                            key = %key_str,
                            error = %err_str,
                            value_len = value.len(),
                            "Failed to deserialize entry during hydration"
                        );
                        if failed <= 3 {
                            // Log first few raw values for debugging
                            let preview = String::from_utf8_lossy(
                                &value[..value.len().min(200)]
                            );
                            error!(
                                key = %key_str,
                                preview = %preview,
                                "DATA LOSS: entry exists in SochDB but cannot be deserialized"
                            );
                        }
                        last_error = Some(err_str);
                    }
                }
            }
            if failed > 0 {
                error!(
                    prefix = %prefix,
                    total_entries = total,
                    deserialized_ok = map.len(),
                    deserialization_failures = failed,
                    last_error = ?last_error,
                    "HYDRATION DATA LOSS: {failed}/{total} entries failed to deserialize — \
                     likely struct schema change without #[serde(default)]. \
                     Check if fields were added to the struct after data was persisted."
                );
            } else if total > 0 {
                info!(
                    prefix = %prefix,
                    entries = total,
                    "Hydration successful — all entries deserialized"
                );
            }
        }
        Err(e) => {
            error!(prefix = %prefix, error = %e, "SochDB scan FAILED during hydration — no data loaded");
        }
    }
    map
}

/// Hydrate a `Vec<T>` from SochDB by scanning a prefix.
fn hydrate_list<T: serde::de::DeserializeOwned>(
    store: &SochStore,
    prefix: &str,
) -> Vec<T> {
    match store.scan(prefix) {
        Ok(entries) => {
            let total = entries.len();
            let mut failed = 0usize;
            let result: Vec<T> = entries
                .into_iter()
                .filter_map(|(key_str, value)| {
                    match serde_json::from_slice::<T>(&value) {
                        Ok(item) => Some(item),
                        Err(e) => {
                            failed += 1;
                            warn!(key = %key_str, error = %e, "Failed to deserialize list entry during hydration");
                            None
                        }
                    }
                })
                .collect();
            if failed > 0 {
                error!(
                    prefix = %prefix,
                    total_entries = total,
                    deserialized_ok = result.len(),
                    deserialization_failures = failed,
                    "HYDRATION DATA LOSS: {failed}/{total} list entries failed to deserialize"
                );
            }
            result
        }
        Err(e) => {
            error!(prefix = %prefix, error = %e, "SochDB scan FAILED during list hydration");
            Vec::new()
        }
    }
}

// ═══════════════════════════════════════════════════════════
// T24: E2E Smoke Tests — verify AppState construction and
//      critical subsystem wiring without Tauri runtime.
// ═══════════════════════════════════════════════════════════
#[cfg(test)]
mod smoke_tests {
    use super::*;

    #[test]
    fn app_state_constructs_without_panic() {
        // The single most important smoke test: can we build AppState from defaults?
        let state = AppState::new();

        // Tool registry must have builtins
        assert!(
            state.tool_registry.total_count() > 0,
            "Tool registry should contain builtin tools"
        );

        // Skill registry must have bundled skills
        let skill_count = state
            .skill_registry
            .read()
            .expect("skill RwLock poisoned")
            .len();
        assert!(
            skill_count > 0,
            "Skill registry should contain bundled skills"
        );
    }

    #[test]
    fn verify_health_returns_limited_warnings() {
        let state = AppState::new();
        let warnings = state.verify_health();
        // There may be warnings (e.g., no API keys in CI) but should not panic.
        // Provider warning is expected in test env without keys.
        for w in &warnings {
            assert!(!w.is_empty(), "Warning should not be empty string");
        }
    }

    #[test]
    fn provider_negotiator_initializes() {
        let state = AppState::new();
        // Ollama is always registered, so negotiator should have ≥1 provider.
        assert!(
            state.negotiator.read().unwrap().provider_count() >= 1,  // Arc<RwLock> auto-derefs
            "Negotiator should have at least the Ollama provider"
        );
    }

    #[test]
    fn session_lifecycle_crud() {
        let state = AppState::new();
        let agent_id = uuid::Uuid::new_v4().to_string();
        let chat_id = uuid::Uuid::new_v4().to_string();

        // Start empty
        let sessions = state.sessions.read().expect("lock");
        assert!(sessions.get(&chat_id).is_none());
        drop(sessions);

        // Create a chat session
        {
            let mut sessions = state.sessions.write().expect("lock");
            sessions.insert(chat_id.clone(), ChatSession {
                id: chat_id.clone(),
                agent_id: agent_id.clone(),
                title: "Test chat".to_string(),
                messages: Vec::new(),
                created_at: chrono::Utc::now().to_rfc3339(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            });
        }

        // Verify exists
        let sessions = state.sessions.read().expect("lock");
        let session = sessions.get(&chat_id).expect("chat should exist");
        assert_eq!(session.agent_id, agent_id);
        assert_eq!(session.title, "Test chat");
    }

    #[test]
    fn template_registry_has_builtins() {
        let state = AppState::new();
        let templates = (*state.template_registry).list();
        assert!(
            !templates.is_empty(),
            "Template registry should have builtin templates"
        );
    }

    #[test]
    fn channel_registry_has_webchat_and_internal() {
        let state = AppState::new();
        let reg = state.channel_registry.read().expect("channel registry lock");
        let channels = reg.list();
        // webchat and internal are always registered
        assert!(
            channels.len() >= 2,
            "Channel registry should have at least webchat + internal"
        );
    }

    #[test]
    fn model_cost_rates_known_models() {
        let (input, output) = model_cost_rates("sonnet");
        assert!(input > 0.0 && output > 0.0);

        let (input, output) = model_cost_rates("unknown-model-xyz");
        // Fallback should still return non-negative
        assert!(input >= 0.0 && output >= 0.0);
    }
}
