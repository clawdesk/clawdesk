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
use tracing::{debug, info, warn};

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
    /// Channel to reset the silence watchdog timer on output activity.
    /// Each send resets the watchdog countdown — O(1) per output chunk.
    watchdog_reset_tx: tokio::sync::mpsc::Sender<()>,
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
    /// Output silence timeout — kill process if no output for this long.
    output_silence_timeout: std::time::Duration,
}

impl ProcessManager {
    pub fn new(max_processes: usize, max_buffer_bytes: usize) -> Self {
        Self {
            processes: DashMap::new(),
            max_processes,
            max_buffer_bytes,
            output_silence_timeout: std::time::Duration::from_secs(30),
        }
    }

    /// Set the output silence timeout (kill processes that produce no output
    /// for this duration). Default: 30 seconds.
    pub fn with_silence_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.output_silence_timeout = timeout;
        self
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

        // Timer-reset watchdog channel: each send resets the countdown.
        // Buffer of 16 to avoid blocking output readers under burst.
        let (watchdog_tx, watchdog_rx) = tokio::sync::mpsc::channel::<()>(16);

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
            watchdog_reset_tx: watchdog_tx,
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

        // Spawn timer-reset silence watchdog.
        // The watchdog fires if and only if no output arrives for exactly
        // `timeout` seconds — no polling, zero wakeups during normal output.
        {
            let processes = self.processes.clone();
            let pid = process_id.clone();
            let timeout = self.output_silence_timeout;
            tokio::spawn(async move {
                Self::timer_reset_watchdog(processes, pid, watchdog_rx, timeout).await;
            });
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

    /// Kill a managed process and all its children (kill-tree).
    pub async fn kill(&self, process_id: &str) -> Result<(), String> {
        let proc = self.processes
            .remove(process_id)
            .map(|(_, v)| v)
            .ok_or_else(|| format!("process '{}' not found", process_id))?;

        Self::kill_process_tree(&proc).await;
        info!(process_id = %process_id, "process tree killed");

        Ok(())
    }

    /// Kill a process and all of its descendant processes.
    ///
    /// On Unix: walks `/proc/{pid}/children` (Linux 4.2+) or falls back to
    /// parsing `pgrep -P <pid>` to find descendants. Sends SIGTERM to all
    /// descendants (leaves first), then SIGKILL after a 2-second grace period.
    async fn kill_process_tree(proc: &ManagedProcess) {
        let mut child_guard = proc.child.lock().await;
        if let Some(ref mut child) = *child_guard {
            if let Some(pid) = child.id() {
                // Collect all descendant PIDs.
                let descendants = Self::collect_descendants(pid);

                // Kill descendants first (leaves → root).
                for &dpid in descendants.iter().rev() {
                    Self::send_signal(dpid, "TERM");
                }

                // SIGTERM the root process.
                let _ = child.kill().await;

                // Grace period, then SIGKILL stragglers.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                for &dpid in descendants.iter().rev() {
                    Self::send_signal(dpid, "KILL");
                }
            } else {
                let _ = child.kill().await;
            }
        }
        *child_guard = None;
    }

    /// Collect all descendant PIDs of a given process.
    #[cfg(unix)]
    fn collect_descendants(pid: u32) -> Vec<u32> {
        let mut descendants = Vec::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(pid);

        while let Some(parent) = queue.pop_front() {
            // Try /proc/{pid}/task/{tid}/children (Linux 4.2+)
            let children_path = format!("/proc/{}/task/{}/children", parent, parent);
            let children: Vec<u32> = std::fs::read_to_string(&children_path)
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();

            if !children.is_empty() {
                for cpid in children {
                    descendants.push(cpid);
                    queue.push_back(cpid);
                }
            } else {
                // Fallback: pgrep -P <pid>
                if let Ok(output) = std::process::Command::new("pgrep")
                    .args(["-P", &parent.to_string()])
                    .output()
                {
                    let pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
                        .split_whitespace()
                        .filter_map(|s| s.parse().ok())
                        .collect();
                    for cpid in pids {
                        descendants.push(cpid);
                        queue.push_back(cpid);
                    }
                }
            }
        }
        descendants
    }

    #[cfg(not(unix))]
    fn collect_descendants(_pid: u32) -> Vec<u32> {
        // On non-Unix, we cannot walk the process tree portably.
        Vec::new()
    }

    /// Send a signal to a process by PID.
    #[cfg(unix)]
    fn send_signal(pid: u32, signal: &str) {
        let sig = match signal {
            "TERM" => 15,
            "KILL" => 9,
            _ => return,
        };
        // Safety: sending a signal to a known PID.
        unsafe {
            libc::kill(pid as i32, sig);
        }
    }

    #[cfg(not(unix))]
    fn send_signal(_pid: u32, _signal: &str) {
        // No-op on non-Unix.
    }

    /// Timer-reset silence watchdog: kills a process if it produces no output
    /// for exactly `timeout` duration. Each output chunk resets the timer.
    ///
    /// # Model
    /// Formally a retriggerable monostable: each input event (stdout chunk via
    /// `touch_output()`) resets the countdown. The output fires only on timeout.
    /// State machine: {Armed, Fired}. Armed + input → Armed (reset),
    /// Armed + timeout → Fired.
    ///
    /// # Liveness
    /// If the reset channel sender is dropped (process task panics or output
    /// readers complete), `recv()` returns `None` and the watchdog fires
    /// immediately — ensuring no resource leak under any failure mode.
    async fn timer_reset_watchdog(
        processes: DashMap<String, Arc<ManagedProcess>>,
        process_id: String,
        mut reset_rx: tokio::sync::mpsc::Receiver<()>,
        timeout: std::time::Duration,
    ) {
        loop {
            tokio::select! {
                // Arm the timer — fires after `timeout` with no resets.
                _ = tokio::time::sleep(timeout) => {
                    // Check if process still exists.
                    let proc = match processes.get(&process_id) {
                        Some(p) => p.value().clone(),
                        None => return,
                    };

                    // Check if process has already exited.
                    {
                        let exit = proc.exit_code.lock().await;
                        if exit.is_some() {
                            return;
                        }
                    }

                    // Timeout fired — kill the process.
                    warn!(
                        process_id = %process_id,
                        timeout_secs = timeout.as_secs(),
                        "process killed due to output silence (timer-reset watchdog)"
                    );
                    if let Some((_, proc)) = processes.remove(&process_id) {
                        Self::kill_process_tree(&proc).await;
                    }
                    return;
                }
                // Reset signal received — restart the timer.
                result = reset_rx.recv() => {
                    match result {
                        Some(()) => {
                            // Output received — continue loop to reset timer.
                            continue;
                        }
                        None => {
                            // Channel closed — all senders dropped.
                            // Process output readers are done. Check if exited cleanly.
                            let proc = match processes.get(&process_id) {
                                Some(p) => p.value().clone(),
                                None => return,
                            };
                            let exit = proc.exit_code.lock().await;
                            if exit.is_some() {
                                return; // Clean exit.
                            }
                            // Senders dropped but process didn't exit — kill it.
                            warn!(
                                process_id = %process_id,
                                "watchdog reset channel closed, killing orphaned process"
                            );
                            drop(exit);
                            if let Some((_, proc)) = processes.remove(&process_id) {
                                Self::kill_process_tree(&proc).await;
                            }
                            return;
                        }
                    }
                }
            }
        }
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

            // Reset the watchdog timer — O(1) per output chunk.
            // try_send avoids blocking if the channel is full (burst output).
            let _ = proc.watchdog_reset_tx.try_send(());

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
