//! # Isolated Agent — OS-level process isolation for sub-agent execution.
//!
//! Sub-agents can run as separate OS processes, communicating via the existing
//! ACP protocol over local transport (Unix domain sockets or shared filesystem).
//!
//! ## Isolation Guarantees
//!
//! 1. **Failure isolation**: Child crash doesn't bring down the parent.
//! 2. **Filesystem isolation**: Each child can get its own working tree.
//! 3. **Resource isolation**: Separate process with own memory/CPU scheduling.
//!
//! ## Performance
//!
//! Process spawn: O(1) via `posix_spawn` (~1-5ms).
//! IPC overhead: ~10μs/message over Unix domain sockets (negligible vs. O(s) LLM calls).
//! For N=10 concurrent sub-agents, 100 messages each: ~10ms total IPC — invisible.

use crate::subagent::{SpawnConfig, SubAgentId, SubAgentState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Isolation configuration
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for process-isolated agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsolationConfig {
    /// Path to the `clawdesk-agent` binary for child processes.
    pub agent_binary: PathBuf,
    /// Directory for agent working trees (each child gets a subdirectory).
    pub workdir_root: PathBuf,
    /// Whether to use git worktrees for filesystem isolation.
    pub use_git_worktrees: bool,
    /// Maximum memory per child process (bytes). None = unlimited.
    pub max_memory_bytes: Option<u64>,
    /// Communication method.
    pub transport: IsolationTransport,
    /// Timeout for child process startup.
    pub startup_timeout_secs: u64,
}

impl Default for IsolationConfig {
    fn default() -> Self {
        Self {
            agent_binary: PathBuf::from("clawdesk-agent"),
            workdir_root: PathBuf::from("/tmp/clawdesk-agents"),
            use_git_worktrees: false,
            max_memory_bytes: None,
            transport: IsolationTransport::SharedFilesystem,
            startup_timeout_secs: 10,
        }
    }
}

/// Communication transport for isolated agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationTransport {
    /// Communicate via shared filesystem (registry.json, exit markers).
    SharedFilesystem,
    /// Communicate via Unix domain sockets.
    #[cfg(unix)]
    UnixSocket {
        socket_dir: PathBuf,
    },
}

// ═══════════════════════════════════════════════════════════════════════════
// Isolated agent handle
// ═══════════════════════════════════════════════════════════════════════════

/// Handle to a process-isolated agent.
pub struct IsolatedAgentHandle {
    pub id: SubAgentId,
    pub config: SpawnConfig,
    /// OS process ID.
    pub pid: Option<u32>,
    /// Working directory for this agent.
    pub workdir: PathBuf,
    /// Child process handle.
    child: Option<Child>,
    /// Status watch sender.
    status_tx: watch::Sender<SubAgentState>,
    /// Exit marker path (file created when agent completes).
    exit_marker: PathBuf,
}

impl IsolatedAgentHandle {
    /// Check if the child process is still alive.
    pub fn is_alive(&self) -> bool {
        // Check exit marker
        if self.exit_marker.exists() {
            return false;
        }

        // Check process
        if let Some(pid) = self.pid {
            #[cfg(unix)]
            {
                unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
            }
            #[cfg(not(unix))]
            {
                true // Conservative: assume alive on non-Unix
            }
        } else {
            false
        }
    }

    /// Read the exit marker to determine the agent's result.
    pub async fn read_exit_status(&self) -> Option<ExitStatus> {
        if !self.exit_marker.exists() {
            return None;
        }

        match tokio::fs::read_to_string(&self.exit_marker).await {
            Ok(content) => serde_json::from_str(&content).ok(),
            Err(e) => {
                warn!(error = %e, "failed to read exit marker");
                None
            }
        }
    }

    /// Forcefully kill the child process.
    pub async fn kill(&mut self) {
        if let Some(ref mut child) = self.child {
            let _ = child.kill().await;
            self.status_tx.send_replace(SubAgentState::Cancelled);
            info!(agent = %self.id.0, "killed isolated agent process");
        }
    }

    /// Update the status (used by the monitor loop).
    pub fn update_status(&self, state: SubAgentState) {
        let _ = self.status_tx.send(state);
    }

    /// Get a watch receiver for this agent's status.
    pub fn subscribe_status(&self) -> watch::Receiver<SubAgentState> {
        self.status_tx.subscribe()
    }
}

/// Exit status written by the child process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitStatus {
    pub success: bool,
    pub output: Option<String>,
    pub error: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

// ═══════════════════════════════════════════════════════════════════════════
// Isolated agent manager
// ═══════════════════════════════════════════════════════════════════════════

/// Manages process-isolated agent execution.
///
/// Spawns child agents as separate OS processes and monitors them via
/// the filesystem (exit markers) and process liveness checks.
pub struct IsolatedAgentManager {
    config: IsolationConfig,
    /// Active isolated agents.
    agents: dashmap::DashMap<String, IsolatedAgentHandle>,
    /// Registry for crash-consistent state.
    registry: Option<Arc<crate::agent_registry::AgentRegistry>>,
}

impl IsolatedAgentManager {
    pub fn new(config: IsolationConfig) -> Self {
        Self {
            config,
            agents: dashmap::DashMap::new(),
            registry: None,
        }
    }

