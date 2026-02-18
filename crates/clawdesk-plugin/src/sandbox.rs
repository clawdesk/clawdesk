//! Plugin sandbox — resource limits enforcement and capability checks.

use clawdesk_types::error::PluginError;
use clawdesk_types::plugin::{PluginCapabilityGrant, PluginResourceLimits};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Runtime sandbox enforcing resource limits and capability grants.
pub struct PluginSandbox {
    pub name: String,
    limits: PluginResourceLimits,
    grants: HashSet<PluginCapabilityGrant>,
    /// Current memory usage tracking.
    memory_used: AtomicU64,
    /// Current active task count (shared with TaskGuard via Arc).
    active_tasks: Arc<AtomicU32>,
    /// Current open file descriptors (shared with FdGuard via Arc).
    open_fds: Arc<AtomicU32>,
}

impl PluginSandbox {
    pub fn new(
        name: String,
        limits: PluginResourceLimits,
        grants: HashSet<PluginCapabilityGrant>,
    ) -> Self {
        Self {
            name,
            limits,
            grants,
            memory_used: AtomicU64::new(0),
            active_tasks: Arc::new(AtomicU32::new(0)),
            open_fds: Arc::new(AtomicU32::new(0)),
        }
    }

    // --- Capability checks ---

    /// Check if the plugin has a specific capability grant.
    pub fn has_grant(&self, grant: &PluginCapabilityGrant) -> bool {
        // Full access implies everything.
        if self.grants.contains(&PluginCapabilityGrant::Full) {
            return true;
        }
        self.grants.contains(grant)
    }

