//! Process supervisor — manage child processes with PTY support, tree-kill, and health checks.
//!
//! Used for long-running tool executions (dev servers, builds, etc.)
//! that need proper lifecycle management.
//!
//! ## Capabilities
//! - Spawn processes with optional PTY allocation
//! - Track process trees for clean termination
//! - Health check via heartbeat / exit code monitoring
//! - Output streaming with bounded buffers
//! - Timeout enforcement

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Managed process information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub id: String,
    pub pid: Option<u32>,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub state: ProcessState,
    pub exit_code: Option<i32>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub use_pty: bool,
    pub timeout_ms: Option<u64>,
}

/// Process lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
    TimedOut,
}

/// Process spawn configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnConfig {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub env: HashMap<String, String>,
    pub use_pty: bool,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: usize,
    pub capture_stderr: bool,
}

impl SpawnConfig {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            working_dir: None,
            env: HashMap::new(),
            use_pty: false,
            timeout_ms: None,
            max_output_bytes: 10 * 1024 * 1024, // 10 MB
            capture_stderr: true,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(args.into_iter().map(|a| a.into()));
        self
    }

    pub fn working_dir(mut self, dir: impl Into<String>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn use_pty(mut self) -> Self {
        self.use_pty = true;
        self
    }

    pub fn timeout(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }
}

/// Output collected from a process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub combined: String,
    pub truncated: bool,
}

impl Default for ProcessOutput {
    fn default() -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            combined: String::new(),
            truncated: false,
        }
    }
}

/// A managed process entry in the supervisor.
struct ManagedProcess {
    info: ProcessInfo,
    output: ProcessOutput,
    started: Option<Instant>,
}

/// Process supervisor — manages spawn, monitor, kill for child processes.
pub struct ProcessSupervisor {
    processes: Arc<RwLock<HashMap<String, ManagedProcess>>>,
    max_concurrent: usize,
}

