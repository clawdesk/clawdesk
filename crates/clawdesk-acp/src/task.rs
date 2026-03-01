//! A2A Task — the unit of inter-agent work delegation.
//!
//! ## Finite State Machine
//!
//! The task lifecycle is a deterministic FSM with 6 states and 8 events.
//! Every (state, event) pair is defined — the compiler guarantees
//! exhaustiveness via `match` on the product type.
//!
//! ```text
//!                    ┌───────────────────────────────┐
//!                    │                               ▼
//! ┌──────────┐  work  ┌─────────┐  complete  ┌───────────┐
//! │ Submitted │──────▶│ Working │───────────▶│ Completed │
//! └──────────┘       └─────────┘            └───────────┘
//!      │                │   ▲                     
//!      │cancel          │   │ resume              
//!      ▼                ▼   │                     
//! ┌──────────┐   ┌───────────────┐  fail  ┌────────┐
//! │ Canceled │   │ InputRequired │──────▶│ Failed │
//! └──────────┘   └───────────────┘       └────────┘
//!                                            ▲
//!                       Working ─── fail ────┘
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Spawn mode — how the sub-agent thread is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnMode {
    /// Fire-and-forget: execute, announce result, optionally clean up.
    Run,
    /// Persistent: keeps the thread alive for interactive back-and-forth.
    Session,
}

impl Default for SpawnMode {
    fn default() -> Self {
        Self::Run
    }
}

impl SpawnMode {
    /// Whether this mode announces the result to the requester's thread.
    pub fn announces_on_complete(self) -> bool {
        matches!(self, Self::Run)
    }
}

impl std::fmt::Display for SpawnMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run => f.write_str("run"),
            Self::Session => f.write_str("session"),
        }
    }
}

impl From<&str> for SpawnMode {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "session" | "persistent" => Self::Session,
            _ => Self::Run,
        }
    }
}

/// Cleanup policy after task completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicy {
    /// Preserve the thread and history.
    Keep,
    /// Remove the sub-agent thread after announce.
    Delete,
}

impl Default for CleanupPolicy {
    fn default() -> Self {
        Self::Keep
    }
}

/// Unique task identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for TaskId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Task lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Task created but not yet started.
    Submitted,
    /// Agent is actively working on the task.
    Working,
    /// Agent needs additional input from the requester.
    InputRequired,
    /// Task completed successfully.
    Completed,
    /// Task failed.
    Failed,
    /// Task was canceled by the requester.
    Canceled,
}

impl TaskState {
    /// Whether this is a terminal state (no further transitions).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
}

/// Events that drive task state transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEvent {
    /// Agent begins working on the task.
    Work,
    /// Agent has completed the task.
    Complete { output: serde_json::Value },
    /// Agent needs more input.
    RequestInput { prompt: String },
    /// Requester provides additional input.
    ProvideInput { input: serde_json::Value },
    /// Agent or requester cancels the task.
    Cancel { reason: Option<String> },
    /// Task has failed.
    Fail { error: String },
    /// Progress update (doesn't change state).
    Progress { percent: f64, message: Option<String> },
    /// Task timeout.
    Timeout,
}

/// A task — the unit of work delegated between agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique task ID.
    pub id: TaskId,
    /// Current state.
    pub state: TaskState,
    /// ID of the requesting agent.
    pub requester_id: String,
    /// ID of the executing agent.
    pub executor_id: String,
    /// The skill being invoked.
    pub skill_id: Option<String>,
    /// Input data for the task.
    pub input: serde_json::Value,
    /// Output data (populated on completion).
    pub output: Option<serde_json::Value>,
    /// Error message (populated on failure).
    pub error: Option<String>,
    /// Task creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last state change timestamp.
    pub updated_at: DateTime<Utc>,
    /// Progress percentage (0.0 - 1.0).
    pub progress: f64,
    /// Artifacts produced during execution.
    pub artifacts: Vec<super::message::Artifact>,
    /// History of state transitions.
    pub history: Vec<TaskTransition>,

    // ── Thread-as-Agent context ──────────────────────────────────────

    /// Thread ID this task is executing within.
    /// Links the A2A task to a specific chat thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<u128>,

    /// Session key for A2A routing (agent-scoped).
    /// Format: `agent:{agent_id}:{identifier}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,

    /// Spawn mode: how the sub-agent thread is created.
    #[serde(default)]
    pub spawn_mode: SpawnMode,

    /// Cleanup policy after task completion.
    #[serde(default)]
    pub cleanup: CleanupPolicy,

    /// Whether to announce the result to the requester's thread.
    #[serde(default = "default_announce")]
    pub announce_on_complete: bool,
}

fn default_announce() -> bool { true }

/// Record of a state transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTransition {
    pub from: TaskState,
    pub to: TaskState,
    pub event: String,
    pub timestamp: DateTime<Utc>,
}

