//! MVCC Configuration Substrate — Atomic Multi-Registry Transitions.
//!
//! Replaces independent `ArcSwap<T>` fields for channels, providers, skills,
//! agent_registry, and a2a_state with a single versioned `ConfigSnapshot`.
//!
//! ## Problem
//!
//! The current `GatewayState` has 5+ independent `ArcSwap<T>` fields:
//! ```text
//! channels:       ArcSwap<ChannelRegistry>
//! providers:      ArcSwap<ProviderRegistry>
//! skills:         ArcSwap<SkillRegistry>
//! agent_registry: ArcSwap<AgentConfigMap>
//! a2a_state:      ArcSwap<A2AState>
//! ```
//!
//! Swapping these independently creates a window where readers see an
//! inconsistent combination (e.g., new skills referencing a provider
//! that hasn't been swapped yet).
//!
//! ## Solution: MVCC Snapshot
//!
//! A single `ArcSwap<ConfigSnapshot>` holds all registries under one
//! generation number. Writers clone the snapshot, mutate, and atomically
//! swap the entire thing. Readers always see a consistent generation.
//!
//! ## Structural Sharing
//!
//! Each registry within the snapshot is wrapped in `Arc<T>`, so unchanged
//! registries share memory with the previous generation (zero-copy for
//! unmodified registries).

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Generation counter
// ---------------------------------------------------------------------------

/// Monotonically increasing generation number for MVCC versioning.
static GENERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Allocate the next generation number (thread-safe, lock-free).
pub fn next_generation() -> u64 {
    GENERATION_COUNTER.fetch_add(1, Ordering::AcqRel)
}

// ---------------------------------------------------------------------------
// Config snapshot
// ---------------------------------------------------------------------------

/// A versioned, structurally-shared configuration snapshot.
///
/// All registries are wrapped in `Arc` for structural sharing:
/// unchanged registries reuse the same allocation across generations.
///
/// Generic over the registry types to decouple from concrete gateway types.
#[derive(Debug, Clone)]
pub struct ConfigSnapshot<C, P, S, A, T> {
    /// MVCC generation number (monotonically increasing).
    pub generation: u64,
    /// Channels registry.
    pub channels: Arc<C>,
    /// Providers registry.
    pub providers: Arc<P>,
    /// Skills registry.
    pub skills: Arc<S>,
    /// Agent config map.
    pub agents: Arc<A>,
    /// A2A state.
    pub a2a: Arc<T>,
    /// When this generation was created.
    pub created_at: Instant,
    /// SHA-256 fingerprint of the configuration (for change detection).
    pub fingerprint: String,
}

impl<C, P, S, A, T> ConfigSnapshot<C, P, S, A, T>
where
    C: Clone,
    P: Clone,
    S: Clone,
    A: Clone,
    T: Clone,
{
    /// Create the initial snapshot (generation 1).
    pub fn initial(
        channels: C,
        providers: P,
        skills: S,
        agents: A,
        a2a: T,
    ) -> Self {
        Self {
            generation: next_generation(),
            channels: Arc::new(channels),
            providers: Arc::new(providers),
            skills: Arc::new(skills),
            agents: Arc::new(agents),
            a2a: Arc::new(a2a),
            created_at: Instant::now(),
            fingerprint: String::new(),
        }
    }

    /// Fork a new generation from this snapshot with a mutation closure.
    ///
    /// The closure receives a mutable `SnapshotBuilder` that holds `Arc`
    /// references to the current generation's registries. The caller can
    /// selectively replace individual registries while unchanged ones
    /// structurally share with the previous generation.
    pub fn fork<F>(&self, mutate: F) -> Self
    where
        F: FnOnce(&mut SnapshotBuilder<C, P, S, A, T>),
    {
        let mut builder = SnapshotBuilder {
            channels: Arc::clone(&self.channels),
            providers: Arc::clone(&self.providers),
            skills: Arc::clone(&self.skills),
            agents: Arc::clone(&self.agents),
            a2a: Arc::clone(&self.a2a),
        };

        mutate(&mut builder);

        Self {
            generation: next_generation(),
            channels: builder.channels,
            providers: builder.providers,
            skills: builder.skills,
            agents: builder.agents,
            a2a: builder.a2a,
            created_at: Instant::now(),
            fingerprint: String::new(),
        }
    }

    /// Age of this generation in seconds.
    pub fn age_secs(&self) -> u64 {
        self.created_at.elapsed().as_secs()
    }
}

