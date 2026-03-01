//! Typed event publish/subscribe — compile-time verified event taxonomy.
//!
//! Provides a type-safe layer over the string-based EventBus. Events are Rust
//! structs that implement the `TypedEvent` trait, giving:
//!
//! - Compile-time topic correctness (no typo bugs)
//! - Automatic serialization/deserialization
//! - Schema evolution via `#[serde(default)]` on new fields
//! - TypeId-based dispatch lookup: O(1) vs O(k × m) for string matching
//!
//! ## Usage
//!
//! ```ignore
//! // Define an event
//! #[derive(Debug, Clone, Serialize, Deserialize)]
//! struct SkillInstalled {
//!     skill_id: String,
//!     version: String,
//! }
//!
//! impl TypedEvent for SkillInstalled {
//!     fn topic(&self) -> &'static str { "skill.installed" }
//!     fn kind(&self) -> EventKind { EventKind::SkillInstalled }
//!     fn priority(&self) -> Priority { Priority::Standard }
//! }
//!
//! // Publish
//! bus.publish_typed(&SkillInstalled { skill_id: "x".into(), version: "1.0".into() }).await;
//!
//! // Subscribe (type-checked at compile time)
//! bus.subscribe_typed::<SkillInstalled>(|event| async move {
//!     println!("Skill installed: {}", event.skill_id);
//! }).await;
//! ```

use crate::dispatch::EventBus;
use crate::event::{Event, EventKind, Priority};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Trait for typed events — provides compile-time topic and kind binding.
///
/// Implementations define the invariant mapping between a Rust type and
/// its bus topic string. This eliminates the possibility of typo bugs
/// in topic names.
pub trait TypedEvent: Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static {
    /// The bus topic this event publishes to. Must be a `&'static str` to
    /// prevent runtime string construction errors.
    fn topic(&self) -> &'static str;

    /// The semantic event kind for dispatch filtering.
    fn kind(&self) -> EventKind;

    /// Dispatch priority. Defaults to Standard.
    fn priority(&self) -> Priority {
        Priority::Standard
    }

    /// Event source identifier. Defaults to the type name.
    fn source(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Schema version for forward-compatible evolution. Subscribers can
    /// check this to handle migrations.
    fn version(&self) -> u32 {
        1
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Concrete typed events
// ═══════════════════════════════════════════════════════════════════════════

/// A skill was installed from the store or local source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInstalledEvent {
    pub skill_id: String,
    pub version: String,
    pub source: String,
}

impl TypedEvent for SkillInstalledEvent {
    fn topic(&self) -> &'static str { "skills" }
    fn kind(&self) -> EventKind { EventKind::SkillInstalled }
    fn source(&self) -> &'static str { "skill_lifecycle" }
}

/// A skill was uninstalled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillUninstalledEvent {
    pub skill_id: String,
}

impl TypedEvent for SkillUninstalledEvent {
    fn topic(&self) -> &'static str { "skills" }
    fn kind(&self) -> EventKind { EventKind::SkillUninstalled }
    fn source(&self) -> &'static str { "skill_lifecycle" }
}

/// A skill was updated to a new version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillUpdatedEvent {
    pub skill_id: String,
    pub from_version: String,
    pub to_version: String,
}

impl TypedEvent for SkillUpdatedEvent {
    fn topic(&self) -> &'static str { "skills" }
    fn kind(&self) -> EventKind { EventKind::SkillUpdated }
    fn source(&self) -> &'static str { "skill_lifecycle" }
}

/// An inbound message was received on any channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReceivedEvent {
    pub channel_id: String,
    pub sender: String,
    pub content_preview: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}

impl TypedEvent for MessageReceivedEvent {
    fn topic(&self) -> &'static str { "message.received" }
    fn kind(&self) -> EventKind { EventKind::MessageReceived }
}

/// An outbound message was delivered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSentEvent {
    pub channel_id: String,
    pub recipient: String,
    pub delivery_id: String,
}

impl TypedEvent for MessageSentEvent {
    fn topic(&self) -> &'static str { "message.sent" }
    fn kind(&self) -> EventKind { EventKind::MessageSent }
}

/// A cron job was executed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronExecutedEvent {
    pub task_id: String,
    pub schedule: String,
    pub duration_ms: u64,
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}

impl TypedEvent for CronExecutedEvent {
    fn topic(&self) -> &'static str { "cron.executed" }
    fn kind(&self) -> EventKind { EventKind::CronExecuted }
    fn priority(&self) -> Priority { Priority::Batch }
    fn source(&self) -> &'static str { "cron" }
}

