//! Session multiplexer for long-lived CLI harness processes.
//!
//! Supports:
//! - Bounded concurrent sessions
//! - Broadcast output observers
//! - Ring-buffer snapshots (direct mode)
//! - Optional tmux-backed detached sessions

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::time::{sleep, Duration};
use uuid::Uuid;

/// Session priority class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPriority {
    Urgent,
    Standard,
    Background,
}

impl Default for SessionPriority {
    fn default() -> Self {
        Self::Standard
    }
}

/// Session execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    Direct,
    Tmux,
}

/// Session lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}

/// Session spawn request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSpawnRequest {
    pub id: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub priority: SessionPriority,
    pub use_tmux: bool,
}

impl SessionSpawnRequest {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            id: None,
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: HashMap::new(),
            priority: SessionPriority::Standard,
            use_tmux: false,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }
}

/// Session event stream item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub session_id: String,
    pub at: DateTime<Utc>,
    pub kind: SessionEventKind,
}

/// Session event payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEventKind {
    Output { line: String, is_stderr: bool },
    Status { status: SessionStatus },
    Completed { exit_code: Option<i32> },
    Error { message: String },
}

/// Snapshot view of session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub id: String,
    pub mode: SessionMode,
    pub status: SessionStatus,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub priority: SessionPriority,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub lines: Vec<String>,
}

/// Session mux configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMuxConfig {
    pub max_sessions: usize,
    pub ring_capacity_lines: usize,
    pub enable_tmux: bool,
    pub tmux_poll_interval_ms: u64,
}

