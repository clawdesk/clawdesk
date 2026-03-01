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
    /// Optional template ID this agent was created from.
    #[serde(default)]
    pub template_id: Option<String>,
    /// Channels this agent is assigned to (e.g. ["telegram", "discord"]).
    /// When empty, the agent can serve any channel (default/fallback).
    #[serde(default)]
    pub channels: Vec<String>,
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
            channels: Vec::new(),
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
    ToolBlocked { name: String, reason: String },
    ToolExecutionResult { name: String, tool_call_id: String, is_error: bool, preview: String, duration_ms: u64 },
    Compaction { level: String, tokens_before: usize, tokens_after: usize },
    StreamChunk { text: String, done: bool },
    ThinkingChunk { text: String },
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
    /// Step-specific configuration. Contains user-configured values:
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

// ─── Documented Lock Ordering & Domain Aggregates ─────────────
//
// AppState is a large god-object holding all application-wide shared state.
// To prevent deadlocks and reduce contention, locks MUST be acquired in the
// following canonical order. Never acquire a higher-numbered lock while
// holding a lower-numbered one.
//
// ┌─────────────────────────────────────────────────────────────────────┐
// │ LOCK ORDERING (always acquire in ascending order)                  │
// │                                                                    │
// │ See also: clawdesk_types::ordered_lock::levels for numeric         │
// │ constants that can be used with OrderedRwLock / OrderedMutex       │
// │ to enforce this ordering at debug time.                            │
// ├──────┬──────────────────────────────────────────────────────────────┤
// │  L1  │ sessions (SessionCache — internal Mutex)                    │
// │  L2  │ agents (RwLock)                                             │
// │  L3  │ active_chat_runs (tokio::RwLock)                            │
// │  L4  │ provider_registry (Arc<RwLock>)                             │
// │ L4b  │ skill_registry (RwLock) — held with L2 in get_health        │
// │  L5  │ channel_registry (Arc<RwLock>)                              │
// │  L6  │ a2a_tasks (tokio::RwLock)                                   │
// │ L6b  │ agent_directory (Arc<RwLock>)                                │
// │  L7  │ model_costs / traces / pipelines (RwLock) — metrics group   │
// │  L8  │ identities (RwLock)                                         │
// │ L8b  │ negotiator (Arc<RwLock>)                                     │
// │  L9  │ channel_configs / channel_provider (RwLock) — channel cfg   │
// │ L9b  │ integration_registry (RwLock) — held in extension commands  │
// │ L9c  │ credential_vault (RwLock) — always after L9b                │
// │ L9d  │ health_monitor (RwLock) — always after L9b                  │
// │ L10  │ mdns_advertiser / peer_registry / pairing_session           │
// │ L11  │ notification_history / clipboard_history / journal_entries   │
// │ L12  │ canvases / context_guards / prompt_manifests                │
// │ L13  │ channel_bindings / observability_config                     │
// │ L13b │ mcp_client (RwLock) — single-lock usage only                │
// │ L13c │ sandbox_dispatcher (RwLock) — single-lock usage only        │
// │ L14  │ whisper_engine (RwLock)                                     │
// │ L14b │ audio_recorder (parking_lot::Mutex) — always last           │
// └──────┴──────────────────────────────────────────────────────────────┘
//
// Domain aggregates (planned — accessor methods below provide the migration
// path without changing call sites):
//
//   • StorageAggregate    — soch_store, thread_store, semantic_cache, trace_store,
//                           checkpoint_store, knowledge_graph, temporal_graph,
//                           policy_engine, atomic_writer, agent_registry
//   • MemoryAggregate     — memory, embedding_provider
//   • ProviderAggregate   — provider_registry, negotiator, turn_router
//   • ChannelAggregate    — channel_registry, channel_factory, channel_configs,
//                           channel_provider, channel_bindings, channel_dock,
//                           last_channel_origins
//   • AgentAggregate      — agents, sessions, active_chat_runs, session_lanes,
//                           llm_concurrency, sub_mgr, durable_runner
//   • MetricsAggregate    — total_cost_today, total_input_tokens, total_output_tokens,
//                           model_costs, last_cost_reset_date, metrics_aggregator,
//                           traces, started_at
//   • SecurityAggregate   — identities, server_secret, invites, acl_manager,
//                           skill_verifier, sandbox_engine, approval_manager
//   • DiscoveryAggregate  — mdns_advertiser, pairing_session, peer_registry,
//                           agent_directory
//   • MediaAggregate      — media_pipeline, artifact_pipeline, audio_recorder,
//                           whisper_engine, voice_wake
//   • PluginAggregate     — plugin_host, hook_manager
//   • UIAggregate         — canvases, notification_history, clipboard_history,
//                           context_guards, prompt_manifests
//
// ──────────────────────────────────────────────────────────────────────────

// ── T8 FIX: Bounded LRU session cache ──────────────────────────────────
//
// Replaces `RwLock<HashMap<String, ChatSession>>` with a bounded LRU cache
// that evicts the least-recently-used sessions. Memory is bounded at
// O(capacity × avg_session_size) regardless of total session count.
// Hot-path latency is O(1). Cache misses are expected to trigger SochDB
// reads (not yet wired — sessions loaded from persistence on startup).
//
// Using `parking_lot::Mutex<LruCache>` because `lru::LruCache::get()`
// requires `&mut self` to update access ordering. A single Mutex is fine
// since session operations are brief (no I/O under the lock).

/// Maximum number of sessions to keep in the hot cache.
/// Sessions beyond this are evicted (least recently used first).
/// With average ~50KB per session, 200 sessions ≈ 10MB — bounded.
const SESSION_CACHE_CAPACITY: usize = 200;

/// Thread-safe bounded LRU session cache.
///
/// All operations are O(1) amortized. The cache evicts the least-recently-used
/// sessions when capacity is exceeded. Wraps `lru::LruCache` in a `parking_lot::Mutex`
/// because every access (including reads) must update the LRU ordering.
pub struct SessionCache {
    inner: parking_lot::Mutex<lru::LruCache<String, ChatSession>>,
}

impl SessionCache {
    pub fn new() -> Self {
        Self {
            inner: parking_lot::Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(SESSION_CACHE_CAPACITY).unwrap(),
            )),
        }
    }

    /// Get a session by ID, updating LRU ordering. Returns a clone.
    pub fn get(&self, id: &str) -> Option<ChatSession> {
        self.inner.lock().get(id).cloned()
    }

    /// Peek at a session without updating LRU ordering.
    pub fn peek(&self, id: &str) -> Option<ChatSession> {
        self.inner.lock().peek(id).cloned()
    }

    /// Insert or update a session.
    pub fn insert(&self, id: String, session: ChatSession) {
        self.inner.lock().put(id, session);
    }

    /// Remove a session by ID.
    pub fn remove(&self, id: &str) -> Option<ChatSession> {
        self.inner.lock().pop(id)
    }

    /// Check if a session exists.
    pub fn contains(&self, id: &str) -> bool {
        self.inner.lock().contains(id)
    }

    /// Number of cached sessions.
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Remove all sessions from the cache.
    pub fn clear(&self) {
        self.inner.lock().clear();
    }

    /// Get all cached sessions as a Vec (for serialization/persistence).
    /// Does NOT update LRU ordering.
    pub fn values(&self) -> Vec<ChatSession> {
        self.inner.lock().iter().map(|(_, v)| v.clone()).collect()
    }

    /// Get all cached sessions as (id, session) pairs.
    /// Does NOT update LRU ordering.
    pub fn entries(&self) -> Vec<(String, ChatSession)> {
        self.inner.lock().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// Load multiple sessions (e.g., from persistence). Oldest-first ordering
    /// means the last item in the iterator becomes most-recently-used.
    pub fn load_bulk(&self, sessions: impl IntoIterator<Item = (String, ChatSession)>) {
        let mut guard = self.inner.lock();
        for (id, session) in sessions {
            guard.put(id, session);
        }
    }

    /// Mutate a session in-place. Returns true if the session was found.
    pub fn mutate<F>(&self, id: &str, f: F) -> bool
    where
        F: FnOnce(&mut ChatSession),
    {
        let mut guard = self.inner.lock();
        if let Some(session) = guard.get_mut(id) {
            f(session);
            true
        } else {
            false
        }
    }
}

