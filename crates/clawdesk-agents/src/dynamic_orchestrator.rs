//! # Dynamic Orchestrator — Runtime-determined agent topology.
//!
//! Enables the parent LLM to decide *during execution* what sub-agents to
//! spawn, without requiring upfront DAG definition. Additive to the static
//! pipeline — the DAG pipeline becomes one strategy the orchestrator can employ.
//!
//! ## Tools Exposed to LLM
//!
//! - `spawn_agent(task, config) → handle_id`
//! - `check_agent(handle_id) → status`
//! - `wait_any(handle_ids, target_states) → first_match`
//! - `send_to_agent(handle_id, message)` (steering)
//!
//! ## Complexity Bounds
//!
//! Handle registry: `DashMap<HandleId, Arc<AgentHandle>>` — O(1) amortized
//! lookup/insertion. `wait_any` over N handles: O(N) per poll via
//! `tokio::select!` over watch receivers. With max_depth=d, max_concurrent=c,
//! total agents bounded by c^d (e.g., d=3, c=5 → max 125).

use crate::agent_event_stream::AgentEventStream;
use crate::agent_registry::AgentRegistry;
use crate::status_watcher::{AgentStatusWatch, StatusWatcher};
use crate::steering::{SteeringSender, SteeringMessage, SteeringSource};
use crate::subagent::{
    SpawnConfig, SubAgentHandle, SubAgentId, SubAgentState, validate_spawn,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Handle types
// ═══════════════════════════════════════════════════════════════════════════

/// Opaque handle ID for a dynamically spawned agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HandleId(pub String);

/// A managed agent handle with status watch and optional steering.
pub struct ManagedAgent {
    pub id: SubAgentId,
    pub handle_id: HandleId,
    pub config: SpawnConfig,
    pub depth: u32,
    /// Watch sender for this agent's status.
    pub status_watch: AgentStatusWatch,
    /// Steering sender for mid-execution redirection.
    pub steering: Option<SteeringSender>,
    /// Event stream for this agent's lifecycle events.
    pub event_stream: Option<AgentEventStream>,
}

/// Status information returned by `check_agent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    pub handle_id: HandleId,
    pub agent_id: String,
    pub state: SubAgentState,
    pub task: String,
    pub output: Option<String>,
    pub error: Option<String>,
}

/// Result of `wait_any`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitResult {
    pub handle_id: HandleId,
    pub agent_id: String,
    pub state: SubAgentState,
}

// ═══════════════════════════════════════════════════════════════════════════
// Dynamic orchestrator
// ═══════════════════════════════════════════════════════════════════════════

/// Dynamic orchestrator for runtime-determined agent topology.
///
/// Manages a mutable set of agent handles, exposing spawn/check/wait/steer
/// operations as tools for the parent LLM.
pub struct DynamicOrchestrator {
    /// Registry of all managed agents.
    agents: DashMap<String, Arc<ManagedAgent>>,
    /// HandleId → SubAgentId mapping.
    handle_map: DashMap<String, SubAgentId>,
    /// Monotonic sequence counter for handle IDs.
    seq: AtomicU64,
    /// Parent agent ID (for hierarchical ID construction).
    parent_id: String,
    /// Current recursion depth.
    current_depth: u32,
    /// Maximum allowed depth.
    max_depth: u32,
    /// Maximum concurrent agents.
    max_concurrent: usize,
    /// Status watcher for push-based transitions.
    status_watcher: StatusWatcher,
    /// Optional persistent registry for crash-consistency.
    registry: Option<Arc<AgentRegistry>>,
}

impl DynamicOrchestrator {
    /// Create a new dynamic orchestrator.
    pub fn new(parent_id: String, current_depth: u32) -> Self {
        Self {
            agents: DashMap::new(),
            handle_map: DashMap::new(),
            seq: AtomicU64::new(0),
            parent_id,
            current_depth,
            max_depth: 3,
            max_concurrent: 5,
            status_watcher: StatusWatcher::new(),
            registry: None,
        }
    }

    /// Set the maximum spawn depth.
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Set the maximum concurrent agents.
    pub fn with_max_concurrent(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent = max_concurrent;
        self
    }

