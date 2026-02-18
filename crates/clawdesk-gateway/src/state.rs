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
use clawdesk_agents::ToolRegistry;
use clawdesk_channel::registry::ChannelRegistry;
use clawdesk_channels::factory::ChannelFactory;
use clawdesk_cron::CronManager;
use clawdesk_plugin::PluginHost;
use clawdesk_providers::registry::ProviderRegistry;
use clawdesk_skills::loader::SkillLoader;
use clawdesk_skills::registry::SkillRegistry;
use clawdesk_sochdb::SochStore;
use crate::thread_ownership::ThreadOwnershipManager;
use std::sync::Arc;
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
    ) -> Self {
        Self {
            channels: ArcSwap::from_pointee(channels),
            providers: ArcSwap::from_pointee(providers),
            skills: ArcSwap::from_pointee(skills),
            skill_loader: Arc::new(skill_loader),
            channel_factory: ArcSwap::from_pointee(channel_factory),
            tools: Arc::new(tools),
            store: Arc::new(store),
            plugin_host: Arc::new(plugin_host),
            cron_manager: Arc::new(cron_manager),
            cancel,
            start_time: std::time::Instant::now(),
            thread_ownership: Arc::new(ThreadOwnershipManager::default()),
            response_store: Some(crate::responses_api::new_response_store()),
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
}
