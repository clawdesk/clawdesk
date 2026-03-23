//! Cognitive Event Loop — the "white matter" connecting all brain regions.
//!
//! Runs as a background tokio task alongside the main agent eval loop.
//! Receives ALL events from the global workspace bus and routes them to
//! the appropriate cognitive subsystems.
//!
//! ## Architecture
//!
//! ```text
//! GlobalWorkspace (broadcast)
//!   ↓
//! cognitive_event_loop (this module)
//!   ├── → Sentinel (anomaly boost from metacognition)
//!   ├── → Homeostasis (budget/overload reactions)
//!   ├── → Classifier (L4→L0 risk feedback from vetos)
//!   ├── → Curiosity (gap registration from tool failures)
//!   ├── → WorldModel (env state tracking from tool results)
//!   └── → UserModel (frustration + expertise signals)
//! ```

use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};

use clawdesk_conscious::workspace::CognitiveEvent;
use clawdesk_conscious::sentinel::Sentinel;
use clawdesk_conscious::homeostasis::HomeostaticController;
use clawdesk_conscious::awareness::AwarenessClassifier;

/// Run the cognitive event loop as a background task.
///
/// This is the central event dispatcher that connects all cognitive
/// subsystems through the global workspace bus. It processes events
/// and routes them to the appropriate handlers.
///
/// The loop runs until the broadcast channel closes (when the gateway
/// or runner is dropped).
pub async fn cognitive_event_loop(
    mut rx: broadcast::Receiver<CognitiveEvent>,
    gateway: Arc<clawdesk_conscious::ConsciousGateway>,
) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                dispatch_event(&event, &gateway).await;
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(skipped = n, "cognitive event loop lagged — skipped events");
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!("cognitive event loop: workspace closed, shutting down");
                break;
            }
        }
    }
}

/// Route a cognitive event to all relevant subsystems.
async fn dispatch_event(
    event: &CognitiveEvent,
    gateway: &clawdesk_conscious::ConsciousGateway,
) {
    // ── Sentinel: reacts to metacognition, user frustration ──
    gateway.sentinel().write().await.handle_cognitive_event(event);

    // ── Classifier: L4→L0 risk adjustment from human vetos ──
    if let CognitiveEvent::HumanVeto { ref tool } = event {
        gateway.classifier().write().await.adjust_base_risk(tool, 0.05);
        debug!(tool, "L4→L0: increased risk for vetoed tool");
    }

    // ── Log significant events for observability ──
    match event {
        CognitiveEvent::AgentStuck { reason, streak } => {
            warn!(reason, streak, "cognitive: agent stuck");
        }
        CognitiveEvent::BudgetWarning { resource, usage_pct } => {
            warn!(resource, usage_pct, "cognitive: budget warning");
        }
        CognitiveEvent::SystemOverloaded { metric, value, setpoint } => {
            warn!(metric, value, setpoint, "cognitive: system overloaded");
        }
        CognitiveEvent::ToolBlocked { tool, level, reason } => {
            info!(tool, level, reason, "cognitive: tool blocked");
        }
        CognitiveEvent::RiskEscalated { tool, from_level, to_level } => {
            info!(tool, from_level, to_level, "cognitive: risk escalated");
        }
        _ => {}
    }
}
