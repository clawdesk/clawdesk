//! Pipeline-aware session coordination — extends `SessionLaneManager`
//! with cross-system session groups and shared cancellation.
//!
//! ## Session Lane Cross-System Coordination (P2)
//!
//! `SessionLaneManager` (session_lane.rs) serializes agent runs within a
//! single session. But pipelines span multiple sessions (main agent +
//! sub-agents), and legacy sessions need coordinated lifecycle management.
//!
//! This module provides:
//! 1. **Session groups**: Track related sessions (parent + sub-agent sessions).
//! 2. **Shared cancellation**: `CancellationToken` propagated to all sessions
//!    in a group — cancel the parent, all children cancel too.
//! 3. **Cross-system session registration**: Map ClawDesk session IDs to
//!    legacy session keys for coordinated cleanup.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Unique identifier for a session group (typically the root pipeline ID).
pub type GroupId = String;

/// Origin of a session — which system owns it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionOrigin {
    /// Native ClawDesk session.
    ClawDesk,
    /// legacy gateway session.
    OpenClaw,
    /// External A2A agent.
    External { endpoint: String },
}

/// A session within a coordinated group.
#[derive(Debug, Clone)]
pub struct CoordinatedSession {
    /// Session identifier.
    pub session_id: String,
    /// Origin system.
    pub origin: SessionOrigin,
    /// Parent session (None for root).
    pub parent_id: Option<String>,
    /// When this session joined the group.
    pub joined_at: DateTime<Utc>,
    /// Whether this session is still active.
    pub active: bool,
}

/// A group of coordinated sessions sharing a cancellation token.
struct SessionGroup {
    /// Group identifier (root pipeline/session ID).
    group_id: GroupId,
    /// Shared cancellation token for the group.
    cancellation: CancellationToken,
    /// Sessions in this group.
    sessions: HashMap<String, CoordinatedSession>,
    /// When the group was created.
    created_at: DateTime<Utc>,
}

impl SessionGroup {
    fn new(group_id: GroupId) -> Self {
        Self {
            group_id,
            cancellation: CancellationToken::new(),
            sessions: HashMap::new(),
            created_at: Utc::now(),
        }
    }

    fn add_session(&mut self, session: CoordinatedSession) {
        self.sessions.insert(session.session_id.clone(), session);
    }

    fn active_count(&self) -> usize {
        self.sessions.values().filter(|s| s.active).count()
    }
}

/// Summary of a session group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupSummary {
    pub group_id: GroupId,
    pub total_sessions: usize,
    pub active_sessions: usize,
    pub origins: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub is_cancelled: bool,
}

/// Internal state protected by a single mutex (fix: eliminates
/// TOCTOU race between groups and session_to_group lookups).
struct CoordinatorState {
    groups: HashMap<GroupId, SessionGroup>,
    /// Reverse index: session_id → group_id for fast lookup.
    session_to_group: HashMap<String, GroupId>,
}

/// Session coordination manager — tracks groups of related sessions
/// with shared cancellation tokens.
pub struct SessionCoordinator {
    /// Single mutex over both maps — atomic access eliminates the TOCTOU
    /// window where a group could be removed between reading the reverse
    /// index and accessing the groups map.
    state: Mutex<CoordinatorState>,
}

impl SessionCoordinator {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(CoordinatorState {
                groups: HashMap::new(),
                session_to_group: HashMap::new(),
            }),
        }
    }

    /// Create a new session group (typically when a pipeline starts).
    /// Returns the shared cancellation token.
    pub async fn create_group(&self, group_id: impl Into<GroupId>) -> CancellationToken {
        let gid = group_id.into();
        let mut st = self.state.lock().await;
        let group = st.groups
            .entry(gid.clone())
            .or_insert_with(|| SessionGroup::new(gid.clone()));
        info!(group_id = %gid, "session group created");
        group.cancellation.clone()
    }

    /// Register a session in a group.
    ///
    /// Returns the group's cancellation token so the session can observe it.
    pub async fn register_session(
        &self,
        group_id: &str,
        session_id: impl Into<String>,
        origin: SessionOrigin,
        parent_id: Option<String>,
    ) -> Option<CancellationToken> {
        let sid = session_id.into();
        let mut st = self.state.lock().await;
        let group = st.groups.get_mut(group_id)?;

        let session = CoordinatedSession {
            session_id: sid.clone(),
            origin,
            parent_id,
            joined_at: Utc::now(),
            active: true,
        };

        group.add_session(session);
        let token = group.cancellation.clone();

        // Update reverse index (same lock — no TOCTOU gap)
        st.session_to_group.insert(sid.clone(), group_id.to_string());

        debug!(group_id, session_id = %sid, "session registered in group");
        Some(token)
    }

    /// Mark a session as inactive (completed or failed).
    ///
    /// Single lock acquisition — no window between reading
    /// the reverse index and mutating the group where a concurrent
    /// cancel_group/gc could invalidate the group_id.
    pub async fn deactivate_session(&self, session_id: &str) {
        let mut st = self.state.lock().await;
        let group_id = match st.session_to_group.get(session_id) {
            Some(gid) => gid.clone(),
            None => return,
        };

        if let Some(group) = st.groups.get_mut(&group_id) {
            if let Some(session) = group.sessions.get_mut(session_id) {
                session.active = false;
                debug!(group_id = %group_id, session_id, "session deactivated");
            }
        }
    }

    /// Cancel all sessions in a group — propagates to all children.
    pub async fn cancel_group(&self, group_id: &str) -> bool {
        let st = self.state.lock().await;
        if let Some(group) = st.groups.get(group_id) {
            group.cancellation.cancel();
            info!(
                group_id,
                sessions = group.sessions.len(),
                "group cancelled"
            );
            true
        } else {
            warn!(group_id, "cannot cancel — group not found");
            false
        }
    }

    /// Check if a group is cancelled.
    pub async fn is_cancelled(&self, group_id: &str) -> bool {
        let st = self.state.lock().await;
        st.groups
            .get(group_id)
            .map_or(false, |g| g.cancellation.is_cancelled())
    }

    /// Get the group ID for a session.
    pub async fn group_for_session(&self, session_id: &str) -> Option<GroupId> {
        let st = self.state.lock().await;
        st.session_to_group.get(session_id).cloned()
    }

    /// Get a summary of a group.
    pub async fn group_summary(&self, group_id: &str) -> Option<GroupSummary> {
        let st = self.state.lock().await;
        let group = st.groups.get(group_id)?;

        let origins: HashSet<String> = group
            .sessions
            .values()
            .map(|s| format!("{:?}", s.origin))
            .collect();

        Some(GroupSummary {
            group_id: group.group_id.clone(),
            total_sessions: group.sessions.len(),
            active_sessions: group.active_count(),
            origins: origins.into_iter().collect(),
            created_at: group.created_at,
            is_cancelled: group.cancellation.is_cancelled(),
        })
    }

    /// Remove completed groups (all sessions inactive).
    /// Returns the number of groups removed.
    pub async fn gc(&self) -> usize {
        let mut st = self.state.lock().await;

        let to_remove: Vec<GroupId> = st.groups
            .iter()
            .filter(|(_, g)| g.active_count() == 0)
            .map(|(id, _)| id.clone())
            .collect();

        let count = to_remove.len();
        for gid in &to_remove {
            if let Some(group) = st.groups.remove(gid) {
                for sid in group.sessions.keys() {
                    st.session_to_group.remove(sid);
                }
            }
        }

        if count > 0 {
            debug!(removed = count, "session groups garbage collected");
        }
        count
    }

    /// Total number of active groups.
    pub async fn group_count(&self) -> usize {
        let st = self.state.lock().await;
        st.groups.len()
    }
}

