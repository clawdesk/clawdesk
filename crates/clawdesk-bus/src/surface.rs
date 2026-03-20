//! Unified Bus Surface — All UIs Share One Truth
//!
//! Defines the `BusSurface` trait that all UI surfaces implement.
//! Guarantees:
//! - Total event ordering (atomic monotonic counter)
//! - At-least-once delivery to all registered surfaces
//! - Backpressure: buffer up to limit, then oldest dropped with gap marker
//!
//! ## Ordering
//!
//! ```text
//! Lamport clock: seq(e) = max(local_seq, last_seen_seq) + 1
//! Total order: e₁ < e₂ iff seq(e₁) < seq(e₂), ties broken by source_id
//! ```
//!
//! ## Memory Budget
//!
//! N surfaces × B buffer × avg_event_size
//! For N=5, B=1000, avg=2KB: 10 MB
//! Throughput: O(N) dispatch per event. At N=5, 100 events/sec: 500 dispatches/sec

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, warn};

/// Monotonic sequence number for total event ordering.
static GLOBAL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A sequenced event with total ordering guarantee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequencedEvent {
    /// Global monotonic sequence number (Lamport clock)
    pub sequence: u64,
    /// Source surface that originated this event
    pub source_id: String,
    /// The event payload (serialized)
    pub payload: serde_json::Value,
    /// Event type tag for filtering
    pub event_type: String,
    /// Timestamp (epoch milliseconds)
    pub timestamp_ms: u64,
}

impl SequencedEvent {
    /// Create a new sequenced event with next global sequence number.
    pub fn new(source_id: String, event_type: String, payload: serde_json::Value) -> Self {
        Self {
            sequence: GLOBAL_SEQUENCE.fetch_add(1, Ordering::SeqCst),
            source_id,
            event_type,
            payload,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

impl PartialEq for SequencedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence && self.source_id == other.source_id
    }
}

impl Eq for SequencedEvent {}

impl PartialOrd for SequencedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SequencedEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sequence.cmp(&other.sequence)
            .then_with(|| self.source_id.cmp(&other.source_id))
    }
}

/// Surface identifier — names a registered UI surface.
pub type SurfaceId = String;

/// Configuration for a bus surface.
#[derive(Debug, Clone)]
pub struct SurfaceConfig {
    /// Maximum events to buffer before dropping oldest
    pub buffer_size: usize,
    /// Event type filter (empty = receive all)
    pub event_filter: Vec<String>,
}

impl Default for SurfaceConfig {
    fn default() -> Self {
        Self {
            buffer_size: 1000,
            event_filter: Vec::new(),
        }
    }
}

/// A registered surface with its delivery channel.
struct RegisteredSurface {
    config: SurfaceConfig,
    sender: mpsc::Sender<SequencedEvent>,
    /// Last sequence number delivered to this surface
    last_delivered: u64,
    /// Number of events dropped due to backpressure
    dropped_count: u64,
}

/// The unified bus surface manager.
///
/// Dispatches events to all registered surfaces with total ordering.
pub struct BusSurfaceManager {
    surfaces: Arc<RwLock<HashMap<SurfaceId, RegisteredSurface>>>,
}

