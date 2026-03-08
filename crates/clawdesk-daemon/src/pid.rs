//! PID file management for daemon process tracking.
//!
//! The PID file at `~/.clawdesk/clawdesk.pid` tracks the running daemon process.
//! It is created on daemon start and removed on clean shutdown. Stale PID files
//! (where the process no longer exists) are automatically cleaned up.

use crate::DaemonError;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Manages the daemon PID file for process tracking and locking.
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    /// Create a PID file manager for the given path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Default PID file location: `~/.clawdesk/clawdesk.pid`.
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".clawdesk").join("clawdesk.pid")
    }

    /// Write the current process PID to the file.
    ///
    /// Returns `AlreadyRunning` if another daemon is alive at the recorded PID.
    pub fn acquire(&self) -> Result<(), DaemonError> {
        // Check for existing PID file.
        if let Some(existing_pid) = self.read()? {
            if is_process_alive(existing_pid) {
                return Err(DaemonError::AlreadyRunning { pid: existing_pid });
            }
            // Stale PID file — the old process is dead.
            warn!(pid = existing_pid, "removing stale PID file");
            self.release()?;
        }

        let pid = std::process::id();
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, pid.to_string())?;
        debug!(pid, path = %self.path.display(), "PID file acquired");
        Ok(())
    }

    /// Remove the PID file (clean shutdown).
    pub fn release(&self) -> Result<(), DaemonError> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
            debug!(path = %self.path.display(), "PID file released");
        }
        Ok(())
    }

    /// Read the PID from the file, if it exists.
    pub fn read(&self) -> Result<Option<u32>, DaemonError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&self.path).map_err(|e| DaemonError::PidFile {
            detail: format!("read failed: {e}"),
        })?;
        let pid: u32 = content.trim().parse().map_err(|e| DaemonError::PidFile {
            detail: format!("invalid PID '{content}': {e}"),
        })?;
        Ok(Some(pid))
    }

    /// Check if the daemon is currently running based on the PID file.
    pub fn is_running(&self) -> Result<bool, DaemonError> {
        match self.read()? {
            Some(pid) => Ok(is_process_alive(pid)),
            None => Ok(false),
        }
    }

    /// Get the PID of the running daemon, if any.
    pub fn running_pid(&self) -> Result<Option<u32>, DaemonError> {
        match self.read()? {
            Some(pid) if is_process_alive(pid) => Ok(Some(pid)),
            _ => Ok(None),
        }
    }

    /// Path to the PID file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        // Best-effort release on drop — only if our PID matches.
        if let Ok(Some(pid)) = self.read() {
            if pid == std::process::id() {
                let _ = self.release();
            }
        }
    }
}

/// Check if a process with the given PID is alive.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // signal 0 checks existence without sending a signal.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Check if a process with the given PID is alive (Windows stub).
#[cfg(not(unix))]
fn is_process_alive(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_and_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pid");
        let pf = PidFile::new(&path);

        pf.acquire().unwrap();
        assert!(path.exists());

        let pid = pf.read().unwrap().unwrap();
        assert_eq!(pid, std::process::id());
        assert!(pf.is_running().unwrap());

        pf.release().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn stale_pid_file_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pid");

        // Write a bogus PID that doesn't exist.
        std::fs::write(&path, "99999999").unwrap();

        let pf = PidFile::new(&path);
        // Should succeed because 99999999 is not alive.
        pf.acquire().unwrap();

        let pid = pf.read().unwrap().unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn missing_pid_file_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.pid");
        let pf = PidFile::new(&path);

        assert!(!pf.is_running().unwrap());
        assert!(pf.running_pid().unwrap().is_none());
    }
}
