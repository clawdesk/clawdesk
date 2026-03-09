//! Gateway shared state — lock-free read path via `ArcSwap`.
//!
//! Channel, provider, and skill registries use `ArcSwap` instead of `RwLock`:
//! reads are a single atomic `Acquire` load (no contention, no poisoning),
//! writes atomically swap the entire `Arc`. This eliminates reader starvation
//! and priority inversion on the hot path (every inbound message reads registries).
//!
//! ## Hot-reload pattern (ArcSwap COW)
//!
//! ```text
//! 1. current = state.skills.load_full()     // Arc<SkillRegistry>
//! 2. new = (*current).clone()                // SkillRegistry (deep copy)
//! 3. new.activate(&id)                       // mutate the clone
//! 4. state.skills.store(Arc::new(new))       // atomic swap
//! ```
//!
//! Readers never block. Writers pay O(n) clone cost, but writes
//! (skill activate/deactivate, channel reload) are rare operations.

use arc_swap::ArcSwap;
use clawdesk_acp::server::A2AState;
use clawdesk_acp::thread_agent::ThreadAgentRegistry;
use clawdesk_agents::ToolRegistry;
use clawdesk_bus::config_events::ConfigEventBus;
use clawdesk_channel::inbound_adapter::InboundAdapterRegistry;
use clawdesk_channel::registry::ChannelRegistry;
use clawdesk_channels::factory::ChannelFactory;
use clawdesk_cron::CronManager;
use clawdesk_plugin::PluginHost;
use clawdesk_providers::registry::ProviderRegistry;
use clawdesk_skills::loader::SkillLoader;
use clawdesk_skills::registry::SkillRegistry;
use clawdesk_sochdb::SochStore;
use crate::config_rollback::RollbackBuffer;
use crate::native_watcher::NativeWatcher;
use crate::reload_policy::ReloadPolicy;
use crate::thread_ownership::ThreadOwnershipManager;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Shared state for the gateway, passed to all routes via Axum state.
///
/// `ArcSwap<T>` replaces `Arc<RwLock<T>>` for registries:
/// - Read: `state.channels.load()` → `Arc<ChannelRegistry>` (wait-free, ~2ns)
/// - Write: `state.channels.store(Arc::new(new_registry))` (atomic swap)
///
/// ## Plug-and-play components
///
/// - `skills` — Hot-swappable skill registry (loaded from `~/.clawdesk/skills/`)
/// - `skill_loader` — Filesystem scanner for hot-reload
/// - `channel_factory` — Config-driven channel construction (extensible)
pub struct GatewayState {
    // --- Registries (hot-swappable via ArcSwap) ---
    pub channels: ArcSwap<ChannelRegistry>,
    pub providers: ArcSwap<ProviderRegistry>,
    pub skills: ArcSwap<SkillRegistry>,

    // --- Plug-and-play infrastructure ---
    pub skill_loader: Arc<SkillLoader>,
    pub channel_factory: ArcSwap<ChannelFactory>,

    // --- Core services ---
    pub tools: Arc<ToolRegistry>,
    pub store: Arc<SochStore>,
    pub plugin_host: Arc<PluginHost>,
    pub cron_manager: Arc<CronManager>,
    pub cancel: CancellationToken,
    pub start_time: std::time::Instant,
    pub thread_ownership: Arc<ThreadOwnershipManager>,

    // --- Responses API persistence ---
    pub response_store: Option<crate::responses_api::ResponseStore>,

    // --- Inbound adapter registry for multi-channel message ingestion ---
    /// Holds registered inbound adapters. Channels that implement `InboundAdapter`
    /// are registered here; the gateway calls `start_all()` after construction
    /// and spawns the `InboundBridge` to publish messages to the event bus.
    pub inbound_registry: Arc<Mutex<InboundAdapterRegistry>>,

    // --- A2A protocol state ---
    /// Shared A2A protocol state (agent card, directory, tasks, policy).
    pub a2a_state: ArcSwap<A2AState>,

    // --- Thread-as-Agent registry ---
    /// Every thread is an A2A-capable agent; this registry holds per-thread
    /// AgentCards keyed by `agent:{id}:{thread_hex}`.
    pub thread_agents: Arc<ThreadAgentRegistry>,

