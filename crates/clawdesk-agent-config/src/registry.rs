//! Thread-safe agent config registry backed by DashMap.
//!
//! Provides O(1) concurrent reads and O(1) amortized writes with
//! fine-grained sharded RwLocks (shard count = CPU cores).

use crate::schema::AgentConfig;
use dashmap::DashMap;
use std::sync::Arc;

/// Unique identifier for a registered agent.
pub type AgentId = String;

/// Thread-safe registry mapping agent names to their parsed configurations.
///
/// Uses `DashMap` with sharded RwLocks for O(1) concurrent access.
pub struct AgentRegistry {
    configs: DashMap<AgentId, Arc<AgentConfig>>,
}

impl AgentRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            configs: DashMap::new(),
        }
    }

    /// Register or update an agent configuration.
    pub fn upsert(&self, config: AgentConfig) -> Arc<AgentConfig> {
        let id = config.agent.name.clone();
        let arc = Arc::new(config);
        self.configs.insert(id, Arc::clone(&arc));
        arc
    }

    /// Remove an agent by name. Returns the removed config if it existed.
    pub fn remove(&self, name: &str) -> Option<Arc<AgentConfig>> {
        self.configs.remove(name).map(|(_, v)| v)
    }

    /// Get an agent config by name.
    pub fn get(&self, name: &str) -> Option<Arc<AgentConfig>> {
        self.configs.get(name).map(|r| Arc::clone(r.value()))
    }

    /// List all registered agent names.
    pub fn list(&self) -> Vec<String> {
        self.configs.iter().map(|r| r.key().clone()).collect()
    }

    /// Number of registered agents.
    pub fn len(&self) -> usize {
        self.configs.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    /// Get all configs as a Vec for iteration.
    pub fn all(&self) -> Vec<Arc<AgentConfig>> {
        self.configs.iter().map(|r| Arc::clone(r.value())).collect()
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}
