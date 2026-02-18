//! A2A HTTP server handlers — Axum-compatible endpoints.
//!
//! Provides the HTTP handlers for the A2A protocol that can be mounted
//! into the gateway's Axum router.
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
use crate::router::{AgentDirectory, AgentRouter};
use crate::task::{Task, TaskEvent, TaskState};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::info;

/// Shared A2A state, passed to handlers.
pub struct A2AState {
    /// This agent's card.
    pub self_card: AgentCard,
    /// Directory of known agents.
    pub directory: RwLock<AgentDirectory>,
    /// Router for capability-based delegation.
    pub router: AgentRouter,
    /// Active tasks (in-memory for now; production would use SochDB).
    pub tasks: RwLock<FxHashMap<String, Task>>,
}

impl A2AState {
    pub fn new(self_card: AgentCard) -> Self {
        Self {
            self_card,
            directory: RwLock::new(AgentDirectory::new()),
            router: AgentRouter::new(),
            tasks: RwLock::new(FxHashMap::default()),
        }
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
            // Direct targeting
            let dir = state.directory.read().await;
            if dir.get(&target).is_none() {
                return Err(format!("target agent '{}' not found in directory", target));
            }
            target
        } else {
            // Capability-based routing
            let caps = req.required_capabilities.unwrap_or_default();
            let dir = state.directory.read().await;
            match state.router.route(&dir, &caps, &[requester_id.to_string()]) {
                crate::router::RoutingDecision::Route { agent_id, .. } => agent_id,
                crate::router::RoutingDecision::NoMatch { reason, .. } => {
                    return Err(format!("no suitable agent: {}", reason));
                }
            }
        };

        // Create the task
        let mut task = Task::new(requester_id, &executor_id, req.input);
        task.skill_id = req.skill_id;

        let task_id = task.id.clone();
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

    /// GET /a2a/agents — list all known agents.
    pub async fn list_agents(state: &A2AState) -> AgentListResponse {
        let dir = state.directory.read().await;
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

        state.directory.write().await.register(card);
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

        // Register the discovered agent
        state.directory.write().await.register(card.clone());
        info!(agent = %card.id, url = %base_url, "discovered and registered agent");

        Ok(card)
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

        // Register a target agent
        let target = AgentCard::new("worker", "Worker Agent", "http://worker.local");
        state.directory.write().await.register(target);

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
        state.directory.write().await.register(target);

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
}