impl Default for SessionMuxConfig {
    fn default() -> Self {
        Self {
            max_sessions: 8,
            ring_capacity_lines: 1_000,
            enable_tmux: true,
            tmux_poll_interval_ms: 1_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionMuxError {
    #[error("session capacity reached: {active}/{max}")]
    CapacityReached { active: usize, max: usize },
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("tmux is unavailable on this host")]
    TmuxUnavailable,
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("kill failed: {0}")]
    KillFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
struct SessionMetadata {
    id: String,
    mode: SessionMode,
    status: SessionStatus,
    command: String,
    args: Vec<String>,
    cwd: Option<String>,
    pid: Option<u32>,
    exit_code: Option<i32>,
    priority: SessionPriority,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
enum SessionControl {
    Direct {
        stdin_tx: mpsc::Sender<String>,
        kill_tx: mpsc::Sender<()>,
    },
    Tmux {
        session_name: String,
    },
}

#[derive(Debug, Clone)]
struct ManagedSession {
    metadata: Arc<RwLock<SessionMetadata>>,
    ring: Arc<RwLock<VecDeque<String>>>,
    event_tx: broadcast::Sender<SessionEvent>,
    control: SessionControl,
}

impl ManagedSession {
    async fn snapshot(&self) -> SessionSnapshot {
        let meta = self.metadata.read().await.clone();
        let lines = self.ring.read().await.iter().cloned().collect::<Vec<_>>();
        SessionSnapshot {
            id: meta.id,
            mode: meta.mode,
            status: meta.status,
            command: meta.command,
            args: meta.args,
            cwd: meta.cwd,
            pid: meta.pid,
            exit_code: meta.exit_code,
            priority: meta.priority,
            started_at: meta.started_at,
            ended_at: meta.ended_at,
            lines,
        }
    }
}

/// Multiplexes long-lived harness sessions.
pub struct SessionMux {
    config: SessionMuxConfig,
    sessions: Arc<RwLock<HashMap<String, ManagedSession>>>,
}

impl SessionMux {
    pub fn new(config: SessionMuxConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Spawn a new session and return its ID.
    pub async fn spawn(&self, req: SessionSpawnRequest) -> Result<String, SessionMuxError> {
        let id = req
            .id
            .clone()
            .unwrap_or_else(|| format!("sess_{}", Uuid::new_v4().simple()));

        {
            let session_values = {
                let sessions = self.sessions.read().await;
                if sessions.contains_key(&id) {
                    return Err(SessionMuxError::SpawnFailed(format!(
                        "session id '{}' already exists",
                        id
                    )));
                }
                sessions.values().cloned().collect::<Vec<_>>()
            };

            let mut active = 0usize;
            for session in &session_values {
                let status = session.metadata.read().await.status;
                if matches!(status, SessionStatus::Pending | SessionStatus::Running) {
                    active += 1;
                }
            }
            if active >= self.config.max_sessions {
                return Err(SessionMuxError::CapacityReached {
                    active,
                    max: self.config.max_sessions,
                });
            }
        }

        let managed = if req.use_tmux {
            self.spawn_tmux(&id, &req).await?
        } else {
            self.spawn_direct(&id, &req).await?
        };

        self.sessions.write().await.insert(id.clone(), managed);
        Ok(id)
    }

    /// Send input to a running direct session.
    pub async fn send_input(&self, session_id: &str, input: &str) -> Result<(), SessionMuxError> {
        let session = self
            .sessions
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionMuxError::NotFound(session_id.to_string()))?;
        match session.control {
            SessionControl::Direct { stdin_tx, .. } => stdin_tx
                .send(input.to_string())
                .await
                .map_err(|e| SessionMuxError::SpawnFailed(format!("stdin channel closed: {e}"))),
            SessionControl::Tmux { .. } => Err(SessionMuxError::SpawnFailed(
                "sending interactive input to tmux sessions is not yet supported".to_string(),
            )),
        }
    }

    /// Subscribe to session events.
    pub async fn subscribe(
        &self,
        session_id: &str,
    ) -> Result<broadcast::Receiver<SessionEvent>, SessionMuxError> {
        let tx = self
            .sessions
            .read()
            .await
            .get(session_id)
            .map(|s| s.event_tx.clone())
            .ok_or_else(|| SessionMuxError::NotFound(session_id.to_string()))?;
        Ok(tx.subscribe())
    }

    /// Get a current snapshot for one session.
    pub async fn snapshot(&self, session_id: &str) -> Result<SessionSnapshot, SessionMuxError> {
        let session = self
            .sessions
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionMuxError::NotFound(session_id.to_string()))?;

        if let SessionControl::Tmux { session_name } = &session.control {
            let mut ring = session.ring.write().await;
            let captured = capture_tmux(session_name, self.config.ring_capacity_lines).await?;
            ring.clear();
            for line in captured {
                ring.push_back(line);
            }
        }
        Ok(session.snapshot().await)
    }

    /// List snapshots for all sessions.
    pub async fn list_snapshots(&self) -> Vec<SessionSnapshot> {
        let sessions = self.sessions.read().await.values().cloned().collect::<Vec<_>>();
        let mut out = Vec::with_capacity(sessions.len());
        for session in sessions {
            out.push(session.snapshot().await);
        }
        out
    }

    /// Kill a session.
    pub async fn kill(&self, session_id: &str) -> Result<(), SessionMuxError> {
        let session = self
            .sessions
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionMuxError::NotFound(session_id.to_string()))?;

        match session.control.clone() {
            SessionControl::Direct { kill_tx, .. } => kill_tx
                .send(())
                .await
                .map_err(|e| SessionMuxError::KillFailed(format!("kill channel closed: {e}"))),
            SessionControl::Tmux { session_name } => {
                let status = Command::new("tmux")
                    .args(["kill-session", "-t", &session_name])
                    .status()
                    .await?;
                if status.success() {
                    let mut meta = session.metadata.write().await;
                    meta.status = SessionStatus::Killed;
                    meta.ended_at = Some(Utc::now());
                    Ok(())
                } else {
                    Err(SessionMuxError::KillFailed(format!(
                        "tmux kill-session failed for {}",
                        session_name
                    )))
                }
            }
        }
    }

    async fn spawn_direct(
        &self,
        id: &str,
        req: &SessionSpawnRequest,
    ) -> Result<ManagedSession, SessionMuxError> {
        let mut cmd = Command::new(&req.command);
        cmd.args(&req.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());

        if let Some(cwd) = &req.cwd {
            cmd.current_dir(cwd);
        }
        for (k, v) in &req.env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| SessionMuxError::SpawnFailed(format!("failed to spawn '{}': {e}", req.command)))?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();

        let meta = Arc::new(RwLock::new(SessionMetadata {
            id: id.to_string(),
            mode: SessionMode::Direct,
            status: SessionStatus::Running,
            command: req.command.clone(),
            args: req.args.clone(),
            cwd: req.cwd.clone(),
            pid,
            exit_code: None,
            priority: req.priority,
            started_at: Utc::now(),
            ended_at: None,
        }));

        let ring = Arc::new(RwLock::new(VecDeque::<String>::with_capacity(
            self.config.ring_capacity_lines,
        )));
        let (event_tx, _) = broadcast::channel::<SessionEvent>(512);
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(64);
        let (kill_tx, mut kill_rx) = mpsc::channel::<()>(1);
        let cap = self.config.ring_capacity_lines;

        if let Some(mut stdin_writer) = stdin {
            tokio::spawn(async move {
                while let Some(line) = stdin_rx.recv().await {
                    if stdin_writer.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    if stdin_writer.write_all(b"\n").await.is_err() {
                        break;
                    }
                    if stdin_writer.flush().await.is_err() {
                        break;
                    }
                }
            });
        }

        if let Some(stdout_reader) = stdout {
            spawn_reader_task(
                id.to_string(),
                false,
                stdout_reader,
                Arc::clone(&ring),
                cap,
                event_tx.clone(),
            );
        }
        if let Some(stderr_reader) = stderr {
            spawn_reader_task(
                id.to_string(),
                true,
                stderr_reader,
                Arc::clone(&ring),
                cap,
                event_tx.clone(),
            );
        }

        let id_for_task = id.to_string();
        let meta_for_task = Arc::clone(&meta);
        let tx_for_task = event_tx.clone();
        tokio::spawn(async move {
            let wait_result = tokio::select! {
                status = child.wait() => status,
                _ = kill_rx.recv() => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };

            match wait_result {
                Ok(status) => {
                    let code = status.code();
                    let final_state = if status.success() {
                        SessionStatus::Completed
                    } else if status.code().is_none() {
                        SessionStatus::Killed
                    } else {
                        SessionStatus::Failed
                    };
                    {
                        let mut meta = meta_for_task.write().await;
                        meta.status = final_state;
                        meta.exit_code = code;
                        meta.ended_at = Some(Utc::now());
                    }
                    let _ = tx_for_task.send(SessionEvent {
                        session_id: id_for_task.clone(),
                        at: Utc::now(),
                        kind: SessionEventKind::Status { status: final_state },
                    });
                    let _ = tx_for_task.send(SessionEvent {
                        session_id: id_for_task,
                        at: Utc::now(),
                        kind: SessionEventKind::Completed { exit_code: code },
                    });
                }
                Err(err) => {
                    {
                        let mut meta = meta_for_task.write().await;
                        meta.status = SessionStatus::Failed;
                        meta.ended_at = Some(Utc::now());
                    }
                    let _ = tx_for_task.send(SessionEvent {
                        session_id: id_for_task,
                        at: Utc::now(),
                        kind: SessionEventKind::Error {
                            message: format!("wait failed: {err}"),
                        },
                    });
                }
            }
        });

        Ok(ManagedSession {
            metadata: meta,
            ring,
            event_tx,
            control: SessionControl::Direct { stdin_tx, kill_tx },
        })
    }

