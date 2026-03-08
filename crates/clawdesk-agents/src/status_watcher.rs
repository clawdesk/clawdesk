//! # Status Watcher — Push-based agent status transition detection.
//!
//! Replaces pull-based status polling with `tokio::sync::watch` channels for
//! immediate state transition notification. Only transitions are emitted
//! (differential propagation), not full status snapshots.
//!
//! ## Performance
//!
//! `tokio::sync::watch`:
//! - `send()`: O(1) — increment atomic counter, write value under lock
//! - `changed().await`: O(1) amortized — compare version counter, park if equal
//! - Zero-cost when idle: no wakeups, no polls, no syscalls
//!
//! For N sub-agents, total watcher overhead is O(N) per transition check —
//! asymptotically optimal. Previous polling was O(N × poll_rate/s) even idle.

use crate::subagent::{SubAgentId, SubAgentState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use tracing::{debug, info};

// ═══════════════════════════════════════════════════════════════════════════
// Status transition events
// ═══════════════════════════════════════════════════════════════════════════

/// A status transition event emitted when any watched agent changes state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatusTransition {
    pub agent_id: SubAgentId,
    pub from: SubAgentState,
    pub to: SubAgentState,
    pub timestamp: String,
}

// ═══════════════════════════════════════════════════════════════════════════
// Per-agent status watch
// ═══════════════════════════════════════════════════════════════════════════

/// Watch handle for a single agent's status.
///
/// The sender is held by the agent execution context; the receiver is
/// given to watchers (parent orchestrator, TUI, etc.).
pub struct AgentStatusWatch {
    /// Send half — used by the agent to announce state changes.
    tx: watch::Sender<SubAgentState>,
}

impl AgentStatusWatch {
    /// Create a new status watch starting in the given state.
    pub fn new(initial: SubAgentState) -> (Self, watch::Receiver<SubAgentState>) {
        let (tx, rx) = watch::channel(initial);
        (Self { tx }, rx)
    }

    /// Update the agent's state. All receivers are notified.
    pub fn update(&self, new_state: SubAgentState) {
        // send() only fails if all receivers are dropped — that's fine.
        let _ = self.tx.send(new_state);
    }

    /// Subscribe to this agent's status changes (additional receiver).
    pub fn subscribe(&self) -> watch::Receiver<SubAgentState> {
        self.tx.subscribe()
    }

    /// Get the current state without waiting.
    pub fn current(&self) -> SubAgentState {
        *self.tx.borrow()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Status watcher — monitors multiple agents
// ═══════════════════════════════════════════════════════════════════════════

/// Manages status watches for multiple agents and emits transitions.
///
/// The watcher maintains a registry of per-agent watch receivers and
/// broadcasts `AgentStatusTransition` events when any agent changes state.
pub struct StatusWatcher {
    /// Broadcast channel for transition events.
    transition_tx: broadcast::Sender<AgentStatusTransition>,
    /// Registry of agent watches.
    watches: Arc<dashmap::DashMap<String, WatchEntry>>,
    /// Handle to the background monitor task.
    _monitor_handle: Option<tokio::task::JoinHandle<()>>,
}

struct WatchEntry {
    rx: watch::Receiver<SubAgentState>,
    last_state: SubAgentState,
}

impl StatusWatcher {
    /// Create a new status watcher with a broadcast channel for transitions.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(128);
        Self {
            transition_tx: tx,
            watches: Arc::new(dashmap::DashMap::new()),
            _monitor_handle: None,
        }
    }

    /// Subscribe to status transition events.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentStatusTransition> {
        self.transition_tx.subscribe()
    }

    /// Register an agent and its watch receiver.
    pub fn register(
        &self,
        agent_id: &SubAgentId,
        rx: watch::Receiver<SubAgentState>,
        initial_state: SubAgentState,
    ) {
        self.watches.insert(
            agent_id.0.clone(),
            WatchEntry {
                rx,
                last_state: initial_state,
            },
        );
        debug!(agent = %agent_id.0, "registered agent watch");
    }

    /// Unregister an agent's watch.
    pub fn unregister(&self, agent_id: &SubAgentId) {
        self.watches.remove(&agent_id.0);
    }

    /// Poll all watches for state changes (one-shot scan).
    ///
    /// Returns detected transitions. This is the pull-mode API for
    /// callers that prefer explicit checking over background monitoring.
    pub fn poll_transitions(&self) -> Vec<AgentStatusTransition> {
        let mut transitions = Vec::new();
        let timestamp = chrono::Utc::now().to_rfc3339();

        for mut entry in self.watches.iter_mut() {
            let current = *entry.rx.borrow();
            if current != entry.last_state {
                let transition = AgentStatusTransition {
                    agent_id: SubAgentId(entry.key().clone()),
                    from: entry.last_state,
                    to: current,
                    timestamp: timestamp.clone(),
                };
                entry.last_state = current;
                transitions.push(transition);
            }
        }

        // Broadcast transitions
        for t in &transitions {
            if self.transition_tx.receiver_count() > 0 {
                let _ = self.transition_tx.send(t.clone());
            }
        }

        transitions
    }

