//! Skill lifecycle event bridge — emits typed events on skill state changes.
//!
//! Provides a thin adapter that converts skill lifecycle operations
//! (install, uninstall, update, activate, deactivate, catalog sync)
//! into typed bus events, enabling real-time propagation across all
//! consumers (CLI, gateway, UI).
//!
//! ## Event flow
//!
//! ```text
//! SkillAdmin/Bridge ──→ SkillEventEmitter ──→ EventBus
//!                                               │
//!                                               ├── WebSocket → UI
//!                                               ├── Subscription → Pipeline
//!                                               └── Topic ring buffer
//! ```
//!
//! ## Throughput
//!
//! Publish is O(1) per event. WebSocket fanout to N connected UI clients
//! is O(N) per event. The bus's backpressure manager bounds memory per
//! subscriber to O(B) where B = channel capacity.

use crate::dispatch::EventBus;
use crate::event::{Event, EventKind, Priority};
use serde_json::json;
use std::sync::Arc;

/// Topic name for all skill lifecycle events.
pub const SKILL_TOPIC: &str = "skills";

/// Emits skill lifecycle events onto the event bus.
pub struct SkillEventEmitter {
    bus: Arc<EventBus>,
}

impl SkillEventEmitter {
    /// Create a new emitter connected to an event bus.
    pub fn new(bus: Arc<EventBus>) -> Self {
        Self { bus }
    }

    /// Emit a skill-installed event.
    pub async fn emit_installed(
        &self,
        skill_id: &str,
        version: &str,
        source: &str,
    ) {
        self.bus
            .emit(
                SKILL_TOPIC,
                EventKind::SkillInstalled,
                Priority::Standard,
                json!({
                    "skill_id": skill_id,
                    "version": version,
                    "source": source,
                }),
                "skill_lifecycle",
            )
            .await;
    }

    /// Emit a skill-uninstalled event.
    pub async fn emit_uninstalled(&self, skill_id: &str) {
        self.bus
            .emit(
                SKILL_TOPIC,
                EventKind::SkillUninstalled,
                Priority::Standard,
                json!({ "skill_id": skill_id }),
                "skill_lifecycle",
            )
            .await;
    }

    /// Emit a skill-updated event.
    pub async fn emit_updated(
        &self,
        skill_id: &str,
        from_version: &str,
        to_version: &str,
    ) {
        self.bus
            .emit(
                SKILL_TOPIC,
                EventKind::SkillUpdated,
                Priority::Standard,
                json!({
                    "skill_id": skill_id,
                    "from_version": from_version,
                    "to_version": to_version,
                }),
                "skill_lifecycle",
            )
            .await;
    }

    /// Emit a skill-activated event.
    pub async fn emit_activated(&self, skill_id: &str) {
        self.bus
            .emit(
                SKILL_TOPIC,
                EventKind::SkillActivated,
                Priority::Standard,
                json!({ "skill_id": skill_id }),
                "skill_lifecycle",
            )
            .await;
    }

    /// Emit a skill-deactivated event.
    pub async fn emit_deactivated(&self, skill_id: &str) {
        self.bus
            .emit(
                SKILL_TOPIC,
                EventKind::SkillDeactivated,
                Priority::Standard,
                json!({ "skill_id": skill_id }),
                "skill_lifecycle",
            )
            .await;
    }

    /// Emit a catalog-synced event.
    pub async fn emit_catalog_synced(
        &self,
        entries_added: usize,
        entries_updated: usize,
    ) {
        self.bus
            .emit(
                SKILL_TOPIC,
                EventKind::CatalogSynced,
                Priority::Batch,
                json!({
                    "entries_added": entries_added,
                    "entries_updated": entries_updated,
                }),
                "skill_lifecycle",
            )
            .await;
    }

    /// Emit a skill eligibility change event.
    pub async fn emit_eligibility_changed(
        &self,
        skill_id: &str,
        eligible: bool,
    ) {
        self.bus
            .emit(
                SKILL_TOPIC,
                EventKind::SkillEligibilityChanged,
                Priority::Standard,
                json!({
                    "skill_id": skill_id,
                    "eligible": eligible,
                }),
                "skill_lifecycle",
            )
            .await;
    }

    /// Emit an install progress event.
    pub async fn emit_install_progress(
        &self,
        skill_id: &str,
        step: usize,
        total_steps: usize,
        description: &str,
    ) {
        self.bus
            .emit(
                SKILL_TOPIC,
                EventKind::SkillInstallProgress,
                Priority::Standard,
                json!({
                    "skill_id": skill_id,
                    "step": step,
                    "total_steps": total_steps,
                    "description": description,
                }),
                "skill_lifecycle",
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::EventBus;

    #[tokio::test]
    async fn emitter_publishes_to_bus() {
        let bus = EventBus::new(256);
        let emitter = SkillEventEmitter::new(bus.clone());

        emitter.emit_installed("core/web-search", "1.0.0", "embedded").await;
        emitter.emit_activated("core/web-search").await;
        emitter.emit_catalog_synced(5, 2).await;

        let topics = bus.list_topics().await;
        assert!(topics.contains(&SKILL_TOPIC.to_string()));
    }

    #[tokio::test]
    async fn emitter_event_kinds() {
        let bus = EventBus::new(64);
        let emitter = SkillEventEmitter::new(bus.clone());

        emitter.emit_uninstalled("test/skill").await;
        emitter.emit_updated("test/skill", "1.0", "2.0").await;
        emitter.emit_deactivated("test/skill").await;
        emitter.emit_eligibility_changed("test/skill", true).await;
        emitter.emit_install_progress("test/skill", 1, 3, "Downloading").await;

        // All events should have been published to the skills topic
        let topics = bus.list_topics().await;
        assert!(topics.contains(&SKILL_TOPIC.to_string()));
    }
}