    async fn spawn_tmux(
        &self,
        id: &str,
        req: &SessionSpawnRequest,
    ) -> Result<ManagedSession, SessionMuxError> {
        if !self.config.enable_tmux || !tmux_available().await {
            return Err(SessionMuxError::TmuxUnavailable);
        }

        let session_name = sanitize_tmux_name(id);
        let shell_line = compose_shell_line(req);
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", &session_name, &shell_line])
            .status()
            .await
            .map_err(|e| SessionMuxError::SpawnFailed(format!("tmux spawn failed: {e}")))?;

        if !status.success() {
            return Err(SessionMuxError::SpawnFailed(format!(
                "tmux new-session failed for {}",
                session_name
            )));
        }

        let meta = Arc::new(RwLock::new(SessionMetadata {
            id: id.to_string(),
            mode: SessionMode::Tmux,
            status: SessionStatus::Running,
            command: req.command.clone(),
            args: req.args.clone(),
            cwd: req.cwd.clone(),
            pid: None,
            exit_code: None,
            priority: req.priority,
            started_at: Utc::now(),
            ended_at: None,
        }));
        let ring = Arc::new(RwLock::new(VecDeque::<String>::with_capacity(
            self.config.ring_capacity_lines,
        )));
        let (event_tx, _) = broadcast::channel::<SessionEvent>(512);
        let poll_every = self.config.tmux_poll_interval_ms;

        let meta_for_task = Arc::clone(&meta);
        let tx_for_task = event_tx.clone();
        let session_name_for_task = session_name.clone();
        let session_id_for_task = id.to_string();
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_millis(poll_every)).await;
                let alive = tmux_has_session(&session_name_for_task).await;
                if !alive {
                    let mut meta = meta_for_task.write().await;
                    if meta.status == SessionStatus::Running || meta.status == SessionStatus::Pending {
                        meta.status = SessionStatus::Completed;
                        meta.ended_at = Some(Utc::now());
                    }
                    let _ = tx_for_task.send(SessionEvent {
                        session_id: session_id_for_task.clone(),
                        at: Utc::now(),
                        kind: SessionEventKind::Status {
                            status: meta.status,
                        },
                    });
                    let _ = tx_for_task.send(SessionEvent {
                        session_id: session_id_for_task,
                        at: Utc::now(),
                        kind: SessionEventKind::Completed {
                            exit_code: meta.exit_code,
                        },
                    });
                    break;
                }
            }
        });

        Ok(ManagedSession {
            metadata: meta,
            ring,
            event_tx,
            control: SessionControl::Tmux { session_name },
        })
    }
}