    // --- Webhook ingestion ---
    /// In-memory store for webhook configurations (GAP-A).
    pub webhook_store: crate::webhook::WebhookStore,

    // --- Reactive event bus (GAP-D) ---
    /// Central event bus for reactive triggers and pipeline dispatch.
    pub event_bus: Arc<clawdesk_bus::dispatch::EventBus>,

    // --- Agent hot-reload registry ---
    /// Hot-swappable agent definition registry.
    /// Loaded from `~/.clawdesk/agents/<id>/agent.toml` files.
    /// Updated atomically via `ArcSwap` on filesystem changes or SIGHUP.
    pub agent_registry: ArcSwap<crate::agent_loader::AgentConfigMap>,

    /// Agent definition loader (filesystem scanner).
    pub agent_loader: Arc<crate::agent_loader::AgentLoader>,

    // --- Per-agent token budget enforcement ---
    /// Sliding-window token budget counters, keyed by agent ID.
    pub token_budgets: Arc<clawdesk_agents::TokenBudgetManager>,

    // --- Outbound webhook delivery queue (GAP-A+) ---
    /// Persistent at-least-once webhook delivery queue backed by SochDB.
    pub webhook_queue: Arc<crate::webhook_queue::WebhookDeliveryQueue>,

    // --- Observability ---
    /// Central metrics collector with counters, gauges, histograms, and SSE broadcast.
    pub metrics: Arc<crate::observability::MetricsCollector>,

    // --- Config reload subsystem ---
    /// Broadcast channel for configuration lifecycle events (file changed, committed, rolled back).
    pub config_event_bus: Arc<ConfigEventBus>,
    /// Content-addressed filesystem watcher with adaptive debounce.
    pub native_watcher: Arc<NativeWatcher>,
    /// Ring buffer of previous config generations for rollback.
    pub rollback_buffer: Arc<RollbackBuffer>,
    /// Environment-specific reload policy (dev/staging/prod presets).
    pub reload_policy: ReloadPolicy,

    // --- Cross-channel artifact pipeline (GAP-E) ---
    /// Content-addressed artifact store backed by MediaCache.
    pub artifact_pipeline: Arc<clawdesk_media::ArtifactPipeline>,

    // --- Browser automation (optional) ---
    /// Browser session manager for CDP-based automation.
    #[cfg(feature = "browser")]
    pub browser_manager: Arc<clawdesk_browser::BrowserManager>,
}

