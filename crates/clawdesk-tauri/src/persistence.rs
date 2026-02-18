//! Disk-backed state persistence — atomic JSON snapshots.
//!
//! Solves the "restart wipes everything" problem by persisting the critical
//! subset of `AppState` to `~/.clawdesk/state.json`. Uses atomic writes
//! (write to `.tmp`, then `rename`) to prevent corruption on crash.
//!
//! ## What is persisted
//!
//! | Field          | Persisted? | Rationale                                        |
//! |----------------|------------|--------------------------------------------------|
//! | agents         | ✅         | Agent definitions are user-created, not re-derivable |
//! | identities     | ✅         | Hash-locked persona contracts                    |
//! | sessions       | ✅         | Conversation history is valuable                 |
//! | pipelines      | ✅         | User-created DAG pipelines                       |
//! | traces         | ❌         | Ephemeral debug data, reconstructable            |
//! | model_costs    | ❌         | Daily counters, reset on restart is acceptable   |
//! | tunnel_metrics | ❌         | Ephemeral network stats                          |
//!
//! ## Atomicity
//!
//! Write path: serialize → write to `state.json.tmp` → `fsync` → `rename`
//! over `state.json`. On POSIX, `rename` is atomic within a filesystem.
//! This guarantees that readers always see either the old or new complete
//! snapshot — never a partial write.

use crate::state::{ChatMessage, DesktopAgent, PipelineDescriptor};
use clawdesk_security::IdentityContract;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

/// Serializable snapshot of the persistent subset of AppState.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Schema version for forward compatibility.
    pub version: u32,
    /// Agents by ID.
    pub agents: HashMap<String, DesktopAgent>,
    /// Identity contracts by agent ID.
    pub identities: HashMap<String, SerializableIdentity>,
    /// Chat sessions by agent ID.
    pub sessions: HashMap<String, Vec<ChatMessage>>,
    /// Pipeline definitions.
    pub pipelines: Vec<PipelineDescriptor>,
}

/// Serializable form of IdentityContract.
/// IdentityContract contains non-serializable fields (Instant), so we
/// extract the serializable subset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableIdentity {
    pub persona: String,
    pub persona_hash: String,
    pub source: String,
    pub version: u64,
}

impl SerializableIdentity {
    pub fn from_contract(contract: &IdentityContract) -> Self {
        Self {
            persona: contract.persona().to_string(),
            persona_hash: contract.persona_hash_hex(),
            source: format!("{:?}", contract.source()),
            version: contract.version(),
        }
    }
}

impl StateSnapshot {
    pub const CURRENT_VERSION: u32 = 1;
}

/// Disk-backed state store with atomic writes.
pub struct DiskStateStore {
    path: PathBuf,
}

