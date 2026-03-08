//! PTY session manager — real terminal sessions for ClawDesk.
//!
//! Inspired by open-terminal's architecture: each session spawns a real PTY
//! (pseudo-terminal) with the user's default shell, allowing full interactive
//! terminal support including vim, top, python REPL, etc.
//!
//! ## Architecture
//!
//! - `PtySession` owns a master file descriptor and child process
//! - A background tokio task reads from the master fd and broadcasts output
//!   via a Tauri event channel
//! - Input (keystrokes) are written to the master fd from Tauri commands
//! - Resize is handled via `TIOCSWINSZ` ioctl
//! - Sessions are tracked in a `PtySessionManager` (thread-safe HashMap)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Maximum number of concurrent terminal sessions.
const MAX_SESSIONS: usize = 8;

/// Read buffer size for PTY output.
const READ_BUF_SIZE: usize = 4096;

/// Thin wrapper around a raw fd for use with `tokio::io::unix::AsyncFd`.
///
/// `AsyncFd` requires a type that implements `AsRawFd`. This wrapper holds
/// a non-owning raw fd and does NOT close it on drop (the `OwnedFd` in
/// `PtySessionInner` owns the fd's lifetime).
#[cfg(unix)]
struct RawFdWrapper(std::os::fd::RawFd);

#[cfg(unix)]
impl std::os::fd::AsRawFd for RawFdWrapper {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0
    }
}

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSession {
    pub id: String,
    pub pid: u32,
    pub shell: String,
    pub cols: u16,
    pub rows: u16,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalOutput {
    pub session_id: String,
    pub data: String,
}

// ─── Platform-specific PTY implementation ───────────────────────────────────

#[cfg(unix)]
mod platform {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

    /// Create a PTY pair and spawn a shell process.
    /// Returns (master_fd, child_pid, shell_name).
    pub fn spawn_pty_shell(
        cols: u16,
        rows: u16,
        cwd: Option<&str>,
    ) -> Result<(OwnedFd, u32, String), String> {
        // Open PTY pair
        let mut master: RawFd = 0;
        let mut slave: RawFd = 0;

        unsafe {
            if libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) != 0 {
                return Err("openpty failed".into());
            }
        }

        // Set window size on slave
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(slave, libc::TIOCSWINSZ, &ws);
        }

        // Determine shell
        let shell = std::env::var("SHELL").unwrap_or_else(|_| {
            if std::path::Path::new("/bin/zsh").exists() {
                "/bin/zsh".into()
            } else {
                "/bin/sh".into()
            }
        });

        let shell_c = CString::new(shell.as_bytes()).map_err(|e| e.to_string())?;

        // Fork
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            unsafe {
                libc::close(master);
                libc::close(slave);
            }
            return Err("fork failed".into());
        }

        if pid == 0 {
            // ── Child process ──
            unsafe {
                libc::close(master);

                // Create new session
                libc::setsid();

                // Set controlling terminal
                libc::ioctl(slave, libc::TIOCSCTTY as _, 0);

                // Redirect stdin/stdout/stderr to slave
                libc::dup2(slave, 0);
                libc::dup2(slave, 1);
                libc::dup2(slave, 2);

                if slave > 2 {
                    libc::close(slave);
                }

                // Set CWD
                if let Some(dir) = cwd {
                    if let Ok(c) = CString::new(dir) {
                        libc::chdir(c.as_ptr());
                    }
                } else if let Ok(home) = std::env::var("HOME") {
                    if let Ok(c) = CString::new(home.as_bytes()) {
                        libc::chdir(c.as_ptr());
                    }
                }

                // Set TERM
                let term = CString::new("TERM=xterm-256color").unwrap();
                libc::putenv(term.as_ptr() as *mut _);

                // Exec the shell (login shell)
                let shell_basename = shell_c.to_str().unwrap_or("sh");
                let login_name = format!("-{}", shell_basename.rsplit('/').next().unwrap_or("sh"));
                let login_c = CString::new(login_name.as_bytes()).unwrap();
                let argv = [login_c.as_ptr(), std::ptr::null()];
                libc::execv(shell_c.as_ptr(), argv.as_ptr());
                libc::_exit(127);
            }
        }

        // ── Parent process ──
        unsafe {
            libc::close(slave);
        }

        // Set master to non-blocking
        unsafe {
            let flags = libc::fcntl(master, libc::F_GETFL);
            libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }

        let master_fd = unsafe { OwnedFd::from_raw_fd(master) };
        Ok((master_fd, pid as u32, shell))
    }

    /// Resize the PTY window.
    pub fn resize_pty(fd: RawFd, cols: u16, rows: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
        }
    }

    /// Write bytes to the master fd.
    pub fn write_to_pty(fd: RawFd, data: &[u8]) -> Result<(), String> {
        let mut offset = 0;
        while offset < data.len() {
            let n = unsafe {
                libc::write(fd, data[offset..].as_ptr() as *const _, data.len() - offset)
            };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::yield_now();
                    continue;
                }
                return Err(format!("write failed: {err}"));
            }
            offset += n as usize;
        }
        Ok(())
    }

    /// Read available bytes from the master fd (non-blocking).
    /// Returns None if nothing available, empty vec if EOF.
    pub fn read_from_pty(fd: RawFd, buf: &mut [u8]) -> Result<Option<usize>, String> {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            // EIO is normal when child exits
            if err.raw_os_error() == Some(libc::EIO) {
                return Ok(Some(0));
            }
            return Err(format!("read failed: {err}"));
        }
        Ok(Some(n as usize))
    }

    /// Kill the child process.
    pub fn kill_process(pid: u32, force: bool) {
        let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
        unsafe {
            libc::kill(pid as i32, sig);
        }
    }

    /// Check if process is still alive.
    pub fn is_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
}