/// Builder for forking a snapshot — allows selective registry replacement.
pub struct SnapshotBuilder<C, P, S, A, T> {
    pub channels: Arc<C>,
    pub providers: Arc<P>,
    pub skills: Arc<S>,
    pub agents: Arc<A>,
    pub a2a: Arc<T>,
}

impl<C, P, S, A, T> SnapshotBuilder<C, P, S, A, T> {
    /// Replace the channels registry.
    pub fn set_channels(&mut self, channels: C) {
        self.channels = Arc::new(channels);
    }

    /// Replace the providers registry.
    pub fn set_providers(&mut self, providers: P) {
        self.providers = Arc::new(providers);
    }

    /// Replace the skills registry.
    pub fn set_skills(&mut self, skills: S) {
        self.skills = Arc::new(skills);
    }

    /// Replace the agents config.
    pub fn set_agents(&mut self, agents: A) {
        self.agents = Arc::new(agents);
    }

    /// Replace the A2A state.
    pub fn set_a2a(&mut self, a2a: T) {
        self.a2a = Arc::new(a2a);
    }
}

// ---------------------------------------------------------------------------
// MVCC store
// ---------------------------------------------------------------------------

/// MVCC configuration store backed by `ArcSwap`.
///
/// Provides lock-free reads via `ArcSwap::load()` and generation-tracked
/// writes via `commit()`.
pub struct MvccConfigStore<C, P, S, A, T> {
    /// The current active snapshot (lock-free read via ArcSwap).
    current: ArcSwap<ConfigSnapshot<C, P, S, A, T>>,
    /// History of recent generations for rollback support.
    history: std::sync::Mutex<Vec<Arc<ConfigSnapshot<C, P, S, A, T>>>>,
    /// Maximum history depth.
    max_history: usize,
}

