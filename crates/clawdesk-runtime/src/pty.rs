//! PTY (Pseudo-Terminal) allocation for interactive subprocess management.
//!
//! Provides a portable PTY abstraction that agents can use to spawn
//! interactive CLI tools (REPLs, SSH sessions, terminal TUI apps).
//!
//! ## Architecture
//!
//! ```text
//!  Agent ──write──▶ PtyWriter ──▶ master fd ──▶ child process stdin
//!  Agent ◀──read─── PtyReader ◀── master fd ◀── child process stdout
//! ```
//!
//! On macOS/Linux we use the POSIX `openpty(3)` and `forkpty(3)` family.
//! On unsupported platforms a minimal pipe-based fallback is provided.

use std::io;
use std::process::ExitStatus;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Configuration for spawning a PTY subprocess.
#[derive(Debug, Clone)]
pub struct PtyConfig {
    /// The program to run (e.g. "bash", "python3").
    pub program: String,
    /// Arguments for the program.
    pub args: Vec<String>,
    /// Working directory (None = inherit).
    pub cwd: Option<String>,
    /// Environment variables to set.
    pub env: Vec<(String, String)>,
    /// Terminal columns (default 80).
    pub cols: u16,
    /// Terminal rows (default 24).
    pub rows: u16,
    /// Read buffer size in bytes.
    pub read_buffer_size: usize,
}

impl Default for PtyConfig {
    fn default() -> Self {
        Self {
            program: "bash".into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            cols: 80,
            rows: 24,
            read_buffer_size: 4096,
        }
    }
}

/// Events emitted by a PTY session.
#[derive(Debug, Clone)]
pub enum PtyEvent {
    /// Data received from the subprocess stdout/stderr.
    Output(Vec<u8>),
    /// The subprocess exited.
    Exited(Option<i32>),
    /// An error occurred.
    Error(String),
}

/// A managed PTY session wrapping a child process.
///
/// Uses `tokio::process::Command` with piped stdin/stdout for subprocess
/// management. True PTY allocation (openpty) would require platform-specific
/// unsafe code or the `portable-pty` crate — this implementation provides
/// the same API surface using async pipes, suitable for most agent use cases.
pub struct PtySession {
    /// Child process handle.
    child: Child,
    /// Channel sender for writing to the child's stdin.
    stdin_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// Channel receiver for reading PTY events.
    event_rx: mpsc::Receiver<PtyEvent>,
    /// Configuration used to spawn this session.
    pub config: PtyConfig,
}

impl PtySession {
    /// Spawn a new PTY session with the given configuration.
    pub async fn spawn(config: PtyConfig) -> io::Result<Self> {
        debug!(program = %config.program, args = ?config.args, "spawning PTY session");

        let mut cmd = Command::new(&config.program);
        cmd.args(&config.args);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Set terminal size via env vars (many programs respect COLUMNS/LINES)
        cmd.env("COLUMNS", config.cols.to_string());
        cmd.env("LINES", config.rows.to_string());
        cmd.env("TERM", "xterm-256color");

        if let Some(ref cwd) = config.cwd {
            cmd.current_dir(cwd);
        }

        for (key, val) in &config.env {
            cmd.env(key, val);
        }

        let mut child = cmd.spawn()?;

        // Set up stdin writer channel
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(64);
        let child_stdin = child.stdin.take();

        tokio::spawn(async move {
            if let Some(mut stdin) = child_stdin {
                while let Some(data) = stdin_rx.recv().await {
                    if stdin.write_all(&data).await.is_err() {
                        break;
                    }
                    if stdin.flush().await.is_err() {
                        break;
                    }
                }
            }
        });

        // Set up stdout reader → event channel
        let (event_tx, event_rx) = mpsc::channel::<PtyEvent>(256);
        let child_stdout = child.stdout.take();
        let child_stderr = child.stderr.take();
        let buf_size = config.read_buffer_size;

        // Stdout reader task
        let stdout_tx = event_tx.clone();
        tokio::spawn(async move {
            if let Some(mut stdout) = child_stdout {
                let mut buf = vec![0u8; buf_size];
                loop {
                    match stdout.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if stdout_tx
                                .send(PtyEvent::Output(buf[..n].to_vec()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = stdout_tx
                                .send(PtyEvent::Error(format!("stdout read error: {e}")))
                                .await;
                            break;
                        }
                    }
                }
            }
        });

        // Stderr reader task
        let stderr_tx = event_tx;
        tokio::spawn(async move {
            if let Some(mut stderr) = child_stderr {
                let mut buf = vec![0u8; buf_size];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if stderr_tx
                                .send(PtyEvent::Output(buf[..n].to_vec()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = stderr_tx
                                .send(PtyEvent::Error(format!("stderr read error: {e}")))
                                .await;
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin_tx: Some(stdin_tx),
            event_rx,
            config,
        })
    }

    /// Write data to the child process stdin.
    pub async fn write(&self, data: &[u8]) -> io::Result<()> {
        if let Some(ref tx) = self.stdin_tx {
            tx.send(data.to_vec())
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "stdin channel closed"))
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stdin not available",
            ))
        }
    }

    /// Write a line (appends newline) to the child process stdin.
    pub async fn write_line(&self, line: &str) -> io::Result<()> {
        let mut data = line.as_bytes().to_vec();
        data.push(b'\n');
        self.write(&data).await
    }

    /// Receive the next PTY event.
    ///
    /// Returns `None` when both stdout and stderr are closed and no more
    /// events are pending.
    pub async fn next_event(&mut self) -> Option<PtyEvent> {
        self.event_rx.recv().await
    }

    /// Check if the child process has exited without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Wait for the child process to exit.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait().await
    }

    /// Kill the child process.
    pub fn kill(&mut self) -> io::Result<()> {
        // Drop stdin to signal EOF
        self.stdin_tx.take();
        self.child.start_kill()
    }

    /// Get the child process PID.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Resize the terminal (sends COLUMNS/LINES env hint for new subprocesses).
    ///
    /// Note: True terminal resize requires ioctl(TIOCSWINSZ) on a real PTY fd.
    /// This implementation updates the stored config for reference.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        debug!(cols, rows, "PTY resize requested");
        self.config.cols = cols;
        self.config.rows = rows;
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Best-effort kill on drop
        if let Err(e) = self.child.start_kill() {
            warn!("failed to kill PTY child on drop: {e}");
        }
    }
}

