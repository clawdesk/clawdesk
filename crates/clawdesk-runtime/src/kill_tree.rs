//! Kill-tree: recursive process group termination.
//!
//! When an agent spawns a subprocess that itself spawns children (e.g.,
//! `bash -c "node server.js"`), killing only the top-level PID leaves
//! orphaned children. Kill-tree walks the process tree depth-first and
//! terminates all descendants before the target process.
//!
//! ## Algorithm
//!
//! ```text
//! kill_tree(pid):
//!   for child in children(pid):
//!     kill_tree(child)
//!   kill(pid, signal)
//! ```
//!
//! O(n) where n = total descendant processes.

use std::collections::HashSet;

/// Signal to send when killing processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSignal {
    /// Graceful termination (SIGTERM on Unix).
    Term,
    /// Forceful kill (SIGKILL on Unix).
    Kill,
    /// Interrupt (SIGINT on Unix).
    Interrupt,
}

/// Result of a kill-tree operation.
#[derive(Debug, Clone)]
pub struct KillTreeResult {
    /// PIDs that were successfully signaled.
    pub killed: Vec<u32>,
    /// PIDs that failed to be signaled, with error messages.
    pub failed: Vec<(u32, String)>,
    /// Total processes found in the tree.
    pub total_found: usize,
}

impl KillTreeResult {
    /// Whether all processes were successfully killed.
    pub fn all_killed(&self) -> bool {
        self.failed.is_empty() && !self.killed.is_empty()
    }
}

/// Kill a process and all its descendants (depth-first).
///
/// On macOS/Linux, uses `ps` to discover child processes. On other platforms,
/// kills only the target PID.
pub fn kill_tree(pid: u32, signal: KillSignal) -> KillTreeResult {
    let mut visited = HashSet::new();
    let mut killed = Vec::new();
    let mut failed = Vec::new();

    // Collect the full tree first (DFS)
    let mut tree = Vec::new();
    collect_descendants(pid, &mut tree, &mut visited);
    let total_found = tree.len() + 1; // descendants + root

    // Kill children first (leaves → root)
    for child_pid in tree.into_iter().rev() {
        match send_signal(child_pid, signal) {
            Ok(()) => killed.push(child_pid),
            Err(e) => failed.push((child_pid, e)),
        }
    }

    // Kill the root process last
    match send_signal(pid, signal) {
        Ok(()) => killed.push(pid),
        Err(e) => failed.push((pid, e)),
    }

    KillTreeResult {
        killed,
        failed,
        total_found,
    }
}

/// Collect all descendant PIDs of the given PID (DFS order).
fn collect_descendants(pid: u32, tree: &mut Vec<u32>, visited: &mut HashSet<u32>) {
    if !visited.insert(pid) {
        return; // Already visited — prevent cycles
    }

    for child in get_children(pid) {
        tree.push(child);
        collect_descendants(child, tree, visited);
    }
}

/// Get direct child PIDs of a process.
///
/// Uses `ps` on Unix systems. Returns empty vec on unsupported platforms.
#[cfg(unix)]
fn get_children(pid: u32) -> Vec<u32> {
    // ps -o pid= --ppid <pid>  (Linux)
    // ps -o pid= -g <pid>      (macOS, but this is process group)
    // Cross-platform: ps -ax -o pid=,ppid= | grep ppid
    let output = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid="])
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout
                .lines()
                .filter_map(|line| {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        let child_pid: u32 = parts[0].parse().ok()?;
                        let parent_pid: u32 = parts[1].parse().ok()?;
                        if parent_pid == pid {
                            Some(child_pid)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect()
        }
        Err(_) => Vec::new(),
    }
}

#[cfg(not(unix))]
fn get_children(_pid: u32) -> Vec<u32> {
    Vec::new()
}

/// Send a signal to a process.
#[cfg(unix)]
fn send_signal(pid: u32, signal: KillSignal) -> Result<(), String> {
    use std::process::Command;

    let sig_str = match signal {
        KillSignal::Term => "-TERM",
        KillSignal::Kill => "-KILL",
        KillSignal::Interrupt => "-INT",
    };

    let output = Command::new("kill")
        .args([sig_str, &pid.to_string()])
        .output()
        .map_err(|e| format!("failed to execute kill: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "No such process" is not an error — it's already dead
        if stderr.contains("No such process") || stderr.contains("no such process") {
            Ok(())
        } else {
            Err(stderr.trim().to_string())
        }
    }
}

#[cfg(not(unix))]
fn send_signal(pid: u32, _signal: KillSignal) -> Result<(), String> {
    Err(format!("kill_tree not supported on this platform for PID {pid}"))
}

/// Kill a process group (PGID). On Unix, sends signal to -pgid.
#[cfg(unix)]
pub fn kill_process_group(pgid: u32, signal: KillSignal) -> Result<(), String> {
    use std::process::Command;

    let sig_str = match signal {
        KillSignal::Term => "-TERM",
        KillSignal::Kill => "-KILL",
        KillSignal::Interrupt => "-INT",
    };

    let output = Command::new("kill")
        .args([sig_str, &format!("-{pgid}")])
        .output()
        .map_err(|e| format!("failed to kill process group: {e}"))?;

    if output.status.success() || String::from_utf8_lossy(&output.stderr).contains("No such process") {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(not(unix))]
pub fn kill_process_group(pgid: u32, _signal: KillSignal) -> Result<(), String> {
    Err(format!("kill_process_group not supported on this platform for PGID {pgid}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kill_signal_variants() {
        assert_ne!(KillSignal::Term, KillSignal::Kill);
        assert_ne!(KillSignal::Kill, KillSignal::Interrupt);
    }

    #[test]
    fn test_kill_tree_result_all_killed() {
        let result = KillTreeResult {
            killed: vec![1, 2, 3],
            failed: vec![],
            total_found: 3,
        };
        assert!(result.all_killed());
    }

    #[test]
    fn test_kill_tree_result_with_failures() {
        let result = KillTreeResult {
            killed: vec![1, 2],
            failed: vec![(3, "error".into())],
            total_found: 3,
        };
        assert!(!result.all_killed());
    }

    #[test]
    fn test_kill_tree_result_empty() {
        let result = KillTreeResult {
            killed: vec![],
            failed: vec![],
            total_found: 0,
        };
        assert!(!result.all_killed()); // No processes killed
    }

    #[test]
    fn test_collect_descendants_no_cycles() {
        let mut tree = Vec::new();
        let mut visited = HashSet::new();
        // PID 0 won't have children in test context
        collect_descendants(0, &mut tree, &mut visited);
        assert!(visited.contains(&0));
    }

    #[test]
    fn test_kill_tree_nonexistent_pid() {
        // PID 999999999 shouldn't exist
        let result = kill_tree(999_999_999, KillSignal::Term);
        // Should not panic — graceful handling
        assert!(result.total_found >= 1); // At least the root
    }

    #[test]
    fn test_get_children_self() {
        // Current process should return some result (even if empty children)
        let pid = std::process::id();
        let children = get_children(pid);
        // We don't assert on content — just that it doesn't crash
        let _ = children;
    }
}
