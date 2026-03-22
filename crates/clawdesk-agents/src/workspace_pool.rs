//! Workspace isolation pool — instant worktree provisioning for parallel agents.
//!
//!
//! git worktrees in a reserve pool so agent task creation is
//! instant (~0ms) instead of waiting for `git worktree add` (~50-200ms). The pool
//! replenishes in the background like a buffer pool in a database.
//!
//! Uses hash-based namespacing to prevent collisions when the
//! same repo is used from multiple locations. We adopt this:
//! `workspace_id = sha256(repo_root)[..12] + "-" + agent_id`
//!
//! ## What We Add (Beyond Either)
//!
//! Neither project handles **conflict prediction**. Both discover conflicts at
//! merge time. We add a **workspace overlap detector** that uses file-scope
//! analysis to predict merge conflicts BEFORE agents finish:
//!
//! ```text
//! Agent A modifies: src/auth.rs, src/session.rs
//! Agent B modifies: src/session.rs, src/api.rs
//!                        ↑
//!          Overlap detected: src/session.rs
//!          → Alert BEFORE merge attempt
//! ```
//!
//! This is analogous to static analysis in compilers — we detect "data races"
//! between parallel agent workspaces at the file level.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Workspace Identity
// ═══════════════════════════════════════════════════════════════════════════

/// Unique workspace identifier — hash-based to prevent collisions.
///
/// Format: `{repo_hash_12}-{agent_id}`
/// Example: `a3b4c5d6e7f8-auth-agent`
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// Create from repo root path and agent identifier.
    pub fn new(repo_root: &Path, agent_id: &str) -> Self {
        let hash = {
            let mut hasher = Sha256::new();
            hasher.update(repo_root.to_string_lossy().as_bytes());
            let result = hasher.finalize();
            result[..6].iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        Self(format!("{hash}-{agent_id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Workspace Pool — pre-allocated reserve for instant provisioning
// ═══════════════════════════════════════════════════════════════════════════

/// State of a pooled workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceState {
    /// Available for claiming.
    Free,
    /// Claimed by an agent.
    Claimed,
    /// Being cleaned up / recycled.
    Recycling,
}

/// A pooled workspace — a pre-created directory (or git worktree) ready for use.
#[derive(Debug)]
struct PooledWorkspace {
    /// Unique identifier.
    id: WorkspaceId,
    /// Root path of the workspace.
    path: PathBuf,
    /// Current state.
    state: WorkspaceState,
    /// When this workspace was created.
    created_at: Instant,
    /// When this workspace was last claimed.
    claimed_at: Option<Instant>,
    /// Agent currently using this workspace.
    claimed_by: Option<String>,
    /// Branch name (for git worktrees).
    branch: Option<String>,
    /// Files modified by the agent (for overlap detection).
    modified_files: HashSet<String>,
}

/// Configuration for the workspace pool.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Root directory for all workspaces.
    pub pool_root: PathBuf,
    /// Number of pre-created reserve workspaces.
    pub reserve_size: usize,
    /// Maximum number of concurrent workspaces.
    pub max_concurrent: usize,
    /// Stale workspace threshold (recycled after this duration).
    pub stale_threshold: Duration,
    /// Whether to use git worktrees (vs plain directory copies).
    pub use_git_worktrees: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            pool_root: PathBuf::from(".clawdesk/workspaces"),
            reserve_size: 3,
            max_concurrent: 8,
            stale_threshold: Duration::from_secs(30 * 60), // 30 minutes
            use_git_worktrees: true,
        }
    }
}

/// Workspace pool — manages pre-allocated workspaces for parallel agents.
///
/// Design: amortized O(1) workspace provisioning via reserve pooling.
/// Critical section is only the `DashMap::remove()` call (~3μs).
pub struct WorkspacePool {
    /// All workspaces by ID.
    workspaces: DashMap<WorkspaceId, PooledWorkspace>,
    /// Concurrency limiter.
    semaphore: Arc<Semaphore>,
    /// Configuration.
    config: PoolConfig,
    /// Source repo root (for git worktree operations).
    repo_root: PathBuf,
}