impl GatewayState {
    pub fn new(
        channels: ChannelRegistry,
        providers: ProviderRegistry,
        tools: ToolRegistry,
        store: SochStore,
        plugin_host: PluginHost,
        cron_manager: CronManager,
        skills: SkillRegistry,
        skill_loader: SkillLoader,
        channel_factory: ChannelFactory,
        cancel: CancellationToken,
        inbound_registry: InboundAdapterRegistry,
    ) -> Self {
        let store = Arc::new(store);
        let webhook_queue = Arc::new(
            crate::webhook_queue::WebhookDeliveryQueue::new(Arc::clone(&store))
        );
        Self {
            channels: ArcSwap::from_pointee(channels),
            providers: ArcSwap::from_pointee(providers),
            skills: ArcSwap::from_pointee(skills),
            skill_loader: Arc::new(skill_loader),
            channel_factory: ArcSwap::from_pointee(channel_factory),
            tools: Arc::new(tools),
            store,
            plugin_host: Arc::new(plugin_host),
            cron_manager: Arc::new(cron_manager),
            cancel,
            start_time: std::time::Instant::now(),
            thread_ownership: Arc::new(ThreadOwnershipManager::default()),
            response_store: Some(crate::responses_api::new_response_store()),
            inbound_registry: Arc::new(Mutex::new(inbound_registry)),
            a2a_state: ArcSwap::from_pointee(A2AState::new(
                clawdesk_acp::agent_card::AgentCard::new(
                    "clawdesk",
                    "ClawDesk",
                    "http://localhost:18789",
                ),
            )),
            thread_agents: Arc::new(ThreadAgentRegistry::new("http://localhost:18789")),
            webhook_store: crate::webhook::WebhookStore::new(),
            event_bus: clawdesk_bus::dispatch::EventBus::new(128),
            agent_registry: ArcSwap::from_pointee(std::collections::HashMap::new()),
            agent_loader: {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| ".".to_string());
                let agents_dir = std::path::PathBuf::from(home)
                    .join(".clawdesk")
                    .join("agents");
                Arc::new(crate::agent_loader::AgentLoader::new(agents_dir))
            },
            token_budgets: clawdesk_agents::TokenBudgetManager::unlimited(),
            webhook_queue,
            metrics: crate::observability::MetricsCollector::new(),
            config_event_bus: Arc::new(ConfigEventBus::new(256)),
            native_watcher: Arc::new(NativeWatcher::new(
                crate::native_watcher::NativeWatcherConfig::default(),
            )),
            rollback_buffer: Arc::new(RollbackBuffer::new(
                ReloadPolicy::default().rollback.buffer_capacity,
            )),
            reload_policy: ReloadPolicy::load_from_file(
                &ReloadPolicy::default_path().unwrap_or_default(),
            ).unwrap_or_default(),
            artifact_pipeline: {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| ".".to_string());
                let cache_dir = std::path::PathBuf::from(home)
                    .join(".clawdesk")
                    .join("artifacts");
                let cache = clawdesk_media::MediaCache::new(cache_dir, 512)
                    .unwrap_or_else(|_| {
                        clawdesk_media::MediaCache::new(
                            std::env::temp_dir().join("clawdesk-gw-artifacts"),
                            512,
                        ).expect("artifact cache")
                    });
                Arc::new(clawdesk_media::ArtifactPipeline::new(Arc::new(cache)))
            },
            #[cfg(feature = "browser")]
            browser_manager: clawdesk_browser::BrowserManager::new(clawdesk_browser::manager::BrowserConfig::default()),
        }
    }

    /// Gateway uptime in seconds.
    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Hot-reload skills from the filesystem.
    ///
    /// 1. `SkillLoader` re-scans `~/.clawdesk/skills/`
    /// 2. Builds a fresh `SkillRegistry`
    /// 3. Atomically swaps via `ArcSwap`
    ///
    /// Returns `(loaded_count, errors)`.
    pub async fn reload_skills(&self) -> (usize, Vec<String>) {
        let result = self.skill_loader.load_fresh(true).await;
        let loaded = result.loaded;
        let errors = result.errors;
        info!(loaded, errors = errors.len(), "skills hot-reloaded");
        self.skills.store(Arc::new(result.registry));
        (loaded, errors)
    }

    /// Hot-reload agent definitions from `~/.clawdesk/agents/`.
    ///
    /// 1. `AgentLoader` re-scans the agents directory.
    /// 2. Builds a fresh `AgentConfigMap`.
    /// 3. Compares config hashes to detect actual changes.
    /// 4. Atomically swaps via `ArcSwap`.
    ///
    /// Returns `(loaded_count, changed_count, errors)`.
    pub fn reload_agents(&self) -> (usize, usize, Vec<String>) {
        let result = self.agent_loader.load_fresh();
        let loaded = result.agents.len();
        let errors: Vec<String> = result.errors.iter()
            .map(|(id, e)| format!("{id}: {e}"))
            .collect();

        // Detect how many actually changed by comparing config hashes.
        let current = self.agent_registry.load();
        let mut changed = 0usize;
        for (id, snap) in &result.agents {
            match current.get(id) {
                Some(old) if old.config_hash == snap.config_hash => {}
                _ => changed += 1,
            }
        }
        // Also count removed agents as changes.
        for id in current.keys() {
            if !result.agents.contains_key(id) {
                changed += 1;
            }
        }

        if changed > 0 {
            info!(loaded, changed, errors = errors.len(), "agents hot-reloaded");
        } else {
            info!(loaded, "agents checked — no changes detected");
        }

        self.agent_registry.store(Arc::new(result.agents));
        (loaded, changed, errors)
    }
}
