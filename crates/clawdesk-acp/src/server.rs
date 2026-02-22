//! A2A HTTP server handlers — Axum-compatible endpoints.
//!
//! Provides the HTTP handlers for the A2A protocol that can be mounted
//! into the gateway's Axum router.
//!
//! ## Concurrency model
//!
//! The agent **directory** uses Read-Copy-Update (RCU) via `ArcSwap`:
//! readers get a lock-free snapshot (no `await`), writers clone-modify-swap
//! serialized by a lightweight `Mutex`.  This eliminates reader contention
//! on the hot path (every `send_task` / `list_agents` call).
//!
//! ## Endpoints
//!
//! | Method | Path                     | Description                    |
//! |--------|--------------------------|--------------------------------|
//! | GET    | /.well-known/agent.json  | Serve Agent Card (discovery)   |
//! | POST   | /a2a/tasks/send          | Create a new task              |
//! | GET    | /a2a/tasks/:id           | Get task status                |
//! | POST   | /a2a/tasks/:id/cancel    | Cancel a task                  |
//! | POST   | /a2a/tasks/:id/input     | Provide input for a task       |
//! | GET    | /a2a/agents              | List known agents (directory)  |
//! | POST   | /a2a/agents/register     | Register an external agent     |

use crate::agent_card::AgentCard;
use crate::policy::{PolicyDecision, PolicyEngine};
use crate::router::{AgentDirectory, AgentRouter};
use crate::session_router::AgentSource;
use crate::task::{Task, TaskEvent, TaskState};
use arc_swap::ArcSwap;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{Mutex as TokioMutex, RwLock};
use tracing::info;

/// Shared A2A state, passed to handlers.
pub struct A2AState {
    /// This agent's card.
    pub self_card: AgentCard,
    /// Directory of known agents (RCU — readers never block).
    pub directory: ArcSwap<AgentDirectory>,
    /// Router for capability-based delegation.
    pub router: AgentRouter,
    /// Active tasks (in-memory for now; production would use SochDB).
    pub tasks: RwLock<FxHashMap<String, Task>>,
    /// A2A policy engine (controls delegation permissions and rate limits).
    pub policy: TokioMutex<PolicyEngine>,
    /// Agent source registry: agent_id → AgentSource.
    pub agent_sources: RwLock<FxHashMap<String, AgentSource>>,
    /// Serializes directory writes (readers remain lock-free).
    directory_mu: tokio::sync::Mutex<()>,
}

impl A2AState {
    pub fn new(self_card: AgentCard) -> Self {
        Self {
            self_card,
            directory: ArcSwap::from_pointee(AgentDirectory::new()),
            router: AgentRouter::new(),
            tasks: RwLock::new(FxHashMap::default()),
            policy: TokioMutex::new(PolicyEngine::permissive()),
            agent_sources: RwLock::new(FxHashMap::default()),
            directory_mu: tokio::sync::Mutex::new(()),
        }
    }

    /// Create with a specific policy configuration.
    pub fn with_policy(self_card: AgentCard, policy: crate::policy::A2APolicy) -> Self {
        Self {
            self_card,
            directory: ArcSwap::from_pointee(AgentDirectory::new()),
            router: AgentRouter::new(),
            tasks: RwLock::new(FxHashMap::default()),
            policy: TokioMutex::new(PolicyEngine::new(policy)),
            agent_sources: RwLock::new(FxHashMap::default()),
            directory_mu: tokio::sync::Mutex::new(()),
        }
    }

    /// RCU write: clone the current directory, apply a mutation, swap in the
    /// new version.  Serialized by `directory_mu` so concurrent writers see
    /// each other's changes; readers never block.
    pub async fn modify_directory<F: FnOnce(&mut AgentDirectory)>(&self, f: F) {
        let _guard = self.directory_mu.lock().await;
        let mut new_dir = (**self.directory.load()).clone();
        f(&mut new_dir);
        self.directory.store(Arc::new(new_dir));
    }
}

/// Handler: `A2AHandler` contains all the A2A protocol endpoints.
///
/// These are pure functions over `A2AState` that can be mounted as
/// Axum handlers in the gateway's router.
pub struct A2AHandler;

// ─── Request / Response types ────────────────────────────────────────

/// Request to create a task.
#[derive(Debug, Deserialize)]
pub struct TaskSendRequest {
    pub skill_id: Option<String>,
    pub input: serde_json::Value,
    /// Optionally specify which agent should handle this.
    pub target_agent: Option<String>,
    /// Required capabilities (for routing if no target specified).
    pub required_capabilities: Option<Vec<crate::agent_card::AgentCapability>>,
}

