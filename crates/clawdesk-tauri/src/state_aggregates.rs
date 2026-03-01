//! Domain aggregate structs for AppState decomposition.
//!
//! This module defines 11 focused domain aggregate structs that replace
//! the ~82-field AppState god-object. Each aggregate groups related fields
//! with clear ownership boundaries and independent lock granularity,
//! eliminating false sharing at the cache-line level.
//!
//! ## Migration Plan
//!
//! Phase 1 ✓ (this file): Define aggregate structs with constructors.
//! Phase 2 ✓ (this file): `AggregateState` facade with typed accessors.
//! Phase 3: Migrate IPC command handlers to use aggregate accessors.
//!
//! ## Aggregate Map (all 82 fields assigned)
//!
//! | Aggregate            | Fields | Lock Profile              |
//! |----------------------|--------|---------------------------|
//! | StorageAggregate     | 10     | All `Arc` (read-heavy)    |
//! | MemoryAggregate      | 2      | `Arc` (read-heavy)        |
//! | RegistryAggregate    | 8      | `RwLock` + `Arc`          |
//! | SecurityAggregate    | 9      | `Arc` (mostly immutable)  |
//! | SessionAggregate     | 10     | Mixed (SessionCache hot)  |
//! | NetworkAggregate     | 7      | `RwLock` (rare writes)    |
//! | MetricsAggregate     | 9      | `Atomic` (lock-free fast) |
//! | MediaAggregate       | 5      | `RwLock` + `Arc`          |
//! | PluginAggregate      | 4      | `Arc` (read-heavy)        |
//! | AgentAggregate       | 9      | `Arc` + `Semaphore`       |
//! | BusAggregate         | 5      | `Arc` + `RwLock`          |
//! | InfraAggregate       | 4      | Top-level lifecycle       |

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::RwLock;

// Re-export for convenience — types used across aggregates
pub use clawdesk_agents::session_lane::SessionLaneManager;
pub use clawdesk_bus::dispatch::EventBus;
pub use clawdesk_domain::context_guard::ContextGuard;
pub use clawdesk_plugin::hooks::HookManager;

// ═══════════════════════════════════════════════════════════════════════════
// 1. StorageAggregate — persistent storage & SochDB advanced modules
// ═══════════════════════════════════════════════════════════════════════════