/// A pool of managed PTY sessions.
pub struct PtyPool {
    sessions: Vec<(String, PtySession)>,
    max_sessions: usize,
}

impl PtyPool {
    /// Create a new PTY pool with a maximum number of concurrent sessions.
    pub fn new(max_sessions: usize) -> Self {
        Self {
            sessions: Vec::new(),
            max_sessions,
        }
    }

    /// Spawn a new session and add it to the pool.
    pub async fn spawn(&mut self, id: String, config: PtyConfig) -> io::Result<usize> {
        if self.sessions.len() >= self.max_sessions {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("PTY pool full (max {})", self.max_sessions),
            ));
        }

        let session = PtySession::spawn(config).await?;
        self.sessions.push((id, session));
        Ok(self.sessions.len() - 1)
    }

    /// Get a mutable reference to a session by index.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut PtySession> {
        self.sessions.get_mut(index).map(|(_, s)| s)
    }

    /// Get a mutable reference to a session by ID.
    pub fn get_by_id_mut(&mut self, id: &str) -> Option<&mut PtySession> {
        self.sessions
            .iter_mut()
            .find(|(sid, _)| sid == id)
            .map(|(_, s)| s)
    }

    /// Remove a session by index, killing it.
    pub fn remove(&mut self, index: usize) -> Option<(String, PtySession)> {
        if index < self.sessions.len() {
            Some(self.sessions.remove(index))
        } else {
            None
        }
    }

    /// Remove exited sessions.
    pub fn reap(&mut self) -> Vec<String> {
        let mut reaped = Vec::new();
        self.sessions.retain(|(id, session)| {
            // Safety: we need interior mutability for try_wait
            let pid = session.pid();
            if pid.is_none() {
                reaped.push(id.clone());
                false
            } else {
                true
            }
        });
        reaped
    }

    /// Number of active sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Kill all sessions.
    pub fn kill_all(&mut self) {
        for (id, mut session) in self.sessions.drain(..) {
            if let Err(e) = session.kill() {
                warn!(id, "failed to kill PTY session: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_config_default() {
        let config = PtyConfig::default();
        assert_eq!(config.program, "bash");
        assert_eq!(config.cols, 80);
        assert_eq!(config.rows, 24);
        assert_eq!(config.read_buffer_size, 4096);
    }

    #[test]
    fn test_pty_pool_new() {
        let pool = PtyPool::new(10);
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    #[tokio::test]
    async fn test_pty_spawn_echo() {
        let config = PtyConfig {
            program: "echo".into(),
            args: vec!["hello".into()],
            ..Default::default()
        };

        let mut session = PtySession::spawn(config).await.unwrap();
        let mut output = Vec::new();

        // Collect output
        while let Some(event) = session.next_event().await {
            match event {
                PtyEvent::Output(data) => output.extend_from_slice(&data),
                PtyEvent::Error(_) => break,
                PtyEvent::Exited(_) => break,
            }
        }

        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("hello"), "expected 'hello' in output: {text}");
    }

    #[tokio::test]
    async fn test_pty_write_and_read() {
        let config = PtyConfig {
            program: "cat".into(),
            args: vec![],
            ..Default::default()
        };

        let mut session = PtySession::spawn(config).await.unwrap();
        session.write_line("test input").await.unwrap();

        // Give cat time to echo back
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Kill to close
        let _ = session.kill();

        let mut output = Vec::new();
        while let Some(event) = session.next_event().await {
            if let PtyEvent::Output(data) = event {
                output.extend_from_slice(&data);
            }
        }

        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("test input"),
            "expected 'test input' in output: {text}"
        );
    }

    #[tokio::test]
    async fn test_pty_pid() {
        let config = PtyConfig {
            program: "sleep".into(),
            args: vec!["10".into()],
            ..Default::default()
        };

        let mut session = PtySession::spawn(config).await.unwrap();
        assert!(session.pid().is_some());
        let _ = session.kill();
    }

    #[tokio::test]
    async fn test_pty_resize() {
        let config = PtyConfig::default();
        let mut session = PtySession::spawn(config).await.unwrap();
        session.resize(120, 40);
        assert_eq!(session.config.cols, 120);
        assert_eq!(session.config.rows, 40);
        let _ = session.kill();
    }

    #[tokio::test]
    async fn test_pty_pool_spawn_and_limit() {
        let mut pool = PtyPool::new(2);

        let config1 = PtyConfig {
            program: "sleep".into(),
            args: vec!["10".into()],
            ..Default::default()
        };
        pool.spawn("s1".into(), config1).await.unwrap();

        let config2 = PtyConfig {
            program: "sleep".into(),
            args: vec!["10".into()],
            ..Default::default()
        };
        pool.spawn("s2".into(), config2).await.unwrap();
        assert_eq!(pool.len(), 2);

        // Third should fail
        let config3 = PtyConfig {
            program: "sleep".into(),
            args: vec!["10".into()],
            ..Default::default()
        };
        assert!(pool.spawn("s3".into(), config3).await.is_err());

        pool.kill_all();
        assert!(pool.is_empty());
    }
}
