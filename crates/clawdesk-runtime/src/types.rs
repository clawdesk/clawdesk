//! Core types for the durable agent runtime.
//!
//! These types model the entire lifecycle of a durable workflow execution:
//! creation → lease acquisition → journaled execution → checkpoint → completion.

use chrono::{DateTime, Utc};
use clawdesk_agents::pipeline::{AgentPipeline, StepResult};
use clawdesk_agents::runner::AgentConfig;
use clawdesk_agents::tools::ToolResult;
use clawdesk_domain::context_guard::CompactionLevel;
use clawdesk_providers::{ChatMessage, FinishReason, ToolCall, TokenUsage};
use clawdesk_types::session::SessionKey;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;
use uuid::Uuid;

// ── Identifiers ──────────────────────────────────────────────

/// Unique identifier for a workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunId(pub String);

impl RunId {
    /// Generate a new random run ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Create from an existing string.
    pub fn from_str(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── WorkflowRun ──────────────────────────────────────────────

/// A durable execution of an agent workflow.
///
/// Persisted to SochDB as `runtime:runs:{run_id}`.
/// This is the top-level entity: every agent execution, pipeline run,
/// or A2A task delegation creates exactly one `WorkflowRun`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    /// Unique run identifier.
    pub id: RunId,
    /// What kind of workflow this run represents.
    pub workflow_type: WorkflowType,
    /// Current lifecycle state.
    pub state: RunState,
    /// Retry policy for the workflow.
    pub retry_policy: RetryPolicy,
    /// How many times this run has been attempted (first attempt = 1).
    pub attempt: u32,
    /// Parent run ID (for nested pipeline steps).
    pub parent_run_id: Option<RunId>,
    /// Worker that currently owns this run.
    pub worker_id: Option<String>,

    // ── Timing ──
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Hard deadline after which the run is force-failed.
    pub deadline: Option<DateTime<Utc>>,

    // ── Cost tracking ──
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tool_calls: u32,
    /// Journal sequence counter (monotonically increasing).
    pub next_seq: u64,
}

impl WorkflowRun {
    /// Create a new workflow run in Pending state.
    pub fn new(workflow_type: WorkflowType, retry_policy: RetryPolicy) -> Self {
        let now = Utc::now();
        Self {
            id: RunId::new(),
            workflow_type,
            state: RunState::Pending,
            retry_policy,
            attempt: 0,
            parent_run_id: None,
            worker_id: None,
            created_at: now,
            updated_at: now,
            deadline: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_tool_calls: 0,
            next_seq: 0,
        }
    }

    /// Create a child run (nested pipeline step).
    pub fn new_child(
        parent: &RunId,
        workflow_type: WorkflowType,
        retry_policy: RetryPolicy,
    ) -> Self {
        let mut run = Self::new(workflow_type, retry_policy);
        run.parent_run_id = Some(parent.clone());
        run
    }

    /// Allocate the next journal sequence number.
    pub fn next_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// Is this run in a terminal state?
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            RunState::Completed { .. } | RunState::Failed { .. } | RunState::Cancelled { .. }
        )
    }
}

// ── WorkflowType ─────────────────────────────────────────────

/// What kind of workflow this run represents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowType {
    /// Single agent execution (wraps AgentRunner::run).
    AgentLoop {
        config: AgentConfig,
        session_key: SessionKey,
    },
    /// Multi-step pipeline (wraps AgentPipeline).
    Pipeline {
        pipeline: AgentPipeline,
    },
    /// A2A task delegation.
    A2ATask {
        task_id: String,
    },
}

// ── RunState ─────────────────────────────────────────────────

/// Run lifecycle states — superset of A2A TaskState.
///
/// State machine:
/// ```text
/// Pending ──→ Running ──→ Completed
///    │           │   ↑         
///    │           ├───┘ (retry)
///    │           │
///    │           ├──→ Suspended ──→ Running (resume)
///    │           │
///    │           └──→ Failed
///    │
///    └──→ Cancelled
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunState {
    /// Queued, waiting for a worker to pick up.
    Pending,
    /// A worker has acquired the lease and is executing.
    Running {
        worker_id: String,
    },
    /// Waiting for external input (human gate, A2A response).
    Suspended {
        reason: SuspendReason,
    },
    /// Completed successfully.
    Completed {
        output: serde_json::Value,
    },
    /// Failed after exhausting retries.
    Failed {
        error: String,
        attempts: u32,
    },
    /// Cancelled by user or system.
    Cancelled {
        reason: String,
    },
}

