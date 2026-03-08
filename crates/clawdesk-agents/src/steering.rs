//! # Steering — Mid-execution redirection via typed message channels.
//!
//! Provides two separate message queues for controlling a running agent:
//!
//! 1. **Steering queue** (`steer()`): Checked after each tool execution.
//!    If non-empty, remaining tool calls in the current round are skipped,
//!    and the steering message is injected before the next LLM call.
//!    Enables "interrupt and redirect" semantics.
//!
//! 2. **Follow-up queue** (`follow_up()`): Checked only when the agent has
//!    no more tool calls and no steering messages. Enables "wait until idle,
//!    then continue" semantics.
//!
//! ## Dequeue Modes
//!
//! - `OneAtATime`: Dequeue a single message per check.
//! - `DrainAll`: Dequeue all queued messages in one batch.
//!
//! ## Performance
//!
//! Steering check is O(1) `try_recv()` on an MPSC channel — zero-cost when
//! empty (no syscall, just an atomic load). For a round with T tool calls,
//! worst-case overhead is T atomic loads — negligible vs. O(seconds) LLM latency.

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, info};

// ═══════════════════════════════════════════════════════════════════════════
// Message types
// ═══════════════════════════════════════════════════════════════════════════

/// A steering message that interrupts the current tool round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteeringMessage {
    /// The message content to inject into the conversation.
    pub content: String,
    /// Source of the steering message (user, parent agent, system).
    pub source: SteeringSource,
}

/// Source of a steering message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteeringSource {
    /// User directly steering via UI.
    User,
    /// Parent orchestrator redirecting a child agent.
    ParentAgent { agent_id: String },
    /// System-level intervention (safety, budget, etc.).
    System { reason: String },
}

/// A follow-up message that continues after the agent reaches idle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowUpMessage {
    /// The message content to inject as the next user turn.
    pub content: String,
    /// Source of the follow-up.
    pub source: FollowUpSource,
}

/// Source of a follow-up message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpSource {
    /// User sending additional input.
    User,
    /// Parent agent adding more work.
    ParentAgent { agent_id: String },
    /// Automated follow-up (e.g., pipeline continuation).
    Automated { pipeline_id: String },
}

/// How messages are dequeued from a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DequeueMode {
    /// Dequeue one message per check.
    OneAtATime,
    /// Drain all available messages in one batch.
    DrainAll,
}

impl Default for DequeueMode {
    fn default() -> Self {
        Self::OneAtATime
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Steering controller — held by the agent runner
// ═══════════════════════════════════════════════════════════════════════════

/// Result of checking for a steering message between tool executions.
#[derive(Debug)]
pub enum SteeringCheck {
    /// No steering message — continue with the next tool call.
    Continue,
    /// A steering message was received — skip remaining tools and inject.
    Steer {
        messages: Vec<SteeringMessage>,
        /// Number of remaining tool calls that were skipped.
        skipped_tools: usize,
    },
}

/// Result of checking for follow-up messages at the end of execution.
#[derive(Debug)]
pub enum FollowUpCheck {
    /// No follow-up — execution is complete.
    Done,
    /// Follow-up messages available — continue with these as input.
    Continue { messages: Vec<FollowUpMessage> },
}

/// Controller for an agent's steering and follow-up channels.
///
/// The sending halves (`SteeringSender`, `FollowUpSender`) are given to
/// external actors (parent orchestrator, TUI, etc.). The receiving half
/// stays with the agent runner.
pub struct SteeringController {
    steering_rx: mpsc::Receiver<SteeringMessage>,
    follow_up_rx: mpsc::Receiver<FollowUpMessage>,
    steering_dequeue: DequeueMode,
    follow_up_dequeue: DequeueMode,
}

/// Handle for sending steering messages to a running agent.
#[derive(Clone)]
pub struct SteeringSender {
    tx: mpsc::Sender<SteeringMessage>,
}

/// Handle for sending follow-up messages to a running agent.
#[derive(Clone)]
pub struct FollowUpSender {
    tx: mpsc::Sender<FollowUpMessage>,
}

impl SteeringSender {
    /// Send a steering message to the running agent.
    /// Returns `Err` if the agent has already finished (receiver dropped).
    pub async fn steer(&self, message: SteeringMessage) -> Result<(), SteeringError> {
        self.tx.send(message).await.map_err(|_| SteeringError::AgentFinished)
    }

    /// Try to send a steering message without waiting.
    pub fn try_steer(&self, message: SteeringMessage) -> Result<(), SteeringError> {
        self.tx.try_send(message).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => SteeringError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => SteeringError::AgentFinished,
        })
    }
}