impl Default for SessionCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_group_and_register_sessions() {
        let coord = SessionCoordinator::new();
        let token = coord.create_group("pipeline-1").await;
        assert!(!token.is_cancelled());

        let t1 = coord
            .register_session("pipeline-1", "main-session", SessionOrigin::ClawDesk, None)
            .await;
        assert!(t1.is_some());

        let t2 = coord
            .register_session(
                "pipeline-1",
                "sub-agent-1",
                SessionOrigin::OpenClaw,
                Some("main-session".into()),
            )
            .await;
        assert!(t2.is_some());

        let summary = coord.group_summary("pipeline-1").await.unwrap();
        assert_eq!(summary.total_sessions, 2);
        assert_eq!(summary.active_sessions, 2);
    }

    #[tokio::test]
    async fn cancel_propagates_to_all_sessions() {
        let coord = SessionCoordinator::new();
        let _parent_token = coord.create_group("g1").await;

        let child_token = coord
            .register_session("g1", "child-1", SessionOrigin::ClawDesk, None)
            .await
            .unwrap();

        assert!(!child_token.is_cancelled());
        coord.cancel_group("g1").await;
        assert!(child_token.is_cancelled());
        assert!(coord.is_cancelled("g1").await);
    }

    #[tokio::test]
    async fn deactivate_session_updates_active_count() {
        let coord = SessionCoordinator::new();
        coord.create_group("g1").await;
        coord
            .register_session("g1", "s1", SessionOrigin::ClawDesk, None)
            .await;
        coord
            .register_session("g1", "s2", SessionOrigin::OpenClaw, None)
            .await;

        assert_eq!(
            coord.group_summary("g1").await.unwrap().active_sessions,
            2
        );

        coord.deactivate_session("s1").await;
        assert_eq!(
            coord.group_summary("g1").await.unwrap().active_sessions,
            1
        );
    }

    #[tokio::test]
    async fn gc_removes_completed_groups() {
        let coord = SessionCoordinator::new();
        coord.create_group("g1").await;
        coord
            .register_session("g1", "s1", SessionOrigin::ClawDesk, None)
            .await;
        coord.deactivate_session("s1").await;

        coord.create_group("g2").await;
        coord
            .register_session("g2", "s2", SessionOrigin::ClawDesk, None)
            .await;
        // s2 still active

        let removed = coord.gc().await;
        assert_eq!(removed, 1);
        assert_eq!(coord.group_count().await, 1);
        assert!(coord.group_summary("g2").await.is_some());
        assert!(coord.group_summary("g1").await.is_none());
    }

    #[tokio::test]
    async fn cross_system_session_tracking() {
        let coord = SessionCoordinator::new();
        coord.create_group("pipeline-x").await;

        coord
            .register_session(
                "pipeline-x",
                "clawdesk-main",
                SessionOrigin::ClawDesk,
                None,
            )
            .await;
        coord
            .register_session(
                "pipeline-x",
                "openclaw-agent",
                SessionOrigin::OpenClaw,
                Some("clawdesk-main".into()),
            )
            .await;
        coord
            .register_session(
                "pipeline-x",
                "external-helper",
                SessionOrigin::External {
                    endpoint: "https://helper.api".into(),
                },
                Some("clawdesk-main".into()),
            )
            .await;

        let gid = coord.group_for_session("openclaw-agent").await;
        assert_eq!(gid, Some("pipeline-x".into()));

        let summary = coord.group_summary("pipeline-x").await.unwrap();
        assert_eq!(summary.total_sessions, 3);
        assert_eq!(summary.origins.len(), 3);
    }
}