fn spawn_reader_task(
    session_id: String,
    is_stderr: bool,
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    ring: Arc<RwLock<VecDeque<String>>>,
    ring_cap: usize,
    event_tx: broadcast::Sender<SessionEvent>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            {
                let mut guard = ring.write().await;
                if guard.len() >= ring_cap {
                    guard.pop_front();
                }
                guard.push_back(line.clone());
            }
            let _ = event_tx.send(SessionEvent {
                session_id: session_id.clone(),
                at: Utc::now(),
                kind: SessionEventKind::Output { line, is_stderr },
            });
        }
    });
}

async fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn tmux_has_session(session_name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn capture_tmux(
    session_name: &str,
    ring_capacity_lines: usize,
) -> Result<Vec<String>, SessionMuxError> {
    let start = format!("-{}", ring_capacity_lines);
    let output = Command::new("tmux")
        .args(["capture-pane", "-pt", session_name, "-S", &start])
        .output()
        .await
        .map_err(|e| SessionMuxError::SpawnFailed(format!("tmux capture-pane failed: {e}")))?;

    if !output.status.success() {
        return Err(SessionMuxError::SpawnFailed(format!(
            "tmux capture-pane non-zero for {}",
            session_name
        )));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().map(|s| s.to_string()).collect())
}

fn sanitize_tmux_name(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn compose_shell_line(req: &SessionSpawnRequest) -> String {
    let mut parts = Vec::new();
    if let Some(cwd) = &req.cwd {
        parts.push(format!("cd {} &&", shell_escape(cwd)));
    }
    for (k, v) in &req.env {
        parts.push(format!("{}={}", k, shell_escape(v)));
    }
    parts.push(shell_escape(&req.command));
    for arg in &req.args {
        parts.push(shell_escape(arg));
    }
    parts.join(" ")
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_./:=+".contains(ch))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_session_snapshot_contains_output() {
        let mux = SessionMux::new(SessionMuxConfig {
            max_sessions: 2,
            ring_capacity_lines: 50,
            enable_tmux: false,
            tmux_poll_interval_ms: 1000,
        });
        let session_id = mux
            .spawn(SessionSpawnRequest::new("echo").arg("hello-session-mux"))
            .await
            .unwrap();
        sleep(Duration::from_millis(100)).await;
        let snapshot = mux.snapshot(&session_id).await.unwrap();
        assert!(snapshot.lines.join("\n").contains("hello-session-mux"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_session_can_be_killed() {
        let mux = SessionMux::new(SessionMuxConfig {
            max_sessions: 2,
            ring_capacity_lines: 50,
            enable_tmux: false,
            tmux_poll_interval_ms: 1000,
        });
        let session_id = mux
            .spawn(SessionSpawnRequest::new("sleep").arg("5"))
            .await
            .unwrap();
        mux.kill(&session_id).await.unwrap();
        sleep(Duration::from_millis(100)).await;
        let snapshot = mux.snapshot(&session_id).await.unwrap();
        assert!(matches!(
            snapshot.status,
            SessionStatus::Killed | SessionStatus::Failed | SessionStatus::Completed
        ));
    }

    #[test]
    fn shell_escape_handles_quotes() {
        let escaped = shell_escape("a'b c");
        assert_eq!(escaped, "'a'\"'\"'b c'");
    }
}