/// Response for task operations.
#[derive(Debug, Serialize)]
pub struct TaskResponse {
    pub task_id: String,
    pub state: TaskState,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub progress: f64,
    pub artifacts: Vec<crate::message::Artifact>,
}

/// Request to provide input for a task.
#[derive(Debug, Deserialize)]
pub struct TaskInputRequest {
    pub input: serde_json::Value,
}

/// Agent list response.
#[derive(Debug, Serialize)]
pub struct AgentListResponse {
    pub agents: Vec<AgentSummary>,
}

/// Summary of an agent for listing.
#[derive(Debug, Serialize)]
pub struct AgentSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub capabilities: Vec<crate::agent_card::AgentCapability>,
    pub skill_count: usize,
    pub is_healthy: bool,
    pub active_tasks: u32,
}

// ─── Handler implementations ────────────────────────────────────────

impl A2AHandler {
    /// GET /.well-known/agent.json — serve this agent's card.
    pub async fn agent_card(state: &A2AState) -> AgentCard {
        state.self_card.clone()
    }

    /// POST /a2a/tasks/send — create and dispatch a new task.
    pub async fn send_task(
        state: &A2AState,
        requester_id: &str,
        req: TaskSendRequest,
    ) -> Result<TaskResponse, String> {
        // Determine the executor agent
        let executor_id = if let Some(target) = req.target_agent {
            // Direct targeting — lock-free snapshot via ArcSwap
            let dir = state.directory.load();
            if dir.get(&target).is_none() {
                return Err(format!("target agent '{}' not found in directory", target));
            }
            target
        } else {
            // Capability-based routing — lock-free snapshot
            let caps = req.required_capabilities.unwrap_or_default();
            let dir = state.directory.load();
            match state.router.route(&dir, &caps, &[requester_id.to_string()]) {
                crate::router::RoutingDecision::Route { agent_id, .. } => agent_id,
                crate::router::RoutingDecision::NoMatch { reason, .. } => {
                    return Err(format!("no suitable agent: {}", reason));
                }
            }
        };

        // ── Policy enforcement ───────────────────────────────────────────
        {
            let source = state.agent_sources.read().await.get(&executor_id).cloned();
            let mut policy = state.policy.lock().await;
            let decision = policy.evaluate(
                requester_id,
                &executor_id,
                req.skill_id.as_deref(),
                source.as_ref(),
            );
            match decision {
                PolicyDecision::Allow => {} // proceed
                PolicyDecision::Deny { reason } => {
                    return Err(format!("policy denied: {}", reason));
                }
                PolicyDecision::RateLimited { retry_after_secs } => {
                    return Err(format!(
                        "rate limited: retry after {} seconds",
                        retry_after_secs
                    ));
                }
            }
        }

        // Create the task
        let mut task = Task::new(requester_id, &executor_id, req.input);
        task.skill_id = req.skill_id;

        let task_id = task.id.clone();
        // Save values needed for RPC dispatch before the task is moved
        let dispatch_input = task.input.clone();
        let dispatch_skill_id = task.skill_id.clone();
        info!(
            task = %task_id,
            executor = %executor_id,
            "created A2A task"
        );

        // Store the task
        let response = TaskResponse {
            task_id: task_id.as_str().to_string(),
            state: task.state,
            output: task.output.clone(),
            error: task.error.clone(),
            progress: task.progress,
            artifacts: task.artifacts.clone(),
        };

        state
            .tasks
            .write()
            .await
            .insert(task_id.as_str().to_string(), task);

        // In a real implementation, we'd now dispatch the task to the executor
        // via HTTP POST to executor_id's A2A endpoint. For now, the task
        // is stored and can be polled.

        // ── RPC dispatch (async, fire-and-forget) ─────────────────────────
        // If the executor is a remote agent (has a known endpoint in the
        // directory), spawn an async task to POST to their /a2a/tasks/send.
        {
            let dir = state.directory.load();
            if let Some(entry) = dir.get(&executor_id) {
                let endpoint_url = entry.card.endpoint.url.clone();
                let task_id_clone = response.task_id.clone();
                let input = dispatch_input;
                let skill_id = dispatch_skill_id;
                let self_id = state.self_card.id.clone();

                // Don't dispatch to ourselves
                if executor_id != self_id {
                    tokio::spawn(async move {
                        let client = reqwest::Client::new();
                        let send_url = format!(
                            "{}/a2a/tasks/send",
                            endpoint_url.trim_end_matches('/')
                        );

                        let dispatch_body = serde_json::json!({
                            "input": input,
                            "skill_id": skill_id,
                            "requester_id": self_id,
                        });

                        match client
                            .post(&send_url)
                            .json(&dispatch_body)
                            .timeout(std::time::Duration::from_secs(30))
                            .send()
                            .await
                        {
                            Ok(resp) if resp.status().is_success() => {
                                info!(
                                    task = %task_id_clone,
                                    executor = %endpoint_url,
                                    "dispatched task to remote agent"
                                );
                            }
                            Ok(resp) => {
                                tracing::warn!(
                                    task = %task_id_clone,
                                    status = %resp.status(),
                                    "remote agent rejected task"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    task = %task_id_clone,
                                    error = %e,
                                    "failed to dispatch task to remote agent"
                                );
                            }
                        }
                    });
                }
            }
        }

        Ok(response)
    }