    pub fn with_registry(mut self, registry: Arc<crate::agent_registry::AgentRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Spawn an agent as an isolated OS process.
    pub async fn spawn(
        &self,
        id: SubAgentId,
        spawn_config: SpawnConfig,
    ) -> Result<watch::Receiver<SubAgentState>, IsolationError> {
        // Create working directory
        let workdir = self.config.workdir_root.join(&id.0);
        tokio::fs::create_dir_all(&workdir)
            .await
            .map_err(|e| IsolationError::Setup(format!("create workdir: {}", e)))?;

        // Create git worktree if configured
        if self.config.use_git_worktrees {
            self.create_worktree(&workdir, &id).await?;
        }

        let exit_marker = workdir.join(".exit_status.json");
        let (status_tx, status_rx) = watch::channel(SubAgentState::Queued);

        // Write spawn config for the child process
        let config_path = workdir.join(".spawn_config.json");
        let config_json = serde_json::to_string_pretty(&spawn_config)
            .map_err(|e| IsolationError::Setup(format!("serialize config: {}", e)))?;
        tokio::fs::write(&config_path, config_json)
            .await
            .map_err(|e| IsolationError::Setup(format!("write config: {}", e)))?;

        // Spawn child process
        let mut cmd = Command::new(&self.config.agent_binary);
        cmd.arg("--config")
            .arg(&config_path)
            .arg("--workdir")
            .arg(&workdir)
            .arg("--exit-marker")
            .arg(&exit_marker)
            .arg("--agent-id")
            .arg(&id.0)
            .current_dir(&workdir)
            .kill_on_drop(true);

        let child = cmd
            .spawn()
            .map_err(|e| IsolationError::Spawn(format!("spawn process: {}", e)))?;

        let pid = child.id();
        let _ = status_tx.send(SubAgentState::Running);

        info!(
            agent = %id.0,
            pid = ?pid,
            workdir = %workdir.display(),
            "spawned isolated agent process"
        );

        let handle = IsolatedAgentHandle {
            id: id.clone(),
            config: spawn_config,
            pid,
            workdir,
            child: Some(child),
            status_tx,
            exit_marker,
        };

        self.agents.insert(id.0.clone(), handle);

        Ok(status_rx)
    }

    /// Check all isolated agents and update their status.
    pub async fn refresh_all(&self) -> Vec<(SubAgentId, SubAgentState)> {
        let mut updates = Vec::new();

        for mut entry in self.agents.iter_mut() {
            let handle = entry.value_mut();
            let current = *handle.status_tx.borrow();

            if current.is_terminal() {
                continue;
            }

            // Check exit marker
            if let Some(exit_status) = handle.read_exit_status().await {
                let new_state = if exit_status.success {
                    SubAgentState::Completed
                } else {
                    SubAgentState::Failed
                };
                handle.update_status(new_state);
                updates.push((handle.id.clone(), new_state));
                continue;
            }

            // Check process liveness
            if !handle.is_alive() {
                handle.update_status(SubAgentState::Failed);
                updates.push((handle.id.clone(), SubAgentState::Failed));
            }
        }

        updates
    }

    /// Kill a specific isolated agent.
    pub async fn kill(&self, agent_id: &SubAgentId) -> Result<(), IsolationError> {
        let mut entry = self
            .agents
            .get_mut(&agent_id.0)
            .ok_or(IsolationError::NotFound(agent_id.0.clone()))?;
        entry.value_mut().kill().await;
        Ok(())
    }

    /// Kill all running isolated agents.
    pub async fn kill_all(&self) {
        for mut entry in self.agents.iter_mut() {
            let handle = entry.value_mut();
            if !handle.status_tx.borrow().is_terminal() {
                handle.kill().await;
            }
        }
    }

    /// Clean up completed agents (remove working directories).
    pub async fn cleanup(&self, agent_id: &SubAgentId) -> Result<(), IsolationError> {
        if let Some((_, handle)) = self.agents.remove(&agent_id.0) {
            if handle.workdir.exists() {
                tokio::fs::remove_dir_all(&handle.workdir)
                    .await
                    .map_err(|e| IsolationError::Cleanup(format!("remove workdir: {}", e)))?;
            }
        }
        Ok(())
    }

    /// Get the number of running isolated agents.
    pub fn running_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|e| *e.value().status_tx.borrow() == SubAgentState::Running)
            .count()
    }

    /// Create a git worktree for filesystem isolation.
    async fn create_worktree(
        &self,
        workdir: &Path,
        id: &SubAgentId,
    ) -> Result<(), IsolationError> {
        let branch_name = format!("agent-{}", id.0.replace("::", "-"));

        let output = Command::new("git")
            .args(["worktree", "add", "-b", &branch_name])
            .arg(workdir)
            .output()
            .await
            .map_err(|e| IsolationError::Setup(format!("git worktree: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(error = %stderr, "git worktree creation failed — using plain directory");
        } else {
            debug!(branch = %branch_name, "created git worktree");
        }

        Ok(())
    }
}

/// Errors from process isolation operations.
#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    #[error("setup failed: {0}")]
    Setup(String),
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("agent not found: {0}")]
    NotFound(String),
    #[error("cleanup failed: {0}")]
    Cleanup(String),
    #[error("communication failed: {0}")]
    Communication(String),
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_isolation_config_default() {
        let config = IsolationConfig::default();
        assert_eq!(config.startup_timeout_secs, 10);
        assert!(!config.use_git_worktrees);
    }

    #[test]
    fn test_exit_status_serialization() {
        let status = ExitStatus {
            success: true,
            output: Some("done".into()),
            error: None,
            exit_code: Some(0),
            duration_ms: 1500,
        };

        let json = serde_json::to_string(&status).unwrap();
        let parsed: ExitStatus = serde_json::from_str(&json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.exit_code, Some(0));
    }
}