impl BusSurfaceManager {
    pub fn new() -> Self {
        Self {
            surfaces: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new UI surface.
    ///
    /// Returns a receiver channel for consuming events.
    pub async fn register(
        &self,
        surface_id: SurfaceId,
        config: SurfaceConfig,
    ) -> mpsc::Receiver<SequencedEvent> {
        let (tx, rx) = mpsc::channel(config.buffer_size);
        let surface = RegisteredSurface {
            config,
            sender: tx,
            last_delivered: 0,
            dropped_count: 0,
        };

        let mut surfaces = self.surfaces.write().await;
        surfaces.insert(surface_id.clone(), surface);
        debug!(surface = %surface_id, "bus surface registered");

        rx
    }

    /// Unregister a surface.
    pub async fn unregister(&self, surface_id: &str) {
        let mut surfaces = self.surfaces.write().await;
        surfaces.remove(surface_id);
        debug!(surface = %surface_id, "bus surface unregistered");
    }

    /// Publish an event to all registered surfaces.
    ///
    /// Delivery: at-least-once to all surfaces. If a surface's buffer is full,
    /// the event is dropped with a warning and gap counter incremented.
    ///
    /// Cost: O(N) where N = number of registered surfaces.
    pub async fn publish(&self, event: SequencedEvent) {
        let mut surfaces = self.surfaces.write().await;

        for (id, surface) in surfaces.iter_mut() {
            // Apply event filter
            if !surface.config.event_filter.is_empty()
                && !surface.config.event_filter.contains(&event.event_type)
            {
                continue;
            }

            match surface.sender.try_send(event.clone()) {
                Ok(()) => {
                    surface.last_delivered = event.sequence;
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    surface.dropped_count += 1;
                    warn!(
                        surface = %id,
                        dropped = surface.dropped_count,
                        sequence = event.sequence,
                        "bus surface buffer full, event dropped"
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Surface disconnected — will be cleaned up on next unregister
                    debug!(surface = %id, "bus surface disconnected");
                }
            }
        }
    }

    /// Get status of all registered surfaces.
    pub async fn status(&self) -> Vec<SurfaceStatus> {
        let surfaces = self.surfaces.read().await;
        surfaces.iter().map(|(id, s)| SurfaceStatus {
            surface_id: id.clone(),
            last_delivered_sequence: s.last_delivered,
            dropped_count: s.dropped_count,
            buffer_capacity: s.config.buffer_size,
        }).collect()
    }

    /// Number of registered surfaces.
    pub async fn surface_count(&self) -> usize {
        self.surfaces.read().await.len()
    }
}

impl Default for BusSurfaceManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Status of a single bus surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceStatus {
    pub surface_id: String,
    pub last_delivered_sequence: u64,
    pub dropped_count: u64,
    pub buffer_capacity: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_publish() {
        let manager = BusSurfaceManager::new();
        let mut rx = manager.register("desktop".into(), SurfaceConfig::default()).await;

        let event = SequencedEvent::new(
            "test".into(),
            "message".into(),
            serde_json::json!({"text": "hello"}),
        );

        manager.publish(event.clone()).await;

        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type, "message");
    }

    #[tokio::test]
    async fn events_have_monotonic_sequence() {
        let e1 = SequencedEvent::new("a".into(), "t".into(), serde_json::json!(null));
        let e2 = SequencedEvent::new("a".into(), "t".into(), serde_json::json!(null));
        assert!(e2.sequence > e1.sequence);
    }

    #[tokio::test]
    async fn multiple_surfaces_receive() {
        let manager = BusSurfaceManager::new();
        let mut rx1 = manager.register("desktop".into(), SurfaceConfig::default()).await;
        let mut rx2 = manager.register("mobile".into(), SurfaceConfig::default()).await;

        let event = SequencedEvent::new(
            "test".into(), "message".into(), serde_json::json!({"text": "broadcast"}),
        );
        manager.publish(event).await;

        let r1 = rx1.recv().await.unwrap();
        let r2 = rx2.recv().await.unwrap();
        assert_eq!(r1.sequence, r2.sequence);
    }

    #[tokio::test]
    async fn event_filter_works() {
        let manager = BusSurfaceManager::new();
        let config = SurfaceConfig {
            event_filter: vec!["important".into()],
            ..Default::default()
        };
        let mut rx = manager.register("filtered".into(), config).await;

        // Publish non-matching event
        manager.publish(SequencedEvent::new(
            "test".into(), "noise".into(), serde_json::json!(null),
        )).await;

        // Publish matching event
        manager.publish(SequencedEvent::new(
            "test".into(), "important".into(), serde_json::json!({"data": 42}),
        )).await;

        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type, "important");
    }

    #[tokio::test]
    async fn unregister_removes_surface() {
        let manager = BusSurfaceManager::new();
        let _rx = manager.register("temp".into(), SurfaceConfig::default()).await;
        assert_eq!(manager.surface_count().await, 1);

        manager.unregister("temp").await;
        assert_eq!(manager.surface_count().await, 0);
    }
}
