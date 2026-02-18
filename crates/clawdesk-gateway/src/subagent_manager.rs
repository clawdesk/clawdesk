//! Gateway-level sub-agent manager.
//!
//! Tracks all running sub-agents across the gateway, enforcing global
//! limits, providing status queries, and handling cleanup.
//!
//! Uses a concurrent `HashMap` for O(1) lookup by `SubAgentId`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// Sub-agent registry (gateway-level)
// ---------------------------------------------------------------------------

/// Unique sub-agent identifier (mirrors agents crate type).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubAgentId(pub String);

/// State of a managed sub-agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedState {
    Queued,
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

impl ManagedState {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

/// Tracked sub-agent entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentEntry {
    pub id: SubAgentId,
    pub parent_agent: String,
    pub child_agent: String,
    pub state: ManagedState,
    pub depth: u32,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub output_preview: Option<String>,
}

// ---------------------------------------------------------------------------
// Manager configuration
// ---------------------------------------------------------------------------

/// Global sub-agent manager configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentManagerConfig {
    /// Maximum concurrent sub-agents across all agents.
    #[serde(default = "default_global_max")]
    pub global_max_concurrent: usize,
    /// Maximum spawn depth globally.
    #[serde(default = "default_global_depth")]
    pub global_max_depth: u32,
    /// How long to retain completed sub-agent entries (seconds).
    #[serde(default = "default_retention")]
    pub retention_secs: u64,
}

fn default_global_max() -> usize { 50 }
fn default_global_depth() -> u32 { 5 }
fn default_retention() -> u64 { 3600 }

impl Default for SubAgentManagerConfig {
    fn default() -> Self {
        Self {
            global_max_concurrent: default_global_max(),
            global_max_depth: default_global_depth(),
            retention_secs: default_retention(),
        }
    }
}

// ---------------------------------------------------------------------------
// Sub-agent manager
// ---------------------------------------------------------------------------

/// Thread-safe sub-agent manager.
#[derive(Debug, Clone)]
pub struct SubAgentManager {
    config: SubAgentManagerConfig,
    entries: Arc<RwLock<HashMap<String, SubAgentEntry>>>,
    seq_counter: Arc<RwLock<u64>>,
}

impl SubAgentManager {
    /// Create a new manager.
    pub fn new(config: SubAgentManagerConfig) -> Self {
        Self {
            config,
            entries: Arc::new(RwLock::new(HashMap::new())),
            seq_counter: Arc::new(RwLock::new(0)),
        }
    }

    /// Register a new sub-agent. Returns the assigned ID or an error.
    pub fn register(
        &self,
        parent_agent: &str,
        child_agent: &str,
        depth: u32,
    ) -> Result<SubAgentId, String> {
        // Check global depth.
        if depth > self.config.global_max_depth {
            return Err(format!(
                "Global max depth exceeded: {} > {}",
                depth, self.config.global_max_depth
            ));
        }

        let entries = self.entries.read().map_err(|e| e.to_string())?;
        let active = entries.values().filter(|e| e.state.is_active()).count();
        if active >= self.config.global_max_concurrent {
            return Err(format!(
                "Global max concurrent exceeded: {} >= {}",
                active, self.config.global_max_concurrent
            ));
        }
        drop(entries);

        // Allocate ID.
        let seq = {
            let mut counter = self.seq_counter.write().map_err(|e| e.to_string())?;
            *counter += 1;
            *counter
        };

        let id = SubAgentId(format!("{parent_agent}::{child_agent}::{seq}"));
        let entry = SubAgentEntry {
            id: id.clone(),
            parent_agent: parent_agent.to_string(),
            child_agent: child_agent.to_string(),
            state: ManagedState::Queued,
            depth,
            started_at: None,
            completed_at: None,
            output_preview: None,
        };

        let mut entries = self.entries.write().map_err(|e| e.to_string())?;
        entries.insert(id.0.clone(), entry);
        Ok(id)
    }

    /// Update a sub-agent's state.
    pub fn update_state(&self, id: &SubAgentId, state: ManagedState) -> Result<(), String> {
        let mut entries = self.entries.write().map_err(|e| e.to_string())?;
        let entry = entries.get_mut(&id.0).ok_or("Sub-agent not found")?;
        entry.state = state;
        Ok(())
    }

    /// Set output preview for a completed sub-agent.
    pub fn set_output(&self, id: &SubAgentId, output: &str) -> Result<(), String> {
        let mut entries = self.entries.write().map_err(|e| e.to_string())?;
        let entry = entries.get_mut(&id.0).ok_or("Sub-agent not found")?;
        // Truncate preview to 500 chars.
        let preview = if output.len() > 500 {
            format!("{}…", &output[..500])
        } else {
            output.to_string()
        };
        entry.output_preview = Some(preview);
        Ok(())
    }

    /// Get a sub-agent entry by ID.
    pub fn get(&self, id: &SubAgentId) -> Option<SubAgentEntry> {
        let entries = self.entries.read().ok()?;
        entries.get(&id.0).cloned()
    }

    /// List all sub-agents for a given parent.
    pub fn list_by_parent(&self, parent: &str) -> Vec<SubAgentEntry> {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        entries
            .values()
            .filter(|e| e.parent_agent == parent)
            .cloned()
            .collect()
    }

