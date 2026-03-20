//! Persistent sandbox sessions with background process lifecycle.
//!
//! Agents can start long-running processes (dev servers, watchers, databases)
//! in round 1, interact in round 2, and kill in round 3. Each session tracks
//! a background process with a rolling output buffer.
//!
//! ## Operations
//!
//! - `start(command)` → session_id
//! - `write(session_id, input)` — send stdin
//! - `read(session_id)` → output since last read
//! - `kill(session_id)` — graceful SIGTERM → SIGKILL
//! - `list()` → active sessions with status

use crate::{ResourceLimits, SandboxError};
use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Maximum output retained per session (1MB rolling buffer).
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// Default session timeout (30 minutes).
const DEFAULT_TIMEOUT_SECS: u64 = 1800;

/// Information about a running session.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub command: String,
    pub started_at_epoch_ms: u64,
    pub elapsed_secs: u64,
    pub output_bytes: usize,
    pub alive: bool,
}

/// Manager for persistent sandbox sessions.
pub struct SandboxSessionManager {
    sessions: DashMap<String, Arc<SessionHandle>>,
    max_sessions: usize,
    session_timeout: Duration,
}

struct SessionHandle {
    id: String,
    command: String,
    started_at: Instant,
    child: RwLock<Option<Child>>,
    stdin_tx: RwLock<Option<tokio::process::ChildStdin>>,
    /// Rolling output buffer with read cursor.
    output: RwLock<OutputBuffer>,
    timeout: Duration,
}

struct OutputBuffer {
    data: VecDeque<u8>,
    /// Position of last read — used for "read since last" semantics.
    read_cursor: usize,
    /// Total bytes ever written (monotonically increasing).
    total_written: usize,
}

impl OutputBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            data: VecDeque::with_capacity(capacity),
            read_cursor: 0,
            total_written: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.data.len() >= MAX_OUTPUT_BYTES {
                self.data.pop_front();
                // Advance cursor if it was pointing at evicted data
                if self.read_cursor > 0 {
                    self.read_cursor = self.read_cursor.saturating_sub(1);
                }
            }
            self.data.push_back(b);
        }
        self.total_written += bytes.len();
    }

    fn read_new(&mut self) -> String {
        let available = self.data.len();
        if self.read_cursor >= available {
            return String::new();
        }
        let bytes: Vec<u8> = self.data.iter().skip(self.read_cursor).copied().collect();
        self.read_cursor = available;
        String::from_utf8_lossy(&bytes).to_string()
    }
}

impl SandboxSessionManager {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            sessions: DashMap::new(),
            max_sessions,
            session_timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.session_timeout = timeout;
        self
    }

    /// Start a background process and return its session ID.
    pub async fn start(
        &self,
        command: &str,
        args: &[String],
        working_dir: Option<&std::path::Path>,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<String, SandboxError> {
        if self.sessions.len() >= self.max_sessions {
            return Err(SandboxError::ResourceLimitExceeded(format!(
                "max sessions ({}) reached",
                self.max_sessions
            )));
        }

        let session_id = uuid::Uuid::new_v4().to_string();

        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .kill_on_drop(true);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| SandboxError::ExecutionFailed(format!("spawn: {e}")))?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let handle = Arc::new(SessionHandle {
            id: session_id.clone(),
            command: format!("{} {}", command, args.join(" ")),
            started_at: Instant::now(),
            child: RwLock::new(Some(child)),
            stdin_tx: RwLock::new(stdin),
            output: RwLock::new(OutputBuffer::new(MAX_OUTPUT_BYTES)),
            timeout: self.session_timeout,
        });

        // Spawn background reader for stdout
        if let Some(mut stdout) = stdout {
            let output = Arc::clone(&handle);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match stdout.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut ob = output.output.write().await;
                            ob.push(&buf[..n]);
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        // Spawn background reader for stderr (merged into output)
        if let Some(mut stderr) = stderr {
            let output = Arc::clone(&handle);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut ob = output.output.write().await;
                            ob.push(&buf[..n]);
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        // Spawn timeout enforcer
        let sessions_ref = self.sessions.clone();
        let sid = session_id.clone();
        let timeout = self.session_timeout;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            if let Some((_, handle)) = sessions_ref.remove(&sid) {
                warn!(session_id = %sid, "session timed out, killing");
                if let Some(mut child) = handle.child.write().await.take() {
                    let _ = child.kill().await;
                }
            }
        });

        self.sessions.insert(session_id.clone(), handle);
        info!(session_id = %session_id, command, "sandbox session started");

        Ok(session_id)
    }

    /// Send input to a running session's stdin.
    pub async fn write(&self, session_id: &str, input: &str) -> Result<(), SandboxError> {
        let handle = self.sessions.get(session_id).ok_or_else(|| {
            SandboxError::NotAvailable(format!("session {session_id} not found"))
        })?;

        let mut stdin = handle.stdin_tx.write().await;
        if let Some(ref mut tx) = *stdin {
            tx.write_all(input.as_bytes())
                .await
                .map_err(|e| SandboxError::ExecutionFailed(format!("write stdin: {e}")))?;
            tx.write_all(b"\n")
                .await
                .map_err(|e| SandboxError::ExecutionFailed(format!("write newline: {e}")))?;
            Ok(())
        } else {
            Err(SandboxError::ExecutionFailed("stdin closed".into()))
        }
    }

    /// Read output since the last read call (incremental).
    pub async fn read(&self, session_id: &str) -> Result<String, SandboxError> {
        let handle = self.sessions.get(session_id).ok_or_else(|| {
            SandboxError::NotAvailable(format!("session {session_id} not found"))
        })?;

        let mut ob = handle.output.write().await;
        Ok(ob.read_new())
    }

    /// Kill a session. Sends SIGTERM, waits 2s, then SIGKILL.
    pub async fn kill(&self, session_id: &str) -> Result<(), SandboxError> {
        let (_, handle) = self.sessions.remove(session_id).ok_or_else(|| {
            SandboxError::NotAvailable(format!("session {session_id} not found"))
        })?;

        if let Some(mut child) = handle.child.write().await.take() {
            // Graceful shutdown: SIGTERM
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                if let Some(pid) = child.id() {
                    unsafe { libc::kill(pid as i32, libc::SIGTERM); }
                }
            }

            // Wait up to 2 seconds for graceful exit
            match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(_) => debug!(session_id, "session exited gracefully"),
                Err(_) => {
                    let _ = child.kill().await;
                    debug!(session_id, "session force-killed after timeout");
                }
            }
        }

        info!(session_id, "sandbox session killed");
        Ok(())
    }

    /// List all active sessions.
    pub fn list(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .map(|entry| {
                let h = entry.value();
                SessionInfo {
                    id: h.id.clone(),
                    command: h.command.clone(),
                    started_at_epoch_ms: h.started_at.elapsed().as_millis() as u64,
                    elapsed_secs: h.started_at.elapsed().as_secs(),
                    output_bytes: 0, // Would need async read
                    alive: true,     // Still in the map = alive
                }
            })
            .collect()
    }

    /// Number of active sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}