impl FollowUpSender {
    /// Send a follow-up message to the agent.
    pub async fn follow_up(&self, message: FollowUpMessage) -> Result<(), SteeringError> {
        self.tx.send(message).await.map_err(|_| SteeringError::AgentFinished)
    }
}

/// Error from steering/follow-up operations.
#[derive(Debug, thiserror::Error)]
pub enum SteeringError {
    #[error("agent has already finished")]
    AgentFinished,
    #[error("steering queue is full")]
    QueueFull,
}

impl SteeringController {
    /// Create a new steering controller with its sending handles.
    ///
    /// `steering_capacity` and `follow_up_capacity` control the channel buffer sizes.
    pub fn new(
        steering_capacity: usize,
        follow_up_capacity: usize,
    ) -> (Self, SteeringSender, FollowUpSender) {
        let (stx, srx) = mpsc::channel(steering_capacity);
        let (ftx, frx) = mpsc::channel(follow_up_capacity);

        let controller = Self {
            steering_rx: srx,
            follow_up_rx: frx,
            steering_dequeue: DequeueMode::OneAtATime,
            follow_up_dequeue: DequeueMode::DrainAll,
        };

        (
            controller,
            SteeringSender { tx: stx },
            FollowUpSender { tx: ftx },
        )
    }

    /// Set the dequeue mode for steering messages.
    pub fn with_steering_dequeue(mut self, mode: DequeueMode) -> Self {
        self.steering_dequeue = mode;
        self
    }

    /// Set the dequeue mode for follow-up messages.
    pub fn with_follow_up_dequeue(mut self, mode: DequeueMode) -> Self {
        self.follow_up_dequeue = mode;
        self
    }

    /// Check for steering messages between tool executions.
    ///
    /// Called by the runner after each tool completes within a round.
    /// If a steering message is present, the caller should:
    /// 1. Skip remaining tool calls in this round
    /// 2. Replace skipped tools' results with "Skipped due to queued user message"
    /// 3. Inject the steering message before the next LLM call
    pub fn check_steering(&mut self, remaining_tools: usize) -> SteeringCheck {
        let messages = drain_mpsc(&mut self.steering_rx, self.steering_dequeue);
        if messages.is_empty() {
            SteeringCheck::Continue
        } else {
            info!(
                count = messages.len(),
                skipped = remaining_tools,
                "steering message received — skipping remaining tools"
            );
            SteeringCheck::Steer {
                messages,
                skipped_tools: remaining_tools,
            }
        }
    }

    /// Check for follow-up messages when the agent would otherwise stop.
    ///
    /// Called by the runner when the LLM returns a final response (no tool calls).
    /// If follow-up messages exist, the runner should inject them as user messages
    /// and continue the loop.
    pub fn check_follow_up(&mut self) -> FollowUpCheck {
        let messages = drain_mpsc(&mut self.follow_up_rx, self.follow_up_dequeue);
        if messages.is_empty() {
            FollowUpCheck::Done
        } else {
            info!(count = messages.len(), "follow-up messages received — continuing");
            FollowUpCheck::Continue { messages }
        }
    }
}

/// Drain messages from an MPSC receiver according to the dequeue mode.
fn drain_mpsc<T>(rx: &mut mpsc::Receiver<T>, mode: DequeueMode) -> Vec<T> {
    let mut messages = Vec::new();
    match mode {
        DequeueMode::OneAtATime => {
            if let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }
        DequeueMode::DrainAll => {
            while let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }
    }
    messages
}

