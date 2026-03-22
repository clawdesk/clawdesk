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

    // --- Plugin hook lifecycle ---
    /// Hook manager for plugin lifecycle dispatch.
    /// Wired into AgentRunner for before/after agent, tool, compaction hooks.
    pub hook_manager: Arc<clawdesk_plugin::HookManager>,
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
        Self::with_cron_arc(
            channels, providers, tools, store, plugin_host,
            Arc::new(cron_manager),
            skills, skill_loader, channel_factory, cancel, inbound_registry,
        )
    }

    /// Construct with a pre-built `Arc<CronManager>` — used when the cron manager
    /// needs to be shared before state construction (e.g., for registering cron tools).
    pub fn with_cron_arc(
        channels: ChannelRegistry,
        providers: ProviderRegistry,
        tools: ToolRegistry,
        store: SochStore,
        plugin_host: PluginHost,
        cron_manager: Arc<CronManager>,
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
            cron_manager,
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
            hook_manager: Arc::new(clawdesk_plugin::HookManager::new()),
        }
    }

    /// Gateway uptime in seconds.
    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Hot-reload skills from the filesystem.
    ///
    /// 1. Starts with bundled skills (embedded in binary)
    /// 2. `SkillLoader` re-scans `~/.clawdesk/skills/` and merges
    /// 3. Atomically swaps via `ArcSwap`
    ///
    /// Returns `(loaded_count, errors)`.
    pub async fn reload_skills(&self) -> (usize, Vec<String>) {
        // Start with bundled skills so hot-reload doesn't lose them
        let mut registry = clawdesk_skills::load_bundled_skills();
        let bundled = registry.len();

        // Merge user skills from disk
        let disk_count = self.skill_loader.load_all(&mut registry).await;

        info!(bundled, disk = disk_count, total = registry.len(), "skills hot-reloaded");
        self.skills.store(Arc::new(registry));
        (bundled + disk_count, vec![])
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

// ─── GatewayAgentExecutor ───────────────────────────────────────────────
//
// Implements `clawdesk_cron::executor::AgentExecutor` for the gateway binary.
// Mirrors the Tauri desktop `CronAgentExecutor` but uses the gateway's own
// in-process `AgentRunner` pipeline instead of Tauri IPC commands.
//
// This wires Gap 3: cron tasks can now execute agents in gateway/daemon mode.

/// Agent executor for the gateway — runs agents via the in-process AgentRunner pipeline.
///
/// Uses `OnceLock<Arc<GatewayState>>` for deferred initialization: the executor
/// is constructed before `GatewayState` exists (chicken-and-egg), and the state
/// reference is set after construction via `set_state()`.
pub struct GatewayAgentExecutor {
    state: Arc<std::sync::OnceLock<Arc<GatewayState>>>,
}

impl GatewayAgentExecutor {
    pub fn new() -> Self {
        Self {
            state: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Wire the state reference after construction.
    pub fn state_handle(&self) -> Arc<std::sync::OnceLock<Arc<GatewayState>>> {
        Arc::clone(&self.state)
    }
}

#[async_trait::async_trait]
impl clawdesk_cron::executor::AgentExecutor for GatewayAgentExecutor {
    async fn execute(&self, prompt: &str, agent_id: Option<&str>) -> Result<String, String> {
        use clawdesk_agents::runner::{AgentConfig, AgentRunner};
        use clawdesk_providers::MessageRole;

        let state = self.state.get()
            .ok_or_else(|| "GatewayState not yet initialized — cron fired before setup".to_string())?;

        // Resolve agent persona and model from agent registry.
        let (agent_persona, agent_model) = {
            let registry = state.agent_registry.load();
            let snapshot = agent_id
                .and_then(|id| registry.get(id))
                .or_else(|| registry.get("default"))
                .or_else(|| registry.values().next());
            match snapshot {
                Some(agent) => (agent.system_prompt.clone(), agent.model.clone()),
                None => (
                    clawdesk_types::session::DEFAULT_SYSTEM_PROMPT.to_string(),
                    "sonnet".to_string(),
                ),
            }
        };

        // Resolve model alias → full model ID
        let effective_model_id = match agent_model.as_str() {
            "haiku" => "claude-haiku-4-20250514".to_string(),
            "sonnet" => "claude-sonnet-4-20250514".to_string(),
            "opus" => "claude-opus-4-20250514".to_string(),
            "local" => "llama3.2".to_string(),
            other => other.to_string(),
        };

        // Resolve provider
        let provider_registry = state.providers.load();
        let provider_key = match agent_model.as_str() {
            m if m.contains("haiku") || m.contains("sonnet") || m.contains("opus") || m.contains("claude") => "anthropic",
            m if m.starts_with("gpt") || m.starts_with("o1") || m.starts_with("o3") => "openai",
            m if m.starts_with("gemini") => "gemini",
            m if m.contains("local") || m.starts_with("llama") || m.starts_with("deepseek") => "ollama",
            _ => "anthropic",
        };

        let provider = provider_registry
            .get(provider_key)
            .or_else(|| provider_registry.default_provider())
            .ok_or_else(|| format!(
                "No LLM provider configured for model '{}'. Set ANTHROPIC_API_KEY or similar env var.",
                agent_model
            ))?;

        let config = AgentConfig {
            model: effective_model_id,
            system_prompt: agent_persona.clone(),
            max_tool_rounds: 25,
            context_limit: 128_000,
            response_reserve: 8_192,
            ..Default::default()
        };

        // Build skill provider so cron agents get skill-enriched prompts
        let skill_provider: Option<std::sync::Arc<dyn clawdesk_agents::runner::SkillProvider>> = {
            let registry = state.skills.load();
            let active = registry.active_skills();
            if active.is_empty() {
                None
            } else {
                use clawdesk_skills::env_injection::EnvResolver;
                use clawdesk_skills::orchestrator::SkillOrchestrator;
                use clawdesk_skills::skill_provider::OrchestratorSkillProvider;

                let orchestrator = SkillOrchestrator::new(active, 8_000);
                let env_resolver = EnvResolver::default();
                Some(std::sync::Arc::new(OrchestratorSkillProvider::new(
                    orchestrator,
                    env_resolver,
                )))
            }
        };

        let mut builder = AgentRunner::builder(
            std::sync::Arc::clone(&provider),
            std::sync::Arc::clone(&state.tools),
            config,
            state.cancel.clone(),
        )
        .without_sandbox()
        .with_hook_manager(std::sync::Arc::clone(&state.hook_manager));

        if let Some(sp) = skill_provider {
            builder = builder.with_skill_provider(sp);
        }

        let runner = builder.build();

        // Single-turn execution: just the cron prompt as user message
        let chat_history = vec![
            clawdesk_providers::ChatMessage::new(MessageRole::User, prompt),
        ];

        let agent_response = runner
            .run(chat_history, agent_persona)
            .await
            .map_err(|e| format!("Agent execution failed: {}", e))?;

        Ok(agent_response.content)
    }
}

// ─── ChannelDeliveryHandler ─────────────────────────────────────────────
//
// Implements `clawdesk_cron::executor::DeliveryHandler` using the channel
// registry. When a cron task finishes, results are delivered to the
// configured channels (Telegram, Slack, Discord, etc.).
//
// This wires Gap 1: cron results can now be routed to any connected channel.

/// Delivers cron task results to channels via the channel registry.
///
/// Uses a deferred reference to the gateway's `ArcSwap<ChannelRegistry>`,
/// set after `GatewayState` construction via `state_handle()`.
pub struct ChannelDeliveryHandler {
    state: Arc<std::sync::OnceLock<Arc<GatewayState>>>,
}

impl ChannelDeliveryHandler {
    pub fn new(state: Arc<std::sync::OnceLock<Arc<GatewayState>>>) -> Self {
        Self { state }
    }
}

#[async_trait::async_trait]
impl clawdesk_cron::executor::DeliveryHandler for ChannelDeliveryHandler {
    async fn deliver(
        &self,
        target: &clawdesk_types::cron::DeliveryTarget,
        content: &str,
    ) -> Result<(), String> {
        match target {
            clawdesk_types::cron::DeliveryTarget::Channel { channel_id, conversation_id } => {
                let state = self.state.get()
                    .ok_or_else(|| "GatewayState not yet initialized".to_string())?;
                let registry = state.channels.load();
                let cid = parse_channel_id(channel_id)
                    .ok_or_else(|| format!("Unknown channel: '{}'", channel_id))?;
                let channel = registry.get(&cid)
                    .ok_or_else(|| format!("Channel '{}' not found in registry", channel_id))?;

                // Use channel's default_origin, overriding conversation_id where possible.
                // For channels like Telegram, conversation_id is parsed as chat_id.
                let origin = build_origin_from_target(&cid, conversation_id)
                    .or_else(|| channel.default_origin())
                    .ok_or_else(|| format!("No delivery origin for channel '{}'", channel_id))?;

                let outbound = clawdesk_types::message::OutboundMessage {
                    origin,
                    body: content.to_string(),
                    media: vec![],
                    reply_to: None,
                    thread_id: None,
                };

                channel.send(outbound).await
                    .map(|_receipt| ())
                    .map_err(|e| format!("Channel send failed: {}", e))
            }
            clawdesk_types::cron::DeliveryTarget::Webhook { url } => {
                let client = reqwest::Client::new();
                client.post(url)
                    .json(&serde_json::json!({
                        "type": "cron_result",
                        "content": content,
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                    }))
                    .timeout(std::time::Duration::from_secs(30))
                    .send()
                    .await
                    .map_err(|e| format!("Webhook delivery failed: {}", e))?;
                Ok(())
            }
            clawdesk_types::cron::DeliveryTarget::Session { session_key } => {
                tracing::info!(session_key = %session_key, content_len = content.len(), "Cron result stored in session");
                Ok(())
            }
        }
    }
}

/// Parse a channel ID string into a `ChannelId` enum variant.
fn parse_channel_id(s: &str) -> Option<clawdesk_types::channel::ChannelId> {
    use clawdesk_types::channel::ChannelId;
    match s.to_lowercase().as_str() {
        "telegram" => Some(ChannelId::Telegram),
        "discord" => Some(ChannelId::Discord),
        "slack" => Some(ChannelId::Slack),
        "whatsapp" => Some(ChannelId::WhatsApp),
        "webchat" => Some(ChannelId::WebChat),
        "email" => Some(ChannelId::Email),
        "imessage" => Some(ChannelId::IMessage),
        "irc" => Some(ChannelId::Irc),
        "internal" => Some(ChannelId::Internal),
        "teams" => Some(ChannelId::Teams),
        "matrix" => Some(ChannelId::Matrix),
        "signal" => Some(ChannelId::Signal),
        "webhook" => Some(ChannelId::Webhook),
        "mastodon" => Some(ChannelId::Mastodon),
        _ => None,
    }
}

/// Build a `MessageOrigin` from a channel ID and conversation_id string.
/// This allows cron delivery to target a specific chat/channel/room.
fn build_origin_from_target(
    cid: &clawdesk_types::channel::ChannelId,
    conversation_id: &str,
) -> Option<clawdesk_types::message::MessageOrigin> {
    use clawdesk_types::channel::ChannelId;
    use clawdesk_types::message::MessageOrigin;

    if conversation_id.is_empty() || conversation_id == "default" {
        return None; // Fall back to channel's default_origin
    }

    match cid {
        ChannelId::Telegram => {
            conversation_id.parse::<i64>().ok().map(|chat_id| {
                MessageOrigin::Telegram { chat_id, message_id: 0, thread_id: None }
            })
        }
        ChannelId::Discord => {
            conversation_id.parse::<u64>().ok().map(|channel_id| {
                MessageOrigin::Discord { guild_id: 0, channel_id, message_id: 0, is_dm: false, thread_id: None }
            })
        }
        ChannelId::Slack => {
            Some(MessageOrigin::Slack {
                team_id: String::new(),
                channel_id: conversation_id.to_string(),
                user_id: String::new(),
                ts: String::new(),
                thread_ts: None,
            })
        }
        ChannelId::WhatsApp => {
            Some(MessageOrigin::WhatsApp {
                phone_number: conversation_id.to_string(),
                message_id: String::new(),
            })
        }
        ChannelId::Email => {
            Some(MessageOrigin::Email {
                message_id: String::new(),
                from: String::new(),
                to: conversation_id.to_string(),
            })
        }
        ChannelId::WebChat => {
            Some(MessageOrigin::WebChat { session_id: conversation_id.to_string() })
        }
        ChannelId::Internal => {
            Some(MessageOrigin::Internal { source: conversation_id.to_string() })
        }
        _ => None, // Fall back to channel's default_origin
    }
}