    /// GET /a2a/tasks/:id — get task status.
    pub async fn get_task(state: &A2AState, task_id: &str) -> Result<TaskResponse, String> {
        let tasks = state.tasks.read().await;
        let task = tasks
            .get(task_id)
            .ok_or_else(|| format!("task '{}' not found", task_id))?;

        Ok(TaskResponse {
            task_id: task.id.as_str().to_string(),
            state: task.state,
            output: task.output.clone(),
            error: task.error.clone(),
            progress: task.progress,
            artifacts: task.artifacts.clone(),
        })
    }

    /// POST /a2a/tasks/:id/cancel — cancel a task.
    pub async fn cancel_task(
        state: &A2AState,
        task_id: &str,
        reason: Option<String>,
    ) -> Result<TaskResponse, String> {
        let mut tasks = state.tasks.write().await;
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task '{}' not found", task_id))?;

        task.apply_event(TaskEvent::Cancel { reason })?;

        Ok(TaskResponse {
            task_id: task.id.as_str().to_string(),
            state: task.state,
            output: task.output.clone(),
            error: task.error.clone(),
            progress: task.progress,
            artifacts: task.artifacts.clone(),
        })
    }

    /// POST /a2a/tasks/:id/input — provide input for a task in InputRequired state.
    pub async fn provide_input(
        state: &A2AState,
        task_id: &str,
        input: serde_json::Value,
    ) -> Result<TaskResponse, String> {
        let mut tasks = state.tasks.write().await;
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task '{}' not found", task_id))?;

        task.apply_event(TaskEvent::ProvideInput { input })?;

        Ok(TaskResponse {
            task_id: task.id.as_str().to_string(),
            state: task.state,
            output: task.output.clone(),
            error: task.error.clone(),
            progress: task.progress,
            artifacts: task.artifacts.clone(),
        })
    }

    /// GET /a2a/agents — list all known agents (lock-free snapshot).
    pub async fn list_agents(state: &A2AState) -> AgentListResponse {
        let dir = state.directory.load();
        let agents = dir
            .agents
            .iter()
            .map(|(_, entry)| AgentSummary {
                id: entry.card.id.clone(),
                name: entry.card.name.clone(),
                description: entry.card.description.clone(),
                capabilities: entry.card.capabilities.clone(),
                skill_count: entry.card.skills.len(),
                is_healthy: entry.is_healthy,
                active_tasks: entry.active_tasks,
            })
            .collect();

        AgentListResponse { agents }
    }

    /// POST /a2a/agents/register — register an external agent.
    pub async fn register_agent(
        state: &A2AState,
        card: AgentCard,
    ) -> Result<AgentSummary, String> {
        let summary = AgentSummary {
            id: card.id.clone(),
            name: card.name.clone(),
            description: card.description.clone(),
            capabilities: card.capabilities.clone(),
            skill_count: card.skills.len(),
            is_healthy: true,
            active_tasks: 0,
        };

        state.modify_directory(|dir| dir.register(card)).await;
        Ok(summary)
    }

    /// Discover an agent by fetching its card from a URL.
    pub async fn discover_agent(
        state: &A2AState,
        base_url: &str,
    ) -> Result<AgentCard, String> {
        let url = format!("{}/.well-known/agent.json", base_url.trim_end_matches('/'));

        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("failed to discover agent at {}: {}", url, e))?;

        if !response.status().is_success() {
            return Err(format!(
                "agent discovery failed: HTTP {}",
                response.status()
            ));
        }

        let card: AgentCard = response
            .json()
            .await
            .map_err(|e| format!("invalid agent card: {}", e))?;

        // Register the discovered agent (RCU swap)
        state.modify_directory(|dir| dir.register(card.clone())).await;
        info!(agent = %card.id, url = %base_url, "discovered and registered agent");

