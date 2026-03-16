//! # Subagent Session Control
//!
//! Hierarchical session-key authorization for subagent lifecycle control.
//!
//! Enforces that controllers can only manage their own children — preventing
//! cross-session interference between independent agent trees.
//!
//! Inspired by openclaw's `subagent-control.ts`.
//!
//! ## Session key format
//!
//! ```text
//! agent:{root_id}:subagent:{child_id}
//! ```
//!
//! A controller with session key `agent:main:subagent:parent` can manage
//! `agent:main:subagent:parent::worker::1` but NOT `agent:main:subagent:other::worker::1`.

use crate::subagent::{SubAgentHandle, SubAgentId, SubAgentState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::debug;

// ───────────────────────────────────────────────────────────────────────────
// Constants
// ───────────────────────────────────────────────────────────────────────────

/// Maximum characters in a steering message sent via session control.
pub const MAX_STEER_MESSAGE_CHARS: usize = 4000;

/// Minimum interval between steering commands to the same subagent.
pub const STEER_RATE_LIMIT: Duration = Duration::from_millis(2000);

/// How far back to look for recent subagent runs (minutes).
pub const DEFAULT_RECENT_MINUTES: u64 = 30;

// ───────────────────────────────────────────────────────────────────────────
// Types
// ───────────────────────────────────────────────────────────────────────────

/// Resolved authorization for a controller trying to manage a subagent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedController {
    /// The session key of the entity trying to control.
    pub controller_session_key: String,
    /// The session key of the caller (may differ from controller).
    pub caller_session_key: String,
    /// Whether the caller is itself a subagent.
    pub caller_is_subagent: bool,
    /// What scope of control is granted.
    pub control_scope: ControlScope,
}

/// What a controller is permitted to manage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlScope {
    /// Can manage children spawned from own session.
    Children,
    /// No control granted (e.g., trying to manage a sibling's children).
    None,
}

/// Result of an authorization check.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum ControlAuthResult {
    /// Access granted.
    #[serde(rename = "ok")]
    Ok { controller: ResolvedController },
    /// Access denied.
    #[serde(rename = "forbidden")]
    Forbidden { error: String },
}

/// A tracked subagent run for listing and control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentRunInfo {
    pub session_key: String,
    pub sub_agent_id: SubAgentId,
    pub state: SubAgentState,
    pub agent_id: String,
    pub task: String,
    pub depth: u32,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    /// Number of still-active descendant runs.
    pub pending_descendants: usize,
}

// ───────────────────────────────────────────────────────────────────────────
// Rate limiter
// ───────────────────────────────────────────────────────────────────────────

struct SteerRateEntry {
    last_steer: Instant,
}

// ───────────────────────────────────────────────────────────────────────────
// Session Controller
// ───────────────────────────────────────────────────────────────────────────

/// Manages hierarchical session-key authorization for subagent control.
///
/// Tracks active runs, enforces parent-child relationships, and rate-limits
/// steering commands.
pub struct SessionController {
    /// All tracked runs, keyed by SubAgentId.
    runs: Arc<RwLock<HashMap<String, SubagentRunInfo>>>,
    /// Parent → children index for fast descendant lookups.
    parent_index: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Rate limiting for steering commands.
    steer_rates: Arc<RwLock<HashMap<String, SteerRateEntry>>>,
}