    /// Check if the plugin can read a specific path.
    pub fn can_read_file(&self, path: &str) -> bool {
        if self.grants.contains(&PluginCapabilityGrant::Full) {
            return true;
        }
        for grant in &self.grants {
            if let PluginCapabilityGrant::FileRead(paths) = grant {
                if paths.iter().any(|p| path.starts_with(p)) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if the plugin can write to a specific path.
    pub fn can_write_file(&self, path: &str) -> bool {
        if self.grants.contains(&PluginCapabilityGrant::Full) {
            return true;
        }
        for grant in &self.grants {
            if let PluginCapabilityGrant::FileWrite(paths) = grant {
                if paths.iter().any(|p| path.starts_with(p)) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if the plugin can access a network host.
    pub fn can_access_network(&self, host: &str) -> bool {
        if self.grants.contains(&PluginCapabilityGrant::Full) {
            return true;
        }
        for grant in &self.grants {
            if let PluginCapabilityGrant::Network(hosts) = grant {
                if hosts.iter().any(|h| host.ends_with(h)) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if the plugin can read a config key.
    pub fn can_read_config(&self, key: &str) -> bool {
        if self.grants.contains(&PluginCapabilityGrant::Full) {
            return true;
        }
        for grant in &self.grants {
            if let PluginCapabilityGrant::ConfigRead(keys) = grant {
                if keys.iter().any(|k| key.starts_with(k)) {
                    return true;
                }
            }
        }
        false
    }

    // --- Resource limits ---

    /// Try to allocate memory. Returns error if limit exceeded.
    ///
    /// Uses compare-and-swap loop to prevent TOCTOU race: the check and
    /// allocation are atomic — no window where a concurrent caller can
    /// slip past the limit.
    pub fn try_allocate_memory(&self, bytes: u64) -> Result<(), PluginError> {
        loop {
            let current = self.memory_used.load(Ordering::Acquire);
            if current + bytes > self.limits.max_memory_bytes {
                return Err(PluginError::Timeout {
                    name: self.name.clone(),
                    timeout_secs: 0,
                });
            }
            // CAS: only commit if no one else changed the value.
            match self.memory_used.compare_exchange_weak(
                current,
                current + bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(_) => continue, // Retry — another thread changed memory_used.
            }
        }
    }

    /// Release previously allocated memory.
    pub fn release_memory(&self, bytes: u64) {
        self.memory_used
            .fetch_sub(bytes.min(self.memory_used.load(Ordering::Relaxed)), Ordering::Relaxed);
    }

    /// Try to spawn a new task. The returned `TaskGuard` decrements the
    /// sandbox's actual `active_tasks` counter on drop (via shared `Arc`).
    ///
    /// Uses CAS loop to prevent TOCTOU race.
    pub fn try_spawn_task(&self) -> Result<TaskGuard, PluginError> {
        loop {
            let current = self.active_tasks.load(Ordering::Acquire);
            if current >= self.limits.max_tasks {
                return Err(PluginError::Timeout {
                    name: self.name.clone(),
                    timeout_secs: 0,
                });
            }
            match self.active_tasks.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(TaskGuard {
                        counter: Arc::clone(&self.active_tasks),
                    });
                }
                Err(_) => continue,
            }
        }
    }

    /// Try to open a file descriptor. The returned `FdGuard` decrements the
    /// sandbox's actual `open_fds` counter on drop (via shared `Arc`).
    ///
    /// Uses CAS loop to prevent TOCTOU race.
    pub fn try_open_fd(&self) -> Result<FdGuard, PluginError> {
        loop {
            let current = self.open_fds.load(Ordering::Acquire);
            if current >= self.limits.max_fds {
                return Err(PluginError::Timeout {
                    name: self.name.clone(),
                    timeout_secs: 0,
                });
            }
            match self.open_fds.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(FdGuard {
                        counter: Arc::clone(&self.open_fds),
                    });
                }
                Err(_) => continue,
            }
        }
    }

    /// Get current resource usage stats.
    pub fn usage(&self) -> SandboxUsage {
        SandboxUsage {
            memory_bytes: self.memory_used.load(Ordering::Relaxed),
            active_tasks: self.active_tasks.load(Ordering::Relaxed),
            open_fds: self.open_fds.load(Ordering::Relaxed),
        }
    }
}

/// RAII guard that decrements the sandbox's `active_tasks` counter on drop.
/// The `Arc<AtomicU32>` points to the same counter in `PluginSandbox`.
pub struct TaskGuard {
    counter: Arc<AtomicU32>,
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        // Release ordering ensures writes made by this task are visible
        // to subsequent task spawns that read with Acquire.
        self.counter.fetch_sub(1, Ordering::Release);
    }
}

/// RAII guard that decrements the sandbox's `open_fds` counter on drop.
pub struct FdGuard {
    counter: Arc<AtomicU32>,
}

impl Drop for FdGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Release);
    }
}

/// Current resource usage of a sandboxed plugin.
#[derive(Debug, Clone)]
pub struct SandboxUsage {
    pub memory_bytes: u64,
    pub active_tasks: u32,
    pub open_fds: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(grants: HashSet<PluginCapabilityGrant>) -> PluginSandbox {
        PluginSandbox::new("test".to_string(), PluginResourceLimits::default(), grants)
    }

    #[test]
    fn test_file_read_grant() {
        let mut grants = HashSet::new();
        grants.insert(PluginCapabilityGrant::FileRead(vec!["/data".to_string()]));
        let sb = sandbox(grants);
        assert!(sb.can_read_file("/data/file.txt"));
        assert!(!sb.can_read_file("/etc/passwd"));
    }

    #[test]
    fn test_network_grant() {
        let mut grants = HashSet::new();
        grants.insert(PluginCapabilityGrant::Network(vec!["api.example.com".to_string()]));
        let sb = sandbox(grants);
        assert!(sb.can_access_network("api.example.com"));
        assert!(!sb.can_access_network("evil.com"));
    }

    #[test]
    fn test_full_access() {
        let mut grants = HashSet::new();
        grants.insert(PluginCapabilityGrant::Full);
        let sb = sandbox(grants);
        assert!(sb.can_read_file("/any"));
        assert!(sb.can_write_file("/any"));
        assert!(sb.can_access_network("any"));
        assert!(sb.can_read_config("any"));
    }

    #[test]
    fn test_memory_limit() {
        let sb = sandbox(HashSet::new());
        // Default limit is 128MB.
        assert!(sb.try_allocate_memory(64 * 1024 * 1024).is_ok());
        assert!(sb.try_allocate_memory(64 * 1024 * 1024).is_ok());
        assert!(sb.try_allocate_memory(1).is_err()); // Should exceed limit.
    }
}