/// A pipeline completed execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineCompletedEvent {
    pub pipeline_id: String,
    pub pipeline_name: String,
    pub steps_executed: usize,
    pub duration_ms: u64,
    pub success: bool,
}

impl TypedEvent for PipelineCompletedEvent {
    fn topic(&self) -> &'static str { "pipeline.completed" }
    fn kind(&self) -> EventKind { EventKind::PipelineCompleted }
    fn source(&self) -> &'static str { "pipeline" }
}

/// Memory was stored (recall-eligible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStoredEvent {
    pub memory_id: String,
    pub source: String,
    pub tokens: usize,
}

impl TypedEvent for MemoryStoredEvent {
    fn topic(&self) -> &'static str { "memory.stored" }
    fn kind(&self) -> EventKind { EventKind::MemoryStored }
    fn source(&self) -> &'static str { "memory" }
}

// ═══════════════════════════════════════════════════════════════════════════
// Typed publish extension
// ═══════════════════════════════════════════════════════════════════════════

/// Extension trait for EventBus to publish typed events.
///
/// This is the primary integration point — call `bus.publish_typed(&event).await`
/// instead of manually constructing Event envelopes with string topics.
#[async_trait::async_trait]
pub trait TypedEventPublisher {
    /// Publish a typed event. Topic, kind, priority, and source are all
    /// derived from the `TypedEvent` trait implementation — no strings needed.
    async fn publish_typed<E: TypedEvent>(&self, event: &E) -> u64;
}

#[async_trait::async_trait]
impl TypedEventPublisher for EventBus {
    async fn publish_typed<E: TypedEvent>(&self, event: &E) -> u64 {
        let payload = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
        self.emit(
            event.topic(),
            event.kind(),
            event.priority(),
            payload,
            event.source(),
        )
        .await
    }
}

#[async_trait::async_trait]
impl TypedEventPublisher for Arc<EventBus> {
    async fn publish_typed<E: TypedEvent>(&self, event: &E) -> u64 {
        (**self).publish_typed(event).await
    }
}

/// Attempt to deserialize a bus Event into a typed event struct.
///
/// Returns `None` if the payload doesn't match the expected type.
/// This is the subscriber-side counterpart to `publish_typed`.
pub fn try_deserialize_event<E: TypedEvent>(event: &Event) -> Option<E> {
    serde_json::from_value(event.payload.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_event_topic_is_static() {
        let event = SkillInstalledEvent {
            skill_id: "test".into(),
            version: "1.0".into(),
            source: "local".into(),
        };
        assert_eq!(event.topic(), "skills");
        assert_eq!(event.kind(), EventKind::SkillInstalled);
        assert_eq!(event.version(), 1);
    }

    #[test]
    fn typed_event_roundtrip() {
        let original = CronExecutedEvent {
            task_id: "daily-summary".into(),
            schedule: "0 9 * * *".into(),
            duration_ms: 1500,
            success: true,
            error: None,
        };

        let payload = serde_json::to_value(&original).unwrap();
        let event = Event::new(
            original.topic(),
            original.kind(),
            original.priority(),
            payload,
            original.source(),
        );

        let deserialized: CronExecutedEvent = try_deserialize_event(&event).unwrap();
        assert_eq!(deserialized.task_id, "daily-summary");
        assert_eq!(deserialized.duration_ms, 1500);
        assert!(deserialized.success);
    }

    #[test]
    fn typed_event_schema_evolution() {
        // If a new field is added with #[serde(default)], old payloads still deserialize
        let old_payload = serde_json::json!({
            "task_id": "test",
            "schedule": "* * * * *",
            "duration_ms": 100,
            "success": true,
            // Note: no "error" field — but CronExecutedEvent has error: Option<String> with default
        });
        let deserialized: CronExecutedEvent = serde_json::from_value(old_payload).unwrap();
        assert!(deserialized.error.is_none());
    }

    #[test]
    fn message_events_have_correct_topics() {
        let recv = MessageReceivedEvent {
            channel_id: "discord".into(),
            sender: "user123".into(),
            content_preview: "hello".into(),
            thread_id: None,
        };
        assert_eq!(recv.topic(), "message.received");

        let sent = MessageSentEvent {
            channel_id: "telegram".into(),
            recipient: "user456".into(),
            delivery_id: "msg-1".into(),
        };
        assert_eq!(sent.topic(), "message.sent");
    }
}