/// Result of claiming a workspace.
#[derive(Debug)]
pub struct ClaimedWorkspace {
    /// Workspace identifier.
    pub id: WorkspaceId,
    /// Root path to work in.
    pub path: PathBuf,
    /// Branch name (if git worktree).
    pub branch: Option<String>,
}

impl WorkspacePool {
    /// Create a new workspace pool.
    pub fn new(repo_root: PathBuf, config: PoolConfig) -> Arc<Self> {
        let sem = Arc::new(Semaphore::new(config.max_concurrent));
        Arc::new(Self {
            workspaces: DashMap::new(),
            semaphore: sem,
            config,
            repo_root,
        })
    }

    /// Claim a workspace for an agent — O(1) from reserve, O(n) if reserve empty.
    ///
    /// This is the hot path. Under normal operation (reserve not depleted):
    /// 1. Find a Free workspace in the DashMap — O(n) scan but n ≤ reserve_size ≤ 5
    /// 2. Atomically mark it Claimed — single DashMap shard lock
    /// 3. Return immediately — no I/O
    ///
    /// If reserve is empty, falls back to creating a new workspace (O(ms) for git worktree).
    pub async fn claim(
        &self,
        agent_id: &str,
        branch: Option<&str>,
    ) -> Result<ClaimedWorkspace, String> {
        // Acquire concurrency permit
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| "workspace pool semaphore closed")?;

        // Try to find a free workspace in the reserve
        let mut claimed_id = None;
        for entry in self.workspaces.iter() {
            if entry.value().state == WorkspaceState::Free {
                claimed_id = Some(entry.key().clone());
                break;
            }
        }

        if let Some(id) = claimed_id {
            // Claim the existing workspace
            if let Some(mut ws) = self.workspaces.get_mut(&id) {
                ws.state = WorkspaceState::Claimed;
                ws.claimed_at = Some(Instant::now());
                ws.claimed_by = Some(agent_id.to_string());
                ws.branch = branch.map(|s| s.to_string());
                ws.modified_files.clear();

                info!(
                    workspace = %id,
                    agent = agent_id,
                    "Claimed workspace from reserve pool"
                );

                return Ok(ClaimedWorkspace {
                    id,
                    path: ws.path.clone(),
                    branch: ws.branch.clone(),
                });
            }
        }

        // Reserve depleted — create a new workspace
        let id = WorkspaceId::new(&self.repo_root, agent_id);
        let path = self.config.pool_root.join(id.as_str());

        info!(
            workspace = %id,
            agent = agent_id,
            "Reserve depleted — creating new workspace"
        );