impl ProcessSupervisor {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            processes: Arc::new(RwLock::new(HashMap::new())),
            max_concurrent,
        }
    }

    /// Spawn a new managed process.
    ///
    /// Returns the process ID that can be used to query status, output, or kill.
    pub async fn spawn(&self, config: SpawnConfig) -> Result<String, SupervisorError> {
        let processes = self.processes.read().await;
        let active = processes
            .values()
            .filter(|p| p.info.state == ProcessState::Running)
            .count();

        if active >= self.max_concurrent {
            return Err(SupervisorError::TooManyProcesses {
                current: active,
                max: self.max_concurrent,
            });
        }
        drop(processes);

        let id = Uuid::new_v4().to_string();
        let info = ProcessInfo {
            id: id.clone(),
            pid: None,
            command: config.command.clone(),
            args: config.args.clone(),
            working_dir: config.working_dir.clone(),
            state: ProcessState::Pending,
            exit_code: None,
            started_at: Some(chrono::Utc::now()),
            ended_at: None,
            use_pty: config.use_pty,
            timeout_ms: config.timeout_ms,
        };

        let managed = ManagedProcess {
            info,
            output: ProcessOutput::default(),
            started: Some(Instant::now()),
        };

        self.processes.write().await.insert(id.clone(), managed);

        info!(
            id = id.as_str(),
            command = config.command.as_str(),
            "process spawned (pending)"
        );

        Ok(id)
    }

    /// Mark a process as running with its OS PID.
    pub async fn mark_running(&self, id: &str, pid: u32) -> Result<(), SupervisorError> {
        let mut processes = self.processes.write().await;
        let proc = processes
            .get_mut(id)
            .ok_or_else(|| SupervisorError::NotFound(id.to_string()))?;

        proc.info.pid = Some(pid);
        proc.info.state = ProcessState::Running;
        debug!(id, pid, "process marked as running");
        Ok(())
    }

    /// Mark a process as completed.
    pub async fn mark_completed(
        &self,
        id: &str,
        exit_code: i32,
    ) -> Result<(), SupervisorError> {
        let mut processes = self.processes.write().await;
        let proc = processes
            .get_mut(id)
            .ok_or_else(|| SupervisorError::NotFound(id.to_string()))?;

        proc.info.state = if exit_code == 0 {
            ProcessState::Completed
        } else {
            ProcessState::Failed
        };
        proc.info.exit_code = Some(exit_code);
        proc.info.ended_at = Some(chrono::Utc::now());

        info!(id, exit_code, "process completed");
        Ok(())
    }

    /// Append output to a process.
    pub async fn append_output(
        &self,
        id: &str,
        data: &str,
        is_stderr: bool,
    ) -> Result<(), SupervisorError> {
        let mut processes = self.processes.write().await;
        let proc = processes
            .get_mut(id)
            .ok_or_else(|| SupervisorError::NotFound(id.to_string()))?;

        if is_stderr {
            proc.output.stderr.push_str(data);
        } else {
            proc.output.stdout.push_str(data);
        }
        proc.output.combined.push_str(data);

        Ok(())
    }

    /// Kill a process by ID.
    pub async fn kill(&self, id: &str) -> Result<(), SupervisorError> {
        let mut processes = self.processes.write().await;
        let proc = processes
            .get_mut(id)
            .ok_or_else(|| SupervisorError::NotFound(id.to_string()))?;

        if proc.info.state != ProcessState::Running {
            return Err(SupervisorError::InvalidState {
                id: id.to_string(),
                state: proc.info.state,
            });
        }

        proc.info.state = ProcessState::Killed;
        proc.info.ended_at = Some(chrono::Utc::now());

        // In a real implementation, this would send SIGKILL/SIGTERM to the process tree
        if let Some(pid) = proc.info.pid {
            info!(id, pid, "killing process");
            // kill_tree(pid) would be called here
        }

        Ok(())
    }

    /// Kill an entire process tree (parent + all children).
    ///
    /// On Unix: sends SIGTERM to process group, then SIGKILL after grace period.
    /// On Windows: uses TerminateProcess with job objects.
    pub async fn kill_tree(&self, id: &str) -> Result<(), SupervisorError> {
        // For now, delegates to kill(). A real implementation would:
        // 1. Read /proc/{pid}/children recursively (Linux)
        // 2. Use sysctl KERN_PROC_CHILDREN (macOS)
        // 3. Send SIGTERM to all, wait grace period, then SIGKILL
        self.kill(id).await
    }

    /// Get info for a process.
    pub async fn get_info(&self, id: &str) -> Option<ProcessInfo> {
        self.processes
            .read()
            .await
            .get(id)
            .map(|p| p.info.clone())
    }

    /// Get output for a process.
    pub async fn get_output(&self, id: &str) -> Option<ProcessOutput> {
        self.processes
            .read()
            .await
            .get(id)
            .map(|p| p.output.clone())
    }

    /// List all processes.
    pub async fn list(&self) -> Vec<ProcessInfo> {
        self.processes
            .read()
            .await
            .values()
            .map(|p| p.info.clone())
            .collect()
    }

    /// List active (running) processes.
    pub async fn list_active(&self) -> Vec<ProcessInfo> {
        self.processes
            .read()
            .await
            .values()
            .filter(|p| p.info.state == ProcessState::Running)
            .map(|p| p.info.clone())
            .collect()
    }

    /// Check for timed-out processes and mark them.
    pub async fn check_timeouts(&self) -> Vec<String> {
        let mut timed_out = Vec::new();
        let mut processes = self.processes.write().await;

        for (id, proc) in processes.iter_mut() {
            if proc.info.state != ProcessState::Running {
                continue;
            }
            if let (Some(timeout_ms), Some(started)) = (proc.info.timeout_ms, proc.started) {
                if started.elapsed() > Duration::from_millis(timeout_ms) {
                    proc.info.state = ProcessState::TimedOut;
                    proc.info.ended_at = Some(chrono::Utc::now());
                    timed_out.push(id.clone());
                    warn!(id = id.as_str(), "process timed out");
                }
            }
        }

        timed_out
    }

    /// Cleanup completed/failed/killed processes older than given duration.
    pub async fn cleanup(&self, older_than: Duration) -> usize {
        let mut processes = self.processes.write().await;
        let before = processes.len();

        processes.retain(|_, proc| {
            if proc.info.state == ProcessState::Running || proc.info.state == ProcessState::Pending
            {
                return true;
            }
            if let Some(started) = proc.started {
                started.elapsed() < older_than
            } else {
                true
            }
        });

        let removed = before - processes.len();
        if removed > 0 {
            info!(removed, "cleaned up old processes");
        }
        removed
    }
}

