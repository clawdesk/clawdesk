//! # Session-Tab Registry — Multi-profile, multi-tab state tracking.
//!
//! Tracks the state of every tab across every browser profile. Provides O(1)
//! lookup by profile+tab and O(1) state transitions.
//!
//! ## State Machine per Tab
//! ```text
//! NoTab → ActiveTab(id) → Switching(from, to) → ActiveTab(to)
//! ```

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Unique identifier for a browser profile.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileId(pub String);

/// Unique identifier for a CDP target (tab).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TargetId(pub String);

/// State of a tab within a profile session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabState {
    /// No tab selected.
    NoTab,
    /// Tab is active and ready for interaction.
    Active {
        target_id: TargetId,
        url: String,
        title: String,
    },
    /// Switching from one tab to another (transient state).
    Switching {
        from: TargetId,
        to: TargetId,
    },
    /// Tab is loading (navigating).
    Loading {
        target_id: TargetId,
        url: String,
    },
    /// Tab has crashed or been closed.
    Closed {
        target_id: TargetId,
    },
}

impl TabState {
    /// Get the current target ID, if any.
    pub fn target_id(&self) -> Option<&TargetId> {
        match self {
            Self::Active { target_id, .. } => Some(target_id),
            Self::Loading { target_id, .. } => Some(target_id),
            Self::Switching { to, .. } => Some(to),
            Self::Closed { target_id } => Some(target_id),
            Self::NoTab => None,
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Active { .. })
    }
}

/// Per-profile session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSession {
    pub profile_id: ProfileId,
    /// Active tab state.
    pub active_tab: TabState,
    /// All known tabs in this profile's browser instance.
    pub tabs: Vec<TabEntry>,
    /// Number of CDP sessions open.
    pub cdp_sessions: u32,
}

/// Entry for a single tab in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabEntry {
    pub target_id: TargetId,
    pub url: String,
    pub title: String,
    pub tab_type: TabType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabType {
    Page,
    BackgroundPage,
    ServiceWorker,
    SharedWorker,
    Other,
}

/// Session-tab registry for concurrent multi-profile management.
///
/// Uses `DashMap` for `O(1)` concurrent reads across non-colliding profile IDs.
pub struct SessionTabRegistry {
    /// ProfileId → ProfileSession for O(1) lookup.
    sessions: DashMap<String, ProfileSession>,
}

impl SessionTabRegistry {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }

    /// Register or update a profile session.
    pub fn upsert_session(&self, session: ProfileSession) {
        let key = session.profile_id.0.clone();
        self.sessions.insert(key, session);
    }

    /// Get the active tab state for a profile.
    pub fn active_tab(&self, profile_id: &str) -> Option<TabState> {
        self.sessions.get(profile_id).map(|s| s.active_tab.clone())
    }

    /// Transition a profile's active tab.
    pub fn set_active_tab(&self, profile_id: &str, state: TabState) {
        if let Some(mut session) = self.sessions.get_mut(profile_id) {
            debug!(profile = profile_id, ?state, "tab state transition");
            session.active_tab = state;
        }
    }

    /// Add a tab to a profile's tab list.
    pub fn add_tab(&self, profile_id: &str, entry: TabEntry) {
        if let Some(mut session) = self.sessions.get_mut(profile_id) {
            // Remove existing entry with same target_id if present.
            session.tabs.retain(|t| t.target_id != entry.target_id);
            session.tabs.push(entry);
        }
    }

    /// Remove a tab from a profile.
    pub fn remove_tab(&self, profile_id: &str, target_id: &TargetId) {
        if let Some(mut session) = self.sessions.get_mut(profile_id) {
            session.tabs.retain(|t| &t.target_id != target_id);
            // If the removed tab was active, transition to NoTab.
            if session.active_tab.target_id() == Some(target_id) {
                session.active_tab = TabState::NoTab;
            }
        }
    }

    /// List all tabs for a profile.
    pub fn list_tabs(&self, profile_id: &str) -> Vec<TabEntry> {
        self.sessions
            .get(profile_id)
            .map(|s| s.tabs.clone())
            .unwrap_or_default()
    }

    /// Number of tracked profiles.
    pub fn profile_count(&self) -> usize {
        self.sessions.len()
    }

    /// Remove a profile session entirely.
    pub fn remove_profile(&self, profile_id: &str) {
        self.sessions.remove(profile_id);
    }
}

impl Default for SessionTabRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(id: &str) -> ProfileSession {
        ProfileSession {
            profile_id: ProfileId(id.to_string()),
            active_tab: TabState::NoTab,
            tabs: vec![],
            cdp_sessions: 0,
        }
    }

    #[test]
    fn upsert_and_lookup() {
        let reg = SessionTabRegistry::new();
        reg.upsert_session(test_session("default"));
        assert_eq!(reg.active_tab("default"), Some(TabState::NoTab));
    }

    #[test]
    fn tab_state_transitions() {
        let reg = SessionTabRegistry::new();
        reg.upsert_session(test_session("p1"));
        reg.set_active_tab("p1", TabState::Active {
            target_id: TargetId("t1".into()),
            url: "https://example.com".into(),
            title: "Example".into(),
        });
        assert!(reg.active_tab("p1").unwrap().is_ready());
    }

    #[test]
    fn remove_active_tab_resets_to_notab() {
        let reg = SessionTabRegistry::new();
        reg.upsert_session(test_session("p1"));
        let tid = TargetId("t1".into());
        reg.add_tab("p1", TabEntry {
            target_id: tid.clone(),
            url: "https://x.com".into(),
            title: "X".into(),
            tab_type: TabType::Page,
        });
        reg.set_active_tab("p1", TabState::Active {
            target_id: tid.clone(),
            url: "https://x.com".into(),
            title: "X".into(),
        });
        reg.remove_tab("p1", &tid);
        assert_eq!(reg.active_tab("p1"), Some(TabState::NoTab));
    }
}