impl Task {
    /// Create a new task.
    pub fn new(
        requester_id: impl Into<String>,
        executor_id: impl Into<String>,
        input: serde_json::Value,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: TaskId::new(),
            state: TaskState::Submitted,
            requester_id: requester_id.into(),
            executor_id: executor_id.into(),
            skill_id: None,
            input,
            output: None,
            error: None,
            created_at: now,
            updated_at: now,
            progress: 0.0,
            artifacts: vec![],
            history: vec![],
            thread_id: None,
            session_key: None,
            spawn_mode: SpawnMode::default(),
            cleanup: CleanupPolicy::default(),
            announce_on_complete: true,
        }
    }

    /// Create a task bound to a specific thread.
    pub fn for_thread(
        requester_id: impl Into<String>,
        executor_id: impl Into<String>,
        input: serde_json::Value,
        thread_id: u128,
        spawn_mode: SpawnMode,
    ) -> Self {
        let mut task = Self::new(requester_id, executor_id, input);
        task.thread_id = Some(thread_id);
        task.spawn_mode = spawn_mode;
        task.session_key = Some(format!("agent:{}:{:032x}", task.executor_id, thread_id));
        task
    }

    /// Apply an event to the task, transitioning its state.
    ///
    /// Returns `Ok(new_state)` if the transition is valid,
    /// `Err(message)` if the transition is invalid from the current state.
    ///
    /// ## Transition table (total function)
    ///
    /// | Current State   | Event         | Next State      |
    /// |-----------------|---------------|-----------------|
    /// | Submitted       | Work          | Working         |
    /// | Submitted       | Cancel        | Canceled        |
    /// | Submitted       | Fail          | Failed          |
    /// | Submitted       | Timeout       | Failed          |
    /// | Working         | Complete      | Completed       |
    /// | Working         | RequestInput  | InputRequired   |
    /// | Working         | Fail          | Failed          |
    /// | Working         | Cancel        | Canceled        |
    /// | Working         | Progress      | Working (same)  |
    /// | Working         | Timeout       | Failed          |
    /// | InputRequired   | ProvideInput  | Working         |
    /// | InputRequired   | Cancel        | Canceled        |
    /// | InputRequired   | Fail          | Failed          |
    /// | InputRequired   | Timeout       | Failed          |
    /// | Completed/Failed/Canceled | *   | Error (terminal)|
    pub fn apply_event(&mut self, event: TaskEvent) -> Result<TaskState, String> {
        if self.state.is_terminal() {
            return Err(format!(
                "task {} is in terminal state {:?}, cannot apply event",
                self.id, self.state
            ));
        }

        let prev_state = self.state;
        let new_state = match (&self.state, &event) {
            // From Submitted
            (TaskState::Submitted, TaskEvent::Work) => TaskState::Working,
            (TaskState::Submitted, TaskEvent::Cancel { .. }) => TaskState::Canceled,
            (TaskState::Submitted, TaskEvent::Fail { .. }) => TaskState::Failed,
            (TaskState::Submitted, TaskEvent::Timeout) => TaskState::Failed,

            // From Working
            (TaskState::Working, TaskEvent::Complete { output }) => {
                self.output = Some(output.clone());
                TaskState::Completed
            }
            (TaskState::Working, TaskEvent::RequestInput { .. }) => TaskState::InputRequired,
            (TaskState::Working, TaskEvent::Fail { error }) => {
                self.error = Some(error.clone());
                TaskState::Failed
            }
            (TaskState::Working, TaskEvent::Cancel { reason }) => {
                self.error = reason.clone();
                TaskState::Canceled
            }
            (TaskState::Working, TaskEvent::Progress { percent, .. }) => {
                self.progress = percent.clamp(0.0, 1.0);
                TaskState::Working // no state change
            }
            (TaskState::Working, TaskEvent::Timeout) => {
                self.error = Some("task timed out".into());
                TaskState::Failed
            }

            // From InputRequired
            (TaskState::InputRequired, TaskEvent::ProvideInput { .. }) => TaskState::Working,
            (TaskState::InputRequired, TaskEvent::Cancel { reason }) => {
                self.error = reason.clone();
                TaskState::Canceled
            }
            (TaskState::InputRequired, TaskEvent::Fail { error }) => {
                self.error = Some(error.clone());
                TaskState::Failed
            }
            (TaskState::InputRequired, TaskEvent::Timeout) => {
                self.error = Some("input request timed out".into());
                TaskState::Failed
            }

            // Invalid transitions
            (state, event) => {
                return Err(format!(
                    "invalid transition: {:?} + {:?}",
                    state,
                    std::mem::discriminant(event)
                ));
            }
        };

        // Record transition
        if new_state != prev_state {
            self.history.push(TaskTransition {
                from: prev_state,
                to: new_state,
                event: format!("{:?}", std::mem::discriminant(&event)),
                timestamp: Utc::now(),
            });
        }

        self.state = new_state;
        self.updated_at = Utc::now();
        Ok(new_state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_lifecycle_happy_path() {
        let mut task = Task::new("agent-a", "agent-b", serde_json::json!({"prompt": "hello"}));
        assert_eq!(task.state, TaskState::Submitted);

        task.apply_event(TaskEvent::Work).unwrap();
        assert_eq!(task.state, TaskState::Working);

        task.apply_event(TaskEvent::Progress {
            percent: 0.5,
            message: Some("halfway".into()),
        })
        .unwrap();
        assert_eq!(task.state, TaskState::Working);
        assert_eq!(task.progress, 0.5);

        task.apply_event(TaskEvent::Complete {
            output: serde_json::json!({"result": "done"}),
        })
        .unwrap();
        assert_eq!(task.state, TaskState::Completed);
        assert!(task.output.is_some());
    }

    #[test]
    fn task_input_required_flow() {
        let mut task = Task::new("a", "b", serde_json::json!({}));
        task.apply_event(TaskEvent::Work).unwrap();
        task.apply_event(TaskEvent::RequestInput {
            prompt: "what file?".into(),
        })
        .unwrap();
        assert_eq!(task.state, TaskState::InputRequired);

        task.apply_event(TaskEvent::ProvideInput {
            input: serde_json::json!({"file": "test.rs"}),
        })
        .unwrap();
        assert_eq!(task.state, TaskState::Working);
    }

    #[test]
    fn terminal_state_rejects_events() {
        let mut task = Task::new("a", "b", serde_json::json!({}));
        task.apply_event(TaskEvent::Cancel {
            reason: Some("test".into()),
        })
        .unwrap();
        assert_eq!(task.state, TaskState::Canceled);

        let result = task.apply_event(TaskEvent::Work);
        assert!(result.is_err());
    }

    #[test]
    fn history_records_transitions() {
        let mut task = Task::new("a", "b", serde_json::json!({}));
        task.apply_event(TaskEvent::Work).unwrap();
        task.apply_event(TaskEvent::Complete {
            output: serde_json::json!({}),
        })
        .unwrap();
        assert_eq!(task.history.len(), 2);
        assert_eq!(task.history[0].from, TaskState::Submitted);
        assert_eq!(task.history[0].to, TaskState::Working);
        assert_eq!(task.history[1].from, TaskState::Working);
        assert_eq!(task.history[1].to, TaskState::Completed);
    }

    #[test]
    fn checkpoint_roundtrip() {
        let task = Task::new("a", "b", serde_json::json!({"q": "hello"}));
        let cp = TaskCheckpoint::new(&task, 3, serde_json::json!({"step_2": "done"}));
        assert_eq!(cp.task_id, task.id);
        assert_eq!(cp.completed_steps, 3);
        assert!(!cp.step_outputs.is_null());
        // Verify serde roundtrip
        let json = serde_json::to_string(&cp).unwrap();
        let restored: TaskCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.completed_steps, 3);
    }
}