    /// Attach a persistent registry for crash-consistent state.
    pub fn with_registry(mut self, registry: Arc<AgentRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Get the number of active (non-terminal) agents.
    pub fn active_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|e| !e.status_watch.current().is_terminal())
            .count()
    }

    /// Get the total number of managed agents (including completed).
    pub fn total_count(&self) -> usize {
        self.agents.len()
    }

    /// Subscribe to status transition events.
    pub fn subscribe_transitions(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::status_watcher::AgentStatusTransition> {
        self.status_watcher.subscribe()
    }

    // ═══════════════════════════════════════════════════════════
    // Core operations (exposed as tools to the LLM)
    // ═══════════════════════════════════════════════════════════

    /// Spawn a new agent with the given configuration.
    ///
    /// Returns a `HandleId` that can be used to check/wait/steer the agent.
    /// The caller is responsible for actually executing the agent (this method
    /// only registers it and returns the handle).
    pub async fn spawn_agent(
        &self,
        config: SpawnConfig,
    ) -> Result<(HandleId, watch::Receiver<SubAgentState>), SpawnError> {
        // Validate
        let errors = validate_spawn(&config, self.current_depth, self.active_count());
        if !errors.is_empty() {
            return Err(SpawnError::ValidationFailed(errors.join("; ")));
        }

        if self.current_depth >= self.max_depth {
            return Err(SpawnError::DepthExceeded {
                current: self.current_depth,
                max: self.max_depth,
            });
        }

        if self.active_count() >= self.max_concurrent {
            return Err(SpawnError::ConcurrencyExceeded {
                current: self.active_count(),
                max: self.max_concurrent,
            });
        }

        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let handle_id = HandleId(format!("h-{}-{}", self.parent_id, seq));
        let sub_agent_id = SubAgentId::new(&self.parent_id, &config.agent_id, seq);

        let (status_watch, status_rx) =
            AgentStatusWatch::new(SubAgentState::Queued);

        // Register with status watcher
        self.status_watcher
            .register(&sub_agent_id, status_rx.clone(), SubAgentState::Queued);

        let managed = Arc::new(ManagedAgent {
            id: sub_agent_id.clone(),
            handle_id: handle_id.clone(),
            config: config.clone(),
            depth: self.current_depth + 1,
            status_watch,
            steering: None,
            event_stream: None,
        });

        self.agents.insert(handle_id.0.clone(), managed);
        self.handle_map
            .insert(handle_id.0.clone(), sub_agent_id.clone());

        // Persist to registry if available
        if let Some(ref registry) = self.registry {
            let handle = SubAgentHandle::new(sub_agent_id, config, self.current_depth + 1);
            if let Err(e) = registry.register(handle).await {
                warn!(error = %e, "failed to persist agent to registry");
            }
        }

        info!(
            handle = %handle_id.0,
            depth = self.current_depth + 1,
            "spawned agent"
        );

        Ok((handle_id, status_rx))
    }

    /// Check the status of a spawned agent.
    pub fn check_agent(&self, handle_id: &HandleId) -> Option<AgentStatus> {
        self.agents.get(&handle_id.0).map(|managed| {
            let state = managed.status_watch.current();
            AgentStatus {
                handle_id: handle_id.clone(),
                agent_id: managed.id.0.clone(),
                state,
                task: managed.config.task.clone(),
                output: None, // Would be populated from registry
                error: None,
            }
        })
    }

    /// Wait until any of the given agents reaches one of the target states.
    pub async fn wait_any(
        &self,
        handle_ids: &[HandleId],
        target_states: &[SubAgentState],
    ) -> Option<WaitResult> {
        let agent_ids: Vec<SubAgentId> = handle_ids
            .iter()
            .filter_map(|h| self.handle_map.get(&h.0).map(|e| e.value().clone()))
            .collect();

        let result = self
            .status_watcher
            .wait_any(&agent_ids, target_states)
            .await;

        result.map(|(id, state)| {
            let handle_id = self
                .agents
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.handle_id.clone())
                .unwrap_or(HandleId(id.0.clone()));

            WaitResult {
                handle_id,
                agent_id: id.0,
                state,
            }
        })
    }

    /// Send a steering message to a running agent.
    pub async fn send_to_agent(
        &self,
        handle_id: &HandleId,
        content: String,
    ) -> Result<(), SpawnError> {
        let managed = self
            .agents
            .get(&handle_id.0)
            .ok_or(SpawnError::NotFound(handle_id.0.clone()))?;

        if let Some(ref steering) = managed.steering {
            steering
                .steer(SteeringMessage {
                    content,
                    source: SteeringSource::ParentAgent {
                        agent_id: self.parent_id.clone(),
                    },
                })
                .await
                .map_err(|e| SpawnError::SteeringFailed(e.to_string()))?;
            Ok(())
        } else {
            Err(SpawnError::NoSteeringChannel(handle_id.0.clone()))
        }
    }

    /// Update an agent's status (called by the execution backend).
    pub fn update_status(&self, handle_id: &HandleId, new_state: SubAgentState) {
        if let Some(managed) = self.agents.get(&handle_id.0) {
            managed.status_watch.update(new_state);
            debug!(handle = %handle_id.0, state = ?new_state, "agent status updated");
        }
    }

    /// Get all handle IDs.
    pub fn all_handles(&self) -> Vec<HandleId> {
        self.agents
            .iter()
            .map(|e| HandleId(e.key().clone()))
            .collect()
    }

    /// Get handles filtered by state.
    pub fn handles_in_state(&self, state: SubAgentState) -> Vec<HandleId> {
        self.agents
            .iter()
            .filter(|e| e.status_watch.current() == state)
            .map(|e| HandleId(e.key().clone()))
            .collect()
    }

    /// Clean up finished agents.
    pub fn gc(&self) -> usize {
        let terminal: Vec<String> = self
            .agents
            .iter()
            .filter(|e| e.status_watch.current().is_terminal())
            .map(|e| e.key().clone())
            .collect();

        let count = terminal.len();
        for key in terminal {
            self.handle_map.remove(&key);
            if let Some((_, managed)) = self.agents.remove(&key) {
                self.status_watcher.unregister(&managed.id);
            }
        }

        if count > 0 {
            debug!(removed = count, "orchestrator GC complete");
        }
        count
    }
}