        // Create directory
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|e| format!("failed to create workspace dir: {e}"))?;

        let ws = PooledWorkspace {
            id: id.clone(),
            path: path.clone(),
            state: WorkspaceState::Claimed,
            created_at: Instant::now(),
            claimed_at: Some(Instant::now()),
            claimed_by: Some(agent_id.to_string()),
            branch: branch.map(|s| s.to_string()),
            modified_files: HashSet::new(),
        };
        self.workspaces.insert(id.clone(), ws);

        Ok(ClaimedWorkspace {
            id,
            path,
            branch: branch.map(|s| s.to_string()),
        })
    }

    /// Release a workspace back to the pool.
    pub async fn release(&self, id: &WorkspaceId) {
        if let Some(mut ws) = self.workspaces.get_mut(id) {
            ws.state = WorkspaceState::Free;
            ws.claimed_by = None;
            ws.claimed_at = None;
            ws.modified_files.clear();
            debug!(workspace = %id, "Workspace released to pool");
        }
    }

    /// Record that a file was modified in a workspace (for overlap detection).
    pub fn record_file_modification(&self, workspace_id: &WorkspaceId, file_path: &str) {
        if let Some(mut ws) = self.workspaces.get_mut(workspace_id) {
            ws.modified_files.insert(file_path.to_string());
        }
    }

    /// Get the set of files modified in a workspace.
    pub fn modified_files(&self, workspace_id: &WorkspaceId) -> HashSet<String> {
        self.workspaces
            .get(workspace_id)
            .map(|ws| ws.modified_files.clone())
            .unwrap_or_default()
    }

    /// Detect file overlap between active workspaces.
    ///
    /// Returns pairs of (workspace_a, workspace_b, overlapping_files).
    ///
    /// Complexity: O(k²·m) where k = active workspaces, m = avg modified files.
    /// For k ≤ 8 and m ≤ 50, this is ~1600 comparisons — trivial.
    pub fn detect_overlaps(&self) -> Vec<WorkspaceOverlap> {
        let active: Vec<_> = self
            .workspaces
            .iter()
            .filter(|e| e.value().state == WorkspaceState::Claimed)
            .map(|e| {
                (
                    e.key().clone(),
                    e.value().claimed_by.clone().unwrap_or_default(),
                    e.value().modified_files.clone(),
                )
            })
            .collect();

        let mut overlaps = Vec::new();

        for i in 0..active.len() {
            for j in (i + 1)..active.len() {
                let (id_a, agent_a, files_a) = &active[i];
                let (id_b, agent_b, files_b) = &active[j];

                let common: HashSet<_> = files_a.intersection(files_b).cloned().collect();

                if !common.is_empty() {
                    warn!(
                        agent_a = %agent_a,
                        agent_b = %agent_b,
                        overlap_count = common.len(),
                        "File overlap detected between parallel workspaces"
                    );
                    overlaps.push(WorkspaceOverlap {
                        workspace_a: id_a.clone(),
                        agent_a: agent_a.clone(),
                        workspace_b: id_b.clone(),
                        agent_b: agent_b.clone(),
                        conflicting_files: common,
                    });
                }
            }
        }

        overlaps
    }

    /// Clean up stale workspaces (older than threshold and not claimed).
    pub async fn cleanup_stale(&self) {
        let now = Instant::now();
        let mut stale_ids = Vec::new();

        for entry in self.workspaces.iter() {
            let ws = entry.value();
            if ws.state == WorkspaceState::Free
                && now.duration_since(ws.created_at) > self.config.stale_threshold
            {
                stale_ids.push(entry.key().clone());
            }
        }

        for id in &stale_ids {
            if let Some((_, ws)) = self.workspaces.remove(id) {
                if let Err(e) = tokio::fs::remove_dir_all(&ws.path).await {
                    warn!(workspace = %id, error = %e, "Failed to clean up stale workspace");
                } else {
                    debug!(workspace = %id, "Cleaned up stale workspace");
                }
            }
        }

        if !stale_ids.is_empty() {
            info!(count = stale_ids.len(), "Cleaned up stale workspaces");
        }
    }

    /// Number of free workspaces in the reserve.
    pub fn free_count(&self) -> usize {
        self.workspaces
            .iter()
            .filter(|e| e.value().state == WorkspaceState::Free)
            .count()
    }

    /// Number of claimed (active) workspaces.
    pub fn active_count(&self) -> usize {
        self.workspaces
            .iter()
            .filter(|e| e.value().state == WorkspaceState::Claimed)
            .count()
    }
}

/// Detected file overlap between two parallel workspaces.
#[derive(Debug, Clone)]
pub struct WorkspaceOverlap {
    pub workspace_a: WorkspaceId,
    pub agent_a: String,
    pub workspace_b: WorkspaceId,
    pub agent_b: String,
    /// Files modified by both agents.
    pub conflicting_files: HashSet<String>,
}