impl RunState {
    /// Short label for indexing.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running { .. } => "running",
            Self::Suspended { .. } => "suspended",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
            Self::Cancelled { .. } => "cancelled",
        }
    }
}

/// Why a run is suspended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuspendReason {
    /// Waiting for human approval at a pipeline gate.
    HumanGate {
        prompt: String,
        timeout_secs: u64,
    },
    /// Waiting for an A2A task response from another agent.
    A2AResponse {
        task_id: String,
    },
    /// Waiting for user input (input_required in A2A protocol).
    UserInput {
        prompt: String,
    },
}

// ── JournalEntry ─────────────────────────────────────────────

/// The unit of durable side-effect.
///
/// Every externally-observable operation is journaled **before** its effects
/// are consumed by the agent loop. On replay, completed journal entries are
/// returned from cache instead of re-executing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEntry {
    /// LLM provider call — input hash + output.
    LlmCall {
        seq: u64,
        round: usize,
        /// FxHash of (model + messages + tools) — for replay cache matching.
        request_hash: u64,
        response: LlmSnapshot,
        started_at: DateTime<Utc>,
        completed_at: DateTime<Utc>,
    },
    /// Tool execution — name + args + result.
    ToolExecution {
        seq: u64,
        round: usize,
        tool_call_id: String,
        name: String,
        args_hash: u64,
        result: ToolResult,
        duration_ms: u64,
    },
    /// Context compaction event.
    Compaction {
        seq: u64,
        round: usize,
        level: CompactionLevel,
        tokens_before: usize,
        tokens_after: usize,
    },
    /// Human gate approval/rejection.
    GateDecision {
        seq: u64,
        step_index: usize,
        prompt: String,
        approved: bool,
        decided_by: String,
        decided_at: DateTime<Utc>,
    },
}

/// Snapshot of an LLM response for journal storage.
///
/// Captures exactly the fields needed to reconstruct a `ProviderResponse`
/// without the ephemeral parts (latency, http details).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSnapshot {
    pub content: String,
    pub model: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    pub finish_reason: FinishReason,
}

impl From<&clawdesk_providers::ProviderResponse> for LlmSnapshot {
    fn from(r: &clawdesk_providers::ProviderResponse) -> Self {
        Self {
            content: r.content.clone(),
            model: r.model.clone(),
            tool_calls: r.tool_calls.clone(),
            usage: r.usage.clone(),
            finish_reason: r.finish_reason,
        }
    }
}

impl LlmSnapshot {
    /// Convert back to a ProviderResponse for injection into the agent loop.
    pub fn into_provider_response(self) -> clawdesk_providers::ProviderResponse {
        clawdesk_providers::ProviderResponse {
            content: self.content,
            model: self.model.clone(),
            provider: String::new(), // not needed for replay
            usage: self.usage,
            tool_calls: self.tool_calls,
            finish_reason: self.finish_reason,
            latency: Duration::from_secs(0),
        }
    }
}

// ── Checkpoint ───────────────────────────────────────────────

/// Minimal state snapshot for crash recovery.
///
/// Taken after each completed round (agent) or step (pipeline).
/// On resume, the runtime deserializes the latest checkpoint and
/// continues from that point — no replay of earlier rounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Checkpoint {
    /// Agent loop checkpoint: taken after each completed round.
    AgentLoop {
        /// Next round to execute (0-indexed).
        round: usize,
        /// Accumulated messages (system + history + tool results).
        messages: Vec<ChatMessage>,
        /// Effective system prompt/instruction bundle for the run.
        #[serde(default)]
        system_prompt: String,
        /// Token accounting.
        total_input_tokens: u64,
        total_output_tokens: u64,
        /// Context guard state for resuming compaction decisions.
        guard_state: GuardSnapshot,
    },
    /// Pipeline checkpoint: taken after each completed step.
    PipelineStep {
        /// Next step index to execute.
        step_index: usize,
        /// Results from completed steps.
        step_results: Vec<StepResult>,
        /// Accumulated context flowing through the pipeline.
        context: serde_json::Value,
    },
}