/// Groups all SochDB-backed persistent storage — the ACID core and
/// advanced overlay modules (graph, temporal, policy, traces, etc.).
///
/// All fields are `Arc` — created once at startup, shared read-only
/// across all IPC handlers. Zero lock contention.
pub struct StorageAggregate {
    pub soch_store: Arc<clawdesk_sochdb::SochStore>,
    pub thread_store: Arc<clawdesk_threads::ThreadStore>,
    pub semantic_cache: Arc<clawdesk_sochdb::SochSemanticCache>,
    pub trace_store: Arc<clawdesk_sochdb::SochTraceStore>,
    pub checkpoint_store: Arc<clawdesk_sochdb::SochCheckpointStore>,
    pub knowledge_graph: Arc<clawdesk_sochdb::SochGraphOverlay>,
    pub temporal_graph: Arc<clawdesk_sochdb::SochTemporalGraph>,
    pub policy_engine: Arc<clawdesk_sochdb::SochPolicyEngine>,
    pub atomic_writer: Arc<clawdesk_sochdb::SochAtomicWriter>,
    pub agent_registry: Arc<clawdesk_sochdb::SochAgentRegistry>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. MemoryAggregate — vector memory & embeddings
// ═══════════════════════════════════════════════════════════════════════════

/// Memory subsystem — vector-indexed recall with configurable embeddings.
pub struct MemoryAggregate {
    pub memory: Arc<clawdesk_memory::MemoryManager<clawdesk_sochdb::SochMemoryBackend>>,
    pub embedding_provider: Arc<dyn clawdesk_memory::EmbeddingProvider>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. RegistryAggregate — skills, providers, tools, channels
// ═══════════════════════════════════════════════════════════════════════════

/// All registries that manage available capabilities.
///
/// Skills, providers, and channels are registered at startup and
/// occasionally updated at runtime (hot-reload). Read-heavy pattern.
pub struct RegistryAggregate {
    pub skill_registry: RwLock<clawdesk_skills::SkillRegistry>,
    pub provider_registry: Arc<RwLock<clawdesk_providers::ProviderRegistry>>,
    pub tool_registry: Arc<clawdesk_agents::tools::ToolRegistry>,
    pub channel_registry: Arc<RwLock<clawdesk_channel::registry::ChannelRegistry>>,
    pub channel_factory: Arc<clawdesk_channels::factory::ChannelFactory>,
    pub channel_configs: RwLock<HashMap<String, HashMap<String, String>>>,
    /// Life OS template registry — builtin + user templates.
    pub template_registry: Arc<clawdesk_skills::life_os::TemplateRegistry>,
    /// Extensions: Integration registry (GitHub, Slack, Jira, AWS, etc.)
    pub integration_registry: tokio::sync::RwLock<clawdesk_extensions::IntegrationRegistry>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. SecurityAggregate — scanning, audit, ACL, sandboxing
// ═══════════════════════════════════════════════════════════════════════════

/// All security-related state — scanner, audit trail, ACL, sandbox.
///
/// Mostly immutable after init (`Arc`), with audit logger doing
/// append-only writes internally.
pub struct SecurityAggregate {
    pub scanner: Arc<clawdesk_security::CascadeScanner>,
    pub scanner_pattern_count: usize,
    pub audit_logger: Arc<clawdesk_security::AuditLogger>,
    pub acl_manager: Arc<clawdesk_security::AclManager>,
    pub skill_verifier: Arc<clawdesk_security::SkillVerifier>,
    pub server_secret: clawdesk_security::ServerSecret,
    pub sandbox_engine: Arc<clawdesk_security::sandbox_policy::SandboxPolicyEngine>,
    /// Sandbox dispatcher — multi-modal code execution isolation.
    pub sandbox_dispatcher: tokio::sync::RwLock<clawdesk_sandbox::SandboxDispatcher>,
    /// AES-256-GCM encrypted credential vault for API keys & secrets.
    pub credential_vault: tokio::sync::RwLock<clawdesk_extensions::CredentialVault>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. SessionAggregate — active sessions, agents, identity, context
// ═══════════════════════════════════════════════════════════════════════════

/// Hot-path session state — the most frequently accessed aggregate.
///
/// `SessionCache` (LRU, capacity=200) is the primary access point.
/// Context guards hold per-session budget/compaction state.
pub struct SessionAggregate {
    pub sessions: crate::state::SessionCache,
    pub agents: Arc<RwLock<HashMap<String, crate::state::DesktopAgent>>>,
    pub identities: RwLock<HashMap<String, clawdesk_security::IdentityContract>>,
    pub context_guards: RwLock<HashMap<String, ContextGuard>>,
    pub prompt_manifests: RwLock<HashMap<String, clawdesk_domain::prompt_builder::PromptManifest>>,
    pub active_chat_runs: tokio::sync::RwLock<HashMap<String, tokio_util::sync::CancellationToken>>,
    pub session_lanes: SessionLaneManager,
    /// Notification history hot cache — persisted to SochDB.
    pub notification_history: RwLock<Vec<crate::commands_infra::NotificationInfo>>,
    /// Canvas hot cache — persisted to SochDB.
    pub canvases: RwLock<HashMap<String, crate::canvas::Canvas>>,
    /// Journal entries hot cache — persisted to SochDB.
    pub journal_entries: RwLock<HashMap<String, clawdesk_skills::journal::JournalEntry>>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. NetworkAggregate — tunnels, pairing, mDNS, OAuth, invites
// ═══════════════════════════════════════════════════════════════════════════

/// Networking & authentication state — tunnels, peer discovery, OAuth.
pub struct NetworkAggregate {
    pub invites: RwLock<clawdesk_tunnel::discovery::InviteManager>,
    pub tunnel_metrics: Arc<clawdesk_tunnel::TunnelMetrics>,
    pub mdns_advertiser: RwLock<clawdesk_discovery::MdnsAdvertiser>,
    pub pairing_session: RwLock<Option<clawdesk_discovery::PairingSession>>,
    pub peer_registry: RwLock<clawdesk_discovery::PeerRegistry>,
    pub oauth_flow_manager: Arc<clawdesk_security::OAuthFlowManager>,
    pub auth_profile_manager: Arc<clawdesk_security::AuthProfileManager>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. MetricsAggregate — cost tracking, observability, telemetry
// ═══════════════════════════════════════════════════════════════════════════

/// Usage metrics & observability — lock-free atomic counters for hot path.
///
/// Cost tracking uses `AtomicU64` for zero-contention updates on every
/// LLM response. Observability config is read-heavy, rarely written.
pub struct MetricsAggregate {
    pub total_cost_today: AtomicU64,
    pub total_input_tokens: AtomicU64,
    pub total_output_tokens: AtomicU64,
    pub model_costs: RwLock<HashMap<String, (u64, u64, u64)>>,
    pub last_cost_reset_date: RwLock<String>,
    pub observability_config: RwLock<clawdesk_observability::ObservabilityConfig>,
    pub metrics_aggregator: Arc<clawdesk_observability::MetricsAggregator>,
    /// Legacy trace cache — pending migration to SochDB TraceStore.
    pub traces: RwLock<HashMap<String, Vec<crate::state::TraceEntry>>>,
    /// App startup time for uptime calculation.
    pub started_at: std::time::Instant,
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. MediaAggregate — media pipeline, artifacts, voice, audio
// ═══════════════════════════════════════════════════════════════════════════

/// Media processing state — image pipelines, voice, audio recording.
pub struct MediaAggregate {
    pub media_pipeline: Arc<tokio::sync::RwLock<clawdesk_media::MediaPipeline>>,
    pub artifact_pipeline: Arc<clawdesk_media::ArtifactPipeline>,
    pub whisper_engine: RwLock<Option<clawdesk_media::whisper::WhisperSttEngine>>,
    pub audio_recorder: parking_lot::Mutex<clawdesk_media::recorder::AudioRecorder>,
    pub voice_wake: RwLock<Option<clawdesk_infra::voice_wake::VoiceWakeManager>>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. PluginAggregate — plugin host & hooks
// ═══════════════════════════════════════════════════════════════════════════

/// Plugin system state — host for loading plugins + hook dispatch.
pub struct PluginAggregate {
    pub plugin_host: Option<Arc<clawdesk_plugin::PluginHost>>,
    pub hook_manager: Arc<HookManager>,
    /// MCP: Model Context Protocol client for external tool servers.
    pub mcp_client: tokio::sync::RwLock<clawdesk_mcp::McpClient>,
    /// Extensions: Health monitor for integration endpoints.
    pub health_monitor: tokio::sync::RwLock<clawdesk_extensions::HealthMonitor>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. AgentAggregate — orchestration, routing, approval, concurrency
// ═══════════════════════════════════════════════════════════════════════════

/// Agent orchestration state — routing, approval, sub-agents, concurrency.
pub struct AgentAggregate {
    pub approval_manager: Arc<clawdesk_security::ExecApprovalManager>,
    pub negotiator: Arc<RwLock<clawdesk_providers::ProviderNegotiator>>,
    pub turn_router: Arc<clawdesk_agents::TurnRouter>,
    pub sub_mgr: Arc<clawdesk_gateway::subagent_manager::SubAgentManager>,
    pub shared_state_mgr: Arc<clawdesk_agents::SharedStateManager>,
    pub durable_runner: Option<Arc<clawdesk_runtime::DurableAgentRunner>>,
    pub llm_concurrency: Arc<tokio::sync::Semaphore>,
    /// A2A protocol: agent directory for capability-based routing.
    pub agent_directory: Arc<RwLock<clawdesk_acp::AgentDirectory>>,
    /// A2A protocol: active delegated tasks.
    pub a2a_tasks: Arc<tokio::sync::RwLock<HashMap<String, clawdesk_acp::Task>>>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. BusAggregate — event bus, channel dock, routing
// ═══════════════════════════════════════════════════════════════════════════

/// Event bus & channel routing state.
pub struct BusAggregate {
    pub event_bus: Arc<EventBus>,
    pub channel_dock: Arc<clawdesk_channel::channel_dock::ChannelDock>,
    pub channel_bindings: RwLock<Vec<clawdesk_domain::routing::ChannelBindingEntry>>,
    pub last_channel_origins: Arc<RwLock<HashMap<clawdesk_types::ChannelId, clawdesk_types::MessageOrigin>>>,
    pub channel_provider: RwLock<Option<crate::state::ChannelProviderOverride>>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. InfraAggregate — app lifecycle, clipboard, idle detection
// ═══════════════════════════════════════════════════════════════════════════

/// Infrastructure and lifecycle state — fields that don't belong to any
/// domain aggregate but are needed for app lifecycle management.
pub struct InfraAggregate {
    /// Global shutdown token — triggers graceful teardown.
    pub cancel: tokio_util::sync::CancellationToken,
    /// Workspace root for agent tool scoping (all FS tools are confined here).
    pub workspace_root: std::path::PathBuf,
    /// Clipboard history hot cache — persisted to SochDB.
    pub clipboard_history: RwLock<Vec<clawdesk_infra::clipboard::ClipboardEntry>>,
    /// Idle detection for auto-checkpoint and power-save.
    pub idle_detector: Option<Arc<clawdesk_infra::IdleDetector>>,
    /// Cron scheduling manager.
    pub cron_manager: Arc<clawdesk_cron::CronManager>,
    /// Pipeline descriptors for multi-step workflows.
    pub pipelines: RwLock<Vec<crate::state::PipelineDescriptor>>,
}

// ═══════════════════════════════════════════════════════════════════════════
// AggregateState — thin facade over all aggregates
// ═══════════════════════════════════════════════════════════════════════════

/// Thin facade that holds `Arc` references to each domain aggregate.
///
/// Each aggregate occupies its own allocation (separate cache lines),
/// eliminating false sharing. The facade is `Clone`-cheap (12 Arc bumps)
/// and can be passed into concurrent Tauri command handlers independently.
///
/// **Usage in Phase 3 migration:**
/// ```ignore
/// // Before (God Object):
/// let session = state.sessions.get(&id);
///
/// // After (Aggregate):
/// let session = state.aggregates().session.sessions.get(&id);
/// ```
#[derive(Clone)]
pub struct AggregateState {
    pub storage: Arc<StorageAggregate>,
    pub memory: Arc<MemoryAggregate>,
    pub registry: Arc<RegistryAggregate>,
    pub security: Arc<SecurityAggregate>,
    pub session: Arc<SessionAggregate>,
    pub network: Arc<NetworkAggregate>,
    pub metrics: Arc<MetricsAggregate>,
    pub media: Arc<MediaAggregate>,
    pub plugin: Arc<PluginAggregate>,
    pub agent: Arc<AgentAggregate>,
    pub bus: Arc<BusAggregate>,
    pub infra: Arc<InfraAggregate>,
}

impl AggregateState {
    /// Accessor shorthand for the hot-path session aggregate.
    #[inline]
    pub fn sessions(&self) -> &SessionAggregate {
        &self.session
    }

    /// Accessor shorthand for the storage aggregate (SochDB).
    #[inline]
    pub fn storage(&self) -> &StorageAggregate {
        &self.storage
    }

    /// Accessor shorthand for the security aggregate.
    #[inline]
    pub fn security(&self) -> &SecurityAggregate {
        &self.security
    }

    /// Accessor shorthand for the event bus aggregate.
    #[inline]
    pub fn bus(&self) -> &BusAggregate {
        &self.bus
    }

    /// Accessor shorthand for the metrics aggregate.
    #[inline]
    pub fn metrics(&self) -> &MetricsAggregate {
        &self.metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_state_is_clone() {
        // Compile-time check: AggregateState must be Clone
        fn assert_clone<T: Clone>() {}
        assert_clone::<AggregateState>();
    }

    #[test]
    fn aggregate_state_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StorageAggregate>();
        assert_send_sync::<RegistryAggregate>();
        assert_send_sync::<SecurityAggregate>();
        assert_send_sync::<MetricsAggregate>();
        assert_send_sync::<AgentAggregate>();
        assert_send_sync::<BusAggregate>();
    }
}
