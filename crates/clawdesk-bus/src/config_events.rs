//! Reactive Configuration Event Bus.
//!
//! Provides a typed event hierarchy for configuration lifecycle events,
//! broadcast via the existing EventBus infrastructure. All config changes
//! flow through this subsystem so that dependent components can react to
//! reloads, rollbacks, and validation results without polling.
//!
//! ## Event flow
//!
//! ```text
//! FileChanged ──► DiffComputed ──► Validated ──► Committed / Rejected
//!                                                  │
//!                                                  ├──► Promoted
//!                                                  └──► RolledBack
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Clock — monotonic causal ordering
// ---------------------------------------------------------------------------

static EVENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Returns the next monotonically increasing sequence number.
fn next_seq() -> u64 {
    EVENT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// Top-level configuration event envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigEvent {
    /// Monotonically increasing sequence number for causal ordering.
    pub seq: u64,
    /// Wall-clock timestamp.
    pub timestamp: SystemTime,
    /// Generation of the config snapshot this event pertains to.
    pub generation: u64,
    /// Specific event kind.
    pub kind: ConfigEventKind,
}

/// Discriminated union of config lifecycle events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConfigEventKind {
    /// A watched file changed on disk.
    FileChanged {
        path: String,
        content_hash: String,
    },

    /// The diff engine computed a delta between generations.
    DiffComputed {
        /// Number of field-level changes detected.
        field_changes: usize,
        /// Overall impact classification.
        max_impact: String,
        /// Per-registry change counts.
        registry_summary: HashMap<String, usize>,
    },

    /// Validation pipeline completed.
    ValidationCompleted {
        passed: bool,
        error_count: usize,
        warning_count: usize,
    },

    /// A new config snapshot was committed (atomically visible).
    Committed {
        previous_generation: u64,
    },

    /// Validation failed; the pending change was rejected.
    Rejected {
        error_count: usize,
        /// First error message for quick logging.
        first_error: String,
    },

    /// Canary health check promoted the new generation.
    Promoted,

    /// Canary health check triggered a rollback.
    RolledBack {
        from_generation: u64,
        to_generation: u64,
        reason: String,
    },

    /// Provider credential rotation event.
    CredentialRotated {
        provider_id: String,
        /// Whether the rotation was automatic or manual.
        automatic: bool,
    },

    /// Reload policy override applied (e.g., dev vs. prod preset).
    PolicyApplied {
        policy_name: String,
    },
}

impl ConfigEvent {
    /// Create a new event with auto-assigned sequence and timestamp.
    pub fn new(generation: u64, kind: ConfigEventKind) -> Self {
        Self {
            seq: next_seq(),
            timestamp: SystemTime::now(),
            generation,
            kind,
        }
    }

    /// Whether this is a terminal event (committed, rejected, promoted, rolled-back).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.kind,
            ConfigEventKind::Committed { .. }
                | ConfigEventKind::Rejected { .. }
                | ConfigEventKind::Promoted
                | ConfigEventKind::RolledBack { .. }
        )
    }
}

// ---------------------------------------------------------------------------
// Event bus
// ---------------------------------------------------------------------------

/// Broadcast channel for configuration events.
///
/// Uses `tokio::sync::broadcast` so that multiple subscribers can
/// independently consume the event stream. Lagging subscribers that
/// fall behind the ring buffer will receive `RecvError::Lagged(n)`.
#[derive(Clone)]
pub struct ConfigEventBus {
    sender: broadcast::Sender<ConfigEvent>,
    /// Number of events emitted (including dropped for lag).
    emitted: Arc<AtomicU64>,
}

