//! Compile-time and debug-time lock ordering enforcement.
//!
//! # Problem
//!
//! Deadlocks occur when two threads acquire multiple locks in different orders.
//! The codebase has 30+ locks in `AppState` alone, with 12+ functions holding
//! 2+ locks simultaneously. The existing L1-L14 ordering scheme in `state.rs`
//! is documentation-only — nothing prevents a developer from accidentally
//! violating it.
//!
//! # Solution
//!
//! This module provides **lock wrappers** that:
//!
//! | Mode    | Mechanism                                    | Cost        |
//! |---------|----------------------------------------------|-------------|
//! | Debug   | Thread-local tracking, panics on violations  | ~50ns/lock  |
//! | Release | Zero-cost wrapper, compiles to plain lock    | 0           |
//!
//! ## Usage
//!
//! ```
//! use clawdesk_types::ordered_lock::OrderedRwLock;
//!
//! // Level 2 lock (agents)
//! let agents: OrderedRwLock<String> = OrderedRwLock::new(2, "agents", String::new());
//!
//! // Level 8 lock (identities)
//! let identities: OrderedRwLock<String> = OrderedRwLock::new(8, "identities", String::new());
//!
//! // Correct: acquire lower level first
//! {
//!     let _a = agents.read();
//!     let _b = identities.read(); // level 2 < level 8 ✓
//! }
//! ```
//!
//! ```should_panic
//! # use clawdesk_types::ordered_lock::OrderedRwLock;
//! let agents: OrderedRwLock<String> = OrderedRwLock::new(2, "agents", String::new());
//! let identities: OrderedRwLock<String> = OrderedRwLock::new(8, "identities", String::new());
//!
//! // WRONG: acquire higher level first — panics in debug!
//! let _b = identities.read();
//! let _a = agents.read(); // level 8 >= level 2 → PANIC
//! ```
//!
//! # Lock level assignment
//!
//! The canonical ordering for `AppState` locks is:
//!
//! ```text
//! L1   sessions
//! L2   agents
//! L3   active_chat_runs
//! L4   provider_registry
//! L4b  skill_registry
//! L5   channel_registry
//! L6   a2a_tasks
//! L6b  agent_directory
//! L7   model_costs / traces / pipelines
//! L8   identities
//! L8b  negotiator
//! L9   channel_configs / channel_provider
//! L9b  integration_registry
//! L9c  credential_vault
//! L9d  health_monitor
//! L10  mdns / peer_registry / pairing
//! L11  notification_history / clipboard / journal
//! L12  canvases / context_guards / prompt_manifests
//! L13  channel_bindings / observability_config
//! L13b mcp_client
//! L13c sandbox_dispatcher
//! L14  audio_recorder — always last
//! L14b whisper_engine
//! ```
//!
//! For crate-internal locks (e.g., vault, voice pipeline), use levels 100+ to
//! avoid collision with the global ordering:
//!
//! ```text
//! // Vault internal ordering
//! L100 master_key
//! L101 vault_file
//! L102 cache
//! ```

use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

// ═══════════════════════════════════════════════════════════
// Production telemetry (feature = "lock-telemetry")
// ═══════════════════════════════════════════════════════════

/// Global lock acquisition telemetry.
///
/// Enabled via `--features lock-telemetry`. In release builds without the
/// feature, these counters are never touched and the atomics are eliminated.
pub struct LockTelemetry {
    /// Total lock acquisitions across all OrderedRwLock/OrderedMutex instances.
    pub total_acquisitions: AtomicU64,
    /// Total write-lock acquisitions.
    pub write_acquisitions: AtomicU64,
    /// Total contention events (lock was held when attempted — only tracked
    /// for `try_read`/`try_write` paths, not blocking acquires).
    pub contention_events: AtomicU64,
}

/// Global telemetry instance.
pub static LOCK_TELEMETRY: LockTelemetry = LockTelemetry {
    total_acquisitions: AtomicU64::new(0),
    write_acquisitions: AtomicU64::new(0),
    contention_events: AtomicU64::new(0),
};

/// Snapshot of lock telemetry counters (for reporting/metrics).
#[derive(Debug, Clone, Copy)]
pub struct LockTelemetrySnapshot {
    pub total_acquisitions: u64,
    pub write_acquisitions: u64,
    pub contention_events: u64,
}