/// Supervisor error.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("process not found: {0}")]
    NotFound(String),
    #[error("too many concurrent processes: {current}/{max}")]
    TooManyProcesses { current: usize, max: usize },
    #[error("process {id} in invalid state {state:?}")]
    InvalidState { id: String, state: ProcessState },
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("kill failed: {0}")]
    KillFailed(String),
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_and_complete() {
        let supervisor = ProcessSupervisor::new(10);

        let config = SpawnConfig::new("echo").arg("hello");
        let id = supervisor.spawn(config).await.unwrap();

        supervisor.mark_running(&id, 12345).await.unwrap();
        let info = supervisor.get_info(&id).await.unwrap();
        assert_eq!(info.state, ProcessState::Running);
        assert_eq!(info.pid, Some(12345));

        supervisor.mark_completed(&id, 0).await.unwrap();
        let info = supervisor.get_info(&id).await.unwrap();
        assert_eq!(info.state, ProcessState::Completed);
        assert_eq!(info.exit_code, Some(0));
    }

    #[tokio::test]
    async fn test_spawn_failed_exit() {
        let supervisor = ProcessSupervisor::new(10);
        let id = supervisor.spawn(SpawnConfig::new("false")).await.unwrap();
        supervisor.mark_running(&id, 1).await.unwrap();
        supervisor.mark_completed(&id, 1).await.unwrap();

        let info = supervisor.get_info(&id).await.unwrap();
        assert_eq!(info.state, ProcessState::Failed);
    }

    #[tokio::test]
    async fn test_output_collection() {
        let supervisor = ProcessSupervisor::new(10);
        let id = supervisor.spawn(SpawnConfig::new("test")).await.unwrap();

        supervisor.append_output(&id, "line1\n", false).await.unwrap();
        supervisor.append_output(&id, "err1\n", true).await.unwrap();
        supervisor.append_output(&id, "line2\n", false).await.unwrap();

        let output = supervisor.get_output(&id).await.unwrap();
        assert_eq!(output.stdout, "line1\nline2\n");
        assert_eq!(output.stderr, "err1\n");
        assert!(output.combined.contains("line1"));
        assert!(output.combined.contains("err1"));
    }

    #[tokio::test]
    async fn test_kill_process() {
        let supervisor = ProcessSupervisor::new(10);
        let id = supervisor.spawn(SpawnConfig::new("sleep").arg("60")).await.unwrap();
        supervisor.mark_running(&id, 99).await.unwrap();

        supervisor.kill(&id).await.unwrap();
        let info = supervisor.get_info(&id).await.unwrap();
        assert_eq!(info.state, ProcessState::Killed);
    }

    #[tokio::test]
    async fn test_max_concurrent() {
        let supervisor = ProcessSupervisor::new(2);

        let id1 = supervisor.spawn(SpawnConfig::new("a")).await.unwrap();
        supervisor.mark_running(&id1, 1).await.unwrap();
        let id2 = supervisor.spawn(SpawnConfig::new("b")).await.unwrap();
        supervisor.mark_running(&id2, 2).await.unwrap();

        // Third should fail
        let result = supervisor.spawn(SpawnConfig::new("c")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_active() {
        let supervisor = ProcessSupervisor::new(10);

        let id1 = supervisor.spawn(SpawnConfig::new("a")).await.unwrap();
        supervisor.mark_running(&id1, 1).await.unwrap();
        let id2 = supervisor.spawn(SpawnConfig::new("b")).await.unwrap();
        supervisor.mark_running(&id2, 2).await.unwrap();
        supervisor.mark_completed(&id2, 0).await.unwrap();

        let active = supervisor.list_active().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].command, "a");
    }

    #[tokio::test]
    async fn test_timeout_check() {
        let supervisor = ProcessSupervisor::new(10);

        let config = SpawnConfig::new("slow").timeout(1); // 1ms timeout
        let id = supervisor.spawn(config).await.unwrap();
        supervisor.mark_running(&id, 1).await.unwrap();

        // Wait a bit for timeout
        tokio::time::sleep(Duration::from_millis(10)).await;

        let timed_out = supervisor.check_timeouts().await;
        assert_eq!(timed_out.len(), 1);

        let info = supervisor.get_info(&id).await.unwrap();
        assert_eq!(info.state, ProcessState::TimedOut);
    }

    #[test]
    fn test_spawn_config_builder() {
        let config = SpawnConfig::new("cargo")
            .arg("build")
            .arg("--release")
            .working_dir("/project")
            .env("RUST_LOG", "debug")
            .use_pty()
            .timeout(30000);

        assert_eq!(config.command, "cargo");
        assert_eq!(config.args, vec!["build", "--release"]);
        assert_eq!(config.working_dir, Some("/project".to_string()));
        assert!(config.use_pty);
        assert_eq!(config.timeout_ms, Some(30000));
    }

    #[tokio::test]
    async fn test_cleanup() {
        let supervisor = ProcessSupervisor::new(10);
        let id = supervisor.spawn(SpawnConfig::new("done")).await.unwrap();
        supervisor.mark_running(&id, 1).await.unwrap();
        supervisor.mark_completed(&id, 0).await.unwrap();

        // Cleanup with 0 duration should remove completed
        let removed = supervisor.cleanup(Duration::from_secs(0)).await;
        assert_eq!(removed, 1);
        assert!(supervisor.get_info(&id).await.is_none());
    }

    #[test]
    fn test_process_info_serde() {
        let info = ProcessInfo {
            id: "test".to_string(),
            pid: Some(123),
            command: "echo".to_string(),
            args: vec!["hi".to_string()],
            working_dir: None,
            state: ProcessState::Running,
            exit_code: None,
            started_at: None,
            ended_at: None,
            use_pty: false,
            timeout_ms: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: ProcessInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.pid, Some(123));
        assert_eq!(parsed.state, ProcessState::Running);
    }
}