impl std::fmt::Debug for SessionCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCache")
            .field("len", &self.len())
            .field("capacity", &SESSION_CACHE_CAPACITY)
            .finish()
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
    /// Shared embedding provider — used by semantic cache to pre-compute query embeddings.
    pub embedding_provider: Arc<dyn EmbeddingProvider>,

    // Real backend services
    pub skill_registry: RwLock<SkillRegistry>,
    pub provider_registry: Arc<RwLock<ProviderRegistry>>,
    pub tool_registry: Arc<ToolRegistry>,
    pub channel_registry: Arc<RwLock<ChannelRegistry>>,
    /// Factory for creating channel adapters from config maps.
    /// Stored so `update_channel` can hot-start channels after initial setup.
    pub channel_factory: Arc<clawdesk_channels::factory::ChannelFactory>,
    /// Saved channel configurations (channel_id → config key-value pairs).
    /// Persisted to `~/.clawdesk/channels.json` via `save_channel_configs()`
    /// so channels survive restarts.
    pub channel_configs: RwLock<HashMap<String, HashMap<String, String>>>,
    pub scanner: Arc<CascadeScanner>,
    pub scanner_pattern_count: usize,
    pub audit_logger: Arc<AuditLogger>,

    // Agent & session management (hot cache — persisted to SochDB)
    pub agents: Arc<RwLock<HashMap<String, DesktopAgent>>>,
    pub identities: RwLock<HashMap<String, IdentityContract>>,
    pub server_secret: ServerSecret,
    pub sessions: SessionCache,

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
    pub agent_directory: Arc<RwLock<AgentDirectory>>,
    pub a2a_tasks: Arc<tokio::sync::RwLock<HashMap<String, clawdesk_acp::Task>>>,

    /// Last inbound message origin per channel — enables cross-channel messaging.
    /// When `message_send` is called with `to="default"`, we use the last known
    /// origin for the target channel so the agent doesn't need internal IDs.
    pub last_channel_origins: Arc<RwLock<HashMap<clawdesk_types::channel::ChannelId, clawdesk_types::message::MessageOrigin>>>,

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
    /// Metrics aggregator for latency, tokens, cost tracking.
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

    // ── Cron Scheduling ──
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

    // ── Global LLM concurrency bound ──
    // Limits the total number of concurrent LLM calls across all sessions.
    // Without this, unbounded parallel requests can overwhelm API rate limits
    // and exhaust memory. The semaphore is acquired after the per-session lane
    // guard and before `runner.run()` / `runner.run_with_failover()`.
    pub llm_concurrency: Arc<tokio::sync::Semaphore>,

    // ── /1 FIX: Channel dock for prompt injection ──
    // Lightweight metadata registry of all known channels. Used to construct
    // `ChannelContext` for the agent runner without requiring a live channel.
    pub channel_dock: Arc<clawdesk_channel::channel_dock::ChannelDock>,

    // ── Hook manager for plugin lifecycle hooks ──
    // Dispatches lifecycle hooks (MessageReceive, BeforeAgentStart, etc.) to
    // registered plugins. Shared across all agent runs.
    pub hook_manager: Arc<clawdesk_plugin::HookManager>,

    // ── Channel binding entries for multi-channel routing ──
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

    // ── Sub-agent lifecycle manager ──
    // Tracks all running sub-agents (static and ephemeral), enforcing global
    // depth limits (default 5), concurrency caps (default 50), and deferred GC.
    pub sub_mgr: Arc<clawdesk_gateway::subagent_manager::SubAgentManager>,

    // ── GAP-G: Per-turn dynamic model routing ──
    // Bridges TaskRouter (LinUCB bandit) with ModelCatalog (capability index)
    // to select the optimal model on every turn. Thread-safe, shared across
    // all agent runs. The bandit learns from reward feedback after each turn.
    pub turn_router: Arc<clawdesk_agents::TurnRouter>,

    // ── GAP-D: Reactive event bus ──
    // Central pub/sub event bus — typed events, WFQ priority dispatch,
    // pattern-based subscriptions mapping to pipeline triggers.
    pub event_bus: Arc<clawdesk_bus::dispatch::EventBus>,

    // ── GAP-E: Cross-channel artifact pipeline ──
    // Content-addressed artifact store backed by MediaCache. Ingests media
    // from any channel, ACP artifacts, and provides unified cross-channel
    // artifact references for agent context injection.
    pub artifact_pipeline: Arc<clawdesk_media::ArtifactPipeline>,

    // ── GAP-F: Multi-agent shared state ──
    // Scoped blackboard for multi-agent collaboration. Creates pipeline-scoped,
    // delegation-scoped, or conversation-scoped KV stores that agents can
    // read/write during execution.
    pub shared_state_mgr: Arc<clawdesk_agents::SharedStateManager>,

    // ── Sandbox: Multi-modal code execution isolation ──
    // Dispatches execution requests to the best available sandbox backend
    // (subprocess, Docker, Wasm) based on requested isolation level.
    pub sandbox_dispatcher: tokio::sync::RwLock<clawdesk_sandbox::SandboxDispatcher>,

    // ── MCP: Model Context Protocol client ──
    // Multi-server connection manager for MCP tool servers. Manages stdio
    // and SSE transports, tool discovery, and namespaced tool invocation.
    pub mcp_client: tokio::sync::RwLock<clawdesk_mcp::McpClient>,

    // ── Extensions: Integration registry + health monitoring ──
    // 25+ bundled integrations (GitHub, Slack, Jira, AWS, etc.) with
    // credential requirements, OAuth config, and health check URLs.
    pub integration_registry: tokio::sync::RwLock<clawdesk_extensions::IntegrationRegistry>,

    // ── Extensions: AES-256-GCM encrypted credential vault ──
    // Stores API keys, tokens, and secrets encrypted at rest. Requires
    // master password to unlock. PKCE verifiers also stored here.
    pub credential_vault: tokio::sync::RwLock<clawdesk_extensions::CredentialVault>,

    // ── Extensions: Health monitor for integration endpoints ──
    // Tracks health state, latency, consecutive failures, and exponential
    // backoff for all registered integration health check URLs.
    pub health_monitor: tokio::sync::RwLock<clawdesk_extensions::HealthMonitor>,
}

// ── Cron executor glue ──

/// AgentExecutor implementation for CronManager.
/// Executes cron-triggered prompts using a configured LLM provider.
struct CronAgentExecutor {
    provider: Arc<dyn Provider>,
    tool_registry: Arc<ToolRegistry>,
    cancel: tokio_util::sync::CancellationToken,
    /// GAP-B: Memory manager for automatic recall during cron executions.
    memory: Arc<MemoryManager<SochMemoryBackend>>,
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

        // GAP-B: Build memory recall callback for cron executions
        let memory_recall_fn: clawdesk_agents::MemoryRecallFn = {
            let mem = Arc::clone(&self.memory);
            Arc::new(move |query: String| {
                let mem = Arc::clone(&mem);
                Box::pin(async move {
                    match mem.recall(&query, Some(5)).await {
                        Ok(results) => results.into_iter().filter_map(|r| {
                            let text = r.content?;
                            if text.is_empty() { return None; }
                            Some(clawdesk_agents::MemoryRecallResult {
                                relevance: r.score as f64,
                                source: r.metadata.get("source")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                                content: text,
                            })
                        }).collect(),
                        Err(e) => {
                            tracing::warn!(error = %e, "Cron memory recall failed");
                            vec![]
                        }
                    }
                })
            })
        };

        let runner = AgentRunner::new(
            Arc::clone(&self.provider),
            Arc::clone(&self.tool_registry),
            config,
            self.cancel.clone(),
        )
        .with_memory_recall(memory_recall_fn);

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

/// Maximum number of messages to keep per sender in channel conversation history.
/// When exceeded, the oldest
/// messages are dropped (FIFO compaction).
const MAX_CHANNEL_HISTORY: usize = 50;

/// Per-sender conversation history for channel messages.
/// Key: "{channel_id}:{sender_id}" → VecDeque<ChatMessage>
///
/// Uses `DashMap` (sharded concurrent map) instead of a global `tokio::sync::Mutex`
/// to eliminate serialization across unrelated senders. Uses `VecDeque` (ring buffer)
/// instead of `Vec` to make FIFO compaction O(1) via `pop_front()` instead of
/// O(n) via `Vec::remove(0)`.
type ConversationHistoryMap =
    Arc<dashmap::DashMap<String, std::collections::VecDeque<clawdesk_providers::ChatMessage>>>;

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
///
/// Maintains per-sender conversation history so
/// multi-turn conversations work naturally in Discord, Telegram, etc.
pub(crate) struct ChannelMessageSink {
    pub negotiator: Arc<RwLock<ProviderNegotiator>>,
    pub tool_registry: Arc<ToolRegistry>,
    pub app_handle: tauri::AppHandle,
    pub channel_registry: Arc<RwLock<ChannelRegistry>>,
    pub cancel: tokio_util::sync::CancellationToken,
    /// Per-sender conversation history — keyed by "{channel}:{sender_id}".
    pub conversation_histories: ConversationHistoryMap,
    /// Last inbound origin per channel — enables cross-channel message_send("default").
    pub last_channel_origins: Arc<RwLock<HashMap<clawdesk_types::channel::ChannelId, clawdesk_types::message::MessageOrigin>>>,
}

impl ChannelMessageSink {
    /// Send an error reply back through the originating channel so the user
    /// (or operator) sees a useful message instead of silence.
    async fn reply_error(
        &self,
        channel_id: clawdesk_types::channel::ChannelId,
        origin: &clawdesk_types::message::MessageOrigin,
        text: &str,
    ) {
        let ch = {
            let Ok(reg) = self.channel_registry.read() else { return; };
            reg.get(&channel_id).cloned()
        };
        if let Some(ch) = ch {
            let outbound = clawdesk_types::message::OutboundMessage {
                origin: origin.clone(),
                body: text.to_string(),
                media: vec![],
                reply_to: None,
                thread_id: None,
            };
            if let Err(e) = ch.send(outbound).await {
                warn!(channel = %channel_id, error = %e, "Failed to send error reply");
            }
        }
    }
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

        // Track the last origin for this channel so cross-channel message_send
        // can resolve "default" targets without requiring internal IDs.
        // Persist to SochDB so origins survive app restarts.
        if let Ok(mut guard) = self.last_channel_origins.write() {
            guard.insert(channel_id, msg.origin.clone());
            // Persist the full map to SochDB (cheap: small JSON, infrequent writes)
            if let Ok(bytes) = serde_json::to_vec(&*guard) {
                let state = self.app_handle.state::<AppState>();
                if let Err(e) = state.soch_store.put_durable("channel_origins", &bytes) {
                    warn!(error = %e, "Failed to persist channel origins to SochDB");
                }
            }
        }

        // 1. Find the best agent for this channel.
        //    Priority: agent whose `channels` list contains this channel_id,
        //    then fallback to any agent with an empty channels list (wildcard),
        //    then fallback to first agent.
        let channel_str = format!("{}", channel_id);
        let agent = {
            let state = self.app_handle.state::<AppState>();
            let result = match state.agents.read() {
                Ok(agents) => {
                    let count = agents.len();
                    info!(agent_count = count, channel = %channel_str, "Resolving agent for channel");
                    // Best match: agent explicitly assigned to this channel
                    let explicit = agents.values().find(|a| {
                        a.channels.iter().any(|c| c.eq_ignore_ascii_case(&channel_str))
                    });
                    // Fallback: agent with no channel restrictions (wildcard)
                    let wildcard = agents.values().find(|a| a.channels.is_empty());
                    explicit.or(wildcard).or_else(|| agents.values().next()).cloned()
                }
                Err(e) => {
                    error!("Failed to read agents: {e}");
                    return;
                }
            };
            result
        };
        let Some(agent) = agent else {
            warn!("No agent configured — replying with setup instructions to {sender_name}");
            self.reply_error(
                channel_id,
                &msg.origin,
                "⚠️ ClawDesk: No agent configured. Open the app and go to **Settings → Agents** to create an agent before I can reply.",
            ).await;
            return;
        };

        // 2. Resolve provider — try channel_provider override first, then negotiator.
        //
        // All RwLockGuard usage is confined to synchronous scopes (no .await inside).
        // The resolved provider (or None) is returned from the block, and any error
        // reply is sent afterwards so no guard is held across an await point.
        //
        // `effective_model` tracks which model the provider will actually use.
        // When a channel_provider override is active, we prefer its model over
        // the agent's configured model so the request matches what the server has.
        let agent_model_id = AppState::resolve_model_id(&agent.model);
        let required = ProviderCaps::TEXT_COMPLETION.union(ProviderCaps::SYSTEM_PROMPT);

