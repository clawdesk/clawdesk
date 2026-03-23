//! Global Workspace Bus — broadcast mechanism for cognitive subsystem coordination.
//!
//! Implements Baars' Global Workspace Theory (1988): specialist modules compete
//! for attention; the winner's information broadcasts to ALL modules simultaneously.
//!
//! Every cognitive subsystem (metacognition, curiosity, world model, user model,
//! planner, sentinel, consciousness gateway, homeostasis, predictive processor)
//! holds an `Arc<GlobalWorkspace>` and both publishes and subscribes to events.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::trace;

/// Cognitive events broadcast across all subsystems.
///
/// Each variant carries just enough information for subscribers to decide
/// whether to act. Heavy payloads should be stored externally (e.g., in SochDB)
/// with only an ID reference in the event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CognitiveEvent {
    // ── From Metacognition ──────────────────────────────────────────────
    /// Agent is stuck — repeated tools with no progress.
    AgentStuck { reason: String, streak: usize },
    /// Current approach is failing — confidence dropped below threshold.
    ApproachFailing { confidence: f64, suggestion: String },

    // ── From Sentinel ───────────────────────────────────────────────────
    /// Anomaly detected in tool execution patterns.
    AnomalyDetected { signal: String, severity: f64 },

    // ── From World Model ────────────────────────────────────────────────
    /// Environment state changed (file modified, service started, etc.).
    EnvironmentChanged { entity: String, delta: String },
    /// Contradiction in world model (two conflicting facts).
    ContradictionFound { entity: String, severity: f64 },

    // ── From User Model ─────────────────────────────────────────────────
    /// User frustration rising (repetition, short messages, "??").
    UserFrustrationRising { level: String },
    /// User expertise inferred from interaction patterns.
    UserExpertiseInferred { domain: String, level: String },

    // ── From Curiosity ──────────────────────────────────────────────────
    /// Information gap identified that exploration could fill.
    InformationGapFound { gap_id: String, urgency: f64 },
    /// Exploration completed for a previously identified gap.
    ExplorationComplete { gap_id: String, resolved: bool },

    // ── From Planner ────────────────────────────────────────────────────
    /// Plan was rewritten due to execution feedback.
    PlanRewritten { reason: String, nodes_changed: usize },
    /// A subtask in the plan completed.
    SubtaskComplete { task_id: String, success: bool },

    // ── From Consciousness Gateway ──────────────────────────────────────
    /// Tool blocked by consciousness gateway (any level).
    ToolBlocked { tool: String, level: String, reason: String },
    /// Human vetoed a tool execution.
    HumanVeto { tool: String },
    /// Risk level was escalated by sentinel or context analysis.
    RiskEscalated { tool: String, from_level: String, to_level: String },

    // ── From Agent Selector ─────────────────────────────────────────────
    /// Agent handoff — task routed to a different specialist agent.
    AgentHandoff { from: String, to: String, reason: String },

    // ── From Homeostasis ────────────────────────────────────────────────
    /// A resource budget is approaching its ceiling.
    BudgetWarning { resource: String, usage_pct: f64 },
    /// System is overloaded — a vital sign exceeded its setpoint.
    SystemOverloaded { metric: String, value: f64, setpoint: f64 },
    /// Corrective action taken by homeostatic controller.
    HomeostaticAction { action: String },

    // ── From Predictive Processor ───────────────────────────────────────
    /// Prediction about next user request.
    PredictionMade { predicted_intent: String, confidence: f64 },
    /// Prediction error measured (actual vs predicted).
    PredictionError { predicted: String, actual: String, error: f64 },
}

/// The Global Workspace — a typed broadcast channel.
///
/// This is the "thalamus" of the cognitive architecture. All subsystems
/// publish events here, and all subsystems subscribe to receive them.
/// The broadcast channel is bounded (default 1024 events) with lagged
/// receivers auto-catching up to the latest event.
///
/// Performance: `tokio::sync::broadcast` is lock-free for sends and uses
/// a ring buffer internally. O(1) publish, O(S) delivery where S = subscriber count.
pub struct GlobalWorkspace {
    tx: broadcast::Sender<CognitiveEvent>,
}

impl GlobalWorkspace {
    /// Create a new workspace with the given event buffer capacity.
    ///
    /// Capacity should be sized for burst absorption. 1024 is good for
    /// typical agent workloads. If a subscriber falls behind by more
    /// than `capacity` events, it receives a `Lagged` error and skips
    /// to the latest event (graceful degradation, not data corruption).
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish an event to all subscribers.
    ///
    /// Returns `Ok(subscriber_count)` or `Err(event)` if no subscribers.
    /// Non-blocking, never fails due to backpressure (ring buffer overwrites).
    pub fn publish(&self, event: CognitiveEvent) -> usize {
        match self.tx.send(event) {
            Ok(n) => {
                trace!(subscribers = n, "cognitive event published");
                n
            }
            Err(_) => 0, // no subscribers — event dropped silently
        }
    }

    /// Subscribe to all cognitive events.
    ///
    /// Returns a `broadcast::Receiver` that yields events in order.
    /// If the receiver falls behind, it skips to the latest and returns
    /// `RecvError::Lagged(n_skipped)`.
    pub fn subscribe(&self) -> broadcast::Receiver<CognitiveEvent> {
        self.tx.subscribe()
    }

    /// Check current number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for GlobalWorkspace {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_subscribe_roundtrip() {
        let ws = GlobalWorkspace::new(64);
        let mut rx = ws.subscribe();

        ws.publish(CognitiveEvent::AgentStuck {
            reason: "looping".into(),
            streak: 3,
        });

        let event = rx.recv().await.unwrap();
        match event {
            CognitiveEvent::AgentStuck { streak, .. } => assert_eq!(streak, 3),
            _ => panic!("unexpected event variant"),
        }
    }

    #[tokio::test]
    async fn no_subscribers_returns_zero() {
        let ws = GlobalWorkspace::new(64);
        let count = ws.publish(CognitiveEvent::HumanVeto {
            tool: "test".into(),
        });
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn multiple_subscribers_all_receive() {
        let ws = GlobalWorkspace::new(64);
        let mut rx1 = ws.subscribe();
        let mut rx2 = ws.subscribe();

        ws.publish(CognitiveEvent::BudgetWarning {
            resource: "tokens".into(),
            usage_pct: 0.85,
        });

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        match (e1, e2) {
            (
                CognitiveEvent::BudgetWarning { usage_pct: a, .. },
                CognitiveEvent::BudgetWarning { usage_pct: b, .. },
            ) => {
                assert!((a - 0.85).abs() < f64::EPSILON);
                assert!((b - 0.85).abs() < f64::EPSILON);
            }
            _ => panic!("both subscribers should receive BudgetWarning"),
        }
    }
}