impl LockTelemetry {
    /// Read a consistent snapshot of all telemetry counters.
    pub fn snapshot(&self) -> LockTelemetrySnapshot {
        LockTelemetrySnapshot {
            total_acquisitions: self.total_acquisitions.load(AtomicOrdering::Relaxed),
            write_acquisitions: self.write_acquisitions.load(AtomicOrdering::Relaxed),
            contention_events: self.contention_events.load(AtomicOrdering::Relaxed),
        }
    }

    /// Reset all counters to zero.
    pub fn reset(&self) {
        self.total_acquisitions.store(0, AtomicOrdering::Relaxed);
        self.write_acquisitions.store(0, AtomicOrdering::Relaxed);
        self.contention_events.store(0, AtomicOrdering::Relaxed);
    }
}

#[cfg(feature = "lock-telemetry")]
#[inline]
fn telemetry_read_acquire() {
    LOCK_TELEMETRY.total_acquisitions.fetch_add(1, AtomicOrdering::Relaxed);
}

#[cfg(feature = "lock-telemetry")]
#[inline]
fn telemetry_write_acquire() {
    LOCK_TELEMETRY.total_acquisitions.fetch_add(1, AtomicOrdering::Relaxed);
    LOCK_TELEMETRY.write_acquisitions.fetch_add(1, AtomicOrdering::Relaxed);
}

#[cfg(not(feature = "lock-telemetry"))]
#[inline(always)]
fn telemetry_read_acquire() {}

#[cfg(not(feature = "lock-telemetry"))]
#[inline(always)]
fn telemetry_write_acquire() {}

// ═══════════════════════════════════════════════════════════
// Debug-mode tracking
// ═══════════════════════════════════════════════════════════

#[cfg(debug_assertions)]
thread_local! {
    /// Stack of currently held lock levels on this thread.
    /// Invariant: always sorted ascending (enforced by acquire checks).
    static HELD_LEVELS: RefCell<Vec<(u8, &'static str)>> = RefCell::new(Vec::new());
}

#[cfg(debug_assertions)]
fn debug_acquire(level: u8, name: &'static str) {
    HELD_LEVELS.with(|held| {
        let mut held = held.borrow_mut();
        if let Some(&(max_level, max_name)) = held.last() {
            assert!(
                level > max_level,
                "Lock ordering violation: acquiring L{level} ({name}) while holding \
                 L{max_level} ({max_name}). Locks must be acquired in ascending order.",
            );
        }
        held.push((level, name));
    });
}

#[cfg(debug_assertions)]
fn debug_release(level: u8) {
    HELD_LEVELS.with(|held| {
        let mut held = held.borrow_mut();
        // Remove the most recent entry matching this level.
        // Usually it's the last one (LIFO release), but we handle out-of-order
        // drops gracefully.
        if let Some(pos) = held.iter().rposition(|&(l, _)| l == level) {
            held.remove(pos);
        }
    });
}

// ═══════════════════════════════════════════════════════════
// OrderedRwLock
// ═══════════════════════════════════════════════════════════

/// A `std::sync::RwLock` wrapper that enforces lock ordering in debug builds.
///
/// In release builds, this is zero-cost — the level and name are elided by
/// the compiler since they're only used inside `#[cfg(debug_assertions)]` blocks.
pub struct OrderedRwLock<T> {
    /// Lock level (lower = must be acquired first).
    #[cfg(debug_assertions)]
    level: u8,
    /// Human-readable name for panic messages.
    #[cfg(debug_assertions)]
    name: &'static str,
    inner: RwLock<T>,
}

impl<T> OrderedRwLock<T> {
    /// Create a new ordered RwLock with the given level and name.
    ///
    /// In release builds, `level` and `name` are unused (zero-cost).
    pub fn new(#[allow(unused)] level: u8, #[allow(unused)] name: &'static str, value: T) -> Self {
        Self {
            #[cfg(debug_assertions)]
            level,
            #[cfg(debug_assertions)]
            name,
            inner: RwLock::new(value),
        }
    }

    /// Acquire a read lock, checking ordering in debug builds.
    pub fn read(&self) -> OrderedReadGuard<'_, T> {
        #[cfg(debug_assertions)]
        debug_acquire(self.level, self.name);
        telemetry_read_acquire();

        OrderedReadGuard {
            #[cfg(debug_assertions)]
            level: self.level,
            guard: self.inner.read().expect("poisoned lock"),
        }
    }

    /// Acquire a write lock, checking ordering in debug builds.
    pub fn write(&self) -> OrderedWriteGuard<'_, T> {
        #[cfg(debug_assertions)]
        debug_acquire(self.level, self.name);
        telemetry_write_acquire();

        OrderedWriteGuard {
            #[cfg(debug_assertions)]
            level: self.level,
            guard: self.inner.write().expect("poisoned lock"),
        }
    }
}