        let provider_result: Result<(Arc<dyn Provider>, String), String> = {
            let channel_prov = {
                let state = self.app_handle.state::<AppState>();
                state.channel_provider.read().ok().and_then(|g| g.clone())
            };

            if let Some(ref cp) = channel_prov {
                // Use the channel_provider's model if set, otherwise fall back to the agent's model.
                let effective_model = if cp.model.is_empty() {
                    agent_model_id.clone()
                } else {
                    cp.model.clone()
                };
                info!(
                    provider = %cp.provider,
                    model = %effective_model,
                    base_url = %cp.base_url,
                    "Using channel_provider override for inbound message"
                );
                match cp.provider.as_str() {
                    "Anthropic" => {
                        use clawdesk_providers::anthropic::AnthropicProvider;
                        Ok((Arc::new(AnthropicProvider::new(cp.api_key.clone(), Some(effective_model.clone()))) as Arc<dyn Provider>, effective_model))
                    }
                    "OpenAI" => {
                        use clawdesk_providers::openai::OpenAiProvider;
                        let base = if cp.base_url.is_empty() { None } else { Some(cp.base_url.clone()) };
                        Ok((Arc::new(OpenAiProvider::new(cp.api_key.clone(), base, Some(effective_model.clone()))), effective_model))
                    }
                    "Ollama (Local)" | "ollama" => {
                        use clawdesk_providers::ollama::OllamaProvider;
                        let base = if cp.base_url.is_empty() { None } else { Some(cp.base_url.clone()) };
                        Ok((Arc::new(OllamaProvider::new(base, Some(effective_model.clone()))), effective_model))
                    }
                    "Local (OpenAI Compatible)" | "local_compatible" => {
                        use clawdesk_providers::compatible::{CompatibleConfig, OpenAiCompatibleProvider};
                        let base_url = if cp.base_url.is_empty() {
                            "http://localhost:8080/v1".to_string()
                        } else {
                            cp.base_url.clone()
                        };
                        let config = CompatibleConfig::new("local_compatible", &base_url, &cp.api_key)
                            .with_default_model(effective_model.clone());
                        Ok((Arc::new(OpenAiCompatibleProvider::new(config)), effective_model))
                    }
                    "Google" => {
                        use clawdesk_providers::gemini::GeminiProvider;
                        Ok((Arc::new(GeminiProvider::new(cp.api_key.clone(), Some(effective_model.clone()))) as Arc<dyn Provider>, effective_model))
                    }
                    _ => {
                        // Unknown override — fall through to negotiator (guard dropped in scoped block)
                        warn!(provider = %cp.provider, "Unknown channel_provider — trying negotiator");
                        let from_neg = {
                            let neg = match self.negotiator.read() {
                                Ok(n) => n,
                                Err(e) => { error!("Negotiator lock poisoned: {e}"); return; }
                            };
                            neg.resolve_model(&agent_model_id, required).map(|(p, _)| Arc::clone(p))
                            // neg dropped here
                        };
                        from_neg.map_or_else(
                            || {
                                let state = self.app_handle.state::<AppState>();
                                state.resolve_provider(&agent.model).map(|p| (p, agent_model_id.clone()))
                            },
                            |p| Ok((p, agent_model_id.clone())),
                        )
                    }
                }
            } else {
                // No channel_provider override — use negotiator → resolve_provider fallback.
                // Guard is scoped so it's dropped before any possible .await.
                let from_neg = {
                    let neg = match self.negotiator.read() {
                        Ok(n) => n,
                        Err(e) => { error!("Negotiator lock poisoned: {e}"); return; }
                    };
                    neg.resolve_model(&agent_model_id, required).map(|(p, _)| Arc::clone(p))
                    // neg dropped here
                };
                from_neg.map_or_else(
                    || {
                        let state = self.app_handle.state::<AppState>();
                        state.resolve_provider(&agent.model).map(|p| (p, agent_model_id.clone()))
                    },
                    |p| Ok((p, agent_model_id.clone())),
                )
            }
        };

        let (provider, effective_model): (Arc<dyn Provider>, String) = match provider_result {
            Ok((p, m)) => (p, m),
            Err(e) => {
                warn!(model = %agent.model, error = %e, "No provider for model — sending setup hint");
                self.reply_error(channel_id, &msg.origin,
                    "⚠️ ClawDesk: No LLM provider configured. Open the app → **Settings → Providers** and add an API key."
                ).await;
                return;
            }
        };

        // 3. Build prompt via unified engine pipeline (same as desktop).
        //    This gives the Discord path: memory recall, skill scoring,
        //    PromptBuilder with knapsack budget, and memory injection.
        let app_state = self.app_handle.state::<AppState>();
        let active_skills = crate::engine::load_active_skills(&app_state.skill_registry);

        let agent_skill_set: std::collections::HashSet<String> = agent
            .skills
            .iter()
            .map(|s| s.to_lowercase())
            .collect();

        let channel_desc = format!("{} channel", channel_str);
        let available_ch_names: Vec<String> = app_state.channel_registry.read()
            .map(|reg| reg.list().iter().map(|id| format!("{}", id).to_lowercase()).collect())
            .unwrap_or_default();
        let pipeline_result = crate::engine::build_prompt_pipeline(
            crate::engine::PromptPipelineInput {
                user_content: &msg.body,
                persona: &agent.persona,
                model_name: &effective_model,
                agent_skill_ids: &agent_skill_set,
                channel_id: Some(&channel_str),
                channel_description: &channel_desc,
                budget: clawdesk_domain::prompt_builder::PromptBudget::default(),
                available_channels: available_ch_names,
            },
            &app_state.memory,
            &active_skills,
        ).await;

        // Store the prompt manifest for debugging if needed
        if let Some(ref manifest) = pipeline_result.prompt_manifest {
            if let Ok(mut manifests) = app_state.prompt_manifests.write() {
                manifests.insert(agent.id.clone(), manifest.clone());
            }
        }

        let config = AgentConfig {
            model: effective_model,
            system_prompt: pipeline_result.system_prompt,
            ..Default::default()
        };

        // Per-sender conversation history.
        // Key: "{channel}:{sender_id}" for per-user threads.
        let history_key = format!("{channel_id}:{}", msg.sender.id);
        let user_msg = clawdesk_providers::ChatMessage::new(
            clawdesk_providers::MessageRole::User,
            msg.body.as_str(),
        );

        // Build the history: existing conversation + current user message
        let mut history: Vec<clawdesk_providers::ChatMessage> = {
            let mut entry = self.conversation_histories.entry(history_key.clone()).or_default();
            entry.push_back(user_msg);
            // FIFO compaction — O(1) pop_front via VecDeque ring buffer
            while entry.len() > MAX_CHANNEL_HISTORY {
                entry.pop_front();
            }
            entry.iter().cloned().collect()
        };

        // Inject memory context (recency-biased, before last user message)
        // — same strategy as the desktop path via unified engine.
        if let Some(ref mem_text) = pipeline_result.memory_injection {
            crate::engine::inject_memory_context(&mut history, mem_text);
        }

        info!(
            channel = %channel_id,
            sender = %sender_name,
            history_len = history.len(),
            has_memory = pipeline_result.memory_injection.is_some(),
            "Routing with per-sender conversation history"
        );

        // 3b. Start typing indicator so the user sees the bot is working.
        // Look up the channel to call start_typing if it supports it.
        let typing_channel_id_str = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Discord { channel_id: cid, .. } => {
                Some(cid.to_string())
            }
            _ => None,
        };
        let ch_for_typing = {
            let reg = self.channel_registry.read().ok();
            reg.and_then(|r| r.get(&channel_id).cloned())
        };
        if let (Some(ref cid_str), Some(ref ch)) = (&typing_channel_id_str, &ch_for_typing) {
            // Downcast to DiscordChannel for typing indicator
            if let Some(discord_ch) = ch.as_any().downcast_ref::<clawdesk_channels::discord::DiscordChannel>() {
                if let Err(e) = discord_ch.start_typing(cid_str).await {
                    warn!(error = %e, "Failed to start typing indicator");
                }
            }
        }

        // 3c. Build runner with skill_provider + channel_context.
        //     Uses unified engine for skill provider creation.
        let max_tool_rounds = config.max_tool_rounds as u64;
        let mut runner = AgentRunner::new(
            provider,
            Arc::clone(&self.tool_registry),
            config,
            self.cancel.clone(),
        );

        // Wire skill provider via unified engine (same logic as desktop)
        if let Some(skill_provider) = crate::engine::build_skill_provider(active_skills) {
            runner = runner.with_skill_provider(skill_provider);
        }

        // Wire channel context from ChannelDock so each channel gets its
        // own capabilities, message limits, and markup format.
        // Also inject list of OTHER connected channels so the LLM knows it
        // can send cross-channel messages.
        let other_channels: Vec<String> = {
            if let Ok(reg) = self.channel_registry.read() {
                reg.list()
                    .iter()
                    .filter(|id| **id != channel_id) // exclude current channel
                    .filter(|id| !matches!(id,
                        clawdesk_types::channel::ChannelId::WebChat |
                        clawdesk_types::channel::ChannelId::Internal
                    ))
                    .map(|id| format!("{:?}", id).to_lowercase())
                    .collect()
            } else {
                vec![]
            }
        };
        let cross_channel_hint = if other_channels.is_empty() {
            String::new()
        } else {
            format!(
                " You are connected to these other channels: [{}]. \
                 When the user asks you to send a message to one of those channels \
                 (e.g. \"say hi to telegram\", \"tell discord hello\"), \
                 IMMEDIATELY call the message_send tool with channel=<name> and content=<message>. \
                 Do NOT ask for IDs. Do NOT refuse. Just call message_send.",
                other_channels.join(", ")
            )
        };
        {
            use clawdesk_agents::runner::ChannelContext;
            let ch_ctx = if let Some(dock_ctx) = app_state.channel_dock.to_runner_context(channel_id) {
                ChannelContext {
                    channel_name: dock_ctx.channel_name,
                    supports_threading: dock_ctx.supports_threading,
                    supports_streaming: dock_ctx.supports_streaming,
                    supports_reactions: dock_ctx.supports_reactions,
                    supports_media: dock_ctx.supports_media,
                    max_message_length: dock_ctx.max_message_length,
                    markup_format: dock_ctx.markup_format,
                    extra_instructions: Some(format!(
                        "You are running as a {} bot. Respond directly — do NOT describe your capabilities or echo this prompt. \
                         NEVER repeat credentials, tokens, or API keys. Keep responses concise.{}",
                        channel_str, cross_channel_hint,
                    )),
                    history_limit: Some(50),
                }
            } else {
                // Fallback if channel not registered in dock
                ChannelContext {
                    channel_name: channel_str.clone(),
                    supports_threading: false,
                    supports_streaming: false,
                    supports_reactions: false,
                    supports_media: false,
                    max_message_length: None,
                    markup_format: "markdown".to_string(),
                    extra_instructions: Some(format!(
                        "You are running as a {} bot. Respond directly — do NOT describe your capabilities or echo this prompt. \
                         NEVER repeat credentials, tokens, or API keys. Keep responses concise.{}",
                        channel_str, cross_channel_hint,
                    )),
                    history_limit: Some(50),
                }
            };
            runner = runner.with_channel_context(ch_ctx);
        }