        Ok(card)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tool Permission Gate — classifies tools for A2A delegation safety.
// ═══════════════════════════════════════════════════════════════════════════

/// Tool safety classification for A2A delegation.
///
/// When Agent A delegates to Agent B, the permission gate determines which
/// of Agent B's tools can run without user confirmation.
///
/// ## Decision tree
///
/// ```text
/// tool_name ─→ SAFE_TOOLS set?
///   └─ yes → auto-approve
///   └─ no  → DANGEROUS_TOOLS set?
///       └─ yes → require user confirmation (or deny if unattended)
///       └─ no  → default_policy (approve or deny based on config)
/// ```
#[derive(Debug, Clone)]
pub struct ToolPermissionGate {
    /// Tools that are always safe to execute without confirmation.
    /// Read-only, search, informational tools.
    pub safe_tools: Vec<String>,
    /// Tools that require explicit user confirmation.
    /// File writes, shell exec, external API calls, message sends.
    pub dangerous_tools: Vec<String>,
    /// Default policy for tools not in either set.
    pub default_policy: ToolPermissionDefault,
}

/// What to do with tools that aren't classified as safe or dangerous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPermissionDefault {
    /// Auto-approve unknown tools (permissive).
    Allow,
    /// Require confirmation for unknown tools (conservative).
    RequireConfirmation,
    /// Deny unknown tools (strict).
    Deny,
}

/// Result of tool permission evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPermission {
    /// Tool is safe — proceed without confirmation.
    AutoApprove,
    /// Tool needs user confirmation before execution.
    RequireConfirmation { reason: String },
    /// Tool is denied in this context.
    Denied { reason: String },
}

impl ToolPermissionGate {
    /// Create a gate with sensible defaults for ClawDesk built-in tools.
    pub fn default_clawdesk() -> Self {
        Self {
            safe_tools: vec![
                "memory_search".into(),
                "memory_store".into(),
                "agents_list".into(),
                "web_search".into(),
                "file_read".into(),
                "file_list".into(),
            ],
            dangerous_tools: vec![
                "shell".into(),
                "file_write".into(),
                "http".into(),
                "message_send".into(),
                "sessions_send".into(),
                "spawn_subagent".into(),
            ],
            default_policy: ToolPermissionDefault::RequireConfirmation,
        }
    }

    /// Create a fully permissive gate (all tools auto-approved).
    pub fn permissive() -> Self {
        Self {
            safe_tools: vec![],
            dangerous_tools: vec![],
            default_policy: ToolPermissionDefault::Allow,
        }
    }

    /// Create a strict gate (only safe tools auto-approved, all others denied).
    pub fn strict() -> Self {
        Self {
            safe_tools: vec![
                "memory_search".into(),
                "memory_store".into(),
                "agents_list".into(),
                "file_read".into(),
                "file_list".into(),
            ],
            dangerous_tools: vec![],
            default_policy: ToolPermissionDefault::Deny,
        }
    }

    /// Evaluate whether a tool should be allowed in an A2A context.
    ///
    /// `requesting_agent` is the agent that wants to use the tool.
    /// `tool_name` is the name of the tool being requested.
    pub fn evaluate(&self, tool_name: &str, requesting_agent: &str) -> ToolPermission {
        // Check safe list first
        if self.safe_tools.iter().any(|t| t.eq_ignore_ascii_case(tool_name)) {
            return ToolPermission::AutoApprove;
        }

        // Check dangerous list
        if self.dangerous_tools.iter().any(|t| t.eq_ignore_ascii_case(tool_name)) {
            return ToolPermission::RequireConfirmation {
                reason: format!(
                    "tool '{}' requested by agent '{}' requires confirmation (classified as dangerous)",
                    tool_name, requesting_agent
                ),
            };
        }

        // Default policy for unclassified tools
        match self.default_policy {
            ToolPermissionDefault::Allow => ToolPermission::AutoApprove,
            ToolPermissionDefault::RequireConfirmation => {
                ToolPermission::RequireConfirmation {
                    reason: format!(
                        "tool '{}' requested by agent '{}' is unclassified — confirmation required",
                        tool_name, requesting_agent
                    ),
                }
            }
            ToolPermissionDefault::Deny => ToolPermission::Denied {
                reason: format!(
                    "tool '{}' is not in the safe tools list — denied by strict policy",
                    tool_name
                ),
            },
        }
    }

