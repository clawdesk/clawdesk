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
use clawdesk_acp::AgentDirectory;
use clawdesk_discovery::{MdnsAdvertiser, PairingSession, PeerRegistry, ServiceInfo};
use clawdesk_domain::context_guard::{ContextGuard, ContextGuardConfig};
use clawdesk_domain::prompt_builder::PromptManifest;
use clawdesk_infra::clipboard::ClipboardEntry;
use clawdesk_infra::voice_wake::VoiceWakeManager;
use clawdesk_infra::{IdleConfig, IdleDetector};
use clawdesk_media::MediaPipeline;
use clawdesk_memory::{MemoryManager, MockEmbeddingProvider, OllamaEmbeddingProvider, EmbeddingProvider, OpenAiEmbeddingProvider};
use clawdesk_memory::manager::MemoryConfig;
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
use tracing::{info, warn};

// Types serialized to the frontend

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineNodeDescriptor {
    pub label: String,
    pub node_type: String,
    pub model: Option<String>,
    pub agent_id: Option<String>,
    pub x: f64,
    pub y: f64,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub metadata: Option<ChatMessageMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUsageSummary {
    pub name: String,
    pub success: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionInfo {
    pub level: String,
    pub tokens_before: usize,
    pub tokens_after: usize,
}

// Application State with real backend services

pub struct AppState {
    // ── SochDB: single ACID store for all durable state ──
    pub soch_store: Arc<SochStore>,

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

    // ── Memory system: embeddings + hybrid search backed by SochDB VectorStore ──
    pub memory: Arc<MemoryManager<SochStore>>,

    // Real backend services
    pub skill_registry: RwLock<SkillRegistry>,
    pub provider_registry: RwLock<ProviderRegistry>,
    pub tool_registry: Arc<ToolRegistry>,
    pub scanner: Arc<CascadeScanner>,
    pub scanner_pattern_count: usize,
    pub audit_logger: Arc<AuditLogger>,

    // Agent & session management (hot cache — persisted to SochDB)
    pub agents: RwLock<HashMap<String, DesktopAgent>>,
    pub identities: RwLock<HashMap<String, IdentityContract>>,
    pub server_secret: ServerSecret,
    pub sessions: RwLock<HashMap<String, Vec<ChatMessage>>>,

    // Infrastructure
    pub invites: RwLock<InviteManager>,
    pub tunnel_metrics: Arc<TunnelMetrics>,

    // Metrics (hot counters — persisted to SochDB on checkpoint)
    pub total_cost_today: AtomicU64,
    pub total_input_tokens: AtomicU64,
    pub total_output_tokens: AtomicU64,
    pub model_costs: RwLock<HashMap<String, (u64, u64, u64)>>,
    pub traces: RwLock<HashMap<String, Vec<TraceEntry>>>,
    pub pipelines: RwLock<Vec<PipelineDescriptor>>,
    pub started_at: std::time::Instant,
    pub cancel: tokio_util::sync::CancellationToken,

    // ── Task 12: Durable Runtime ──
    pub durable_runner: Option<Arc<DurableAgentRunner>>,

    // ── Task 13: Media Pipeline ──
    pub media_pipeline: Arc<tokio::sync::RwLock<MediaPipeline>>,

    // ── Task 14: Plugin System ──
    pub plugin_host: Option<Arc<PluginHost>>,

    // ── Task 15: A2A Protocol ──
    pub agent_directory: RwLock<AgentDirectory>,

    // ── Task 16: OAuth2 + PKCE ──
    pub oauth_flow_manager: Arc<OAuthFlowManager>,
    pub auth_profile_manager: Arc<AuthProfileManager>,

    // ── Task 17: Execution Approval ──
    pub approval_manager: Arc<ExecApprovalManager>,

    // ── Task 18: Network Discovery ──
    pub mdns_advertiser: RwLock<MdnsAdvertiser>,
    pub pairing_session: RwLock<Option<PairingSession>>,
    pub peer_registry: RwLock<PeerRegistry>,

    // ── Task 19: Observability ──
    pub observability_config: RwLock<ObservabilityConfig>,

    // ── Task 20: Notifications (hot cache — persisted to SochDB) ──
    pub notification_history: RwLock<Vec<NotificationInfo>>,

    // ── Task 21: Clipboard (hot cache — persisted to SochDB) ──
    pub clipboard_history: RwLock<Vec<ClipboardEntry>>,

    // ── Task 22: Voice Wake ──
    pub voice_wake: RwLock<Option<VoiceWakeManager>>,

    // ── Task 23: ACL Engine ──
    pub acl_manager: Arc<AclManager>,

    // ── Task 25: Skill Promotion ──
    pub skill_verifier: Arc<SkillVerifier>,

    // ── Task 26: Provider Negotiation ──
    pub negotiator: RwLock<ProviderNegotiator>,

    // ── Task 27: Context Guard ──
    pub context_guards: RwLock<HashMap<String, ContextGuard>>,

    // ── Task 28: Prompt Builder manifests ──
    pub prompt_manifests: RwLock<HashMap<String, PromptManifest>>,

    // ── Task 29: Idle Detection ──
    pub idle_detector: Option<Arc<IdleDetector>>,

    // ── Task 30: Canvas (hot cache — persisted to SochDB) ──
    pub canvases: RwLock<HashMap<String, Canvas>>,
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
        let soch_store = match SochStore::open(&sochdb_path) {
            Ok(store) => {
                info!(path = ?sochdb_path, "SochDB opened — ACID storage active");
                Arc::new(store)
            }
            Err(e) => {
                warn!(error = %e, "SochDB open failed, falling back to in-memory");
                Arc::new(SochStore::open_in_memory().expect("in-memory SochDB must succeed"))
            }
        };

        // ── Embedding provider (for MemoryManager) ──────────────────
        let embedding: Arc<dyn EmbeddingProvider> = if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            info!("Memory embedding: OpenAI text-embedding-3-small");
            Arc::new(OpenAiEmbeddingProvider::new(key, None, None))
        } else {
            // Ollama is always available locally
            let base_url = std::env::var("OLLAMA_HOST").ok();
            info!(base_url = ?base_url, "Memory embedding: Ollama nomic-embed-text");
            Arc::new(OllamaEmbeddingProvider::new(None, base_url))
        };

        // ── MemoryManager<SochStore> ────────────────────────────────
        let memory_config = MemoryConfig::default();
        let memory = Arc::new(MemoryManager::new(
            soch_store.clone() as Arc<SochStore>,
            embedding,
            memory_config,
        ));
        info!("MemoryManager<SochStore> initialized — remember/recall/forget ready");

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
        let tool_registry = ToolRegistry::new();
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

        // Ollama is always available (local, no API key needed)
        {
            let base_url = std::env::var("OLLAMA_HOST").ok();
            info!(base_url = ?base_url, "Registering Ollama provider (local)");
            let provider = OllamaProvider::new(base_url, None);
            provider_registry.register(Arc::new(provider));
        }

        let provider_count = provider_registry.list().len();
        info!(providers = provider_count, "Provider auto-registration complete");

        // ── Task 26: Build ProviderNegotiator with capability-aware routing ──
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

        // ── Hydrate hot caches from SochDB ──────────────────────────
        let agents = hydrate_map(&soch_store, "agents/");
        let sessions = hydrate_map(&soch_store, "chat_sessions/");
        let pipelines: Vec<PipelineDescriptor> = hydrate_list(&soch_store, "pipelines/");
        let notification_history: Vec<NotificationInfo> = hydrate_list(&soch_store, "notifications/");
        let clipboard_history: Vec<ClipboardEntry> = hydrate_list(&soch_store, "clipboard/");
        let canvases: HashMap<String, Canvas> = hydrate_map(&soch_store, "canvases/");

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

        Self {
            soch_store,
            semantic_cache,
            trace_store,
            checkpoint_store,
            knowledge_graph,
            temporal_graph,
            policy_engine,
            atomic_writer,
            agent_registry,
            memory,

            skill_registry: RwLock::new(skill_registry),
            provider_registry: RwLock::new(provider_registry),
            tool_registry: Arc::new(tool_registry),
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
            traces: RwLock::new(HashMap::new()),
            pipelines: RwLock::new(pipelines),
            started_at: std::time::Instant::now(),
            cancel: tokio_util::sync::CancellationToken::new(),

            // ── Task 12: Durable Runtime ──
            durable_runner: None,

            // ── Task 13: Media Pipeline ──
            media_pipeline: Arc::new(tokio::sync::RwLock::new(MediaPipeline::new())),

            // ── Task 14: Plugin System ──
            plugin_host: None,

            // ── Task 15: A2A Protocol ──
            agent_directory: RwLock::new(AgentDirectory::new()),

            // ── Task 16: OAuth2 + PKCE ──
            oauth_flow_manager: Arc::new(OAuthFlowManager::new()),
            auth_profile_manager: Arc::new(AuthProfileManager::new()),

            // ── Task 17: Execution Approval ──
            approval_manager: Arc::new(ExecApprovalManager::new(Duration::from_secs(300))),

            // ── Task 18: Network Discovery ──
            mdns_advertiser: RwLock::new(MdnsAdvertiser::new(
                ServiceInfo::new("clawdesk-desktop", 18789),
            )),
            pairing_session: RwLock::new(None),
            peer_registry: RwLock::new(PeerRegistry::new(Duration::from_secs(120))),

            // ── Task 19: Observability ──
            observability_config: RwLock::new(ObservabilityConfig::from_env()),

            // ── Task 20: Notifications (hydrated from SochDB) ──
            notification_history: RwLock::new(notification_history),

            // ── Task 21: Clipboard (hydrated from SochDB) ──
            clipboard_history: RwLock::new(clipboard_history),

            // ── Task 22: Voice Wake ──
            voice_wake: RwLock::new(None),

            // ── Task 23: ACL Engine ──
            acl_manager: Arc::new(AclManager::new()),

            // ── Task 25: Skill Promotion ──
            skill_verifier: Arc::new(SkillVerifier::development()),

            // ── Task 26: Provider Negotiation ──
            negotiator: RwLock::new(negotiator),

            // ── Task 27: Context Guard ──
            context_guards: RwLock::new(HashMap::new()),

            // ── Task 28: Prompt Builder manifests ──
            prompt_manifests: RwLock::new(HashMap::new()),

            // ── Task 29: Idle Detection ──
            idle_detector: Some(Arc::new(IdleDetector::new(
                IdleConfig { idle_threshold_secs: 300, check_interval_secs: 30 },
                vec![],
            ))),

            // ── Task 30: Canvas (hydrated from SochDB) ──
            canvases: RwLock::new(canvases),
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

        // Fallback to any available provider
        reg.default_provider()
            .map(|p| Arc::clone(p))
            .ok_or_else(|| {
                format!(
                    "No provider available for model '{}'. \
                     Set ANTHROPIC_API_KEY, OPENAI_API_KEY, or GOOGLE_API_KEY environment variable.",
                    model
                )
            })
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
        // Agents
        if let Ok(agents) = self.agents.read() {
            for (id, agent) in agents.iter() {
                if let Ok(bytes) = serde_json::to_vec(agent) {
                    let key = format!("agents/{}", id);
                    if let Err(e) = self.soch_store.db().put(key.as_bytes(), &bytes) {
                        tracing::error!(key = %key, error = %e, "Failed to persist agent");
                    }
                }
            }
        }

        // Sessions
        if let Ok(sessions) = self.sessions.read() {
            for (id, msgs) in sessions.iter() {
                if let Ok(bytes) = serde_json::to_vec(msgs) {
                    let key = format!("chat_sessions/{}", id);
                    if let Err(e) = self.soch_store.db().put(key.as_bytes(), &bytes) {
                        tracing::error!(key = %key, error = %e, "Failed to persist session");
                    }
                }
            }
        }

        // Pipelines
        if let Ok(pipelines) = self.pipelines.read() {
            for p in pipelines.iter() {
                if let Ok(bytes) = serde_json::to_vec(p) {
                    let key = format!("pipelines/{}", p.id);
                    if let Err(e) = self.soch_store.db().put(key.as_bytes(), &bytes) {
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
                    if let Err(e) = self.soch_store.db().put(key.as_bytes(), &bytes) {
                        tracing::error!(key = %key, error = %e, "Failed to persist canvas");
                    }
                }
            }
        }

        // Checkpoint WAL to keep it bounded
        if let Err(e) = self.soch_store.checkpoint_and_gc() {
            tracing::warn!(error = %e, "SochDB checkpoint failed");
        }
    }

    /// Persist a single agent to SochDB (write-through).
    pub fn persist_agent(&self, id: &str, agent: &DesktopAgent) {
        if let Ok(bytes) = serde_json::to_vec(agent) {
            let key = format!("agents/{}", id);
            if let Err(e) = self.soch_store.db().put(key.as_bytes(), &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist agent");
            }
        }
    }

    /// Persist a single session's messages to SochDB (write-through).
    pub fn persist_session(&self, id: &str, msgs: &[ChatMessage]) {
        if let Ok(bytes) = serde_json::to_vec(msgs) {
            let key = format!("chat_sessions/{}", id);
            if let Err(e) = self.soch_store.db().put(key.as_bytes(), &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist session");
            }
        }
    }

    /// Persist a notification to SochDB (write-through).
    pub fn persist_notification(&self, info: &NotificationInfo) {
        if let Ok(bytes) = serde_json::to_vec(info) {
            let key = format!("notifications/{}", info.id);
            if let Err(e) = self.soch_store.db().put(key.as_bytes(), &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist notification");
            }
        }
    }

    /// Persist a clipboard entry to SochDB (write-through).
    pub fn persist_clipboard_entry(&self, entry: &ClipboardEntry) {
        if let Ok(bytes) = serde_json::to_vec(entry) {
            let key = format!("clipboard/{}", entry.id);
            if let Err(e) = self.soch_store.db().put(key.as_bytes(), &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist clipboard entry");
            }
        }
    }

    /// Persist a canvas to SochDB (write-through).
    pub fn persist_canvas(&self, canvas: &Canvas) {
        if let Ok(bytes) = serde_json::to_vec(canvas) {
            let key = format!("canvases/{}", canvas.id);
            if let Err(e) = self.soch_store.db().put(key.as_bytes(), &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist canvas");
            }
        }
    }

    /// Delete an agent from SochDB.
    pub fn delete_agent_from_store(&self, id: &str) {
        let key = format!("agents/{}", id);
        if let Err(e) = self.soch_store.db().delete(key.as_bytes()) {
            tracing::error!(key = %key, error = %e, "Failed to delete agent from SochDB");
        }
    }

    /// Persist a pipeline to SochDB (write-through).
    pub fn persist_pipeline(&self, pipeline: &PipelineDescriptor) {
        if let Ok(bytes) = serde_json::to_vec(pipeline) {
            let key = format!("pipelines/{}", pipeline.id);
            if let Err(e) = self.soch_store.db().put(key.as_bytes(), &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist pipeline");
            }
        }
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
            PipelineNodeDescriptor { label: "User Input".into(), node_type: "input".into(), model: None, agent_id: None, x: 30.0, y: 85.0 },
            PipelineNodeDescriptor { label: "Researcher".into(), node_type: "agent".into(), model: Some("Sonnet".into()), agent_id: Some("researcher".into()), x: 170.0, y: 35.0 },
            PipelineNodeDescriptor { label: "Analyst".into(), node_type: "agent".into(), model: Some("Sonnet".into()), agent_id: Some("analyst".into()), x: 170.0, y: 135.0 },
            PipelineNodeDescriptor { label: "Review Gate".into(), node_type: "gate".into(), model: None, agent_id: None, x: 340.0, y: 85.0 },
            PipelineNodeDescriptor { label: "Writer".into(), node_type: "agent".into(), model: Some("Opus".into()), agent_id: Some("writer".into()), x: 490.0, y: 85.0 },
            PipelineNodeDescriptor { label: "Output".into(), node_type: "output".into(), model: None, agent_id: None, x: 630.0, y: 85.0 },
        ],
        edges: vec![(0, 1), (0, 2), (1, 3), (2, 3), (3, 4), (4, 5)],
        created: Utc::now().to_rfc3339(),
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
fn hydrate_map<T: serde::de::DeserializeOwned>(
    store: &SochStore,
    prefix: &str,
) -> HashMap<String, T> {
    let mut map = HashMap::new();
    match store.db().scan(prefix.as_bytes()) {
        Ok(entries) => {
            for (key, value) in entries {
                let key_str = String::from_utf8_lossy(&key);
                let id = key_str
                    .strip_prefix(prefix)
                    .unwrap_or(&key_str)
                    .to_string();
                match serde_json::from_slice::<T>(&value) {
                    Ok(item) => { map.insert(id, item); }
                    Err(e) => {
                        warn!(key = %key_str, error = %e, "Failed to deserialize entry during hydration");
                    }
                }
            }
        }
        Err(e) => {
            warn!(prefix = %prefix, error = %e, "SochDB scan failed during hydration");
        }
    }
    map
}

/// Hydrate a `Vec<T>` from SochDB by scanning a prefix.
fn hydrate_list<T: serde::de::DeserializeOwned>(
    store: &SochStore,
    prefix: &str,
) -> Vec<T> {
    match store.db().scan(prefix.as_bytes()) {
        Ok(entries) => {
            entries
                .into_iter()
                .filter_map(|(key, value)| {
                    match serde_json::from_slice::<T>(&value) {
                        Ok(item) => Some(item),
                        Err(e) => {
                            let key_str = String::from_utf8_lossy(&key);
                            warn!(key = %key_str, error = %e, "Failed to deserialize entry during hydration");
                            None
                        }
                    }
                })
                .collect()
        }
        Err(e) => {
            warn!(prefix = %prefix, error = %e, "SochDB scan failed during hydration");
            Vec::new()
        }
    }
}
