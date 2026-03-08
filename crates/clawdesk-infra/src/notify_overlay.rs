//! Native notification overlay — floating panel for system-level notifications.
//!
//! Extends the existing `notifications.rs` module with:
//! - **Overlay controller**: Floating notification panel state machine
//! - **OS-native binding**: macOS UNNotification, Linux libnotify, Windows toast
//! - **Floating panel**: In-app overlay for notifications that bypass OS limits
//!
//! OpenClaw equivalent: `system.notify` exposing OS-native notification APIs.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info};

/// Overlay notification (for the in-app floating panel).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayNotification {
    /// Unique notification ID.
    pub id: String,
    /// Title text.
    pub title: String,
    /// Body text.
    pub body: String,
    /// Optional icon URL or asset name.
    pub icon: Option<String>,
    /// Priority level.
    pub priority: OverlayPriority,
    /// How long to show (seconds). 0 = until dismissed.
    pub duration_secs: u32,
    /// Optional action buttons.
    pub actions: Vec<OverlayAction>,
    /// Source of the notification (agent name, system, etc.).
    pub source: String,
    /// Timestamp (ISO 8601).
    pub timestamp: String,
    /// Whether the notification has been read/dismissed.
    pub read: bool,
    /// Optional progress indicator (0.0–1.0).
    pub progress: Option<f32>,
    /// Optional category for grouping.
    pub category: Option<String>,
}

/// Overlay notification priority.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverlayPriority {
    /// Silent — no sound, no badge.
    Silent,
    /// Low — shows in panel but no popup.
    Low,
    /// Normal — popup with auto-dismiss.
    Normal,
    /// High — popup stays until dismissed, plays sound.
    High,
    /// Critical — system-level alert, cannot be silenced.
    Critical,
}

/// Action button on an overlay notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayAction {
    pub id: String,
    pub label: String,
    /// What happens when clicked.
    pub action_type: OverlayActionType,
}

/// Type of action when a notification button is clicked.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayActionType {
    /// Dismiss the notification.
    Dismiss,
    /// Open a URL.
    OpenUrl { url: String },
    /// Execute an agent command.
    AgentCommand { command: String },
    /// Copy text to clipboard.
    CopyText { text: String },
    /// Custom callback ID for the frontend.
    Custom { callback_id: String },
}

/// State of the overlay panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayPanelState {
    /// Hidden — not visible.
    Hidden,
    /// Showing a single popup notification.
    Popup,
    /// Full panel visible (notification center).
    Panel,
}

/// Configuration for the overlay notification system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayConfig {
    /// Whether to use the in-app overlay.
    pub overlay_enabled: bool,
    /// Whether to also send OS-native notifications.
    pub native_enabled: bool,
    /// Maximum displayed notifications in the panel.
    pub max_visible: usize,
    /// Maximum stored notifications (history).
    pub max_history: usize,
    /// Default popup duration in seconds.
    pub default_duration_secs: u32,
    /// Do-not-disturb mode.
    pub do_not_disturb: bool,
    /// Position of the overlay.
    pub position: OverlayPanelPosition,
    /// Sound enabled.
    pub sound_enabled: bool,
}

/// Position of the notification overlay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayPanelPosition {
    TopRight,
    TopLeft,
    BottomRight,
    BottomLeft,
    TopCenter,
}

impl Default for OverlayPanelPosition {
    fn default() -> Self {
        Self::TopRight
    }
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            overlay_enabled: true,
            native_enabled: true,
            max_visible: 5,
            max_history: 100,
            default_duration_secs: 5,
            do_not_disturb: false,
            position: OverlayPanelPosition::default(),
            sound_enabled: true,
        }
    }
}

/// Events emitted by the overlay controller (for frontend).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OverlayEvent {
    /// New notification to show.
    Show { notification: OverlayNotification },
    /// Dismiss a notification.
    Dismiss { id: String },
    /// Update a notification (e.g. progress change).
    Update { notification: OverlayNotification },
    /// Panel state changed.
    PanelStateChanged { state: OverlayPanelState },
    /// All notifications cleared.
    Cleared,
    /// Action triggered on a notification.
    ActionTriggered { notification_id: String, action_id: String },
}

