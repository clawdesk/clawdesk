//! PTY-backed interactive sandbox execution.
//!
//! Allocates a pseudo-terminal for commands that require terminal interaction
//! (Python REPL, npm prompts, git credential helpers, ANSI-formatted output).
//! Falls back to `Stdio::piped()` if PTY allocation fails.
//!
//! ## Platform support
//!
//! - **Linux/macOS**: native PTY via libc `openpty()`
//! - **Windows**: not supported (falls back to piped)

use crate::{SandboxResult, ResourceUsage};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, warn};

/// Result of a PTY execution — includes whether a PTY was actually allocated.
#[derive(Debug, Clone)]
pub struct PtyResult {
    pub result: SandboxResult,
    /// True if a real PTY was used, false if piped fallback.
    pub pty_allocated: bool,
}

/// Execute a command with PTY allocation and optional input.
///
/// If PTY allocation fails (e.g., resource exhaustion, CI environment),
/// transparently falls back to piped stdio.
pub async fn execute_with_pty(
    command: &str,
    args: &[String],
    working_dir: Option<&std::path::Path>,
    env: &std::collections::HashMap<String, String>,
    input: Option<&str>,
    timeout: Duration,
) -> Result<PtyResult, String> {
    // Try PTY first, fall back to piped on failure
    match try_pty_execute(command, args, working_dir, env, input, timeout).await {
        Ok(result) => Ok(PtyResult {
            result,
            pty_allocated: true,
        }),
        Err(pty_err) => {
            warn!(
                error = %pty_err,
                command,
                "PTY allocation failed, falling back to piped stdio"
            );
            let result = piped_execute(command, args, working_dir, env, input, timeout).await?;
            Ok(PtyResult {
                result,
                pty_allocated: false,
            })
        }
    }
}

/// Attempt PTY-based execution. Only available on Unix.
#[cfg(unix)]
async fn try_pty_execute(
    command: &str,
    args: &[String],
    working_dir: Option<&std::path::Path>,
    env: &std::collections::HashMap<String, String>,
    input: Option<&str>,
    timeout: Duration,
) -> Result<SandboxResult, String> {
    use std::os::unix::io::FromRawFd;

    // Allocate PTY pair
    let pty = unsafe {
        let mut master: libc::c_int = 0;
        let mut slave: libc::c_int = 0;
        let ret = libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if ret != 0 {
            return Err(format!(
                "openpty failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        (master, slave)
    };

    let (master_fd, slave_fd) = pty;

    // Build command with slave PTY as stdin/stdout/stderr
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args);

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    // Sanitized environment
    cmd.env_clear();
    cmd.env("TERM", "xterm-256color");
    cmd.env("LANG", "en_US.UTF-8");
    cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin");
    for (k, v) in env {
        cmd.env(k, v);
    }

    // Connect slave FD as stdio
    unsafe {
        let slave_in = std::fs::File::from_raw_fd(slave_fd);
        let slave_out = slave_in.try_clone().map_err(|e| format!("clone slave fd: {e}"))?;
        let slave_err = slave_in.try_clone().map_err(|e| format!("clone slave fd: {e}"))?;
        cmd.stdin(slave_in);
        cmd.stdout(slave_out);
        cmd.stderr(slave_err);
    }

    let start = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("spawn with PTY: {e}"))?;

    // Close slave fd in parent (child has its own copy)
    unsafe { libc::close(slave_fd); }

    // Wrap master fd for async I/O
    let master_file = unsafe { std::fs::File::from_raw_fd(master_fd) };
    master_file.set_nonblocking(true).map_err(|e| format!("set nonblocking: {e}"))?;
    let mut master_async = tokio::fs::File::from_std(master_file);

    // Send input if provided
    if let Some(input_text) = input {
        let _ = master_async.write_all(input_text.as_bytes()).await;
        let _ = master_async.write_all(b"\n").await;
    }

    // Read output with timeout
    let mut output = Vec::with_capacity(64 * 1024);
    let mut buf = [0u8; 8192];

    let read_result = tokio::time::timeout(timeout, async {
        loop {
            match master_async.read(&mut buf).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    output.extend_from_slice(&buf[..n]);
                    if output.len() > 10 * 1024 * 1024 {
                        break; // Cap at 10MB
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(_) => break,
            }
        }
    })
    .await;

    // Kill if still running
    let exit_code = match child.try_wait() {
        Ok(Some(status)) => status.code().unwrap_or(-1),
        _ => {
            let _ = child.kill().await;
            let status = child.wait().await.map_err(|e| format!("wait: {e}"))?;
            if read_result.is_err() { -1 } else { status.code().unwrap_or(-1) }
        }
    };

    let duration = start.elapsed();
    let output_str = String::from_utf8_lossy(&output).to_string();

    Ok(SandboxResult {
        exit_code,
        stdout: output_str,
        stderr: String::new(), // PTY merges stdout+stderr
        duration,
        resource_usage: ResourceUsage {
            wall_time_ms: duration.as_millis() as u64,
            output_bytes: output.len() as u64,
            ..Default::default()
        },
    })
}

/// PTY not available on non-Unix platforms.
#[cfg(not(unix))]
async fn try_pty_execute(
    _command: &str,
    _args: &[String],
    _working_dir: Option<&std::path::Path>,
    _env: &std::collections::HashMap<String, String>,
    _input: Option<&str>,
    _timeout: Duration,
) -> Result<SandboxResult, String> {
    Err("PTY not supported on this platform".into())
}

/// Standard piped execution fallback.
async fn piped_execute(
    command: &str,
    args: &[String],
    working_dir: Option<&std::path::Path>,
    env: &std::collections::HashMap<String, String>,
    input: Option<&str>,
    timeout: Duration,
) -> Result<SandboxResult, String> {
    use tokio::process::Command;
    use std::process::Stdio;

    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "en_US.UTF-8");

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }

    let start = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;

    if let Some(input_text) = input {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input_text.as_bytes()).await;
        }
    }

    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| "command timed out".to_string())?
        .map_err(|e| format!("wait: {e}"))?;

    let duration = start.elapsed();

    Ok(SandboxResult {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        duration,
        resource_usage: ResourceUsage {
            wall_time_ms: duration.as_millis() as u64,
            output_bytes: (output.stdout.len() + output.stderr.len()) as u64,
            ..Default::default()
        },
    })
}

// Trait for setting non-blocking on a std File
trait SetNonBlocking {
    fn set_nonblocking(&self, nonblocking: bool) -> std::io::Result<()>;
}

#[cfg(unix)]
impl SetNonBlocking for std::fs::File {
    fn set_nonblocking(&self, nonblocking: bool) -> std::io::Result<()> {
        use std::os::unix::io::AsRawFd;
        let fd = self.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = if nonblocking {
                flags | libc::O_NONBLOCK
            } else {
                flags & !libc::O_NONBLOCK
            };
            if libc::fcntl(fd, libc::F_SETFL, flags) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(not(unix))]
impl SetNonBlocking for std::fs::File {
    fn set_nonblocking(&self, _nonblocking: bool) -> std::io::Result<()> {
        Ok(())
    }
}