    /// Count active sub-agents.
    pub fn active_count(&self) -> usize {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        entries.values().filter(|e| e.state.is_active()).count()
    }

    /// Remove all completed/failed entries (garbage collection).
    pub fn gc(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|_, e| e.state.is_active());
        }
    }

    /// Cancel all sub-agents for a parent.
    pub fn cancel_by_parent(&self, parent: &str) -> usize {
        let mut count = 0;
        if let Ok(mut entries) = self.entries.write() {
            for entry in entries.values_mut() {
                if entry.parent_agent == parent && entry.state.is_active() {
                    entry.state = ManagedState::Cancelled;
                    count += 1;
                }
            }
        }
        count
    }

    /// Get global stats.
    pub fn stats(&self) -> ManagerStats {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut stats = ManagerStats::default();
        for entry in entries.values() {
            stats.total += 1;
            match entry.state {
                ManagedState::Queued => stats.queued += 1,
                ManagedState::Running => stats.running += 1,
                ManagedState::Completed => stats.completed += 1,
                ManagedState::Failed => stats.failed += 1,
                ManagedState::TimedOut => stats.timed_out += 1,
                ManagedState::Cancelled => stats.cancelled += 1,
            }
        }
        stats
    }
}

/// Manager-level statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManagerStats {
    pub total: usize,
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub cancelled: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager() -> SubAgentManager {
        SubAgentManager::new(SubAgentManagerConfig {
            global_max_concurrent: 3,
            global_max_depth: 3,
            retention_secs: 60,
        })
    }

    #[test]
    fn test_register_and_get() {
        let mgr = test_manager();
        let id = mgr.register("parent", "child", 1).unwrap();
        let entry = mgr.get(&id).unwrap();
        assert_eq!(entry.parent_agent, "parent");
        assert_eq!(entry.child_agent, "child");
        assert_eq!(entry.state, ManagedState::Queued);
    }

    #[test]
    fn test_update_state() {
        let mgr = test_manager();
        let id = mgr.register("p", "c", 0).unwrap();
        mgr.update_state(&id, ManagedState::Running).unwrap();
        assert_eq!(mgr.get(&id).unwrap().state, ManagedState::Running);

        mgr.update_state(&id, ManagedState::Completed).unwrap();
        assert_eq!(mgr.get(&id).unwrap().state, ManagedState::Completed);
    }

    #[test]
    fn test_depth_limit() {
        let mgr = test_manager();
        let result = mgr.register("p", "c", 4);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("depth"));
    }

    #[test]
    fn test_concurrent_limit() {
        let mgr = test_manager(); // max 3
        mgr.register("p", "a", 0).unwrap();
        mgr.register("p", "b", 0).unwrap();
        mgr.register("p", "c", 0).unwrap();
        let result = mgr.register("p", "d", 0);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("concurrent"));
    }

    #[test]
    fn test_list_by_parent() {
        let mgr = test_manager();
        mgr.register("p1", "a", 0).unwrap();
        mgr.register("p2", "b", 0).unwrap();
        mgr.register("p1", "c", 0).unwrap();

        let p1_children = mgr.list_by_parent("p1");
        assert_eq!(p1_children.len(), 2);
    }

    #[test]
    fn test_gc() {
        let mgr = test_manager();
        let id1 = mgr.register("p", "a", 0).unwrap();
        let id2 = mgr.register("p", "b", 0).unwrap();

        mgr.update_state(&id1, ManagedState::Completed).unwrap();
        mgr.gc();

        assert!(mgr.get(&id1).is_none());
        assert!(mgr.get(&id2).is_some());
    }

    #[test]
    fn test_cancel_by_parent() {
        let mgr = test_manager();
        mgr.register("p1", "a", 0).unwrap();
        mgr.register("p1", "b", 0).unwrap();
        mgr.register("p2", "c", 0).unwrap();

        let cancelled = mgr.cancel_by_parent("p1");
        assert_eq!(cancelled, 2);
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn test_stats() {
        let mgr = test_manager();
        let id1 = mgr.register("p", "a", 0).unwrap();
        let _id2 = mgr.register("p", "b", 0).unwrap();
        mgr.update_state(&id1, ManagedState::Running).unwrap();

        let stats = mgr.stats();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.running, 1);
        assert_eq!(stats.queued, 1);
    }

    #[test]
    fn test_set_output_truncation() {
        let mgr = test_manager();
        let id = mgr.register("p", "c", 0).unwrap();
        let long_output = "x".repeat(1000);
        mgr.set_output(&id, &long_output).unwrap();

        let entry = mgr.get(&id).unwrap();
        let preview = entry.output_preview.unwrap();
        assert!(preview.len() <= 504); // 500 + "…" (3 bytes in UTF-8)
    }

    #[test]
    fn test_managed_state_is_active() {
        assert!(ManagedState::Queued.is_active());
        assert!(ManagedState::Running.is_active());
        assert!(!ManagedState::Completed.is_active());
        assert!(!ManagedState::Failed.is_active());
        assert!(!ManagedState::TimedOut.is_active());
        assert!(!ManagedState::Cancelled.is_active());
    }
}