/// Format skipped tool results for tools that were bypassed due to steering.
pub fn skipped_tool_result(tool_call_id: &str, tool_name: &str) -> crate::tools::ToolResult {
    crate::tools::ToolResult {
        tool_call_id: tool_call_id.to_string(),
        name: tool_name.to_string(),
        content: "Skipped due to queued user message".to_string(),
        is_error: true,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_steering_empty_is_continue() {
        let (mut ctrl, _steer, _follow) = SteeringController::new(16, 16);
        let check = ctrl.check_steering(3);
        assert!(matches!(check, SteeringCheck::Continue));
    }

    #[tokio::test]
    async fn test_steering_message_received() {
        let (mut ctrl, steer, _follow) = SteeringController::new(16, 16);

        steer
            .steer(SteeringMessage {
                content: "stop and do X instead".into(),
                source: SteeringSource::User,
            })
            .await
            .unwrap();

        let check = ctrl.check_steering(5);
        match check {
            SteeringCheck::Steer {
                messages,
                skipped_tools,
            } => {
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].content, "stop and do X instead");
                assert_eq!(skipped_tools, 5);
            }
            SteeringCheck::Continue => panic!("expected Steer"),
        }
    }

    #[tokio::test]
    async fn test_follow_up_empty_is_done() {
        let (mut ctrl, _steer, _follow) = SteeringController::new(16, 16);
        let check = ctrl.check_follow_up();
        assert!(matches!(check, FollowUpCheck::Done));
    }

    #[tokio::test]
    async fn test_follow_up_message_continues() {
        let (mut ctrl, _steer, follow) = SteeringController::new(16, 16);

        follow
            .follow_up(FollowUpMessage {
                content: "now do Y".into(),
                source: FollowUpSource::User,
            })
            .await
            .unwrap();

        let check = ctrl.check_follow_up();
        match check {
            FollowUpCheck::Continue { messages } => {
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].content, "now do Y");
            }
            FollowUpCheck::Done => panic!("expected Continue"),
        }
    }

    #[tokio::test]
    async fn test_drain_all_mode() {
        let (mut ctrl, steer, _follow) = SteeringController::new(16, 16);
        let ctrl = ctrl.with_steering_dequeue(DequeueMode::DrainAll);

        for i in 0..3 {
            steer
                .steer(SteeringMessage {
                    content: format!("msg {}", i),
                    source: SteeringSource::User,
                })
                .await
                .unwrap();
        }

        // The ctrl was moved, so we need to use it directly
        // Actually the with_ methods consume and return self
        let mut ctrl = ctrl;
        let check = ctrl.check_steering(0);
        match check {
            SteeringCheck::Steer { messages, .. } => {
                assert_eq!(messages.len(), 3);
            }
            SteeringCheck::Continue => panic!("expected Steer"),
        }
    }

    #[tokio::test]
    async fn test_one_at_a_time_mode() {
        let (ctrl, steer, _follow) = SteeringController::new(16, 16);
        let mut ctrl = ctrl.with_steering_dequeue(DequeueMode::OneAtATime);

        for i in 0..3 {
            steer
                .steer(SteeringMessage {
                    content: format!("msg {}", i),
                    source: SteeringSource::User,
                })
                .await
                .unwrap();
        }

        let check = ctrl.check_steering(0);
        match check {
            SteeringCheck::Steer { messages, .. } => {
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].content, "msg 0");
            }
            _ => panic!("expected Steer"),
        }
    }

    #[tokio::test]
    async fn test_skipped_tool_result() {
        let result = skipped_tool_result("call-123", "shell_exec");
        assert!(result.is_error);
        assert!(result.content.contains("Skipped"));
        assert_eq!(result.name, "shell_exec");
    }

    #[tokio::test]
    async fn test_steer_after_agent_finished() {
        let (ctrl, steer, _follow) = SteeringController::new(16, 16);
        drop(ctrl); // Agent finished — receiver dropped

        let result = steer
            .steer(SteeringMessage {
                content: "too late".into(),
                source: SteeringSource::User,
            })
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SteeringError::AgentFinished));
    }
}