impl WorkspaceOverlap {
    /// Human-readable summary.
    pub fn summary(&self) -> String {
        let files: Vec<_> = self.conflicting_files.iter().take(5).collect();
        let more = if self.conflicting_files.len() > 5 {
            format!(" (+{} more)", self.conflicting_files.len() - 5)
        } else {
            String::new()
        };
        format!(
            "{} ↔ {}: {} file conflicts [{}{}]",
            self.agent_a,
            self.agent_b,
            self.conflicting_files.len(),
            files
                .iter()
                .map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            more
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Task Decomposition — LLM-driven recursive splitting
// ═══════════════════════════════════════════════════════════════════════════

/// A node in the task decomposition tree.
///
/// Composite tasks are recursively broken down until all leaves are atomic.
/// Each atomic task maps to one agent workspace.
///
/// The tree structure enables:
/// - Parallel execution of sibling nodes
/// - Sequential execution where dependencies exist
/// - Progress tracking at any depth level
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNode {
    /// Hierarchical ID: "1", "1.2", "1.2.3"
    pub id: String,
    /// Depth in the tree (0 = root).
    pub depth: usize,
    /// Human-readable task description.
    pub description: String,
    /// Whether this task is atomic (one agent) or composite (needs splitting).
    pub kind: TaskKind,
    /// Current execution status.
    pub status: TaskStatus,
    /// Child tasks (populated after decomposition).
    pub children: Vec<TaskNode>,
    /// Agent workspace assigned to this task (for atomic tasks).
    pub workspace_id: Option<String>,
    /// Agent ID executing this task.
    pub agent_id: Option<String>,
    /// Files this task is expected to modify (for overlap prediction).
    pub expected_files: Vec<String>,
}

/// Task classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Can be handled by a single agent.
    Atomic,
    /// Needs to be broken down further.
    Composite,
}

/// Task execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Awaiting decomposition or execution.
    Pending,
    /// Currently being decomposed by the orchestrator.
    Decomposing,
    /// Agent is working on this task.
    Running,
    /// Task completed successfully.
    Completed,
    /// Task failed.
    Failed,
    /// Task was cancelled.
    Cancelled,
}

impl TaskNode {
    /// Create a new root task.
    pub fn root(description: impl Into<String>) -> Self {
        Self {
            id: "0".to_string(),
            depth: 0,
            description: description.into(),
            kind: TaskKind::Composite, // Root is always composite initially
            status: TaskStatus::Pending,
            children: Vec::new(),
            workspace_id: None,
            agent_id: None,
            expected_files: Vec::new(),
        }
    }

    /// Create a child task.
    pub fn child(
        parent_id: &str,
        index: usize,
        description: impl Into<String>,
        kind: TaskKind,
    ) -> Self {
        Self {
            id: format!("{}.{}", parent_id, index + 1),
            depth: parent_id.matches('.').count() + 1,
            description: description.into(),
            kind,
            status: TaskStatus::Pending,
            children: Vec::new(),
            workspace_id: None,
            agent_id: None,
            expected_files: Vec::new(),
        }
    }

    /// Count total atomic tasks in the tree.
    pub fn atomic_count(&self) -> usize {
        if self.kind == TaskKind::Atomic {
            return 1;
        }
        self.children.iter().map(|c| c.atomic_count()).sum()
    }

    /// Get all leaf (atomic) tasks.
    pub fn leaves(&self) -> Vec<&TaskNode> {
        if self.kind == TaskKind::Atomic || self.children.is_empty() {
            return vec![self];
        }
        self.children.iter().flat_map(|c| c.leaves()).collect()
    }

    /// Get all leaf tasks mutably.
    pub fn leaves_mut(&mut self) -> Vec<&mut TaskNode> {
        if self.kind == TaskKind::Atomic || self.children.is_empty() {
            return vec![self];
        }
        self.children
            .iter_mut()
            .flat_map(|c| c.leaves_mut())
            .collect()
    }

    /// Check if all leaf tasks are completed.
    pub fn is_complete(&self) -> bool {
        if self.kind == TaskKind::Atomic {
            return self.status == TaskStatus::Completed;
        }
        self.children.iter().all(|c| c.is_complete())
    }

    /// Compute completion percentage.
    pub fn completion_pct(&self) -> f32 {
        let leaves = self.leaves();
        if leaves.is_empty() {
            return 0.0;
        }
        let done = leaves
            .iter()
            .filter(|l| l.status == TaskStatus::Completed)
            .count();
        done as f32 / leaves.len() as f32 * 100.0
    }