// ─── Session struct ─────────────────────────────────────────────────────────

struct PtySessionInner {
    id: String,
    #[cfg(unix)]
    master_fd: std::os::fd::OwnedFd,
    pid: u32,
    shell: String,
    cols: u16,
    rows: u16,
    created_at: String,
    shutdown_tx: mpsc::Sender<()>,
}

impl PtySessionInner {
    fn info(&self) -> TerminalSession {
        TerminalSession {
            id: self.id.clone(),
            pid: self.pid,
            shell: self.shell.clone(),
            cols: self.cols,
            rows: self.rows,
            created_at: self.created_at.clone(),
        }
    }
}

// ─── Session Manager ────────────────────────────────────────────────────────

/// Manages all active PTY sessions.
pub struct PtySessionManager {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<PtySessionInner>>>>>,
}

impl PtySessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a new PTY session. Returns session metadata.
    #[cfg(unix)]
    pub async fn create_session(
        &self,
        app_handle: tauri::AppHandle,
        cols: u16,
        rows: u16,
        cwd: Option<String>,
    ) -> Result<TerminalSession, String> {
        let sessions = self.sessions.lock().await;
        if sessions.len() >= MAX_SESSIONS {
            return Err(format!("Maximum {MAX_SESSIONS} terminal sessions reached"));
        }
        drop(sessions);

        let cols = if cols == 0 { 80 } else { cols };
        let rows = if rows == 0 { 24 } else { rows };

        let (master_fd, pid, shell) = platform::spawn_pty_shell(cols, rows, cwd.as_deref())?;
        let id = Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();

        info!(session_id = %id, pid, shell = %shell, cols, rows, "PTY session created");

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        let session = Arc::new(Mutex::new(PtySessionInner {
            id: id.clone(),
            master_fd,
            pid,
            shell,
            cols,
            rows,
            created_at,
            shutdown_tx,
        }));

        let info = session.lock().await.info();

        // Store session
        self.sessions.lock().await.insert(id.clone(), session.clone());

        // Spawn background reader task.
        //
        // Uses tokio::io::unix::AsyncFd to properly integrate with the async
        // reactor. The task only wakes when data is actually available on the
        // master fd (via epoll/kqueue), eliminating the previous 10ms polling
        // loop that could starve the tokio runtime under heavy load.
        let session_id = id.clone();
        let sessions_ref = self.sessions.clone();

        // Extract the raw fd for AsyncFd — we need to hand ownership of the
        // OwnedFd to the session (for write_input/resize) while giving the
        // reader a non-owning raw fd wrapped in AsyncFd.
        let reader_fd = {
            let sess = session.lock().await;
            use std::os::fd::AsRawFd;
            sess.master_fd.as_raw_fd()
        };

        tokio::spawn(async move {
            use tauri::Emitter;

            // Wrap the raw fd in AsyncFd for reactor-driven readiness.
            // Safety: the fd is valid as long as the session is alive.
            // We check for shutdown (which kills the process and invalidates
            // the fd) before each read.
            let async_fd = match tokio::io::unix::AsyncFd::new(RawFdWrapper(reader_fd)) {
                Ok(fd) => fd,
                Err(e) => {
                    error!(session_id = %session_id, error = %e, "Failed to register PTY fd with async reactor");
                    sessions_ref.lock().await.remove(&session_id);
                    return;
                }
            };

            let mut buf = vec![0u8; READ_BUF_SIZE];

            loop {
                // Check for shutdown signal (non-blocking)
                if shutdown_rx.try_recv().is_ok() {
                    debug!(session_id = %session_id, "PTY reader shutdown signal received");
                    break;
                }

                // Wait for the fd to become readable (reactor-driven, no polling).
                // This uses epoll on Linux, kqueue on macOS — zero CPU when idle.
                let ready = tokio::select! {
                    result = async_fd.readable() => result,
                    _ = shutdown_rx.recv() => {
                        debug!(session_id = %session_id, "PTY reader shutdown via channel");
                        break;
                    }
                };

                let mut guard = match ready {
                    Ok(g) => g,
                    Err(e) => {
                        warn!(session_id = %session_id, error = %e, "AsyncFd readable error");
                        break;
                    }
                };

                // Read all available data in a loop (drain the buffer).
                loop {
                    let n = unsafe {
                        libc::read(reader_fd, buf.as_mut_ptr() as *mut _, buf.len())
                    };

                    if n > 0 {
                        let data = String::from_utf8_lossy(&buf[..n as usize]).to_string();
                        let _ = app_handle.emit("terminal-output", TerminalOutput {
                            session_id: session_id.clone(),
                            data,
                        });
                    } else if n == 0 {
                        // EOF — child exited
                        info!(session_id = %session_id, "PTY child exited (EOF)");
                        let _ = app_handle.emit("terminal-output", TerminalOutput {
                            session_id: session_id.clone(),
                            data: String::new(),
                        });
                        // Break both loops
                        sessions_ref.lock().await.remove(&session_id);
                        info!(session_id = %session_id, "PTY session cleaned up");
                        // Prevent async_fd from being used after cleanup
                        std::mem::forget(async_fd);
                        return;
                    } else {
                        let err = std::io::Error::last_os_error();
                        if err.kind() == std::io::ErrorKind::WouldBlock {
                            // No more data available — tell the reactor to re-arm
                            guard.clear_ready();
                            break;
                        }
                        if err.raw_os_error() == Some(libc::EIO) {
                            // EIO = child exited (normal on macOS/Linux)
                            info!(session_id = %session_id, "PTY child exited (EIO)");
                            let _ = app_handle.emit("terminal-output", TerminalOutput {
                                session_id: session_id.clone(),
                                data: String::new(),
                            });
                            sessions_ref.lock().await.remove(&session_id);
                            info!(session_id = %session_id, "PTY session cleaned up");
                            std::mem::forget(async_fd);
                            return;
                        }
                        warn!(session_id = %session_id, error = %err, "PTY read error");
                        break;
                    }
                }
            }

            // Cleanup
            sessions_ref.lock().await.remove(&session_id);
            info!(session_id = %session_id, "PTY session cleaned up");

            // Don't let AsyncFd close the fd — it's owned by PtySessionInner
            std::mem::forget(async_fd);
        });

        Ok(info)
    }

    /// Write input (keystrokes) to a session.
    #[cfg(unix)]
    pub async fn write_input(&self, session_id: &str, data: &[u8]) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        let sessions = self.sessions.lock().await;
        let session = sessions.get(session_id).ok_or("Session not found")?;
        let sess = session.lock().await;
        let fd = sess.master_fd.as_raw_fd();
        platform::write_to_pty(fd, data)
    }

    /// Resize a session's PTY.
    #[cfg(unix)]
    pub async fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        let sessions = self.sessions.lock().await;
        let session = sessions.get(session_id).ok_or("Session not found")?;
        let mut sess = session.lock().await;
        let fd = sess.master_fd.as_raw_fd();
        platform::resize_pty(fd, cols, rows);
        sess.cols = cols;
        sess.rows = rows;
        debug!(session_id, cols, rows, "PTY resized");
        Ok(())
    }

    /// Kill and remove a session.
    pub async fn kill_session(&self, session_id: &str) -> Result<(), String> {
        let sessions = self.sessions.lock().await;
        let session = sessions.get(session_id).ok_or("Session not found")?;
        let sess = session.lock().await;
        #[cfg(unix)]
        platform::kill_process(sess.pid, false);
        let _ = sess.shutdown_tx.send(()).await;
        drop(sess);
        drop(sessions);
        // Session will be removed by the reader task
        info!(session_id, "PTY session kill requested");
        Ok(())
    }

    /// List all active sessions.
    pub async fn list_sessions(&self) -> Vec<TerminalSession> {
        let sessions = self.sessions.lock().await;
        let mut result = Vec::with_capacity(sessions.len());
        for sess_arc in sessions.values() {
            let sess = sess_arc.lock().await;
            result.push(sess.info());
        }
        result
    }

    /// Clean up dead sessions (called periodically or on demand).
    #[cfg(unix)]
    pub async fn prune_dead(&self) {
        let mut sessions = self.sessions.lock().await;
        let dead: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| {
                // Try to check if process is alive without blocking
                if let Ok(sess) = s.try_lock() {
                    !platform::is_alive(sess.pid)
                } else {
                    false
                }
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &dead {
            sessions.remove(id);
            info!(session_id = %id, "Pruned dead PTY session");
        }
    }
}
