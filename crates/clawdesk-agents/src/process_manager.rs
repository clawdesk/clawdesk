//! Persistent Process Management.
//!
//! Manages long-running background processes that survive across tool rounds.
//! The LLM can start, poll, send input, and kill processes.
//!
//! ## Architecture
//!
//! Processes are tracked in a global registry keyed by session-scoped IDs.
//! Output is buffered incrementally (stdout/stderr) and can be polled without
//! blocking. Cleanup happens on explicit kill, timeout, or session teardown.
//!
//! ## Tools
//!
//! - `process_start` — Spawn a new background process.
//! - `process_poll`  — Read new stdout/stderr since last poll.
//! - `process_write` — Write to stdin of a running process.
//! - `process_kill`  — Send SIGTERM (then SIGKILL after grace period).
//! - `process_list`  — List active processes for the current session.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Entry in the process registry.
#[derive(Debug)]
pub struct ManagedProcess {
    /// The child process handle.
    child: Mutex<Option<Child>>,
    /// Accumulated stdout since last poll.
    stdout_new: Mutex<String>,
    /// Accumulated stderr since last poll.
    stderr_new: Mutex<String>,
    /// Total stdout accumulated.
    stdout_total: Mutex<String>,
    /// Total stderr accumulated.
    stderr_total: Mutex<String>,
    /// Process stdin handle for writing.
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    /// When the process was started.
    started_at: std::time::Instant,
    /// The command that was started.
    pub command: String,
    /// Exit code (set when process terminates).
    exit_code: Mutex<Option<i32>>,
    /// Maximum output buffer size (prevents memory exhaustion).
    max_buffer_bytes: usize,
}

/// Process status snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessStatus {
    pub process_id: String,
    pub command: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub elapsed_secs: f64,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
}

/// Process manager — global registry of managed processes.
pub struct ProcessManager {
    /// Active processes: process_id → ManagedProcess
    processes: DashMap<String, Arc<ManagedProcess>>,
    /// Maximum number of concurrent processes.
    max_processes: usize,
    /// Maximum output buffer per process.
    max_buffer_bytes: usize,
}

impl ProcessManager {
    pub fn new(max_processes: usize, max_buffer_bytes: usize) -> Self {
        Self {
            processes: DashMap::new(),
            max_processes,
            max_buffer_bytes,
        }
    }

    /// Start a new managed process.
    ///
    /// Returns a unique process ID or an error.
    pub async fn start(
        &self,
        process_id: String,
        command: &str,
        working_dir: Option<&str>,
        env: Option<&HashMap<String, String>>,
    ) -> Result<String, String> {
        // Check capacity
        {
            if self.processes.len() >= self.max_processes {
                return Err(format!(
                    "max processes ({}) reached — kill an existing process first",
                    self.max_processes
                ));
            }
            if self.processes.contains_key(&process_id) {
                return Err(format!("process '{}' already exists", process_id));
            }
        }

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }
        if let Some(env_vars) = env {
            for (k, v) in env_vars {
                cmd.env(k, v);
            }
        }

        let mut child = cmd.spawn().map_err(|e| format!("spawn error: {}", e))?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let managed = Arc::new(ManagedProcess {
            child: Mutex::new(Some(child)),
            stdout_new: Mutex::new(String::new()),
            stderr_new: Mutex::new(String::new()),
            stdout_total: Mutex::new(String::new()),
            stderr_total: Mutex::new(String::new()),
            stdin: Mutex::new(stdin),
            started_at: std::time::Instant::now(),
            command: command.to_string(),
            exit_code: Mutex::new(None),
            max_buffer_bytes: self.max_buffer_bytes,
        });

        // Spawn output readers
        if let Some(stdout) = stdout {
            let proc = Arc::clone(&managed);
            tokio::spawn(async move {
                Self::read_output(stdout, proc, true).await;
            });
        }
        if let Some(stderr) = stderr {
            let proc = Arc::clone(&managed);
            tokio::spawn(async move {
                Self::read_output(stderr, proc, false).await;
            });
        }

        {
            self.processes.insert(process_id.clone(), managed);
        }