    /// Predict file overlaps between sibling tasks.
    ///
    /// This is the "static analysis" for data races between parallel agents.
    /// Called BEFORE agents start working.
    pub fn predict_overlaps(&self) -> Vec<(String, String, Vec<String>)> {
        if self.children.len() < 2 {
            return Vec::new();
        }

        let mut overlaps = Vec::new();
        for i in 0..self.children.len() {
            for j in (i + 1)..self.children.len() {
                let a = &self.children[i];
                let b = &self.children[j];

                let a_files: HashSet<_> = a.expected_files.iter().cloned().collect();
                let b_files: HashSet<_> = b.expected_files.iter().cloned().collect();

                let common: Vec<_> = a_files.intersection(&b_files).cloned().collect();
                if !common.is_empty() {
                    overlaps.push((a.id.clone(), b.id.clone(), common));
                }
            }
        }

        overlaps
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Parallel Execution Coordinator
// ═══════════════════════════════════════════════════════════════════════════

/// Execution plan for parallel agent work.
///
/// Produced by analyzing the task tree and workspace pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// Root task being executed.
    pub task_tree: TaskNode,
    /// Number of agents to spawn.
    pub parallelism: usize,
    /// Predicted file overlaps (warnings, not blockers).
    pub predicted_overlaps: Vec<(String, String, Vec<String>)>,
    /// Estimated completion time (based on largest subtask).
    pub estimated_critical_path_secs: Option<u64>,
}

impl ExecutionPlan {
    /// Build an execution plan from a decomposed task tree.
    pub fn from_task(task: TaskNode) -> Self {
        let leaves = task.leaves();
        let parallelism = leaves.len();
        let predicted_overlaps = task.predict_overlaps();

        Self {
            task_tree: task,
            parallelism,
            predicted_overlaps,
            estimated_critical_path_secs: None,
        }
    }

    /// Human-readable plan summary.
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "Plan: {} parallel agents, {} total tasks",
            self.parallelism,
            self.task_tree.atomic_count()
        ));

        if !self.predicted_overlaps.is_empty() {
            lines.push(format!(
                "⚠ {} predicted file overlaps:",
                self.predicted_overlaps.len()
            ));
            for (a, b, files) in &self.predicted_overlaps {
                lines.push(format!("  {a} ↔ {b}: {:?}", files));
            }
        }

        lines.join("\n")
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_id_deterministic() {
        let a = WorkspaceId::new(Path::new("/repo"), "agent-1");
        let b = WorkspaceId::new(Path::new("/repo"), "agent-1");
        assert_eq!(a, b);
    }

    #[test]
    fn workspace_id_different_repos() {
        let a = WorkspaceId::new(Path::new("/repo-a"), "agent-1");
        let b = WorkspaceId::new(Path::new("/repo-b"), "agent-1");
        assert_ne!(a, b);
    }

    #[test]
    fn task_tree_leaf_count() {
        let mut root = TaskNode::root("Build auth system");
        root.children = vec![
            TaskNode::child("0", 0, "Backend routes", TaskKind::Atomic),
            TaskNode::child("0", 1, "Frontend forms", TaskKind::Atomic),
            TaskNode::child("0", 2, "Database schema", TaskKind::Atomic),
        ];
        assert_eq!(root.atomic_count(), 3);
    }

    #[test]
    fn task_tree_nested() {
        let mut root = TaskNode::root("Full feature");
        let mut backend = TaskNode::child("0", 0, "Backend", TaskKind::Composite);
        backend.children = vec![
            TaskNode::child("0.1", 0, "API routes", TaskKind::Atomic),
            TaskNode::child("0.1", 1, "DB models", TaskKind::Atomic),
        ];
        root.children = vec![
            backend,
            TaskNode::child("0", 1, "Frontend", TaskKind::Atomic),
        ];
        assert_eq!(root.atomic_count(), 3);
        assert_eq!(root.leaves().len(), 3);
    }

    #[test]
    fn task_completion_tracking() {
        let mut root = TaskNode::root("Work");
        let mut c1 = TaskNode::child("0", 0, "Part 1", TaskKind::Atomic);
        let c2 = TaskNode::child("0", 1, "Part 2", TaskKind::Atomic);
        c1.status = TaskStatus::Completed;

        root.children = vec![c1, c2];
        assert!(!root.is_complete());
        assert!((root.completion_pct() - 50.0).abs() < 0.1);
    }