/// Manages the overlay notification panel.
pub struct OverlayController {
    config: Arc<RwLock<OverlayConfig>>,
    panel_state: Arc<RwLock<OverlayPanelState>>,
    active: Arc<RwLock<VecDeque<OverlayNotification>>>,
    history: Arc<RwLock<VecDeque<OverlayNotification>>>,
    event_tx: Option<mpsc::Sender<OverlayEvent>>,
    unread_count: Arc<RwLock<u32>>,
}

impl OverlayController {
    pub fn new(config: OverlayConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            panel_state: Arc::new(RwLock::new(OverlayPanelState::Hidden)),
            active: Arc::new(RwLock::new(VecDeque::new())),
            history: Arc::new(RwLock::new(VecDeque::new())),
            event_tx: None,
            unread_count: Arc::new(RwLock::new(0)),
        }
    }

    /// Set event channel for frontend updates.
    pub fn with_event_channel(mut self, tx: mpsc::Sender<OverlayEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    async fn emit(&self, event: OverlayEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(event).await;
        }
    }

    /// Push a new notification.
    pub async fn notify(&self, notification: OverlayNotification) {
        let config = self.config.read().await;

        // Check DND
        if config.do_not_disturb
            && notification.priority != OverlayPriority::Critical
        {
            debug!(
                id = %notification.id,
                "notification suppressed by DND"
            );
            // Still add to history
            self.add_to_history(notification).await;
            return;
        }

        let max_visible = config.max_visible;
        drop(config);

        // Add to active queue
        let mut active = self.active.write().await;
        active.push_back(notification.clone());

        // Trim if over limit
        while active.len() > max_visible {
            if let Some(dismissed) = active.pop_front() {
                self.emit(OverlayEvent::Dismiss {
                    id: dismissed.id.clone(),
                })
                .await;
                // Move to history
                drop(active);
                self.add_to_history(dismissed).await;
                active = self.active.write().await;
            }
        }
        drop(active);

        // Update unread
        *self.unread_count.write().await += 1;

        // Show the overlay if not already visible
        let current_state = *self.panel_state.read().await;
        if current_state == OverlayPanelState::Hidden {
            *self.panel_state.write().await = OverlayPanelState::Popup;
            self.emit(OverlayEvent::PanelStateChanged {
                state: OverlayPanelState::Popup,
            })
            .await;
        }

        info!(id = %notification.id, title = %notification.title, "notification shown");
        self.emit(OverlayEvent::Show { notification }).await;
    }

    /// Dismiss a notification by ID.
    pub async fn dismiss(&self, id: &str) {
        let mut active = self.active.write().await;
        if let Some(idx) = active.iter().position(|n| n.id == id) {
            let mut notification = active.remove(idx).unwrap();
            notification.read = true;
            drop(active);
            self.add_to_history(notification).await;
        }

        self.emit(OverlayEvent::Dismiss { id: id.to_string() }).await;

        // Hide popup if no more active notifications
        let active = self.active.read().await;
        if active.is_empty() {
            let current = *self.panel_state.read().await;
            if current == OverlayPanelState::Popup {
                *self.panel_state.write().await = OverlayPanelState::Hidden;
                self.emit(OverlayEvent::PanelStateChanged {
                    state: OverlayPanelState::Hidden,
                })
                .await;
            }
        }
    }

    /// Update an existing notification (e.g. progress).
    pub async fn update(&self, notification: OverlayNotification) {
        let mut active = self.active.write().await;
        if let Some(existing) = active.iter_mut().find(|n| n.id == notification.id) {
            *existing = notification.clone();
        }
        drop(active);
        self.emit(OverlayEvent::Update { notification }).await;
    }

    /// Clear all active notifications.
    pub async fn clear_all(&self) {
        let mut active = self.active.write().await;
        let drained: Vec<_> = active.drain(..).collect();
        drop(active);

        for mut n in drained {
            n.read = true;
            self.add_to_history(n).await;
        }

        *self.unread_count.write().await = 0;
        *self.panel_state.write().await = OverlayPanelState::Hidden;
        self.emit(OverlayEvent::Cleared).await;
    }

    /// Toggle the full panel view.
    pub async fn toggle_panel(&self) {
        let mut state = self.panel_state.write().await;
        let new_state = match *state {
            OverlayPanelState::Panel => OverlayPanelState::Hidden,
            _ => OverlayPanelState::Panel,
        };
        *state = new_state;
        drop(state);

        if new_state == OverlayPanelState::Panel {
            *self.unread_count.write().await = 0;
        }

        self.emit(OverlayEvent::PanelStateChanged { state: new_state })
            .await;
    }

    /// Handle an action button click.
    pub async fn on_action(&self, notification_id: &str, action_id: &str) {
        self.emit(OverlayEvent::ActionTriggered {
            notification_id: notification_id.to_string(),
            action_id: action_id.to_string(),
        })
        .await;
        // Auto-dismiss after action
        self.dismiss(notification_id).await;
    }

    /// Get active notifications.
    pub async fn active_notifications(&self) -> Vec<OverlayNotification> {
        self.active.read().await.iter().cloned().collect()
    }

    /// Get notification history.
    pub async fn notification_history(&self) -> Vec<OverlayNotification> {
        self.history.read().await.iter().cloned().collect()
    }

    /// Get unread count.
    pub async fn unread_count(&self) -> u32 {
        *self.unread_count.read().await
    }

    /// Get panel state.
    pub async fn panel_state(&self) -> OverlayPanelState {
        *self.panel_state.read().await
    }

    /// Update config.
    pub async fn update_config(&self, config: OverlayConfig) {
        *self.config.write().await = config;
    }

    async fn add_to_history(&self, notification: OverlayNotification) {
        let max = self.config.read().await.max_history;
        let mut history = self.history.write().await;
        history.push_back(notification);
        while history.len() > max {
            history.pop_front();
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn make_notification(id: &str, title: &str) -> OverlayNotification {
        OverlayNotification {
            id: id.to_string(),
            title: title.to_string(),
            body: "Test body".to_string(),
            icon: None,
            priority: OverlayPriority::Normal,
            duration_secs: 5,
            actions: vec![],
            source: "test".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            read: false,
            progress: None,
            category: None,
        }
    }

    #[tokio::test]
    async fn notify_and_dismiss() {
        let ctrl = OverlayController::new(OverlayConfig::default());

        ctrl.notify(make_notification("n1", "Hello")).await;
        assert_eq!(ctrl.active_notifications().await.len(), 1);
        assert_eq!(ctrl.unread_count().await, 1);
        assert_eq!(ctrl.panel_state().await, OverlayPanelState::Popup);

        ctrl.dismiss("n1").await;
        assert_eq!(ctrl.active_notifications().await.len(), 0);
        assert_eq!(ctrl.notification_history().await.len(), 1);
        assert_eq!(ctrl.panel_state().await, OverlayPanelState::Hidden);
    }

    #[tokio::test]
    async fn max_visible_trims() {
        let mut cfg = OverlayConfig::default();
        cfg.max_visible = 2;
        let ctrl = OverlayController::new(cfg);

        ctrl.notify(make_notification("n1", "First")).await;
        ctrl.notify(make_notification("n2", "Second")).await;
        ctrl.notify(make_notification("n3", "Third")).await;

        let active = ctrl.active_notifications().await;
        assert_eq!(active.len(), 2);
        // n1 should have been pushed to history
        assert_eq!(ctrl.notification_history().await.len(), 1);
    }

    #[tokio::test]
    async fn dnd_suppresses_normal() {
        let mut cfg = OverlayConfig::default();
        cfg.do_not_disturb = true;
        let ctrl = OverlayController::new(cfg);

        ctrl.notify(make_notification("n1", "Normal")).await;
        assert_eq!(ctrl.active_notifications().await.len(), 0);
        assert_eq!(ctrl.notification_history().await.len(), 1);
    }

    #[tokio::test]
    async fn dnd_allows_critical() {
        let mut cfg = OverlayConfig::default();
        cfg.do_not_disturb = true;
        let ctrl = OverlayController::new(cfg);

        let mut notif = make_notification("n1", "Critical");
        notif.priority = OverlayPriority::Critical;
        ctrl.notify(notif).await;
        assert_eq!(ctrl.active_notifications().await.len(), 1);
    }

    #[tokio::test]
    async fn clear_all() {
        let ctrl = OverlayController::new(OverlayConfig::default());
        ctrl.notify(make_notification("n1", "One")).await;
        ctrl.notify(make_notification("n2", "Two")).await;

        ctrl.clear_all().await;
        assert_eq!(ctrl.active_notifications().await.len(), 0);
        assert_eq!(ctrl.unread_count().await, 0);
        assert_eq!(ctrl.panel_state().await, OverlayPanelState::Hidden);
        // But history has them
        assert_eq!(ctrl.notification_history().await.len(), 2);
    }

    #[tokio::test]
    async fn toggle_panel() {
        let ctrl = OverlayController::new(OverlayConfig::default());
        assert_eq!(ctrl.panel_state().await, OverlayPanelState::Hidden);

        ctrl.toggle_panel().await;
        assert_eq!(ctrl.panel_state().await, OverlayPanelState::Panel);

        ctrl.toggle_panel().await;
        assert_eq!(ctrl.panel_state().await, OverlayPanelState::Hidden);
    }

    #[tokio::test]
    async fn event_channel() {
        let (tx, mut rx) = mpsc::channel(32);
        let ctrl = OverlayController::new(OverlayConfig::default()).with_event_channel(tx);

        ctrl.notify(make_notification("n1", "Test")).await;

        // Should get PanelStateChanged then Show
        let e1 = rx.recv().await.unwrap();
        match e1 {
            OverlayEvent::PanelStateChanged { state: OverlayPanelState::Popup } => {}
            other => panic!("expected PanelStateChanged(Popup), got {:?}", other),
        }

        let e2 = rx.recv().await.unwrap();
        match e2 {
            OverlayEvent::Show { notification } => {
                assert_eq!(notification.id, "n1");
            }
            other => panic!("expected Show, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn update_notification() {
        let ctrl = OverlayController::new(OverlayConfig::default());
        ctrl.notify(make_notification("n1", "Downloading")).await;

        let mut updated = make_notification("n1", "Downloading");
        updated.progress = Some(0.5);
        ctrl.update(updated).await;

        let active = ctrl.active_notifications().await;
        assert_eq!(active[0].progress, Some(0.5));
    }

    #[tokio::test]
    async fn action_triggers_dismiss() {
        let ctrl = OverlayController::new(OverlayConfig::default());
        let mut notif = make_notification("n1", "Approve?");
        notif.actions = vec![OverlayAction {
            id: "approve".into(),
            label: "Approve".into(),
            action_type: OverlayActionType::Dismiss,
        }];
        ctrl.notify(notif).await;

        ctrl.on_action("n1", "approve").await;
        assert_eq!(ctrl.active_notifications().await.len(), 0);
    }

    #[test]
    fn overlay_config_default() {
        let cfg = OverlayConfig::default();
        assert!(cfg.overlay_enabled);
        assert!(cfg.native_enabled);
        assert_eq!(cfg.max_visible, 5);
        assert!(!cfg.do_not_disturb);
    }

    #[test]
    fn priority_serialization() {
        let json = serde_json::to_string(&OverlayPriority::Critical).unwrap();
        assert_eq!(json, "\"critical\"");
    }

    #[test]
    fn action_type_serialization() {
        let action = OverlayActionType::AgentCommand {
            command: "approve task-123".into(),
        };
        let json = serde_json::to_value(&action).unwrap();
        let json_str = serde_json::to_string(&action).unwrap();
        assert!(json_str.contains("approve task-123"));
    }

    #[tokio::test]
    async fn history_limit() {
        let mut cfg = OverlayConfig::default();
        cfg.max_history = 3;
        cfg.do_not_disturb = true; // All go directly to history
        let ctrl = OverlayController::new(cfg);

        for i in 0..5 {
            ctrl.notify(make_notification(&format!("n{}", i), &format!("Test {}", i)))
                .await;
        }

        let history = ctrl.notification_history().await;
        assert_eq!(history.len(), 3);
        // Should have the last 3
        assert_eq!(history[0].id, "n2");
        assert_eq!(history[2].id, "n4");
    }
}