/// Serialized ContextGuard state — enough to reconstruct the guard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardSnapshot {
    /// Current estimated token count.
    pub estimated_tokens: usize,
    /// How many compactions have been performed.
    pub compaction_count: u32,
    /// Circuit breaker consecutive failure count.
    pub circuit_breaker_failures: u32,
}

// ── Lease ────────────────────────────────────────────────────

/// Worker lease for distributed execution safety.
///
/// Only one worker may hold the lease for a given run at a time.
/// The fence token prevents stale workers from writing after lease expiry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub run_id: RunId,
    pub worker_id: String,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub ttl_secs: u64,
    /// Monotonic fencing token — incremented on each acquisition.
    /// Prevents stale workers from corrupting state after their lease expires.
    pub fence_token: u64,
}

impl Lease {
    /// Is this lease currently valid?
    pub fn is_valid(&self) -> bool {
        Utc::now() < self.expires_at
    }

    /// Is this lease held by the given worker?
    pub fn is_held_by(&self, worker_id: &str) -> bool {
        self.worker_id == worker_id && self.is_valid()
    }

    /// Create a new lease with a specific fence token.
    pub fn new(run_id: RunId, worker_id: String, ttl_secs: u64, fence_token: u64) -> Self {
        let now = Utc::now();
        let ttl = chrono::Duration::seconds(ttl_secs as i64);
        Self {
            run_id,
            worker_id,
            acquired_at: now,
            expires_at: now + ttl,
            ttl_secs,
            fence_token,
        }
    }

    /// Renew the lease (extend expiry), keeping the same fence token.
    /// Returns a new Lease value (for re-serialization).
    pub fn renew(&self, ttl_secs: u64) -> Self {
        let ttl = chrono::Duration::seconds(ttl_secs as i64);
        Self {
            run_id: self.run_id.clone(),
            worker_id: self.worker_id.clone(),
            acquired_at: self.acquired_at,
            expires_at: Utc::now() + ttl,
            ttl_secs,
            fence_token: self.fence_token,
        }
    }
}

// ── RetryPolicy ──────────────────────────────────────────────

/// Retry policy for workflow execution.
///
/// Implements capped exponential backoff with jitter to prevent
/// thundering herd on provider recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum retry attempts (0 = no retries).
    pub max_attempts: u32,
    /// Base delay between retries (seconds).
    pub initial_backoff_secs: u64,
    /// Maximum delay (seconds).
    pub max_backoff_secs: u64,
    /// Multiplier per retry (typically 2.0).
    pub backoff_multiplier: f64,
    /// Error classes that should NOT be retried.
    pub non_retryable: Vec<ErrorClass>,
}

impl RetryPolicy {
    /// Compute the delay for a given attempt number.
    ///
    /// Uses capped exponential backoff: base × multiplier^attempt,
    /// capped at max_backoff, with ±25% jitter.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base_ms = self.initial_backoff_secs as f64 * 1000.0;
        let delay = base_ms * self.backoff_multiplier.powi(attempt as i32);
        let max_ms = self.max_backoff_secs as f64 * 1000.0;
        let capped = delay.min(max_ms);
        // Deterministic jitter (±25%) using attempt as seed.
        let jitter_factor = 0.75 + (((attempt as f64 * 1.618033988) % 1.0) * 0.5);
        Duration::from_millis((capped * jitter_factor) as u64)
    }

    /// Should this error class be retried?
    pub fn should_retry(&self, error: &ErrorClass) -> bool {
        !self.non_retryable.contains(error)
    }

    /// Default policy for agent loop execution.
    pub fn default_agent() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_secs: 5,
            max_backoff_secs: 300,
            backoff_multiplier: 2.0,
            non_retryable: vec![
                ErrorClass::ClientError,
                ErrorClass::MaxIterations,
                ErrorClass::ContextOverflow,
            ],
        }
    }

    /// Default policy for pipeline execution.
    pub fn default_pipeline() -> Self {
        Self {
            max_attempts: 2,
            initial_backoff_secs: 10,
            max_backoff_secs: 600,
            backoff_multiplier: 2.0,
            non_retryable: vec![ErrorClass::ClientError, ErrorClass::MaxIterations],
        }
    }

    /// No retries — fail immediately.
    pub fn none() -> Self {
        Self {
            max_attempts: 0,
            initial_backoff_secs: 0,
            max_backoff_secs: 0,
            backoff_multiplier: 1.0,
            non_retryable: vec![],
        }
    }
}