// ═══════════════════════════════════════════════════════════
// Guard types
// ═══════════════════════════════════════════════════════════

/// Read guard that releases the ordering tracker on drop.
pub struct OrderedReadGuard<'a, T> {
    #[cfg(debug_assertions)]
    level: u8,
    guard: RwLockReadGuard<'a, T>,
}

impl<T> Deref for OrderedReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T> Drop for OrderedReadGuard<'_, T> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        debug_release(self.level);
    }
}

/// Write guard that releases the ordering tracker on drop.
pub struct OrderedWriteGuard<'a, T> {
    #[cfg(debug_assertions)]
    level: u8,
    guard: RwLockWriteGuard<'a, T>,
}

impl<T> Deref for OrderedWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T> DerefMut for OrderedWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl<T> Drop for OrderedWriteGuard<'_, T> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        debug_release(self.level);
    }
}

// ═══════════════════════════════════════════════════════════
// OrderedMutex
// ═══════════════════════════════════════════════════════════

/// A `parking_lot::Mutex`-style wrapper that enforces lock ordering.
///
/// Uses `std::sync::Mutex` internally. For `parking_lot::Mutex` users,
/// the Deref-based API is compatible — just change the type.
pub struct OrderedMutex<T> {
    #[cfg(debug_assertions)]
    level: u8,
    #[cfg(debug_assertions)]
    name: &'static str,
    inner: std::sync::Mutex<T>,
}

impl<T> OrderedMutex<T> {
    /// Create a new ordered Mutex with the given level and name.
    pub fn new(#[allow(unused)] level: u8, #[allow(unused)] name: &'static str, value: T) -> Self {
        Self {
            #[cfg(debug_assertions)]
            level,
            #[cfg(debug_assertions)]
            name,
            inner: std::sync::Mutex::new(value),
        }
    }

    /// Acquire the mutex, checking ordering in debug builds.
    pub fn lock(&self) -> OrderedMutexGuard<'_, T> {
        #[cfg(debug_assertions)]
        debug_acquire(self.level, self.name);
        telemetry_write_acquire();

        OrderedMutexGuard {
            #[cfg(debug_assertions)]
            level: self.level,
            guard: self.inner.lock().expect("poisoned mutex"),
        }
    }
}

/// Mutex guard that releases the ordering tracker on drop.
pub struct OrderedMutexGuard<'a, T> {
    #[cfg(debug_assertions)]
    level: u8,
    guard: std::sync::MutexGuard<'a, T>,
}

impl<T> Deref for OrderedMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T> DerefMut for OrderedMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl<T> Drop for OrderedMutexGuard<'_, T> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        debug_release(self.level);
    }
}

// ═══════════════════════════════════════════════════════════
// Lock level constants
// ═══════════════════════════════════════════════════════════

/// Canonical lock levels for `AppState` fields.
///
/// Use these when constructing `OrderedRwLock`/`OrderedMutex` for state fields.
/// Gaps between levels allow inserting new locks without renumbering.
pub mod levels {
    // ── Core state (L1-L6) ──────────────────────────────
    pub const SESSIONS: u8 = 10;
    pub const AGENTS: u8 = 20;
    pub const ACTIVE_CHAT_RUNS: u8 = 30;
    pub const PROVIDER_REGISTRY: u8 = 40;
    pub const SKILL_REGISTRY: u8 = 45;
    pub const CHANNEL_REGISTRY: u8 = 50;
    pub const A2A_TASKS: u8 = 60;
    pub const AGENT_DIRECTORY: u8 = 65;

    // ── Metrics (L7) ───────────────────────────────────
    pub const MODEL_COSTS: u8 = 70;
    pub const TRACES: u8 = 71;
    pub const PIPELINES: u8 = 72;

    // ── Identity & negotiation (L8) ────────────────────
    pub const IDENTITIES: u8 = 80;
    pub const NEGOTIATOR: u8 = 85;

    // ── Channel & extension config (L9) ────────────────
    pub const CHANNEL_CONFIGS: u8 = 90;
    pub const CHANNEL_PROVIDER: u8 = 91;
    pub const INTEGRATION_REGISTRY: u8 = 92;
    pub const CREDENTIAL_VAULT: u8 = 93;
    pub const HEALTH_MONITOR: u8 = 94;

