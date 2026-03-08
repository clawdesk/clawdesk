//! # Pipeline Bridge — Forwards pipeline events to the Tauri frontend.
//!
//! Subscribes to the `PipelineExecutor`'s broadcast channel and maps
//! `PipelineEvent` variants to Tauri `app.emit()` calls, enabling the
//! frontend to render multi-step pipeline progress in real-time.
//!
//! Also bridges `AgentLoopEvent`s from the new event stream (Rec 1)
//! to the Tauri frontend for sub-agent observability.
//!
//! ## Architecture
//!
//! ```text
//! PipelineExecutor → broadcast::Sender<PipelineEvent>
//!                      ↓
//! PipelineBridge::spawn() → tokio::spawn(forward loop)
//!                      ↓
//! app.emit("pipeline-event", TauriPipelineEvent)
//! ```

use clawdesk_agents::PipelineEvent;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Tauri-side pipeline event types
// ═══════════════════════════════════════════════════════════════════════════

/// Pipeline event forwarded to the Tauri frontend.
///
/// Mapped from `PipelineEvent` at the bridge layer. The frontend receives
/// these as `pipeline-event` Tauri events and can render progress indicators,
/// gate approval prompts, and error states.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TauriPipelineEvent {
    /// A pipeline step has started executing.
    StepStarted {
        pipeline_id: String,
        step_index: usize,
        step_name: String,
        step_type: String,
    },
    /// A pipeline step has completed.
    StepCompleted {
        pipeline_id: String,
        step_index: usize,
        step_name: String,
        success: bool,
        duration_ms: u64,
        output_preview: String,
    },
    /// A pipeline step has failed.
    StepFailed {
        pipeline_id: String,
        step_index: usize,
        step_name: String,
        error: String,
    },
    /// Pipeline is waiting at a gate for human approval.
    GateWaiting {
        pipeline_id: String,
        step_index: usize,
        prompt: String,
    },
    /// A gate was approved or denied.
    GateResolved {
        pipeline_id: String,
        step_index: usize,
        approved: bool,
    },
    /// Parallel branches started.
    ParallelStarted {
        pipeline_id: String,
        step_index: usize,
        branch_count: usize,
    },
    /// A parallel branch completed.
    BranchCompleted {
        pipeline_id: String,
        step_index: usize,
        branch_index: usize,
        success: bool,
    },
    /// Pipeline execution completed.
    PipelineCompleted {
        pipeline_id: String,
        success: bool,
        total_steps: usize,
        duration_ms: u64,
    },
    /// Pipeline execution failed.
    PipelineFailed {
        pipeline_id: String,
        error: String,
    },
    /// Progress update (for long-running steps).
    Progress {
        pipeline_id: String,
        step_index: usize,
        message: String,
    },
}

/// The Tauri event name used for pipeline events.
pub const PIPELINE_EVENT_NAME: &str = "pipeline-event";

/// The Tauri event name used for agent lifecycle events.
pub const AGENT_LIFECYCLE_EVENT_NAME: &str = "agent-lifecycle-event";

// ═══════════════════════════════════════════════════════════════════════════
// Pipeline bridge
// ═══════════════════════════════════════════════════════════════════════════

/// Spawns a background task that forwards pipeline events to the Tauri frontend.
///
/// Returns a `JoinHandle` for the forwarding task. The task runs until
/// the cancel token is triggered or the broadcast sender is dropped.
pub fn spawn_pipeline_bridge(
    app: AppHandle,
    mut rx: broadcast::Receiver<PipelineEvent>,
    pipeline_id: String,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!(pipeline_id, "pipeline bridge cancelled");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(event) => {
                            if let Some(tauri_event) = map_pipeline_event(&pipeline_id, &event) {
                                if let Err(e) = app.emit(PIPELINE_EVENT_NAME, &tauri_event) {
                                    warn!(error = %e, "failed to emit pipeline event to frontend");
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            debug!(skipped = n, "pipeline bridge lagged — skipping events");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            debug!(pipeline_id, "pipeline event channel closed");
                            break;
                        }
                    }
                }
            }
        }
    })
}

/// Spawns a bridge for `AgentLoopEvent`s from the new event stream system (Rec 1).
///
/// This enables the frontend to observe sub-agent execution lifecycle
/// in real-time, including tool execution progress and turn transitions.
pub fn spawn_agent_event_bridge(
    app: AppHandle,
    mut rx: broadcast::Receiver<clawdesk_agents::AgentLoopEvent>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("agent event bridge cancelled");
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Ok(event) => {
                            // Serialize the event directly — AgentLoopEvent is already Serialize
                            if let Err(e) = app.emit(AGENT_LIFECYCLE_EVENT_NAME, &event) {
                                warn!(error = %e, "failed to emit agent lifecycle event");
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            debug!(skipped = n, "agent event bridge lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Event mapping
// ═══════════════════════════════════════════════════════════════════════════

/// Map a `PipelineEvent` to its Tauri-frontend representation.
///
/// Returns `None` for events that don't need frontend forwarding.
fn map_pipeline_event(
    pipeline_id: &str,
    event: &PipelineEvent,
) -> Option<TauriPipelineEvent> {
    match event {
        PipelineEvent::Started {
            pipeline_name,
            step_count,
        } => Some(TauriPipelineEvent::StepStarted {
            pipeline_id: pipeline_id.to_string(),
            step_index: 0,
            step_name: pipeline_name.clone(),
            step_type: "pipeline".to_string(),
        }),
        PipelineEvent::StepStarted {
            step_index,
            step_type,
        } => Some(TauriPipelineEvent::StepStarted {
            pipeline_id: pipeline_id.to_string(),
            step_index: *step_index,
            step_name: format!("step_{}", step_index),
            step_type: step_type.clone(),
        }),
        PipelineEvent::StepCompleted {
            step_index,
            success,
            duration_ms,
        } => Some(TauriPipelineEvent::StepCompleted {
            pipeline_id: pipeline_id.to_string(),
            step_index: *step_index,
            step_name: format!("step_{}", step_index),
            success: *success,
            duration_ms: *duration_ms,
            output_preview: String::new(),
        }),
        PipelineEvent::Error {
            step_index,
            error,
        } => Some(TauriPipelineEvent::StepFailed {
            pipeline_id: pipeline_id.to_string(),
            step_index: *step_index,
            step_name: format!("step_{}", step_index),
            error: error.clone(),
        }),
        PipelineEvent::GateWaiting {
            prompt,
            timeout_secs,
        } => Some(TauriPipelineEvent::GateWaiting {
            pipeline_id: pipeline_id.to_string(),
            step_index: 0,
            prompt: prompt.clone(),
        }),
        PipelineEvent::Finished {
            success,
            total_duration_ms,
        } => Some(TauriPipelineEvent::PipelineCompleted {
            pipeline_id: pipeline_id.to_string(),
            success: *success,
            total_steps: 0,
            duration_ms: *total_duration_ms,
        }),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tauri_pipeline_event_serialization() {
        let event = TauriPipelineEvent::StepStarted {
            pipeline_id: "p1".into(),
            step_index: 0,
            step_name: "research".into(),
            step_type: "agent".into(),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("step_started"));
        assert!(json.contains("research"));
    }

    #[test]
    fn test_pipeline_completed_event() {
        let event = TauriPipelineEvent::PipelineCompleted {
            pipeline_id: "p1".into(),
            success: true,
            total_steps: 5,
            duration_ms: 12345,
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("pipeline_completed"));
        assert!(json.contains("12345"));
    }
}