/// Classification of errors for retry decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Provider returned 4xx — don't retry (invalid request).
    ClientError,
    /// Provider returned 5xx — retry with backoff.
    ServerError,
    /// Rate limited — retry after provider's indicated delay.
    RateLimited,
    /// Tool execution failed — retry if idempotent.
    ToolError,
    /// Context overflow — don't retry without compaction.
    ContextOverflow,
    /// Max iterations reached — don't retry (infinite loop).
    MaxIterations,
    /// Worker lease expired — reassign to different worker.
    LeaseExpired,
    /// Unknown error — retry with backoff.
    Unknown,
}

impl ErrorClass {
    /// Classify a ClawDeskError into an ErrorClass.
    pub fn classify(err: &clawdesk_types::error::ClawDeskError) -> Self {
        use clawdesk_types::error::ClawDeskError;
        match err {
            ClawDeskError::Agent(e) => {
                use clawdesk_types::error::AgentError;
                match e {
                    AgentError::MaxIterations { .. } => Self::MaxIterations,
                    AgentError::Cancelled => Self::ClientError,
                    AgentError::ContextAssemblyFailed { .. } => Self::ContextOverflow,
                    AgentError::AllProvidersExhausted => Self::ServerError,
                    AgentError::ToolFailed { .. } => Self::ToolError,
                    _ => Self::Unknown,
                }
            }
            ClawDeskError::Provider(_) => Self::ServerError,
            ClawDeskError::Storage(_) => Self::ServerError,
            _ => Self::Unknown,
        }
    }
}

// ── DeadLetterEntry ──────────────────────────────────────────

/// An entry in the dead letter queue — a permanently failed run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetterEntry {
    pub run_id: RunId,
    pub workflow_type: WorkflowType,
    pub error: String,
    pub attempts: u32,
    pub first_attempt_at: DateTime<Utc>,
    pub last_attempt_at: DateTime<Utc>,
    /// Last checkpoint before failure — for potential manual retry.
    pub last_checkpoint: Option<Checkpoint>,
    /// Aggregated cost for all attempts.
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

// ── RuntimeError ─────────────────────────────────────────────

/// Errors specific to the durable runtime layer.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("run not found: {run_id}")]
    RunNotFound { run_id: RunId },

    #[error("lease conflict: run {run_id} is held by worker {holder}")]
    LeaseConflict { run_id: RunId, holder: String },

    #[error("lease expired for run {run_id} (fence expected={expected_fence}, actual={actual_fence})")]
    StaleLease {
        run_id: RunId,
        expected_fence: u64,
        actual_fence: u64,
    },

    #[error("run {run_id} is in terminal state: {state}")]
    TerminalState { run_id: RunId, state: String },

    #[error("checkpoint deserialization failed: {detail}")]
    CheckpointCorrupted { detail: String },

    #[error("journal entry deserialization failed at seq {seq}: {detail}")]
    JournalCorrupted { seq: u64, detail: String },

    #[error("run {run_id} exceeded deadline")]
    DeadlineExceeded { run_id: RunId },

    #[error("writer channel closed")]
    WriterClosed,

    #[error("backpressure: write buffer full")]
    BackpressureFull,

    #[error("storage error: {0}")]
    Storage(#[from] clawdesk_types::error::StorageError),

    #[error("agent error: {0}")]
    Agent(String),
}

// ── Convenience conversions ──────────────────────────────────

impl From<RuntimeError> for clawdesk_types::error::ClawDeskError {
    fn from(e: RuntimeError) -> Self {
        match e {
            RuntimeError::Storage(s) => Self::Storage(s),
            other => Self::Agent(clawdesk_types::error::AgentError::ContextAssemblyFailed {
                detail: other.to_string(),
            }),
        }
    }
}

// ── RunnerFactory trait ──────────────────────────────────────