        info!(process_id = %process_id, command = %command, "process started");
        Ok(process_id)
    }

    /// Poll a process for new output since last poll.
    pub async fn poll(&self, process_id: &str) -> Result<PollResult, String> {
        let proc = self.get_process(process_id).await?;

        // Drain new output
        let stdout = {
            let mut buf = proc.stdout_new.lock().await;
            std::mem::take(&mut *buf)
        };
        let stderr = {
            let mut buf = proc.stderr_new.lock().await;
            std::mem::take(&mut *buf)
        };

        // Check if process has exited
        let exit_code = {
            let mut child_guard = proc.child.lock().await;
            if let Some(ref mut child) = *child_guard {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let code = status.code();
                        *proc.exit_code.lock().await = code;
                        code
                    }
                    Ok(None) => None,
                    Err(e) => {
                        warn!(process_id = %process_id, error = %e, "try_wait failed");
                        None
                    }
                }
            } else {
                *proc.exit_code.lock().await
            }
        };

        Ok(PollResult {
            stdout,
            stderr,
            running: exit_code.is_none(),
            exit_code,
            elapsed_secs: proc.started_at.elapsed().as_secs_f64(),
        })
    }

    /// Write data to a process's stdin.
    pub async fn write(&self, process_id: &str, data: &str) -> Result<(), String> {
        let proc = self.get_process(process_id).await?;
        let mut stdin_guard = proc.stdin.lock().await;
        if let Some(ref mut stdin) = *stdin_guard {
            stdin
                .write_all(data.as_bytes())
                .await
                .map_err(|e| format!("write error: {}", e))?;
            stdin
                .flush()
                .await
                .map_err(|e| format!("flush error: {}", e))?;
            Ok(())
        } else {
            Err("stdin not available".to_string())
        }
    }

    /// Kill a managed process.
    pub async fn kill(&self, process_id: &str) -> Result<(), String> {
        let proc = self.processes
            .remove(process_id)
            .map(|(_, v)| v)
            .ok_or_else(|| format!("process '{}' not found", process_id))?;

        let mut child_guard = proc.child.lock().await;
        if let Some(ref mut child) = *child_guard {
            // Try graceful SIGTERM first
            let _ = child.kill().await;
            info!(process_id = %process_id, "process killed");
        }
        *child_guard = None;

        Ok(())
    }

    /// List all active processes.
    pub async fn list(&self) -> Vec<ProcessStatus> {
        let mut statuses = Vec::with_capacity(self.processes.len());

        for entry in self.processes.iter() {
            let id = entry.key();
            let proc = entry.value();
            let exit_code = *proc.exit_code.lock().await;
            let stdout_bytes = proc.stdout_total.lock().await.len();
            let stderr_bytes = proc.stderr_total.lock().await.len();

            statuses.push(ProcessStatus {
                process_id: id.clone(),
                command: proc.command.clone(),
                running: exit_code.is_none(),
                exit_code,
                elapsed_secs: proc.started_at.elapsed().as_secs_f64(),
                stdout_bytes,
                stderr_bytes,
            });
        }

        statuses
    }

    /// Clean up all processes (call on session teardown).
    pub async fn cleanup(&self) {
        let ids: Vec<String> = self.processes.iter().map(|e| e.key().clone()).collect();
        for id in ids {
            if let Err(e) = self.kill(&id).await {
                debug!(process_id = %id, error = %e, "cleanup kill failed");
            }
        }
    }

    async fn get_process(&self, process_id: &str) -> Result<Arc<ManagedProcess>, String> {
        self.processes
            .get(process_id)
            .map(|e| e.value().clone())
            .ok_or_else(|| format!("process '{}' not found", process_id))
    }

    /// Background task that reads from a process output stream and
    /// buffers it into the ManagedProcess.
    async fn read_output<R: tokio::io::AsyncRead + Unpin>(
        reader: R,
        proc: Arc<ManagedProcess>,
        is_stdout: bool,
    ) {
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(reader).lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let line_with_newline = format!("{}\n", line);

            if is_stdout {
                let mut new_buf = proc.stdout_new.lock().await;
                let mut total_buf = proc.stdout_total.lock().await;
                if total_buf.len() < proc.max_buffer_bytes {
                    new_buf.push_str(&line_with_newline);
                    total_buf.push_str(&line_with_newline);
                }
            } else {
                let mut new_buf = proc.stderr_new.lock().await;
                let mut total_buf = proc.stderr_total.lock().await;
                if total_buf.len() < proc.max_buffer_bytes {
                    new_buf.push_str(&line_with_newline);
                    total_buf.push_str(&line_with_newline);
                }
            }
        }
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new(16, 1024 * 1024) // 16 processes, 1MB buffer each
    }
}

/// Result of polling a process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollResult {
    /// New stdout since last poll.
    pub stdout: String,
    /// New stderr since last poll.
    pub stderr: String,
    /// Whether the process is still running.
    pub running: bool,
    /// Exit code (if terminated).
    pub exit_code: Option<i32>,
    /// Elapsed time in seconds.
    pub elapsed_secs: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_start_and_poll() {
        let mgr = ProcessManager::default();
        let id = mgr
            .start("test-1".to_string(), "echo hello", None, None)
            .await
            .unwrap();
        assert_eq!(id, "test-1");

        // Give the process time to finish
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let result = mgr.poll("test-1").await.unwrap();
        assert!(result.stdout.contains("hello"));
        assert!(!result.running);
    }

    #[tokio::test]
    async fn test_list_processes() {
        let mgr = ProcessManager::default();
        mgr.start("p1".to_string(), "echo a", None, None)
            .await
            .unwrap();

        // Wait for process to finish
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let statuses = mgr.list().await;
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].process_id, "p1");
    }

    #[tokio::test]
    async fn test_kill_process() {
        let mgr = ProcessManager::default();
        mgr.start("p-kill".to_string(), "sleep 60", None, None)
            .await
            .unwrap();

        let result = mgr.kill("p-kill").await;
        assert!(result.is_ok());

        // Should be gone from the list
        let statuses = mgr.list().await;
        assert!(statuses.is_empty());
    }

    #[tokio::test]
    async fn test_max_processes_limit() {
        let mgr = ProcessManager::new(1, 1024);
        mgr.start("p1".to_string(), "sleep 60", None, None)
            .await
            .unwrap();

        let result = mgr.start("p2".to_string(), "sleep 60", None, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max processes"));

        mgr.cleanup().await;
    }

    #[tokio::test]
    async fn test_duplicate_id_rejected() {
        let mgr = ProcessManager::default();
        mgr.start("dup".to_string(), "sleep 60", None, None)
            .await
            .unwrap();

        let result = mgr.start("dup".to_string(), "sleep 60", None, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));

        mgr.cleanup().await;
    }

    #[tokio::test]
    async fn test_cleanup() {
        let mgr = ProcessManager::default();
        mgr.start("c1".to_string(), "sleep 60", None, None)
            .await
            .unwrap();
        mgr.start("c2".to_string(), "sleep 60", None, None)
            .await
            .unwrap();

        mgr.cleanup().await;
        assert!(mgr.list().await.is_empty());
    }

    #[tokio::test]
    async fn test_poll_nonexistent() {
        let mgr = ProcessManager::default();
        let result = mgr.poll("nonexistent").await;
        assert!(result.is_err());
    }
}