// ─── Durable Task Checkpoint ────────────────────────────────────────────────

/// Serializable checkpoint for long-running task execution.
///
/// Captures enough state to resume a multi-step pipeline or task from the
/// last successful step after a process restart. The checkpoint is designed
/// to be persisted to SochDB, filesystem, or any `CronPersistence`-like backend.
///
/// ## Invariant
///
/// `completed_steps ≤ total_steps` (when total is known). Steps `0..completed_steps`
/// can be skipped on resume; execution restarts at step `completed_steps`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCheckpoint {
    /// Task this checkpoint belongs to.
    pub task_id: TaskId,
    /// Snapshot of the full task state at checkpoint time.
    pub task_snapshot: Task,
    /// Number of completed pipeline steps.
    pub completed_steps: usize,
    /// Intermediate step outputs: `{ "step_0": <value>, "step_1": <value>, ... }`.
    /// Used to feed prior outputs into resumed steps without re-execution.
    pub step_outputs: serde_json::Value,
    /// When this checkpoint was created.
    pub checkpoint_at: DateTime<Utc>,
    /// Optional human-readable label (e.g., "after web-search step").
    pub label: Option<String>,
}

impl TaskCheckpoint {
    /// Create a checkpoint from the current task state.
    pub fn new(
        task: &Task,
        completed_steps: usize,
        step_outputs: serde_json::Value,
    ) -> Self {
        Self {
            task_id: task.id.clone(),
            task_snapshot: task.clone(),
            completed_steps,
            step_outputs,
            checkpoint_at: Utc::now(),
            label: None,
        }
    }

    /// Create a labeled checkpoint.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}