    /// Start a background monitor that watches all registered agents.
    ///
    /// The monitor uses `tokio::select!` over all watch receivers to wake
    /// only on actual state changes — zero-cost when idle.
    pub fn start_monitor(&mut self) {
        let watches = Arc::clone(&self.watches);
        let tx = self.transition_tx.clone();

        let handle = tokio::spawn(async move {
            loop {
                // Collect current watch receivers
                let entries: Vec<(String, SubAgentState)> = watches
                    .iter()
                    .map(|e| (e.key().clone(), e.last_state))
                    .collect();

                if entries.is_empty() {
                    // No agents to watch — sleep briefly
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }

                // Wait for any change
                let changed = wait_for_any_change(&watches).await;

                if let Some((agent_id, from, to)) = changed {
                    let timestamp = chrono::Utc::now().to_rfc3339();
                    let transition = AgentStatusTransition {
                        agent_id: SubAgentId(agent_id),
                        from,
                        to,
                        timestamp,
                    };

                    if tx.receiver_count() > 0 {
                        let _ = tx.send(transition);
                    }
                }
            }
        });

        self._monitor_handle = Some(handle);
    }

    /// Wait until any registered agent reaches one of the target states.
    ///
    /// Implements `wait_any(handle_ids, target_states)` semantics.
    /// Returns the first agent that matches.
    pub async fn wait_any(
        &self,
        agent_ids: &[SubAgentId],
        target_states: &[SubAgentState],
    ) -> Option<(SubAgentId, SubAgentState)> {
        let mut receivers: Vec<(String, watch::Receiver<SubAgentState>)> = Vec::new();

        for id in agent_ids {
            if let Some(entry) = self.watches.get(&id.0) {
                receivers.push((id.0.clone(), entry.rx.clone()));
            }
        }

        if receivers.is_empty() {
            return None;
        }

        // Check current states first (instant return if already matching)
        for (id, rx) in &receivers {
            let current = *rx.borrow();
            if target_states.contains(&current) {
                return Some((SubAgentId(id.clone()), current));
            }
        }

        // Wait for changes using JoinSet — each receiver is moved into its own task
        let mut set = tokio::task::JoinSet::new();
        for (id, mut rx) in receivers {
            let targets = target_states.to_vec();
            set.spawn(async move {
                loop {
                    if rx.changed().await.is_err() {
                        return None;
                    }
                    let state = *rx.borrow();
                    if targets.contains(&state) {
                        return Some((id, state));
                    }
                }
            });
        }

        // Wait for first matching result
        while let Some(result) = set.join_next().await {
            if let Ok(Some((id, state))) = result {
                set.abort_all();
                return Some((SubAgentId(id), state));
            }
        }

        None
    }
}

/// Wait for any agent in the DashMap to change state.
/// Returns (agent_id, old_state, new_state) or None if all watches are closed.
async fn wait_for_any_change(
    watches: &dashmap::DashMap<String, WatchEntry>,
) -> Option<(String, SubAgentState, SubAgentState)> {
    // Snapshot current states and receivers
    let entries: Vec<(String, SubAgentState, watch::Receiver<SubAgentState>)> = watches
        .iter()
        .map(|e| (e.key().clone(), e.last_state, e.rx.clone()))
        .collect();

    if entries.is_empty() {
        return None;
    }

    // Poll each receiver for changes
    let mut set = tokio::task::JoinSet::new();
    for (id, old_state, mut rx) in entries {
        set.spawn(async move {
            let _ = rx.changed().await;
            let new_state = *rx.borrow();
            (id, old_state, new_state)
        });
    }

    if let Some(Ok((id, old, new))) = set.join_next().await {
        // Update last_state in the watch entry
        if let Some(mut entry) = watches.get_mut(&id) {
            entry.last_state = new;
        }
        if old != new {
            return Some((id, old, new));
        }
    }

    None
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_status_watch_basic() {
        let (watch, mut rx) = AgentStatusWatch::new(SubAgentState::Queued);
        assert_eq!(watch.current(), SubAgentState::Queued);
        assert_eq!(*rx.borrow(), SubAgentState::Queued);

        watch.update(SubAgentState::Running);
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), SubAgentState::Running);
    }

    #[tokio::test]
    async fn test_status_watcher_poll() {
        let watcher = StatusWatcher::new();

        let agent_id = SubAgentId("agent-1".into());
        let (watch, rx) = AgentStatusWatch::new(SubAgentState::Queued);
        watcher.register(&agent_id, rx, SubAgentState::Queued);

        // No transitions yet
        let transitions = watcher.poll_transitions();
        assert!(transitions.is_empty());

        // State change
        watch.update(SubAgentState::Running);

        let transitions = watcher.poll_transitions();
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].from, SubAgentState::Queued);
        assert_eq!(transitions[0].to, SubAgentState::Running);

        // No new transitions
        let transitions = watcher.poll_transitions();
        assert!(transitions.is_empty());
    }

    #[tokio::test]
    async fn test_status_watcher_broadcast() {
        let watcher = StatusWatcher::new();
        let mut rx = watcher.subscribe();

        let agent_id = SubAgentId("agent-2".into());
        let (watch, wrx) = AgentStatusWatch::new(SubAgentState::Queued);
        watcher.register(&agent_id, wrx, SubAgentState::Queued);

        watch.update(SubAgentState::Completed);
        watcher.poll_transitions();

        let transition = rx.recv().await.unwrap();
        assert_eq!(transition.to, SubAgentState::Completed);
    }

    #[tokio::test]
    async fn test_wait_any_immediate() {
        let watcher = StatusWatcher::new();

        let id1 = SubAgentId("a1".into());
        let (watch1, rx1) = AgentStatusWatch::new(SubAgentState::Completed);
        watcher.register(&id1, rx1, SubAgentState::Completed);

        let result = watcher
            .wait_any(
                &[id1.clone()],
                &[SubAgentState::Completed, SubAgentState::Failed],
            )
            .await;

        assert!(result.is_some());
        let (id, state) = result.unwrap();
        assert_eq!(id, id1);
        assert_eq!(state, SubAgentState::Completed);
    }
}