/// Errors from dynamic orchestration operations.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("spawn validation failed: {0}")]
    ValidationFailed(String),
    #[error("max depth exceeded: current={current}, max={max}")]
    DepthExceeded { current: u32, max: u32 },
    #[error("max concurrent exceeded: current={current}, max={max}")]
    ConcurrencyExceeded { current: usize, max: usize },
    #[error("agent not found: {0}")]
    NotFound(String),
    #[error("no steering channel for agent: {0}")]
    NoSteeringChannel(String),
    #[error("steering failed: {0}")]
    SteeringFailed(String),
    #[error("registry error: {0}")]
    RegistryError(String),
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(task: &str) -> SpawnConfig {
        SpawnConfig {
            agent_id: "test-agent".into(),
            task: task.into(),
            timeout_secs: 60,
            max_depth: 3,
            max_concurrent: 5,
            result_format: crate::subagent::ResultFormat::Text,
            announce_target: crate::subagent::AnnounceTarget::Parent,
            cleanup: crate::subagent::CleanupPolicy::Immediate,
        }
    }

    #[tokio::test]
    async fn test_spawn_agent() {
        let orch = DynamicOrchestrator::new("parent".into(), 0);
        let (handle_id, _rx) = orch.spawn_agent(make_config("do work")).await.unwrap();

        assert!(handle_id.0.contains("parent"));
        assert_eq!(orch.total_count(), 1);
        assert_eq!(orch.active_count(), 1);
    }

    #[tokio::test]
    async fn test_check_agent() {
        let orch = DynamicOrchestrator::new("parent".into(), 0);
        let (handle_id, _rx) = orch.spawn_agent(make_config("task")).await.unwrap();

        let status = orch.check_agent(&handle_id).unwrap();
        assert_eq!(status.state, SubAgentState::Queued);
        assert_eq!(status.task, "task");
    }

    #[tokio::test]
    async fn test_update_status() {
        let orch = DynamicOrchestrator::new("parent".into(), 0);
        let (handle_id, mut rx) = orch.spawn_agent(make_config("task")).await.unwrap();

        orch.update_status(&handle_id, SubAgentState::Running);

        let status = orch.check_agent(&handle_id).unwrap();
        assert_eq!(status.state, SubAgentState::Running);
    }

    #[tokio::test]
    async fn test_depth_limit() {
        let orch = DynamicOrchestrator::new("parent".into(), 3)
            .with_max_depth(3);

        let result = orch.spawn_agent(make_config("deep")).await;
        assert!(result.is_err());
        // validate_spawn catches this as ValidationFailed (depth check in config)
        // or DepthExceeded (orchestrator-level check) — both are correct
        match result.unwrap_err() {
            SpawnError::DepthExceeded { .. } | SpawnError::ValidationFailed(_) => {}
            other => panic!("expected depth error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_concurrency_limit() {
        let orch = DynamicOrchestrator::new("parent".into(), 0)
            .with_max_concurrent(2);

        orch.spawn_agent(make_config("task 1")).await.unwrap();
        orch.spawn_agent(make_config("task 2")).await.unwrap();

        let result = orch.spawn_agent(make_config("task 3")).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SpawnError::ConcurrencyExceeded { .. }
        ));
    }

    #[tokio::test]
    async fn test_gc_removes_terminal() {
        let orch = DynamicOrchestrator::new("parent".into(), 0);
        let (h1, _) = orch.spawn_agent(make_config("task 1")).await.unwrap();
        let (h2, _) = orch.spawn_agent(make_config("task 2")).await.unwrap();

        orch.update_status(&h1, SubAgentState::Completed);

        let removed = orch.gc();
        assert_eq!(removed, 1);
        assert_eq!(orch.total_count(), 1);
    }

    #[tokio::test]
    async fn test_handles_in_state() {
        let orch = DynamicOrchestrator::new("parent".into(), 0);
        let (h1, _) = orch.spawn_agent(make_config("task 1")).await.unwrap();
        let (h2, _) = orch.spawn_agent(make_config("task 2")).await.unwrap();

        orch.update_status(&h1, SubAgentState::Running);

        let running = orch.handles_in_state(SubAgentState::Running);
        assert_eq!(running.len(), 1);

        let queued = orch.handles_in_state(SubAgentState::Queued);
        assert_eq!(queued.len(), 1);
    }
}