impl SessionController {
    pub fn new() -> Self {
        Self {
            runs: Arc::new(RwLock::new(HashMap::new())),
            parent_index: Arc::new(RwLock::new(HashMap::new())),
            steer_rates: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Resolve whether a controller can manage a target subagent.
    pub async fn resolve_controller(
        &self,
        controller_session_key: &str,
        target_sub_agent_id: &str,
    ) -> ControlAuthResult {
        let caller_is_subagent = controller_session_key.contains(":subagent:");

        // Parse the target to find its parent.
        let target_id = SubAgentId(target_sub_agent_id.to_string());
        let target_parent = target_id.parent_id().map(|s| s.to_string());

        // A controller can manage a target if:
        // 1. The target was spawned by the controller's session (parent match)
        // 2. Or the controller owns an ancestor of the target.
        let is_descendant = if let Some(parent) = &target_parent {
            controller_session_key == *parent
                || target_sub_agent_id.starts_with(controller_session_key)
        } else {
            false
        };

        if is_descendant {
            ControlAuthResult::Ok {
                controller: ResolvedController {
                    controller_session_key: controller_session_key.to_string(),
                    caller_session_key: controller_session_key.to_string(),
                    caller_is_subagent,
                    control_scope: ControlScope::Children,
                },
            }
        } else {
            ControlAuthResult::Forbidden {
                error: "Subagents can only control runs spawned from their own session."
                    .to_string(),
            }
        }
    }

    /// Register a new subagent run for tracking.
    pub async fn register_run(&self, handle: &SubAgentHandle, session_key: &str) {
        let info = SubagentRunInfo {
            session_key: session_key.to_string(),
            sub_agent_id: handle.id.clone(),
            state: handle.state,
            agent_id: handle.config.agent_id.clone(),
            task: handle.config.task.clone(),
            depth: handle.depth,
            started_at: handle.started_at.clone(),
            completed_at: handle.completed_at.clone(),
            pending_descendants: 0,
        };

        let id_str = handle.id.0.clone();

        // Update parent index.
        if let Some(parent) = handle.id.parent_id() {
            let mut idx = self.parent_index.write().await;
            idx.entry(parent.to_string())
                .or_default()
                .push(id_str.clone());
        }

        let mut runs = self.runs.write().await;
        runs.insert(id_str, info);
        debug!(id = %handle.id.0, "registered subagent run for session control");
    }

    /// Update the state of a tracked run.
    pub async fn update_state(&self, sub_agent_id: &str, new_state: SubAgentState) {
        let mut runs = self.runs.write().await;
        if let Some(info) = runs.get_mut(sub_agent_id) {
            info.state = new_state;
            if new_state.is_terminal() {
                info.completed_at = Some(chrono::Utc::now().to_rfc3339());
            }
        }
    }

    /// List subagent runs controlled by the given session key.
    ///
    /// Only returns runs that are descendants of the controller's session,
    /// sorted by start time (most recent first).
    pub async fn list_controlled_runs(
        &self,
        controller_session_key: &str,
    ) -> Vec<SubagentRunInfo> {
        let runs = self.runs.read().await;
        let parent_idx = self.parent_index.read().await;

        // Collect direct children.
        let children_ids = parent_idx
            .get(controller_session_key)
            .cloned()
            .unwrap_or_default();

        // Also include deeper descendants (BFS).
        let mut all_descendant_ids = children_ids.clone();
        let mut queue = children_ids;
        while let Some(id) = queue.pop() {
            if let Some(grandchildren) = parent_idx.get(&id) {
                for gc in grandchildren {
                    all_descendant_ids.push(gc.clone());
                    queue.push(gc.clone());
                }
            }
        }

        let mut result: Vec<SubagentRunInfo> = all_descendant_ids
            .iter()
            .filter_map(|id| runs.get(id))
            .cloned()
            .collect();

        // Count pending descendants for each run.
        for info in &mut result {
            let id = &info.sub_agent_id.0;
            info.pending_descendants = self.count_active_descendants_inner(id, &runs, &parent_idx);
        }

        // Sort by start time, most recent first.
        result.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        result
    }

    /// Check if a subagent is still active (running or has pending children).
    pub async fn is_active(&self, sub_agent_id: &str) -> bool {
        let runs = self.runs.read().await;
        let parent_idx = self.parent_index.read().await;

        if let Some(info) = runs.get(sub_agent_id) {
            if !info.state.is_terminal() {
                return true;
            }
            // Even if terminal, check for pending descendants.
            self.count_active_descendants_inner(sub_agent_id, &runs, &parent_idx) > 0
        } else {
            false
        }
    }

    /// Check rate limit for steering a specific subagent.
    pub async fn check_steer_rate(&self, sub_agent_id: &str) -> Result<(), String> {
        let mut rates = self.steer_rates.write().await;
        if let Some(entry) = rates.get(sub_agent_id) {
            let elapsed = entry.last_steer.elapsed();
            if elapsed < STEER_RATE_LIMIT {
                let remaining = STEER_RATE_LIMIT - elapsed;
                return Err(format!(
                    "rate limited — wait {}ms before steering again",
                    remaining.as_millis()
                ));
            }
        }
        rates.insert(
            sub_agent_id.to_string(),
            SteerRateEntry {
                last_steer: Instant::now(),
            },
        );
        Ok(())
    }

    /// Validate a steering message.
    pub fn validate_steer_message(message: &str) -> Result<(), String> {
        if message.is_empty() {
            return Err("steering message cannot be empty".to_string());
        }
        if message.len() > MAX_STEER_MESSAGE_CHARS {
            return Err(format!(
                "steering message too long ({} chars, max {MAX_STEER_MESSAGE_CHARS})",
                message.len()
            ));
        }
        Ok(())
    }

    /// Prune terminal runs older than the cutoff.
    pub async fn prune_old_runs(&self, max_age: Duration) {
        let cutoff = chrono::Utc::now() - chrono::Duration::from_std(max_age).unwrap_or_default();
        let cutoff_str = cutoff.to_rfc3339();

        let mut runs = self.runs.write().await;
        let before = runs.len();
        runs.retain(|_, info| {
            if info.state.is_terminal() {
                info.completed_at
                    .as_ref()
                    .map(|t| t.as_str() > cutoff_str.as_str())
                    .unwrap_or(true)
            } else {
                true // Keep active runs.
            }
        });
        let pruned = before - runs.len();
        if pruned > 0 {
            debug!(pruned, "pruned old subagent runs");
        }
    }

    /// Count active (non-terminal) descendants of a given run.
    fn count_active_descendants_inner(
        &self,
        parent_id: &str,
        runs: &HashMap<String, SubagentRunInfo>,
        parent_idx: &HashMap<String, Vec<String>>,
    ) -> usize {
        let mut count = 0;
        let mut queue = vec![parent_id.to_string()];
        while let Some(id) = queue.pop() {
            if let Some(children) = parent_idx.get(&id) {
                for child in children {
                    if let Some(info) = runs.get(child.as_str()) {
                        if !info.state.is_terminal() {
                            count += 1;
                        }
                    }
                    queue.push(child.clone());
                }
            }
        }
        count
    }
}

impl Default for SessionController {
    fn default() -> Self {
        Self::new()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::SpawnConfig;

    fn make_handle(parent: &str, child: &str, seq: u64) -> SubAgentHandle {
        let id = SubAgentId::new(parent, child, seq);
        SubAgentHandle::new(
            id,
            SpawnConfig {
                agent_id: child.to_string(),
                task: "test task".to_string(),
                timeout_secs: 60,
                max_depth: 3,
                max_concurrent: 5,
                result_format: crate::subagent::ResultFormat::Text,
                announce_target: crate::subagent::AnnounceTarget::Parent,
                cleanup: crate::subagent::CleanupPolicy::Immediate,
            },
            1,
        )
    }

    #[tokio::test]
    async fn controller_can_manage_own_children() {
        let ctrl = SessionController::new();
        let handle = make_handle("agent:main", "worker", 1);
        ctrl.register_run(&handle, "agent:main").await;

        let result = ctrl
            .resolve_controller("agent:main", &handle.id.0)
            .await;
        assert!(matches!(result, ControlAuthResult::Ok { .. }));
    }

    #[tokio::test]
    async fn controller_cannot_manage_siblings_children() {
        let ctrl = SessionController::new();
        let handle = make_handle("agent:other", "worker", 1);
        ctrl.register_run(&handle, "agent:other").await;

        let result = ctrl
            .resolve_controller("agent:main", &handle.id.0)
            .await;
        assert!(matches!(result, ControlAuthResult::Forbidden { .. }));
    }

    #[tokio::test]
    async fn list_controlled_runs_returns_descendants() {
        let ctrl = SessionController::new();

        let h1 = make_handle("agent:main", "worker", 1);
        let h2 = make_handle("agent:main", "worker", 2);
        ctrl.register_run(&h1, "agent:main").await;
        ctrl.register_run(&h2, "agent:main").await;

        let runs = ctrl.list_controlled_runs("agent:main").await;
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn validate_empty_message_rejected() {
        let result = SessionController::validate_steer_message("");
        assert!(result.is_err());
    }

    #[test]
    fn validate_long_message_rejected() {
        let long = "a".repeat(MAX_STEER_MESSAGE_CHARS + 1);
        let result = SessionController::validate_steer_message(&long);
        assert!(result.is_err());
    }

    #[test]
    fn validate_normal_message_ok() {
        let result = SessionController::validate_steer_message("redirect to task B");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn rate_limit_prevents_rapid_steering() {
        let ctrl = SessionController::new();
        let id = "test::agent::1";

        // First steer should pass.
        assert!(ctrl.check_steer_rate(id).await.is_ok());

        // Immediate second steer should be rate-limited.
        assert!(ctrl.check_steer_rate(id).await.is_err());
    }

    #[tokio::test]
    async fn is_active_for_running_agent() {
        let ctrl = SessionController::new();
        let mut handle = make_handle("agent:main", "worker", 1);
        handle.start("2026-03-15T00:00:00Z");
        ctrl.register_run(&handle, "agent:main").await;

        assert!(ctrl.is_active(&handle.id.0).await);
    }

    #[tokio::test]
    async fn is_not_active_for_completed_agent() {
        let ctrl = SessionController::new();
        let mut handle = make_handle("agent:main", "worker", 1);
        handle.start("2026-03-15T00:00:00Z");
        handle.complete("done".to_string(), "2026-03-15T00:00:01Z");
        ctrl.register_run(&handle, "agent:main").await;

        assert!(!ctrl.is_active(&handle.id.0).await);
    }
}