/// Factory for creating AgentRunner instances from configuration.
///
/// Used by the DAG executor to create runners for individual pipeline steps.
#[async_trait::async_trait]
pub trait RunnerFactory: Send + Sync + 'static {
    /// Create an AgentRunner for the given agent configuration.
    fn create_runner(
        &self,
        config: &AgentConfig,
    ) -> Result<clawdesk_agents::runner::AgentRunner, clawdesk_types::error::ClawDeskError>;
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_generation() {
        let a = RunId::new();
        let b = RunId::new();
        assert_ne!(a, b);
        assert!(!a.0.is_empty());
    }

    #[test]
    fn run_state_labels() {
        assert_eq!(RunState::Pending.label(), "pending");
        assert_eq!(
            RunState::Running {
                worker_id: "w1".into()
            }
            .label(),
            "running"
        );
        assert_eq!(
            RunState::Completed {
                output: serde_json::Value::Null
            }
            .label(),
            "completed"
        );
        assert_eq!(
            RunState::Failed {
                error: "x".into(),
                attempts: 1
            }
            .label(),
            "failed"
        );
    }

    #[test]
    fn workflow_run_terminal_check() {
        let mut run = WorkflowRun::new(
            WorkflowType::A2ATask {
                task_id: "t1".into(),
            },
            RetryPolicy::none(),
        );
        assert!(!run.is_terminal());
        run.state = RunState::Completed {
            output: serde_json::json!({"result": "ok"}),
        };
        assert!(run.is_terminal());
    }

    #[test]
    fn retry_policy_backoff() {
        let policy = RetryPolicy::default_agent();
        let d0 = policy.delay_for_attempt(0);
        let d1 = policy.delay_for_attempt(1);
        let d2 = policy.delay_for_attempt(2);
        // Each attempt should be roughly double the previous (within jitter).
        assert!(d1.as_millis() > d0.as_millis());
        assert!(d2.as_millis() > d1.as_millis());
        // Should not exceed max_backoff * max_jitter_factor (1.25).
        let d_large = policy.delay_for_attempt(100);
        let max_with_jitter = (policy.max_backoff_secs as f64 * 1.3) as u64;
        assert!(d_large.as_secs() <= max_with_jitter);
    }

    #[test]
    fn retry_policy_should_retry() {
        let policy = RetryPolicy::default_agent();
        assert!(!policy.should_retry(&ErrorClass::ClientError));
        assert!(!policy.should_retry(&ErrorClass::MaxIterations));
        assert!(policy.should_retry(&ErrorClass::ServerError));
        assert!(policy.should_retry(&ErrorClass::RateLimited));
    }

    #[test]
    fn lease_validity() {
        let lease = Lease::new(RunId::new(), "worker-1".into(), 60, 1);
        assert!(lease.is_valid());
        assert!(lease.is_held_by("worker-1"));
        assert!(!lease.is_held_by("worker-2"));
    }

    #[test]
    fn lease_renewal() {
        let lease = Lease::new(RunId::new(), "w1".into(), 60, 1);
        let old_expires = lease.expires_at;
        std::thread::sleep(Duration::from_millis(10));
        let renewed = lease.renew(60);
        assert!(renewed.expires_at > old_expires);
        assert_eq!(renewed.fence_token, lease.fence_token);
    }

    #[test]
    fn next_seq_monotonic() {
        let mut run = WorkflowRun::new(
            WorkflowType::A2ATask {
                task_id: "t".into(),
            },
            RetryPolicy::none(),
        );
        assert_eq!(run.next_seq(), 0);
        assert_eq!(run.next_seq(), 1);
        assert_eq!(run.next_seq(), 2);
    }

    #[test]
    fn run_state_serialization_roundtrip() {
        let state = RunState::Failed {
            error: "timeout".into(),
            attempts: 3,
        };
        let json = serde_json::to_string(&state).unwrap();
        let deser: RunState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, deser);
    }

    #[test]
    fn dead_letter_entry_creation() {
        let entry = DeadLetterEntry {
            run_id: RunId::new(),
            workflow_type: WorkflowType::A2ATask {
                task_id: "t".into(),
            },
            error: "max retries".into(),
            attempts: 3,
            first_attempt_at: Utc::now(),
            last_attempt_at: Utc::now(),
            last_checkpoint: None,
            total_input_tokens: 5000,
            total_output_tokens: 2000,
        };
        assert_eq!(entry.attempts, 3);
    }
}