impl DiskStateStore {
    /// Create a new store. Creates the parent directory if it doesn't exist.
    pub fn new(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self { path })
    }

    /// Default path: `~/.clawdesk/state.json`
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".clawdesk").join("state.json")
    }

    /// Load state from disk. Returns `None` if the file doesn't exist.
    /// Returns `Err` only on I/O or parse errors (not on missing file).
    pub fn load(&self) -> Result<Option<StateSnapshot>, PersistenceError> {
        if !self.path.exists() {
            info!(path = %self.path.display(), "No state file found, starting fresh");
            return Ok(None);
        }

        let data = std::fs::read_to_string(&self.path).map_err(|e| {
            PersistenceError::ReadFailed {
                path: self.path.clone(),
                detail: e.to_string(),
            }
        })?;

        let snapshot: StateSnapshot =
            serde_json::from_str(&data).map_err(|e| PersistenceError::DeserializationFailed {
                path: self.path.clone(),
                detail: e.to_string(),
            })?;

        if snapshot.version > StateSnapshot::CURRENT_VERSION {
            warn!(
                file_version = snapshot.version,
                current_version = StateSnapshot::CURRENT_VERSION,
                "State file has newer schema version, some data may be lost"
            );
        }

        info!(
            path = %self.path.display(),
            agents = snapshot.agents.len(),
            sessions = snapshot.sessions.len(),
            pipelines = snapshot.pipelines.len(),
            "Loaded state from disk"
        );

        Ok(Some(snapshot))
    }

    /// Persist state to disk atomically.
    ///
    /// Write path: serialize → `.tmp` file → `fsync` → `rename`.
    pub fn save(&self, snapshot: &StateSnapshot) -> Result<(), PersistenceError> {
        let tmp_path = self.path.with_extension("json.tmp");

        let data = serde_json::to_string_pretty(snapshot).map_err(|e| {
            PersistenceError::SerializationFailed {
                detail: e.to_string(),
            }
        })?;

        // Write to temp file
        let mut file = std::fs::File::create(&tmp_path).map_err(|e| {
            PersistenceError::WriteFailed {
                path: tmp_path.clone(),
                detail: e.to_string(),
            }
        })?;

        file.write_all(data.as_bytes()).map_err(|e| {
            PersistenceError::WriteFailed {
                path: tmp_path.clone(),
                detail: e.to_string(),
            }
        })?;

        // Ensure data reaches disk before rename
        file.sync_all().map_err(|e| {
            PersistenceError::WriteFailed {
                path: tmp_path.clone(),
                detail: format!("fsync failed: {}", e),
            }
        })?;

        // Atomic rename (POSIX guarantees atomicity within a filesystem)
        std::fs::rename(&tmp_path, &self.path).map_err(|e| {
            PersistenceError::WriteFailed {
                path: self.path.clone(),
                detail: format!("rename failed: {}", e),
            }
        })?;

        debug!(
            path = %self.path.display(),
            agents = snapshot.agents.len(),
            sessions = snapshot.sessions.len(),
            bytes = data.len(),
            "State persisted to disk"
        );

        Ok(())
    }

    /// Convenience: build a snapshot from AppState fields and save.
    pub fn save_state(
        &self,
        agents: &HashMap<String, DesktopAgent>,
        identities: &HashMap<String, IdentityContract>,
        sessions: &HashMap<String, Vec<ChatMessage>>,
        pipelines: &[PipelineDescriptor],
    ) -> Result<(), PersistenceError> {
        let snapshot = StateSnapshot {
            version: StateSnapshot::CURRENT_VERSION,
            agents: agents.clone(),
            identities: identities
                .iter()
                .map(|(k, v)| (k.clone(), SerializableIdentity::from_contract(v)))
                .collect(),
            sessions: sessions.clone(),
            pipelines: pipelines.to_vec(),
        };
        self.save(&snapshot)
    }
}

/// Persistence-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("failed to read state from {path}: {detail}")]
    ReadFailed { path: PathBuf, detail: String },

    #[error("failed to write state to {path}: {detail}")]
    WriteFailed { path: PathBuf, detail: String },

    #[error("failed to serialize state: {detail}")]
    SerializationFailed { detail: String },

    #[error("failed to deserialize state from {path}: {detail}")]
    DeserializationFailed { path: PathBuf, detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_test_snapshot() -> StateSnapshot {
        let mut agents = HashMap::new();
        agents.insert(
            "test-1".to_string(),
            DesktopAgent {
                id: "test-1".to_string(),
                name: "Test Agent".to_string(),
                icon: "bot".to_string(),
                color: "#ff0000".to_string(),
                persona: "You are a test agent.".to_string(),
                persona_hash: "abc123".to_string(),
                skills: vec!["web-search".to_string()],
                model: "sonnet".to_string(),
                created: "2025-01-01T00:00:00Z".to_string(),
                msg_count: 5,
                status: "ready".to_string(),
                token_budget: 128_000,
                tokens_used: 1000,
                source: "clawdesk".to_string(),
            },
        );

        StateSnapshot {
            version: StateSnapshot::CURRENT_VERSION,
            agents,
            identities: HashMap::new(),
            sessions: HashMap::new(),
            pipelines: vec![],
        }
    }

    #[test]
    fn roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let store = DiskStateStore::new(path).unwrap();

        let snapshot = make_test_snapshot();
        store.save(&snapshot).unwrap();

        let loaded = store.load().unwrap().expect("should load");
        assert_eq!(loaded.version, StateSnapshot::CURRENT_VERSION);
        assert_eq!(loaded.agents.len(), 1);
        assert_eq!(loaded.agents["test-1"].name, "Test Agent");
    }

    #[test]
    fn load_missing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.json");
        let store = DiskStateStore::new(path).unwrap();

        let result = store.load().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn atomic_write_no_partial() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let store = DiskStateStore::new(path.clone()).unwrap();

        // Write initial state
        let snapshot = make_test_snapshot();
        store.save(&snapshot).unwrap();

        // Verify no .tmp file lingers
        let tmp_path = path.with_extension("json.tmp");
        assert!(!tmp_path.exists(), ".tmp file should be cleaned up by rename");

        // Verify the file is valid JSON
        let data = fs::read_to_string(&path).unwrap();
        let _: StateSnapshot = serde_json::from_str(&data).unwrap();
    }
}