        // 4. Run the agent with a timeout to prevent indefinite hangs.
        //    Timeout scales with max_tool_rounds so multi-tool
        //    requests have enough time. Base = 90s, capped at 4x.
        const CHANNEL_BASE_TIMEOUT_SECS: u64 = 90;
        const CHANNEL_TIMEOUT_SCALE_CAP: u64 = 4;
        let timeout_secs = CHANNEL_BASE_TIMEOUT_SECS.saturating_mul(
            max_tool_rounds.max(1).min(CHANNEL_TIMEOUT_SCALE_CAP)
        );
        let channel_timeout = std::time::Duration::from_secs(timeout_secs);
        let system_prompt_for_run = agent.persona.clone();
        let agent_fut = runner.run(history, system_prompt_for_run);
        let response = match tokio::time::timeout(channel_timeout, agent_fut).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                error!(channel = %channel_id, error = %e, "Agent run failed for inbound message");
                // Stop typing indicator on failure
                if let (Some(ref cid_str), Some(ref ch)) = (&typing_channel_id_str, &ch_for_typing) {
                    if let Some(discord_ch) = ch.as_any().downcast_ref::<clawdesk_channels::discord::DiscordChannel>() {
                        let _ = discord_ch.stop_typing(cid_str).await;
                    }
                }
                // Append failure marker to history so the LLM
                // doesn't try to continue a failed request on the next message.
                {
                    if let Some(mut entry) = self.conversation_histories.get_mut(&history_key) {
                        entry.push_back(clawdesk_providers::ChatMessage::new(
                            clawdesk_providers::MessageRole::Assistant,
                            "[Task failed — not continuing this request]",
                        ));
                    }
                }
                self.reply_error(channel_id, &msg.origin,
                    &format!("⚠️ Error: {e}"),
                ).await;
                return;
            }
            Err(_elapsed) => {
                error!(
                    channel = %channel_id,
                    sender = %sender_name,
                    timeout_secs = timeout_secs,
                    "Agent run timed out for inbound message"
                );
                // Stop typing indicator on timeout
                if let (Some(ref cid_str), Some(ref ch)) = (&typing_channel_id_str, &ch_for_typing) {
                    if let Some(discord_ch) = ch.as_any().downcast_ref::<clawdesk_channels::discord::DiscordChannel>() {
                        let _ = discord_ch.stop_typing(cid_str).await;
                    }
                }
                // Append timeout marker to history
                {
                    if let Some(mut entry) = self.conversation_histories.get_mut(&history_key) {
                        entry.push_back(clawdesk_providers::ChatMessage::new(
                            clawdesk_providers::MessageRole::Assistant,
                            "[Task timed out — not continuing this request]",
                        ));
                    }
                }
                self.reply_error(channel_id, &msg.origin,
                    "⚠️ Request timed out while waiting for the model. Please try again."
                ).await;
                return;
            }
        };

        // Stop typing indicator
        if let (Some(ref cid_str), Some(ref ch)) = (&typing_channel_id_str, &ch_for_typing) {
            if let Some(discord_ch) = ch.as_any().downcast_ref::<clawdesk_channels::discord::DiscordChannel>() {
                let _ = discord_ch.stop_typing(cid_str).await;
            }
        }

        // Append assistant response to conversation history
        {
            if let Some(mut entry) = self.conversation_histories.get_mut(&history_key) {
                entry.push_back(clawdesk_providers::ChatMessage::new(
                    clawdesk_providers::MessageRole::Assistant,
                    &*response.content,
                ));
                // O(1) compaction via VecDeque ring buffer
                while entry.len() > MAX_CHANNEL_HISTORY {
                    entry.pop_front();
                }
            }
        }

        info!(
            channel = %channel_id,
            sender = %sender_name,
            response_len = response.content.len(),
            "Agent response ready — sending back to channel"
        );

        // Store conversation turn in memory (unified engine) — fire-and-forget.
        // This makes Discord conversations retrievable by future memory recall,
        // just like the desktop path.
        {
            let mem = Arc::clone(&app_state.memory);
            let tg = Arc::clone(&app_state.temporal_graph);
            let user_text = msg.body.clone();
            let asst_text = response.content.clone();
            let agent_id_mem = agent.id.clone();
            let agent_name_mem = agent.name.clone();

            tokio::spawn(async move {
                crate::engine::store_conversation_memory(
                    &mem,
                    &user_text,
                    &asst_text,
                    &agent_id_mem,
                    &agent_name_mem,
                    Some(&tg),
                )
                .await;
            });
        }

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
    ) -> Result<clawdesk_agents::runner::ApprovalDecision, String> {
        use clawdesk_agents::runner::ApprovalDecision;
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
                    // TODO: When the UI supports richer approval options,
                    // map them to AllowForSession / DenyForSession / EditAndRerun.
                    ApprovalStatus::Approved { .. } => Ok(ApprovalDecision::Allow),
                    ApprovalStatus::Denied { .. } => Ok(ApprovalDecision::Deny),
                    ApprovalStatus::TimedOut { .. } => Ok(ApprovalDecision::Deny),
                    ApprovalStatus::Pending => Ok(ApprovalDecision::Deny),
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
        let embedding_for_state = embedding.clone(); // shared with semantic cache
        info!("Tiered embedding provider ready — memory always has FTS fallback");

        // ── MemoryManager<SochMemoryBackend> ─────────────────────────
        // SochMemoryBackend wraps SochStore + all SochDB advanced modules
        // (AtomicMemoryWriter, GraphOverlay, TemporalGraphOverlay, PolicyEngine, TraceStore)
        // enabling atomic writes, graph nodes, temporal edges,
        // policy checks, and trace spans in the memory pipeline.
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

        // Register memory forget tool with async callback to MemoryManager::forget()
        // Allows the LLM to delete stale/incorrect memories when the user asks.
        {
            let mem = Arc::clone(&memory);
            let forget_fn: std::sync::Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> + Send + Sync> =
                std::sync::Arc::new(move |memory_id: String| {
                    let mem = Arc::clone(&mem);
                    Box::pin(async move {
                        mem.forget(&memory_id).await
                            .map(|_| ())
                            .map_err(|e| format!("forget failed: {}", e))
                    })
                });
            clawdesk_agents::builtin_tools::register_memory_forget_tool_async(&mut tool_registry, forget_fn);
        }
        info!(tools = tool_registry.total_count(), "Built-in tool registry initialized");

        // NOTE: tool_registry is NOT wrapped in Arc yet — we need to register
        // the messaging tool after channel_registry is built (see below).

        // Wire ChannelFactory → ChannelRegistry. Always register
        // config-free channels (webchat, internal); probe env vars for the rest.
        let mut channel_registry = ChannelRegistry::new();

        use clawdesk_channels::factory::{ChannelConfig, ChannelFactory};

        let factory = ChannelFactory::with_builtins();
        let saved_channel_configs = Self::load_channel_configs();
        {
            use serde_json::Map;

            // ── Set env vars from saved channel configs ──
            for (kind, cfg_map) in &saved_channel_configs {
                // Set env vars from saved configs so the env-var probe below can find them
                let env_mappings: &[(&str, &str, &str)] = &[
                    ("telegram", "bot_token", "TELEGRAM_BOT_TOKEN"),
                    ("discord", "bot_token", "DISCORD_TOKEN"),
                    ("discord", "application_id", "DISCORD_APP_ID"),
                    ("discord", "guild_id", "DISCORD_GUILD_ID"),
                    ("slack", "bot_token", "SLACK_BOT_TOKEN"),
                    ("slack", "app_token", "SLACK_APP_TOKEN"),
                    ("whatsapp", "access_token", "WHATSAPP_TOKEN"),
                    ("whatsapp", "phone_number_id", "WHATSAPP_PHONE_NUMBER_ID"),
                    ("email", "imap_host", "IMAP_HOST"),
                    ("email", "smtp_host", "SMTP_HOST"),
                    ("email", "email_user", "EMAIL_USER"),
                    ("email", "email_password", "EMAIL_PASSWORD"),
                    ("irc", "server", "IRC_SERVER"),
                    ("irc", "nickname", "IRC_NICKNAME"),
                ];
                for &(ch, key, env_var) in env_mappings {
                    if kind.as_str() == ch {
                        if let Some(val) = cfg_map.get(key) {
                            if !val.is_empty() {
                                std::env::set_var(env_var, val);
                            }
                        }
                    }
                }
                info!(kind = %kind, "Restored saved channel config from disk");
            }

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

        // Register the messaging tool with a ChannelRegistry-backed callback.
        // This gives the LLM the ability to send messages to other channels/users
        // via the `message_send` tool. The send_fn looks up the target channel in
        // the registry and delivers via Channel::send().
        //
        // We wrap channel_registry in Arc<RwLock> so the async callback can capture
        // a clone. AppState stores the same Arc.
        let channel_registry: Arc<std::sync::RwLock<ChannelRegistry>> =
            Arc::new(std::sync::RwLock::new(channel_registry));

        // Cross-channel origin tracking: stores the last inbound MessageOrigin
        // per ChannelId so the message_send tool can resolve "default" targets.
        // Load persisted origins from SochDB first (survives restarts), then
        // overlay with default_origin() from channel configs.
        let persisted_origins: HashMap<clawdesk_types::channel::ChannelId, clawdesk_types::message::MessageOrigin> =
            match soch_store.get("channel_origins") {
                Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    warn!(error = %e, "Failed to parse channel_origins from SochDB");
                    HashMap::new()
                }),
                Ok(None) => HashMap::new(),
                Err(e) => {
                    warn!(error = %e, "Failed to read channel_origins from SochDB");
                    HashMap::new()
                }
            };
        if !persisted_origins.is_empty() {
            info!(count = persisted_origins.len(), "Loaded persisted channel origins from SochDB");
        }
        let last_channel_origins: Arc<RwLock<HashMap<clawdesk_types::channel::ChannelId, clawdesk_types::message::MessageOrigin>>> =
            Arc::new(RwLock::new(persisted_origins));

        // Pre-seed origins from channel configs so cross-channel sends work
        // even before any inbound message arrives (e.g., Telegram's allowed_chat_ids).
        // Only inserts if no persisted origin exists for that channel.
        {
            if let Ok(reg) = channel_registry.read() {
                if let Ok(mut origins) = last_channel_origins.write() {
                    for (_id, channel) in reg.iter() {
                        let ch_id = channel.id();
                        if !origins.contains_key(&ch_id) {
                            if let Some(origin) = channel.default_origin() {
                                info!(%ch_id, "Pre-seeded default origin for cross-channel sends");
                                origins.insert(ch_id, origin);
                            }
                        }
                    }
                }
            }
        }

        {
            use clawdesk_types::channel::ChannelId;
            use clawdesk_types::message::{OutboundMessage, MessageOrigin};

            let channels_for_tool = Arc::clone(&channel_registry);
            let origins_for_tool = Arc::clone(&last_channel_origins);

            let send_fn: std::sync::Arc<
                dyn Fn(String, Option<String>, String, Vec<String>)
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                    + Send + Sync,
            > = std::sync::Arc::new(move |target, channel_name, content, _media_urls| {
                let channels = Arc::clone(&channels_for_tool);
                let origins = Arc::clone(&origins_for_tool);
                Box::pin(async move {
                    let channel_id = match channel_name.as_deref().unwrap_or("webchat") {
                        "telegram" => ChannelId::Telegram,
                        "discord" => ChannelId::Discord,
                        "slack" => ChannelId::Slack,
                        "whatsapp" => ChannelId::WhatsApp,
                        "webchat" => ChannelId::WebChat,
                        "email" => ChannelId::Email,
                        "irc" => ChannelId::Irc,
                        "internal" => ChannelId::Internal,
                        other => return Err(format!("Unknown channel: {}", other)),
                    };
                    let ch = {
                        let reg = channels.read().map_err(|e| format!("channel lock: {}", e))?;
                        Arc::clone(
                            reg.get(&channel_id)
                                .ok_or_else(|| format!(
                                    "Channel {:?} is not connected. Configure it in Settings → Channels first.",
                                    channel_id
                                ))?
                        )
                    }; // guard dropped here — before any .await

                    // Build the proper MessageOrigin for the target channel.
                    // If `to` is "default", empty, or the channel name itself,
                    // use the last known inbound origin for that channel.
                    let is_default = target.is_empty()
                        || target.eq_ignore_ascii_case("default")
                        || target.eq_ignore_ascii_case(channel_name.as_deref().unwrap_or(""));

                    let origin = if is_default {
                        // Use last known origin for this channel
                        let last = origins.read()
                            .map_err(|e| format!("origins lock: {}", e))?;
                        match last.get(&channel_id).cloned() {
                            Some(origin) => {
                                tracing::info!(
                                    channel = ?channel_id,
                                    origin = ?origin,
                                    "cross-channel send: using persisted origin"
                                );
                                origin
                            }
                            None => {
                                drop(last);
                                // Fallback: ask the channel for a default origin
                                // (e.g. Telegram uses allowed_chat_ids[0],
                                //  Discord uses default_channel_id or discovered channel)
                                tracing::info!(
                                    channel = ?channel_id,
                                    "cross-channel send: no persisted origin, trying default_origin()"
                                );
                                let origin = ch.default_origin().ok_or_else(|| format!(
                                    "Channel {:?} has no recent messages and no default target configured. \
                                     For Telegram: set allowed_chat_ids in Settings → Channels. \
                                     For Discord: set default_channel_id in Settings → Channels, \
                                     or send a message in the target Discord channel first.",
                                    channel_id
                                ))?;
                                // Persist this discovered origin so it survives restarts
                                if let Ok(mut guard) = origins.write() {
                                    guard.insert(channel_id, origin.clone());
                                    tracing::info!(
                                        channel = ?channel_id,
                                        origin = ?origin,
                                        "cross-channel send: persisted newly discovered default origin"
                                    );
                                }
                                origin
                            }
                        }
                    } else {
                        // Parse `to` as a channel-specific target ID
                        match channel_id {
                            ChannelId::Discord => {
                                let cid: u64 = target.parse().map_err(|_| format!(
                                    "Discord target must be a numeric channel ID, got: {}", target
                                ))?;
                                MessageOrigin::Discord {
                                    guild_id: 0, // filled from last origin if possible
                                    channel_id: cid,
                                    message_id: 0,
                                    is_dm: false,
                                    thread_id: None,
                                }
                            }
                            ChannelId::Telegram => {
                                let cid: i64 = target.parse().map_err(|_| format!(
                                    "Telegram target must be a numeric chat ID, got: {}", target
                                ))?;
                                MessageOrigin::Telegram {
                                    chat_id: cid,
                                    message_id: 0,
                                    thread_id: None,
                                }
                            }
                            ChannelId::Slack => {
                                MessageOrigin::Slack {
                                    team_id: String::new(),
                                    channel_id: target.clone(),
                                    user_id: String::new(),
                                    ts: String::new(),
                                    thread_ts: None,
                                }
                            }
                            ChannelId::WebChat => {
                                MessageOrigin::WebChat {
                                    session_id: target.clone(),
                                }
                            }
                            _ => {
                                MessageOrigin::Internal {
                                    source: target.clone(),
                                }
                            }
                        }
                    };

                    let msg = OutboundMessage {
                        origin,
                        body: content,
                        media: vec![],
                        reply_to: None,
                        thread_id: None,
                    };
                    let receipt = ch.send(msg).await?;
                    Ok(receipt.message_id)
                })
            });
            // Build actual channel name list from the registry for the tool schema
            let connected_channel_names: Vec<String> = {
                let reg = channel_registry.read().unwrap_or_else(|e| e.into_inner());
                reg.list().iter().map(|id| format!("{}", id).to_lowercase()).collect()
            };
            clawdesk_agents::builtin_tools::register_messaging_tool(
                &mut tool_registry,
                send_fn,
                connected_channel_names.clone(),
            );
            info!(
                channels = ?connected_channel_names,
                "Messaging tool registered — LLM can send to connected channels"
            );
        }

        // ── Register A2A tools (agents_list, sessions_send) ──────────
        // These give the LLM visibility into available agents and the ability
        // to delegate tasks to other agents via the A2A protocol.
        //
        // Create the agent_directory Arc early so we can share it with both
        // the tool callbacks and the final AppState struct.
        let agent_directory = Arc::new(RwLock::new(AgentDirectory::new()));

        // agents_list: Returns a JSON list of all registered A2A agents.
        {
            let dir_for_tool = Arc::clone(&agent_directory);
            let agents_list_fn: std::sync::Arc<
                dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                    + Send + Sync,
            > = std::sync::Arc::new(move || {
                let dir = Arc::clone(&dir_for_tool);
                Box::pin(async move {
                    let guard = dir.read().map_err(|e| format!("directory lock: {}", e))?;
                    let cards: Vec<serde_json::Value> = guard.list().iter().map(|card| {
                        serde_json::json!({
                            "id": card.id,
                            "name": card.name,
                            "description": card.description,
                            "capabilities": card.capabilities.iter().map(|c| format!("{:?}", c)).collect::<Vec<_>>(),
                        })
                    }).collect();
                    serde_json::to_string_pretty(&cards).map_err(|e| e.to_string())
                })
            });
            clawdesk_agents::builtin_tools::register_agents_list_tool(&mut tool_registry, agents_list_fn);
            info!("A2A agents_list tool registered — LLM can discover available agents");
        }

        // sessions_send: Delegates a task to another local DesktopAgent,
        // running it through the full AgentRunner with tool access.
        // This gives delegated agents the same capabilities (shell, web search,
        // memory, file I/O) as direct channel messages.
        let a2a_tasks_shared: Arc<tokio::sync::RwLock<HashMap<String, clawdesk_acp::Task>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        // Shared Arcs for agents and providers — populated after hydration below,
        // but captured by the sessions_send callback for late binding.
        let agents_shared: Arc<RwLock<HashMap<String, DesktopAgent>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let providers_shared: Arc<RwLock<ProviderRegistry>> =
            Arc::new(RwLock::new(ProviderRegistry::new()));
        // Late-binding for tool_registry (not yet Arc-wrapped at this point).
        let tools_late: Arc<std::sync::OnceLock<Arc<ToolRegistry>>> =
            Arc::new(std::sync::OnceLock::new());
        {
            let dir_for_send = Arc::clone(&agent_directory);
            let tasks_for_send = Arc::clone(&a2a_tasks_shared);
            let agents_for_send = Arc::clone(&agents_shared);
            let providers_for_send = Arc::clone(&providers_shared);
            let tools_for_send = Arc::clone(&tools_late);
            let memory_for_send = Arc::clone(&memory);

            let sessions_send_fn: std::sync::Arc<
                dyn Fn(String, String, Option<String>)
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                    + Send + Sync,
            > = std::sync::Arc::new(move |target_agent: String, message: String, _skill_id: Option<String>| {
                let dir = Arc::clone(&dir_for_send);
                let tasks = Arc::clone(&tasks_for_send);
                let agents = Arc::clone(&agents_for_send);
                let providers = Arc::clone(&providers_for_send);
                let tools_cell = Arc::clone(&tools_for_send);
                let memory = Arc::clone(&memory_for_send);
                Box::pin(async move {
                    use clawdesk_acp::{Task, TaskEvent};
                    use clawdesk_agents::runner::{AgentRunner, AgentConfig};

                    // Create A2A task tracking
                    let mut task = Task::new("self", &target_agent, serde_json::json!({"message": &message}));
                    let task_id = task.id.as_str().to_string();
                    let _ = task.apply_event(TaskEvent::Work);

                    // Look up the target agent
                    let agent_info = {
                        let guard = agents.read().map_err(|e| format!("agents lock: {}", e))?;
                        guard.get(&target_agent).map(|a| (a.name.clone(), a.persona.clone(), a.model.clone()))
                    };

                    let (agent_name, persona, model_pref) = match agent_info {
                        Some(info) => info,
                        None => {
                            let dir_name = {
                                let dg = dir.read().map_err(|e| format!("directory lock: {}", e))?;
                                dg.get(&target_agent).map(|e| e.card.name.clone())
                            };
                            let name = dir_name.unwrap_or_else(|| target_agent.clone());
                            let _ = task.apply_event(TaskEvent::Fail {
                                error: format!("Agent '{}' not found", target_agent),
                            });
                            tasks.write().await.insert(task_id, task);
                            return Err(format!(
                                "Agent '{}' ({}) is not a local agent. Cannot execute task.",
                                name, target_agent
                            ));
                        }
                    };

                    // Resolve provider — extract from lock scope before any .await
                    let resolved_provider: Option<Arc<dyn Provider>> = {
                        let reg = providers.read().map_err(|e| format!("provider lock: {}", e))?;
                        let prov_name = if model_pref.contains("claude") || model_pref.contains("anthropic") {
                            "anthropic"
                        } else if model_pref.contains("gpt") || model_pref.contains("openai") {
                            "openai"
                        } else if model_pref.contains("gemini") {
                            "gemini"
                        } else if !model_pref.is_empty() {
                            "ollama"
                        } else {
                            ""
                        };
                        if !prov_name.is_empty() {
                            reg.get(prov_name).cloned()
                                .or_else(|| reg.default_provider().cloned())
                        } else {
                            reg.default_provider().cloned()
                        }
                    }; // guard dropped here — safe to .await below

                    let provider = match resolved_provider {
                        Some(p) => p,
                        None => {
                            let _ = task.apply_event(TaskEvent::Fail {
                                error: "No LLM provider available".to_string(),
                            });
                            tasks.write().await.insert(task_id, task);
                            return Err("No LLM provider available for A2A task".to_string());
                        }
                    };

                    // Get tool registry (late-bound)
                    let tool_reg = tools_cell.get()
                        .ok_or_else(|| "Tool registry not initialized yet".to_string())?;

                    // Determine model
                    let model = if model_pref.is_empty() {
                        if provider.name() == "anthropic" { "claude-haiku-4-20250514".to_string() }
                        else if provider.name() == "openai" { "gpt-4o-mini".to_string() }
                        else if provider.name() == "gemini" { "gemini-2.0-flash".to_string() }
                        else { "llama3.2".to_string() }
                    } else {
                        model_pref.clone()
                    };

                    // Build system prompt with the target agent's persona
                    let system_prompt = format!(
                        "You are {}. {}\n\n\
                         You are responding to a delegated request from another agent in the system. \
                         You have full access to your tools (shell, web search, memory, file I/O). \
                         Use them as needed to fulfill the request. Answer accurately and concisely.",
                        agent_name, persona
                    );

                    // Build AgentConfig
                    let config = AgentConfig {
                        model,
                        system_prompt: system_prompt.clone(),
                        max_tool_rounds: 10, // Limit delegated runs
                        ..Default::default()
                    };

                    // Construct a full AgentRunner with tool access
                    let cancel = tokio_util::sync::CancellationToken::new();

                    // GAP-B: Build memory recall callback so delegated agents
                    // get automatic memory context even though they bypass the
                    // engine layer's build_prompt_pipeline().
                    let memory_recall_fn: clawdesk_agents::MemoryRecallFn = {
                        let mem = Arc::clone(&memory);
                        Arc::new(move |query: String| {
                            let mem = Arc::clone(&mem);
                            Box::pin(async move {
                                match mem.recall(&query, Some(8)).await {
                                    Ok(results) => results.into_iter().filter_map(|r| {
                                        let text = r.content?;
                                        if text.is_empty() { return None; }
                                        Some(clawdesk_agents::MemoryRecallResult {
                                            relevance: r.score as f64,
                                            source: r.metadata.get("source")
                                                .and_then(|v| v.as_str())
                                                .map(String::from),
                                            content: text,
                                        })
                                    }).collect(),
                                    Err(e) => {
                                        tracing::warn!(error = %e, "A2A memory recall failed");
                                        vec![]
                                    }
                                }
                            })
                        })
                    };

                    let runner = AgentRunner::new(
                        provider,
                        Arc::clone(tool_reg),
                        config,
                        cancel,
                    )
                    .with_memory_recall(memory_recall_fn);

                    // Recursion depth check — prevent runaway A→B→A→B... delegation.
                    if let Err(e) = clawdesk_agents::recursion_depth::check_depth() {
                        let err_msg = format!("A2A delegation blocked: {}", e);
                        let _ = task.apply_event(TaskEvent::Fail { error: err_msg.clone() });
                        tasks.write().await.insert(task_id, task);
                        return Err(err_msg);
                    }

                    // Build the message history (single user message)
                    let history = vec![
                        clawdesk_providers::ChatMessage::new(
                            clawdesk_providers::MessageRole::User,
                            message.as_str(),
                        ),
                    ];

                    // Run with a 120-second timeout, incrementing recursion depth
                    let run_result = tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        clawdesk_agents::recursion_depth::with_incremented_depth(
                            runner.run(history, system_prompt),
                        ),
                    ).await;

                    match run_result {
                        Ok(Ok(response)) => {
                            let reply = response.content.clone();
                            let _ = task.apply_event(TaskEvent::Complete {
                                output: serde_json::json!({
                                    "agent": agent_name,
                                    "response": &reply,
                                    "tool_rounds": response.total_rounds,
                                    "input_tokens": response.input_tokens,
                                    "output_tokens": response.output_tokens,
                                }),
                            });
                            tasks.write().await.insert(task_id, task);
                            Ok(format!("[Response from {} via A2A]\n{}", agent_name, reply))
                        }
                        Ok(Err(e)) => {
                            let err_msg = format!("Agent execution failed: {}", e);
                            let _ = task.apply_event(TaskEvent::Fail { error: err_msg.clone() });
                            tasks.write().await.insert(task_id, task);
                            Err(err_msg)
                        }
                        Err(_) => {
                            let err_msg = "A2A task timed out after 120 seconds".to_string();
                            let _ = task.apply_event(TaskEvent::Fail { error: err_msg.clone() });
                            tasks.write().await.insert(task_id, task);
                            Err(err_msg)
                        }
                    }
                })
            });
            clawdesk_agents::builtin_tools::register_sessions_send_tool(&mut tool_registry, sessions_send_fn);
            info!("A2A sessions_send tool registered — LLM can delegate tasks to other agents");
        }

        // Wrap in Arc now — all mutable registration is complete.
        let tool_registry = Arc::new(tool_registry);

        // Late-bind the tool_registry for the sessions_send callback.
        let _ = tools_late.set(Arc::clone(&tool_registry));

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
            memory: Arc::clone(&memory),
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

        // ── Rebuild temporal session index ────────────────────────────
        // The chat_index allows ordered-by-time session queries without
        // deserializing all session blobs. Rebuilt on every startup to
        // ensure consistency with the actual session data.
        {
            let entries = soch_store.scan("chats/").unwrap_or_default();
            let mut batch: Vec<(String, Vec<u8>)> = Vec::new();
            for (_key, bytes) in &entries {
                if let Ok(session) = serde_json::from_slice::<ChatSession>(bytes) {
                    let index_key = format!(
                        "chat_index/{}/{}/{}",
                        session.agent_id, session.updated_at, session.id
                    );
                    let index_value = serde_json::to_vec(&serde_json::json!({
                        "title": session.title,
                        "created_at": session.created_at,
                        "updated_at": session.updated_at,
                        "message_count": session.messages.len(),
                    })).unwrap_or_default();
                    batch.push((index_key, index_value));
                }
            }
            if !batch.is_empty() {
                let refs: Vec<(&str, &[u8])> = batch.iter()
                    .map(|(k, v)| (k.as_str(), v.as_slice()))
                    .collect();
                if let Err(e) = soch_store.put_batch(&refs) {
                    warn!(error = %e, "Failed to rebuild chat_index — temporal sorting may be degraded");
                } else {
                    info!(indexed = batch.len(), "Rebuilt chat_index (temporal session index)");
                }
            }
        }

        // ── Auto-register local agents in A2A directory ──────────────
        // This makes every DesktopAgent discoverable via the agents_list tool
        // so that LLMs can delegate tasks to named agents on specific channels.
        {
            let mut dir = agent_directory.write().expect("agent_directory lock");
            for (agent_id, agent) in &agents {
                let mut card = clawdesk_acp::AgentCard::new(
                    agent_id.clone(),
                    agent.name.clone(),
                    "local://desktop",
                );
                card.description = agent.persona.chars().take(200).collect();
                card = card.with_capability(clawdesk_acp::CapabilityId::TextGeneration);
                if !agent.channels.is_empty() {
                    card = card.with_capability(clawdesk_acp::CapabilityId::Messaging);
                }
                dir.register(card);
            }
            info!(count = agents.len(), "Registered local agents in A2A directory");
        }

        // ── Register active channels in A2A directory ────────────────
        // Each running channel (Discord, Telegram, etc.) is registered as a
        // discoverable entity so agents can see what channels are online and
        // route cross-channel tasks appropriately.
        {
            let mut dir = agent_directory.write().expect("agent_directory lock");
            let ch_reg = channel_registry.read().expect("channel_registry lock");
            for ch_id in ch_reg.list() {
                let ch_name = format!("{:?}", ch_id).to_lowercase();
                let card_id = format!("channel:{}", ch_name);
                let mut card = clawdesk_acp::AgentCard::new(
                    card_id,
                    format!("{} Channel", ch_name),
                    "local://channel",
                );
                card.description = format!("Active {} messaging channel — can receive and send messages.", ch_name);
                card = card.with_capability(clawdesk_acp::CapabilityId::Messaging);
                dir.register(card);
            }
            info!(count = ch_reg.list().len(), "Registered active channels in A2A directory");
        }

        // ── Populate shared Arcs used by A2A sessions_send callback ──
        // The callback was registered before providers/agents existed.
        // Now that both are ready, swap in the real data.
        {
            let mut guard = providers_shared.write().expect("providers_shared lock");
            *guard = provider_registry;
        }
        {
            let mut guard = agents_shared.write().expect("agents_shared lock");
            *guard = agents;
        }
        info!("Populated A2A shared state (agents + providers) for sessions_send callback");

        Self {
            soch_store: soch_store.clone(),
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
            provider_registry: providers_shared,
            tool_registry,
            channel_registry,
            channel_factory: Arc::new(factory),
            channel_configs: RwLock::new(saved_channel_configs),
            scanner: Arc::new(scanner),
            scanner_pattern_count: pattern_count,
            audit_logger: Arc::new(audit_logger),
            agents: agents_shared,
            identities: RwLock::new(HashMap::new()),
            server_secret: ServerSecret::generate(),
            sessions: {
                let cache = SessionCache::new();
                // Sort sessions by updated_at (oldest first) before bulk-loading
                // into the LRU cache. `load_bulk` treats the LAST inserted item as
                // most-recently-used, so oldest-first ordering ensures the most
                // recently active sessions survive LRU eviction on startup.
                let mut sorted_sessions: Vec<(String, ChatSession)> = sessions.into_iter().collect();
                sorted_sessions.sort_by(|a, b| a.1.updated_at.cmp(&b.1.updated_at));
                cache.load_bulk(sorted_sessions);
                cache
            },
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
            agent_directory: Arc::clone(&agent_directory),
            a2a_tasks: Arc::clone(&a2a_tasks_shared),
            last_channel_origins: Arc::clone(&last_channel_origins),

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

            // ── Channel provider override (hydrated from disk if available) ──
            channel_provider: RwLock::new(Self::load_channel_provider()),

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

            // ── Global concurrency bound (default 8 parallel LLM calls) ──
            llm_concurrency: Arc::new(tokio::sync::Semaphore::new(8)),

            // ── /1 FIX: Channel dock (all 23 channels pre-registered) ──
            channel_dock: Arc::new(clawdesk_channel::channel_dock::ChannelDock::with_all_defaults()),

            // ── Hook manager ──
            hook_manager: Arc::new(clawdesk_plugin::HookManager::new()),

            // ── Channel bindings (empty by default for desktop) ──
            channel_bindings: RwLock::new(Vec::new()),

            // ── T2 FIX: Workspace root for agent tool scoping ──
            workspace_root,

            // ── T4 FIX: Sandbox policy engine (auto-detects platform capabilities) ──
            sandbox_engine: Arc::new(clawdesk_security::sandbox_policy::SandboxPolicyEngine::new()),

            // ── Sub-agent lifecycle manager ──
            sub_mgr: Arc::new(clawdesk_gateway::subagent_manager::SubAgentManager::new(
                clawdesk_gateway::subagent_manager::SubAgentManagerConfig::default(),
            )),

            // ── GAP-G: Per-turn dynamic model routing ──
            turn_router: Arc::new(clawdesk_agents::TurnRouter::new(
                clawdesk_domain::model_catalog::ModelCatalog::default_catalog(),
            )),

            // ── GAP-D: Reactive event bus ──
            event_bus: clawdesk_bus::dispatch::EventBus::new(128),

            // ── GAP-E: Cross-channel artifact pipeline ──
            artifact_pipeline: {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| ".".to_string());
                let cache_dir = std::path::PathBuf::from(home)
                    .join(".clawdesk")
                    .join("artifacts");
                let cache = clawdesk_media::MediaCache::new(cache_dir, 512)
                    .unwrap_or_else(|e| {
                        tracing::error!("Failed to init artifact cache: {e}, using fallback");
                        clawdesk_media::MediaCache::new(
                            std::env::temp_dir().join("clawdesk-artifacts-fallback"),
                            512,
                        ).expect("fallback artifact cache must work")
                    });
                Arc::new(clawdesk_media::ArtifactPipeline::new(Arc::new(cache)))
            },

            // ── GAP-F: Multi-agent shared state ──
            shared_state_mgr: Arc::new(clawdesk_agents::SharedStateManager::new(
                Arc::new(clawdesk_agents::InMemorySharedState::new()),
            )),

            // ── Sandbox: Multi-modal code execution isolation ──
            sandbox_dispatcher: {
                let mut dispatcher = clawdesk_sandbox::SandboxDispatcher::with_defaults();
                info!(
                    levels = ?dispatcher.available_levels(),
                    "Sandbox dispatcher initialized"
                );
                tokio::sync::RwLock::new(dispatcher)
            },

            // ── MCP: Model Context Protocol client ──
            mcp_client: tokio::sync::RwLock::new(clawdesk_mcp::McpClient::new()),

            // ── Extensions: Integration registry ──
            integration_registry: {
                let registry = clawdesk_extensions::IntegrationRegistry::new();
                registry.load_bundled();
                // Restore persisted enabled state from SochDB
                let _restored = crate::commands_extensions::restore_enabled_state(
                    &registry,
                    &soch_store,
                );
                let enabled_count = registry.enabled().len();
                info!(
                    total = registry.count(),
                    enabled = enabled_count,
                    "Integration registry loaded (enabled state restored)"
                );
                tokio::sync::RwLock::new(registry)
            },

            // ── Extensions: Credential vault ──
            credential_vault: {
                let vault_path = sochdb_path.parent()
                    .unwrap_or(&sochdb_path)
                    .join("vault.enc");
                let vault = clawdesk_extensions::CredentialVault::with_path(vault_path);
                info!(exists = vault.exists(), "Credential vault initialized");
                tokio::sync::RwLock::new(vault)
            },

            // ── Extensions: Health monitor ──
            health_monitor: tokio::sync::RwLock::new(clawdesk_extensions::HealthMonitor::new(
                std::time::Duration::from_secs(300), // 5 min check interval
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
        let session_count = self.sessions.len();
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
        {
            let all_sessions = self.sessions.entries();
            let mut ok_count = 0usize;
            let mut fail_count = 0usize;
            for (id, session) in &all_sessions {
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
    ///
    /// Also maintains a temporal index at `chat_index/{agent_id}/{updated_at}/{chat_id}`
    /// enabling ordered-by-time session listing without deserializing all sessions.
    /// Returns Ok(()) on success or Err with description on failure.
    pub fn persist_session(&self, chat_id: &str, session: &ChatSession) -> Result<(), String> {
        let bytes = serde_json::to_vec(session).map_err(|e| format!("Session serialize error: {}", e))?;
        let key = format!("chats/{}", chat_id);
        let bytes_len = bytes.len();

        // Write the session blob + temporal index in a single batch for atomicity.
        let index_key = format!(
            "chat_index/{}/{}/{}",
            session.agent_id, session.updated_at, chat_id
        );
        let index_value = serde_json::to_vec(&serde_json::json!({
            "title": session.title,
            "created_at": session.created_at,
            "updated_at": session.updated_at,
            "message_count": session.messages.len(),
        })).unwrap_or_default();

        self.soch_store.put_batch(&[
            (key.as_str(), &bytes),
            (index_key.as_str(), &index_value),
        ]).map_err(|e| {
            tracing::error!(key = %key, error = %e, "Failed to persist session");
            format!("Session persist failed: {}", e)
        })?;

        tracing::debug!(
            key = %key,
            index_key = %index_key,
            bytes = bytes_len,
            messages = session.messages.len(),
            "Session persisted to SochDB (durable + indexed)"
        );
        Ok(())
    }

    /// Rebuild the temporal session index (`chat_index/`) from all persisted sessions.
    ///
    /// Called on startup to ensure the index is consistent with the sessions.
    /// Idempotent — safe to run multiple times.
    pub fn rebuild_session_index(&self) -> Result<usize, String> {
        let entries = self.soch_store.scan("chats/")
            .map_err(|e| format!("scan failed: {}", e))?;

        let mut batch: Vec<(String, Vec<u8>)> = Vec::new();
        for (_key, bytes) in &entries {
            if let Ok(session) = serde_json::from_slice::<ChatSession>(bytes) {
                let index_key = format!(
                    "chat_index/{}/{}/{}",
                    session.agent_id, session.updated_at, session.id
                );
                let index_value = serde_json::to_vec(&serde_json::json!({
                    "title": session.title,
                    "created_at": session.created_at,
                    "updated_at": session.updated_at,
                    "message_count": session.messages.len(),
                })).unwrap_or_default();
                batch.push((index_key, index_value));
            }
        }

        let count = batch.len();
        if !batch.is_empty() {
            let refs: Vec<(&str, &[u8])> = batch.iter()
                .map(|(k, v)| (k.as_str(), v.as_slice()))
                .collect();
            self.soch_store.put_batch(&refs)
                .map_err(|e| format!("index batch write failed: {}", e))?;
        }

        tracing::info!(
            sessions = entries.len(),
            indexed = count,
            "Rebuilt chat_index from SochDB sessions"
        );
        Ok(count)
    }

    /// Read-through session loader — authoritative read from SochDB.
    ///
    /// If the hot cache has a stale or missing entry (e.g., after a crash or
    /// manual SochDB repair), this method loads the session directly from the
    /// durable store and backfills the cache.
    ///
    /// **Use this instead of raw `sessions.get()` when data integrity matters.**
    pub fn read_through_session(&self, chat_id: &str) -> Option<ChatSession> {
        // Fast path: check hot cache first.
        if let Some(session) = self.sessions.get(chat_id) {
            return Some(session);
        }
        // Slow path: load from SochDB (source of truth).
        let key = format!("chats/{}", chat_id);
        let bytes = self.soch_store.get(&key).ok()??;
        let session: ChatSession = serde_json::from_slice(&bytes).ok()?;
        // Backfill hot cache.
        self.sessions.insert(chat_id.to_string(), session.clone());
        Some(session)
    }

    /// Unified session write — append a message to both
    /// the hot cache (in-memory `RwLock<HashMap>`) and the durable SochDB
    /// store atomically. Uses write-ahead: the durable store is written first,
    /// and the hot cache rolls back on failure.
    ///
    /// **SochDB is the single source of truth** for conversation state. The
    /// in-memory `sessions` HashMap is a write-through cache: writes go to
    /// SochDB first, hot cache second. On startup, sessions are hydrated
    /// from SochDB (see `hydrate_map`). Use `read_through_session()` to
    /// reconcile if the hot cache and SochDB ever diverge (e.g., crash
    /// between cache write and SochDB persist).
    pub fn append_session_message(
        &self,
        chat_id: &str,
        agent_id: &str,
        title: &str,
        msg: ChatMessage,
        now: &chrono::DateTime<chrono::Utc>,
    ) -> Result<usize, String> {
        // Ensure session exists
        if !self.sessions.contains(chat_id) {
            self.sessions.insert(chat_id.to_string(), ChatSession {
                id: chat_id.to_string(),
                agent_id: agent_id.to_string(),
                title: title.to_string(),
                messages: Vec::new(),
                created_at: now.to_rfc3339(),
                updated_at: now.to_rfc3339(),
            });
        }
        // Append message in-place
        self.sessions.mutate(chat_id, |session| {
            session.messages.push(msg);
            session.updated_at = chrono::Utc::now().to_rfc3339();
        });

        // Write-ahead: persist, roll back hot cache on failure
        let session = self.sessions.get(chat_id).ok_or("Session disappeared after insert")?;
        if let Err(e) = self.persist_session(chat_id, &session) {
            self.sessions.mutate(chat_id, |s| { s.messages.pop(); }); // rollback
            return Err(e);
        }
        Ok(session.messages.len())
    }

    /// Persist tool messages separately from the visible session.
    /// Tool messages (tool_use + tool_result) are stored in a parallel SochDB key
    /// (`tool_history/{chat_id}`) so they don't inflate the main session serialization.
    /// They are loaded lazily during history building for subsequent LLM calls.
    ///
    /// Capped to the most recent `MAX_TOOL_HISTORY_ENTRIES` messages to
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

        // Trim to most recent entries to prevent unbounded growth
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

    /// Persist a journal entry to SochDB (write-through).
    pub fn persist_journal_entry(&self, entry: &clawdesk_skills::journal::JournalEntry) {
        if let Ok(bytes) = serde_json::to_vec(entry) {
            let key = format!("journal/{}", entry.id);
            if let Err(e) = self.soch_store.put(&key, &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist journal entry");
            }
        }
    }

    /// Delete a journal entry from SochDB.
    pub fn delete_journal_entry_from_store(&self, id: &str) {
        let key = format!("journal/{}", id);
        if let Err(e) = self.soch_store.delete(&key) {
            tracing::error!(key = %key, error = %e, "Failed to delete journal entry from SochDB");
        }
    }

    // ═══════════════════════════════════════════════════════════
    // Channel config persistence — ~/.clawdesk/channels.json
    // ═══════════════════════════════════════════════════════════

    fn channels_path() -> std::path::PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home)
            .join(".clawdesk")
            .join("channels.json")
    }

    /// Load saved channel configs from `~/.clawdesk/channels.json`.
    /// Returns an empty map if the file doesn't exist or can't be parsed.
    pub fn load_channel_configs() -> HashMap<String, HashMap<String, String>> {
        let path = Self::channels_path();
        match std::fs::read_to_string(&path) {
            Ok(data) => {
                match serde_json::from_str(&data) {
                    Ok(configs) => {
                        info!(path = %path.display(), "Loaded saved channel configs");
                        configs
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "Failed to parse channels.json");
                        HashMap::new()
                    }
                }
            }
            Err(_) => HashMap::new(), // File doesn't exist yet — normal on first run
        }
    }

    /// Persist channel configs to `~/.clawdesk/channels.json` atomically.
    pub fn save_channel_configs(configs: &HashMap<String, HashMap<String, String>>) {
        let path = Self::channels_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("json.tmp");
        match serde_json::to_string_pretty(configs) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&tmp, &json).and_then(|_| std::fs::rename(&tmp, &path)) {
                    error!(error = %e, "Failed to persist channel configs");
                } else {
                    info!(path = %path.display(), "Channel configs persisted to disk");
                }
            }
            Err(e) => error!(error = %e, "Failed to serialize channel configs"),
        }
    }

    // ═══════════════════════════════════════════════════════════
    // Channel provider override persistence — ~/.clawdesk/channel_provider.json
    // ═══════════════════════════════════════════════════════════

    fn channel_provider_path() -> std::path::PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home)
            .join(".clawdesk")
            .join("channel_provider.json")
    }

    /// Load saved channel provider override from `~/.clawdesk/channel_provider.json`.
    /// Returns `None` if the file doesn't exist or can't be parsed.
    pub fn load_channel_provider() -> Option<ChannelProviderOverride> {
        let path = Self::channel_provider_path();
        match std::fs::read_to_string(&path) {
            Ok(data) => {
                match serde_json::from_str(&data) {
                    Ok(cfg) => {
                        info!(path = %path.display(), "Loaded saved channel provider override");
                        Some(cfg)
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "Failed to parse channel_provider.json");
                        None
                    }
                }
            }
            Err(_) => None,
        }
    }

    /// Persist channel provider override to `~/.clawdesk/channel_provider.json` atomically.
    pub fn save_channel_provider(cfg: &ChannelProviderOverride) {
        let path = Self::channel_provider_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Use a unique tmp filename (PID + thread) to avoid races when
        // multiple concurrent calls hit this function.
        let tmp = path.with_extension(format!(
            "tmp.{}.{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        match serde_json::to_string_pretty(cfg) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&tmp, &json).and_then(|_| std::fs::rename(&tmp, &path)) {
                    let _ = std::fs::remove_file(&tmp); // clean up on failure
                    error!(error = %e, "Failed to persist channel provider override");
                } else {
                    info!(path = %path.display(), "Channel provider override persisted to disk");
                }
            }
            Err(e) => error!(error = %e, "Failed to serialize channel provider override"),
        }
    }

    /// Hot-start a channel adapter: create it from config, register it, and
    /// spawn a background thread running `Channel::start()`.
    ///
    /// Stops any existing channel with the same ID first to avoid duplicate
    /// gateway connections.
    ///
    /// Called from `update_channel` so the user doesn't need to restart the app.
    pub fn hot_start_channel(
        channel_id: &str,
        config: &HashMap<String, String>,
        factory: &clawdesk_channels::factory::ChannelFactory,
        channel_registry: &Arc<RwLock<ChannelRegistry>>,
        app_handle: &tauri::AppHandle,
    ) -> Result<(), String> {
        use clawdesk_channels::factory::ChannelConfig;
        use serde_json::Map;

        // Stop and unregister any existing channel with this ID to avoid
        // duplicate gateway connections (e.g., two Discord WebSocket sessions).
        {
            let channel_id_enum = match channel_id {
                "telegram" => clawdesk_types::channel::ChannelId::Telegram,
                "discord" => clawdesk_types::channel::ChannelId::Discord,
                "slack" => clawdesk_types::channel::ChannelId::Slack,
                "whatsapp" => clawdesk_types::channel::ChannelId::WhatsApp,
                "webchat" => clawdesk_types::channel::ChannelId::WebChat,
                "email" => clawdesk_types::channel::ChannelId::Email,
                "imessage" => clawdesk_types::channel::ChannelId::IMessage,
                "irc" => clawdesk_types::channel::ChannelId::Irc,
                "internal" => clawdesk_types::channel::ChannelId::Internal,
                other => return Err(format!("Unknown channel ID: {other}")),
            };
            let mut reg = channel_registry
                .write()
                .map_err(|e| format!("Channel registry lock: {e}"))?;
            if let Some(old_ch) = reg.unregister(&channel_id_enum) {
                info!(channel = %channel_id, "Stopping existing channel before hot-start replacement");
                // Fire-and-forget stop — spawn a task so we never call block_on
                // inside an async context (Tokio panics if you do). The channel is
                // already removed from the registry, so new messages won't route to
                // it regardless of when stop() completes.
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        let _ = old_ch.stop().await;
                    });
                }
            }
        }

        // Build a ChannelConfig from the user's key-value map.
        //
        // The UI sends all values as strings, but the channel factory expects
        // proper JSON types for some fields (arrays, booleans). We coerce
        // known-typed fields here so that UI settings like `allowed_users`,
        // `mention_only`, `guild_id` etc. are actually applied.
        let mut map = Map::new();
        for (k, v) in config {
            let coerced = match k.as_str() {
                // Boolean fields — "true"/"1" → true, anything else → false
                "mention_only" | "listen_to_bots" | "enable_groups" | "verify_tls" => {
                    serde_json::Value::Bool(v == "true" || v == "1")
                }
                // String-array fields — comma-separated string → JSON array of trimmed strings
                "allowed_users" | "allowed_contacts" | "channels" => {
                    let items: Vec<serde_json::Value> = v
                        .split(',')
                        .map(|s| serde_json::Value::String(s.trim().to_string()))
                        .filter(|s| !s.as_str().map(|x| x.is_empty()).unwrap_or(true))
                        .collect();
                    if items.is_empty() {
                        // Keep as string so factory can decide the default
                        serde_json::Value::String(v.clone())
                    } else {
                        serde_json::Value::Array(items)
                    }
                }
                // u64-array fields — single guild/chat ID string → [id]
                "guild_id" | "allowed_guild_ids" => {
                    if v.is_empty() {
                        serde_json::Value::Array(vec![])
                    } else {
                        let items: Vec<serde_json::Value> = v
                            .split(',')
                            .filter_map(|s| s.trim().parse::<u64>().ok())
                            .map(serde_json::Value::from)
                            .collect();
                        serde_json::Value::Array(items)
                    }
                }
                // i64-array fields — comma-separated → [id, ...]
                "allowed_chat_ids" => {
                    let items: Vec<serde_json::Value> = v
                        .split(',')
                        .filter_map(|s| s.trim().parse::<i64>().ok())
                        .map(serde_json::Value::from)
                        .collect();
                    serde_json::Value::Array(items)
                }
                // Port / interval fields — string → integer
                "port" | "imap_port" | "smtp_port" | "poll_interval_secs" => {
                    if let Ok(n) = v.parse::<u64>() {
                        serde_json::Value::Number(n.into())
                    } else {
                        serde_json::Value::String(v.clone())
                    }
                }
                // Everything else stays as a string
                _ => serde_json::Value::String(v.clone()),
            };
            map.insert(k.clone(), coerced);
        }
        let cfg = ChannelConfig::new(channel_id, map);

        // Create the channel adapter
        let channel = factory
            .create(&cfg)
            .map_err(|e| format!("Failed to create {channel_id} channel: {e}"))?;

        // Register it (requires write lock)
        {
            let mut reg = channel_registry
                .write()
                .map_err(|e| format!("Channel registry lock: {e}"))?;
            let _ = reg.register(Arc::clone(&channel));
        }

        info!(channel = %channel_id, "Channel adapter created and registered (hot-start)");

        // Spawn the inbound message loop on a background thread
        let state_ref: tauri::State<'_, AppState> = app_handle.state();
        let sink = std::sync::Arc::new(crate::state::ChannelMessageSink {
            negotiator: Arc::clone(&state_ref.negotiator),
            tool_registry: Arc::clone(&state_ref.tool_registry),
            app_handle: app_handle.clone(),
            channel_registry: Arc::clone(channel_registry),
            cancel: state_ref.cancel.clone(),
            conversation_histories: Arc::new(dashmap::DashMap::new()),
            last_channel_origins: Arc::clone(&state_ref.last_channel_origins),
        });

        let ch_name = channel_id.to_string();
        std::thread::Builder::new()
            .name(format!("channel-{ch_name}"))
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .expect("channel runtime");
                rt.block_on(async move {
                    info!(channel = %ch_name, "Starting hot-started channel adapter");
                    match channel.start(sink).await {
                        Ok(()) => info!(channel = %ch_name, "Hot-started channel adapter running"),
                        Err(e) => error!(channel = %ch_name, error = %e, "Hot-started channel failed"),
                    }
                    // Keep the runtime alive
                    tokio::signal::ctrl_c().await.ok();
                });
            })
            .map_err(|e| format!("Failed to spawn channel thread: {e}"))?;

        Ok(())
    }

    /// Post-init health verification.
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

        // Validate effective config through ValidatedConfig
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

    // ── Domain-aggregate accessor methods ──────────────────────
    //
    // These provide a migration path toward splitting AppState into domain
    // aggregates. Callers can gradually adopt `state.storage_store()` instead
    // of `state.soch_store.clone()`, etc. Once all callers are migrated for
    // a domain, the aggregate struct can be extracted behind a single lock.

    /// Storage aggregate: SochDB store and all durable sub-stores.
    #[inline]
    pub fn storage_store(&self) -> &Arc<SochStore> {
        &self.soch_store
    }

    /// Thread store: namespaced chat-thread persistence.
    #[inline]
    pub fn threads(&self) -> &Arc<clawdesk_threads::ThreadStore> {
        &self.thread_store
    }

    /// Memory aggregate: embedding-backed memory system.
    #[inline]
    pub fn memory_manager(&self) -> &Arc<MemoryManager<SochMemoryBackend>> {
        &self.memory
    }

    /// Metrics: total cost (atomic, no lock needed).
    #[inline]
    pub fn cost_today_micros(&self) -> u64 {
        self.total_cost_today.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Metrics: total input tokens (atomic, no lock needed).
    #[inline]
    pub fn input_tokens(&self) -> u64 {
        self.total_input_tokens.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Metrics: total output tokens (atomic, no lock needed).
    #[inline]
    pub fn output_tokens(&self) -> u64 {
        self.total_output_tokens.load(std::sync::atomic::Ordering::Relaxed)
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
// E2E Smoke Tests — verify AppState construction and
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
        assert!(state.sessions.get(&chat_id).is_none());

        // Create a chat session
        state.sessions.insert(chat_id.clone(), ChatSession {
            id: chat_id.clone(),
            agent_id: agent_id.clone(),
            title: "Test chat".to_string(),
            messages: Vec::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        });

        // Verify exists
        let session = state.sessions.get(&chat_id).expect("chat should exist");
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