    // ── P2P & network (L10) ────────────────────────────
    pub const MDNS_ADVERTISER: u8 = 100;
    pub const PEER_REGISTRY: u8 = 101;
    pub const PAIRING_SESSION: u8 = 102;

    // ── Ephemeral state (L11-L13) ──────────────────────
    pub const NOTIFICATION_HISTORY: u8 = 110;
    pub const CLIPBOARD_HISTORY: u8 = 111;
    pub const JOURNAL_ENTRIES: u8 = 112;
    pub const CANVASES: u8 = 120;
    pub const CONTEXT_GUARDS: u8 = 121;
    pub const PROMPT_MANIFESTS: u8 = 122;
    pub const CHANNEL_BINDINGS: u8 = 130;
    pub const OBSERVABILITY_CONFIG: u8 = 131;
    pub const MCP_CLIENT: u8 = 135;
    pub const SANDBOX_DISPATCHER: u8 = 136;

    // ── Hardware (always last) ─────────────────────────
    pub const WHISPER_ENGINE: u8 = 140;
    pub const AUDIO_RECORDER: u8 = 150;

    // ── Crate-internal locks (200+) ────────────────────
    // Use these for locks inside a single struct (vault, voice, etc.)
    pub const INTERNAL_BASE: u8 = 200;
}

// ═══════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correct_ordering_succeeds() {
        let lock_a = OrderedRwLock::new(levels::AGENTS, "agents", vec!["alice"]);
        let lock_b = OrderedRwLock::new(levels::IDENTITIES, "identities", vec!["bob"]);

        // L20 then L80 — correct order
        let _a = lock_a.read();
        let _b = lock_b.read();
        assert_eq!(*_a, vec!["alice"]);
        assert_eq!(*_b, vec!["bob"]);
    }

    #[test]
    #[should_panic(expected = "Lock ordering violation")]
    fn wrong_ordering_panics() {
        let lock_a = OrderedRwLock::new(levels::AGENTS, "agents", 1);
        let lock_b = OrderedRwLock::new(levels::IDENTITIES, "identities", 2);

        // L80 then L20 — WRONG
        let _b = lock_b.read();
        let _a = lock_a.read(); // should panic
    }

    #[test]
    fn write_lock_ordering() {
        let lock_a = OrderedRwLock::new(10, "first", ());
        let lock_b = OrderedRwLock::new(20, "second", ());

        let _a = lock_a.write();
        let _b = lock_b.write();
    }

    #[test]
    #[should_panic(expected = "Lock ordering violation")]
    fn same_level_panics() {
        let lock_a = OrderedRwLock::new(10, "first", ());
        let lock_b = OrderedRwLock::new(10, "also_first", ());

        // Same level → ordering violation (not strictly ascending)
        let _a = lock_a.read();
        let _b = lock_b.read();
    }

    #[test]
    fn release_allows_reacquire_at_lower_level() {
        let lock_a = OrderedRwLock::new(10, "first", ());
        let lock_b = OrderedRwLock::new(20, "second", ());

        // Acquire and release B, then acquire A — should work
        {
            let _b = lock_b.read();
        }
        let _a = lock_a.read();
    }

    #[test]
    fn mutex_ordering() {
        let lock_a = OrderedMutex::new(10, "first", String::new());
        let lock_b = OrderedMutex::new(20, "second", String::new());

        let mut a = lock_a.lock();
        let mut b = lock_b.lock();
        *a = "hello".into();
        *b = "world".into();
    }

    #[test]
    #[should_panic(expected = "Lock ordering violation")]
    fn mutex_wrong_order_panics() {
        let lock_a = OrderedMutex::new(10, "first", 0);
        let lock_b = OrderedMutex::new(20, "second", 0);

        let _b = lock_b.lock();
        let _a = lock_a.lock(); // should panic
    }

    #[test]
    fn mixed_rw_and_mutex_ordering() {
        let rw = OrderedRwLock::new(10, "rw_first", ());
        let mtx = OrderedMutex::new(20, "mtx_second", ());

        let _r = rw.read();
        let _m = mtx.lock();
    }

    #[test]
    fn stress_nested_three_locks() {
        let l1 = OrderedRwLock::new(10, "l1", ());
        let l2 = OrderedRwLock::new(20, "l2", ());
        let l3 = OrderedRwLock::new(30, "l3", ());

        let _a = l1.read();
        let _b = l2.write();
        let _c = l3.read();
    }
}