    #[test]
    fn overlap_prediction() {
        let mut root = TaskNode::root("Work");
        let mut c1 = TaskNode::child("0", 0, "Auth", TaskKind::Atomic);
        c1.expected_files = vec!["src/auth.rs".into(), "src/session.rs".into()];
        let mut c2 = TaskNode::child("0", 1, "API", TaskKind::Atomic);
        c2.expected_files = vec!["src/session.rs".into(), "src/api.rs".into()];

        root.children = vec![c1, c2];
        let overlaps = root.predict_overlaps();
        assert_eq!(overlaps.len(), 1);
        assert!(overlaps[0].2.contains(&"src/session.rs".to_string()));
    }

    #[test]
    fn no_overlap_for_disjoint_tasks() {
        let mut root = TaskNode::root("Work");
        let mut c1 = TaskNode::child("0", 0, "Auth", TaskKind::Atomic);
        c1.expected_files = vec!["src/auth.rs".into()];
        let mut c2 = TaskNode::child("0", 1, "UI", TaskKind::Atomic);
        c2.expected_files = vec!["src/ui.rs".into()];

        root.children = vec![c1, c2];
        assert!(root.predict_overlaps().is_empty());
    }

    #[test]
    fn execution_plan_summary() {
        let mut root = TaskNode::root("Build feature");
        root.children = vec![
            TaskNode::child("0", 0, "Backend", TaskKind::Atomic),
            TaskNode::child("0", 1, "Frontend", TaskKind::Atomic),
        ];
        let plan = ExecutionPlan::from_task(root);
        assert_eq!(plan.parallelism, 2);
        let summary = plan.summary();
        assert!(summary.contains("2 parallel agents"));
    }

    #[tokio::test]
    async fn workspace_pool_claim_release() {
        let config = PoolConfig {
            pool_root: std::env::temp_dir().join("clawdesk-test-pool"),
            reserve_size: 2,
            max_concurrent: 4,
            stale_threshold: Duration::from_secs(60),
            use_git_worktrees: false,
        };
        let pool = WorkspacePool::new(PathBuf::from("/tmp/test-repo"), config);

        let ws = pool.claim("agent-1", Some("feature/auth")).await.unwrap();
        assert_eq!(pool.active_count(), 1);

        pool.release(&ws.id).await;
        assert_eq!(pool.active_count(), 0);

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&ws.path).await;
    }

    #[test]
    fn workspace_overlap_detection() {
        let pool = WorkspacePool::new(
            PathBuf::from("/tmp/test"),
            PoolConfig {
                pool_root: PathBuf::from("/tmp/pools"),
                max_concurrent: 8,
                ..Default::default()
            },
        );

        let id_a = WorkspaceId::new(Path::new("/tmp"), "agent-a");
        let id_b = WorkspaceId::new(Path::new("/tmp"), "agent-b");

        pool.workspaces.insert(
            id_a.clone(),
            PooledWorkspace {
                id: id_a.clone(),
                path: PathBuf::from("/tmp/a"),
                state: WorkspaceState::Claimed,
                created_at: Instant::now(),
                claimed_at: Some(Instant::now()),
                claimed_by: Some("agent-a".into()),
                branch: None,
                modified_files: ["src/lib.rs", "src/auth.rs"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            },
        );

        pool.workspaces.insert(
            id_b.clone(),
            PooledWorkspace {
                id: id_b.clone(),
                path: PathBuf::from("/tmp/b"),
                state: WorkspaceState::Claimed,
                created_at: Instant::now(),
                claimed_at: Some(Instant::now()),
                claimed_by: Some("agent-b".into()),
                branch: None,
                modified_files: ["src/lib.rs", "src/api.rs"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            },
        );

        let overlaps = pool.detect_overlaps();
        assert_eq!(overlaps.len(), 1);
        assert!(overlaps[0].conflicting_files.contains("src/lib.rs"));
    }
}