impl ConfigEventBus {
    /// Create a new config event bus with the given capacity.
    ///
    /// `capacity` controls the broadcast ring buffer size; subscribers that
    /// lag behind this many events will be notified with a `Lagged` error.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            emitted: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Emit a config event. Returns the number of active receivers.
    pub fn emit(&self, event: ConfigEvent) -> usize {
        self.emitted.fetch_add(1, Ordering::Relaxed);
        debug!(
            seq = event.seq,
            gen = event.generation,
            kind = ?std::mem::discriminant(&event.kind),
            "config event emitted"
        );
        // If no receivers, send returns Err but that's fine — we don't block.
        self.sender.send(event).unwrap_or(0)
    }

    /// Convenience: emit a `FileChanged` event.
    pub fn emit_file_changed(&self, generation: u64, path: String, content_hash: String) {
        self.emit(ConfigEvent::new(
            generation,
            ConfigEventKind::FileChanged { path, content_hash },
        ));
    }

    /// Convenience: emit a `Committed` event.
    pub fn emit_committed(&self, generation: u64, previous_generation: u64) {
        self.emit(ConfigEvent::new(
            generation,
            ConfigEventKind::Committed {
                previous_generation,
            },
        ));
    }

    /// Convenience: emit a `RolledBack` event.
    pub fn emit_rolled_back(
        &self,
        generation: u64,
        from_generation: u64,
        to_generation: u64,
        reason: String,
    ) {
        self.emit(ConfigEvent::new(
            generation,
            ConfigEventKind::RolledBack {
                from_generation,
                to_generation,
                reason,
            },
        ));
    }

    /// Subscribe to config events.
    pub fn subscribe(&self) -> broadcast::Receiver<ConfigEvent> {
        self.sender.subscribe()
    }

    /// Number of events emitted since creation.
    pub fn emitted_count(&self) -> u64 {
        self.emitted.load(Ordering::Relaxed)
    }

    /// Number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl std::fmt::Debug for ConfigEventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigEventBus")
            .field("emitted", &self.emitted_count())
            .field("subscribers", &self.subscriber_count())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emit_and_receive() {
        let bus = ConfigEventBus::new(16);
        let mut rx = bus.subscribe();

        bus.emit_file_changed(1, "/etc/config.toml".into(), "abc123".into());

        let event = rx.recv().await.unwrap();
        assert_eq!(event.generation, 1);
        assert!(matches!(event.kind, ConfigEventKind::FileChanged { .. }));
    }

    #[tokio::test]
    async fn multiple_subscribers() {
        let bus = ConfigEventBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.emit_committed(2, 1);

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert_eq!(e1.seq, e2.seq);
    }

    #[test]
    fn causal_ordering() {
        let bus = ConfigEventBus::new(16);
        let e1 = ConfigEvent::new(1, ConfigEventKind::Promoted);
        let e2 = ConfigEvent::new(1, ConfigEventKind::Promoted);
        assert!(e2.seq > e1.seq, "sequences must be monotonically increasing");
        let _ = bus; // just to suppress unused warning
    }

    #[test]
    fn terminal_events() {
        let e = ConfigEvent::new(1, ConfigEventKind::Committed { previous_generation: 0 });
        assert!(e.is_terminal());

        let e2 = ConfigEvent::new(1, ConfigEventKind::FileChanged {
            path: "x".into(),
            content_hash: "y".into(),
        });
        assert!(!e2.is_terminal());
    }

    #[test]
    fn emitted_count_tracks() {
        let bus = ConfigEventBus::new(16);
        assert_eq!(bus.emitted_count(), 0);
        bus.emit_committed(1, 0);
        bus.emit_committed(2, 1);
        assert_eq!(bus.emitted_count(), 2);
    }

    #[tokio::test]
    async fn rolled_back_event() {
        let bus = ConfigEventBus::new(16);
        let mut rx = bus.subscribe();
        bus.emit_rolled_back(3, 3, 1, "canary failed".into());
        let ev = rx.recv().await.unwrap();
        assert!(matches!(
            ev.kind,
            ConfigEventKind::RolledBack { from_generation: 3, to_generation: 1, .. }
        ));
    }
}