impl<C, P, S, A, T> MvccConfigStore<C, P, S, A, T>
where
    C: Clone + Send + Sync + 'static,
    P: Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
{
    /// Create a new MVCC store with the given initial snapshot.
    pub fn new(initial: ConfigSnapshot<C, P, S, A, T>, max_history: usize) -> Self {
        let arc_initial = Arc::new(initial);
        Self {
            current: ArcSwap::from(Arc::clone(&arc_initial)),
            history: std::sync::Mutex::new(vec![arc_initial]),
            max_history,
        }
    }

    /// Load the current snapshot (lock-free, wait-free read).
    ///
    /// Cost: single `Acquire` atomic load.
    pub fn load(&self) -> Arc<ConfigSnapshot<C, P, S, A, T>> {
        self.current.load_full()
    }

    /// Load the current generation number.
    pub fn generation(&self) -> u64 {
        self.current.load().generation
    }

    /// Commit a new snapshot, atomically replacing the current one.
    ///
    /// The old snapshot is pushed to history for rollback support.
    /// Returns the committed generation number.
    pub fn commit(&self, snapshot: ConfigSnapshot<C, P, S, A, T>) -> u64 {
        let gen = snapshot.generation;
        let arc_snapshot = Arc::new(snapshot);

        // Push to history.
        if let Ok(mut history) = self.history.lock() {
            history.push(Arc::clone(&arc_snapshot));
            // Trim history.
            while history.len() > self.max_history {
                history.remove(0);
            }
        }

        // Atomic swap.
        self.current.store(arc_snapshot);

        info!(generation = gen, "MVCC config snapshot committed");
        gen
    }

    /// Fork the current snapshot, apply mutations, and commit.
    ///
    /// Convenience method combining `load() → fork() → commit()`.
    pub fn fork_and_commit<F>(&self, mutate: F) -> u64
    where
        F: FnOnce(&mut SnapshotBuilder<C, P, S, A, T>),
    {
        let current = self.load();
        let new_snapshot = current.fork(mutate);
        self.commit(new_snapshot)
    }

    /// Roll back to a specific generation.
    ///
    /// Returns Ok(generation) if the rollback target was found, Err otherwise.
    pub fn rollback_to(&self, target_generation: u64) -> Result<u64, MvccError> {
        let history = self.history.lock().map_err(|_| MvccError::LockPoisoned)?;

        let target = history
            .iter()
            .find(|snap| snap.generation == target_generation)
            .ok_or(MvccError::GenerationNotFound(target_generation))?;

        self.current.store(Arc::clone(target));

        info!(
            target_generation,
            "MVCC rollback to generation"
        );

        Ok(target_generation)
    }

    /// Get all available generations in history.
    pub fn available_generations(&self) -> Vec<u64> {
        self.history
            .lock()
            .map(|h| h.iter().map(|s| s.generation).collect())
            .unwrap_or_default()
    }

    /// Get the history depth.
    pub fn history_depth(&self) -> usize {
        self.history
            .lock()
            .map(|h| h.len())
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Transition log
// ---------------------------------------------------------------------------

/// Records what changed between two generations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionRecord {
    /// Source generation.
    pub from_generation: u64,
    /// Target generation.
    pub to_generation: u64,
    /// Which registries were modified.
    pub modified: Vec<String>,
    /// Timestamp of the transition.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Optional reason for the transition.
    pub reason: Option<String>,
}

/// Tracks transitions between generations.
pub struct TransitionLog {
    entries: std::sync::Mutex<Vec<TransitionRecord>>,
    max_entries: usize,
}

impl TransitionLog {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: std::sync::Mutex::new(Vec::new()),
            max_entries,
        }
    }

    /// Log a transition.
    pub fn record(&self, record: TransitionRecord) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.push(record);
            while entries.len() > self.max_entries {
                entries.remove(0);
            }
        }
    }

    /// Get recent transitions.
    pub fn recent(&self, count: usize) -> Vec<TransitionRecord> {
        self.entries
            .lock()
            .map(|e| e.iter().rev().take(count).cloned().collect())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum MvccError {
    #[error("generation {0} not found in history")]
    GenerationNotFound(u64),
    #[error("internal lock poisoned")]
    LockPoisoned,
    #[error("validation failed: {0}")]
    ValidationFailed(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestSnapshot = ConfigSnapshot<String, String, String, String, String>;

    fn make_initial() -> TestSnapshot {
        ConfigSnapshot::initial(
            "channels".into(),
            "providers".into(),
            "skills".into(),
            "agents".into(),
            "a2a".into(),
        )
    }

    #[test]
    fn initial_snapshot_has_generation() {
        let snap = make_initial();
        assert!(snap.generation >= 1);
    }

    #[test]
    fn fork_increments_generation() {
        let snap1 = make_initial();
        let gen1 = snap1.generation;
        let snap2 = snap1.fork(|_| {});
        assert!(snap2.generation > gen1);
    }

    #[test]
    fn fork_shares_unchanged_registries() {
        let snap1 = make_initial();
        let channels_ptr = Arc::as_ptr(&snap1.channels);

        let snap2 = snap1.fork(|b| {
            b.set_skills("new_skills".into());
        });

        // Channels should share the same allocation.
        assert_eq!(
            Arc::as_ptr(&snap2.channels),
            channels_ptr,
            "unchanged registry should share Arc allocation"
        );
        // Skills should be different.
        assert_eq!(*snap2.skills, "new_skills");
    }

    #[test]
    fn mvcc_store_load_and_commit() {
        let store = MvccConfigStore::new(make_initial(), 10);
        let gen1 = store.generation();

        let gen2 = store.fork_and_commit(|b| {
            b.set_providers("new_providers".into());
        });
        assert!(gen2 > gen1);

        let current = store.load();
        assert_eq!(*current.providers, "new_providers");
        assert_eq!(*current.channels, "channels"); // unchanged
    }

    #[test]
    fn mvcc_rollback() {
        let store = MvccConfigStore::new(make_initial(), 10);
        let gen1 = store.generation();

        store.fork_and_commit(|b| {
            b.set_skills("updated_skills".into());
        });

        assert_ne!(*store.load().skills, "skills");

        // Rollback to gen1.
        let result = store.rollback_to(gen1);
        assert!(result.is_ok());
        assert_eq!(*store.load().skills, "skills");
    }

    #[test]
    fn mvcc_history_trimming() {
        let store = MvccConfigStore::new(make_initial(), 3);

        for i in 0..10 {
            store.fork_and_commit(|b| {
                b.set_skills(format!("v{i}"));
            });
        }

        assert!(store.history_depth() <= 3);
    }

    #[test]
    fn rollback_to_nonexistent_fails() {
        let store = MvccConfigStore::new(make_initial(), 3);
        let result = store.rollback_to(99999);
        assert!(result.is_err());
    }

    #[test]
    fn transition_log() {
        let log = TransitionLog::new(5);
        log.record(TransitionRecord {
            from_generation: 1,
            to_generation: 2,
            modified: vec!["skills".into()],
            timestamp: chrono::Utc::now(),
            reason: Some("skill reload".into()),
        });
        let recent = log.recent(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].from_generation, 1);
    }
}