    /// Batch-evaluate a set of tools. Returns a map of tool_name → permission.
    pub fn evaluate_batch(
        &self,
        tool_names: &[&str],
        requesting_agent: &str,
    ) -> Vec<(String, ToolPermission)> {
        tool_names
            .iter()
            .map(|name| {
                (name.to_string(), self.evaluate(name, requesting_agent))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> A2AState {
        let card = AgentCard::new("self", "ClawDesk", "http://localhost:18789");
        A2AState::new(card)
    }

    #[tokio::test]
    async fn send_and_get_task() {
        let state = test_state();

        // Register a target agent (RCU)
        let target = AgentCard::new("worker", "Worker Agent", "http://worker.local");
        state.modify_directory(|dir| dir.register(target)).await;

        // Send a task
        let req = TaskSendRequest {
            skill_id: Some("code-review".into()),
            input: serde_json::json!({"file": "main.rs"}),
            target_agent: Some("worker".into()),
            required_capabilities: None,
        };

        let result = A2AHandler::send_task(&state, "self", req).await.unwrap();
        assert_eq!(result.state, TaskState::Submitted);

        // Get the task
        let fetched = A2AHandler::get_task(&state, &result.task_id).await.unwrap();
        assert_eq!(fetched.state, TaskState::Submitted);
    }

    #[tokio::test]
    async fn cancel_task() {
        let state = test_state();
        let target = AgentCard::new("worker", "Worker", "http://w.local");
        state.modify_directory(|dir| dir.register(target)).await;

        let req = TaskSendRequest {
            skill_id: None,
            input: serde_json::json!({}),
            target_agent: Some("worker".into()),
            required_capabilities: None,
        };

        let result = A2AHandler::send_task(&state, "self", req).await.unwrap();
        let canceled = A2AHandler::cancel_task(&state, &result.task_id, Some("test".into()))
            .await
            .unwrap();
        assert_eq!(canceled.state, TaskState::Canceled);
    }

    #[test]
    fn tool_permission_gate_safe_tools() {
        let gate = ToolPermissionGate::default_clawdesk();
        assert_eq!(
            gate.evaluate("memory_search", "agent-a"),
            ToolPermission::AutoApprove
        );
        assert_eq!(
            gate.evaluate("file_read", "agent-a"),
            ToolPermission::AutoApprove
        );
    }

    #[test]
    fn tool_permission_gate_dangerous_tools() {
        let gate = ToolPermissionGate::default_clawdesk();
        assert!(matches!(
            gate.evaluate("shell", "agent-a"),
            ToolPermission::RequireConfirmation { .. }
        ));
        assert!(matches!(
            gate.evaluate("file_write", "agent-a"),
            ToolPermission::RequireConfirmation { .. }
        ));
    }

    #[test]
    fn tool_permission_gate_unclassified() {
        let gate = ToolPermissionGate::default_clawdesk();
        // Default policy is RequireConfirmation for unclassified
        assert!(matches!(
            gate.evaluate("unknown_tool", "agent-a"),
            ToolPermission::RequireConfirmation { .. }
        ));
    }

    #[test]
    fn tool_permission_gate_strict() {
        let gate = ToolPermissionGate::strict();
        assert_eq!(
            gate.evaluate("memory_search", "agent-a"),
            ToolPermission::AutoApprove
        );
        assert!(matches!(
            gate.evaluate("shell", "agent-a"),
            ToolPermission::Denied { .. }
        ));
        assert!(matches!(
            gate.evaluate("unknown_tool", "agent-a"),
            ToolPermission::Denied { .. }
        ));
    }

    #[test]
    fn tool_permission_gate_permissive() {
        let gate = ToolPermissionGate::permissive();
        assert_eq!(
            gate.evaluate("shell", "agent-a"),
            ToolPermission::AutoApprove
        );
        assert_eq!(
            gate.evaluate("unknown_tool", "agent-a"),
            ToolPermission::AutoApprove
        );
    }

    #[test]
    fn tool_permission_gate_batch() {
        let gate = ToolPermissionGate::default_clawdesk();
        let results = gate.evaluate_batch(
            &["memory_search", "shell", "unknown_tool"],
            "agent-a"
        );
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].1, ToolPermission::AutoApprove);
        assert!(matches!(results[1].1, ToolPermission::RequireConfirmation { .. }));
        assert!(matches!(results[2].1, ToolPermission::RequireConfirmation { .. }));
    }

    #[test]
    fn tool_permission_case_insensitive() {
        let gate = ToolPermissionGate::default_clawdesk();
        assert_eq!(
            gate.evaluate("Memory_Search", "agent-a"),
            ToolPermission::AutoApprove
        );
        assert!(matches!(
            gate.evaluate("SHELL", "agent-a"),
            ToolPermission::RequireConfirmation { .. }
        ));
    }
}
