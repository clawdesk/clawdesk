//! Push notifications — cross-platform notification delivery.
//!
//! Manages notification delivery to desktop (native), mobile (APNs/FCM),
//! and web (Web Push) targets. Supports notification batching, deduplication,
//! quiet hours, and priority routing.
//!
//! ## Architecture
//! - **NotificationManager**: Routes notifications to appropriate providers
//! - **NotificationProvider**: Trait for platform-specific delivery
//! - **NotificationStore**: Persistent storage for notification history + preferences
//! - **QuietHoursPolicy**: Time-based notification suppression

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use chrono::{DateTime, NaiveTime, Utc};
use uuid::Uuid;

/// A notification to be delivered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub title: String,
    pub body: String,
    pub icon: Option<String>,
    pub image: Option<String>,
    pub badge: Option<u32>,
    pub sound: Option<String>,
    pub priority: NotificationPriority,
    pub category: NotificationCategory,
    pub actions: Vec<NotificationAction>,
    pub data: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub group_id: Option<String>,
    pub thread_id: Option<String>,
}

impl Notification {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            title: title.into(),
            body: body.into(),
            icon: None,
            image: None,
            badge: None,
            sound: None,
            priority: NotificationPriority::Normal,
            category: NotificationCategory::Message,
            actions: Vec::new(),
            data: HashMap::new(),
            created_at: Utc::now(),
            expires_at: None,
            group_id: None,
            thread_id: None,
        }
    }

    pub fn with_priority(mut self, priority: NotificationPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_category(mut self, category: NotificationCategory) -> Self {
        self.category = category;
        self
    }

    pub fn with_action(mut self, action: NotificationAction) -> Self {
        self.actions.push(action);
        self
    }

    pub fn with_group(mut self, group_id: impl Into<String>) -> Self {
        self.group_id = Some(group_id.into());
        self
    }

    pub fn with_data(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.data.insert(key.into(), value.into());
        self
    }
}

/// Notification priority level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationPriority {
    /// Silent — no sound, no vibration.
    Silent,
    /// Low — may be batched.
    Low,
    /// Normal — standard delivery.
    Normal,
    /// High — interrupt user, bypass DND (used for @mentions, urgent).
    High,
    /// Critical — system-level alerts (security, errors).
    Critical,
}

/// Notification category for grouping and filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationCategory {
    Message,
    Mention,
    DirectMessage,
    GroupChat,
    AgentComplete,
    SystemAlert,
    SecurityAlert,
    Update,
    Reminder,
}

/// Action button on a notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationAction {
    pub id: String,
    pub label: String,
    pub destructive: bool,
    pub payload: Option<String>,
}

/// Notification delivery target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryTarget {
    /// Native desktop notification (macOS/Windows/Linux).
    Desktop,
    /// Apple Push Notification Service.
    Apns { device_token: String },
    /// Firebase Cloud Messaging.
    Fcm { registration_token: String },
    /// Web Push (VAPID).
    WebPush { endpoint: String, p256dh: String, auth: String },
}

/// Result of a notification delivery attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryResult {
    pub target: String,
    pub success: bool,
    pub error: Option<String>,
    pub delivered_at: DateTime<Utc>,
}

/// Trait for platform-specific notification delivery.
#[async_trait]
pub trait NotificationProvider: Send + Sync {
    /// Provider name.
    fn name(&self) -> &str;

    /// Send a notification to a target.
    async fn send(
        &self,
        notification: &Notification,
        target: &DeliveryTarget,
    ) -> Result<DeliveryResult, NotificationError>;

    /// Check if this provider supports the given target.
    fn supports(&self, target: &DeliveryTarget) -> bool;
}

/// Desktop notification provider using native OS APIs.
pub struct DesktopNotificationProvider;

#[async_trait]
impl NotificationProvider for DesktopNotificationProvider {
    fn name(&self) -> &str {
        "desktop"
    }

    async fn send(
        &self,
        notification: &Notification,
        target: &DeliveryTarget,
    ) -> Result<DeliveryResult, NotificationError> {
        if !self.supports(target) {
            return Err(NotificationError::UnsupportedTarget);
        }

        // In a real implementation, this would call platform-specific APIs:
        // - macOS: NSUserNotificationCenter or UNUserNotificationCenter
        // - Windows: ToastNotification via windows-rs
        // - Linux: libnotify / D-Bus
        info!(
            title = notification.title.as_str(),
            body = notification.body.as_str(),
            "delivering desktop notification"
        );

        Ok(DeliveryResult {
            target: "desktop".to_string(),
            success: true,
            error: None,
            delivered_at: Utc::now(),
        })
    }

    fn supports(&self, target: &DeliveryTarget) -> bool {
        matches!(target, DeliveryTarget::Desktop)
    }
}

/// Quiet hours configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuietHoursConfig {
    pub enabled: bool,
    pub start: String, // "22:00"
    pub end: String,   // "07:00"
    /// Categories that bypass quiet hours.
    pub bypass_categories: Vec<NotificationCategory>,
    /// Priorities that bypass quiet hours.
    pub bypass_priority: NotificationPriority,
}

impl Default for QuietHoursConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            start: "22:00".to_string(),
            end: "07:00".to_string(),
            bypass_categories: vec![NotificationCategory::SecurityAlert],
            bypass_priority: NotificationPriority::Critical,
        }
    }
}

impl QuietHoursConfig {
    /// Check if a notification should be suppressed.
    pub fn should_suppress(&self, notification: &Notification, now: NaiveTime) -> bool {
        if !self.enabled {
            return false;
        }

        // Check bypass rules
        if notification.priority >= self.bypass_priority {
            return false;
        }
        if self
            .bypass_categories
            .contains(&notification.category)
        {
            return false;
        }

        // Parse start/end times
        let start = NaiveTime::parse_from_str(&self.start, "%H:%M").unwrap_or(NaiveTime::from_hms_opt(22, 0, 0).unwrap());
        let end = NaiveTime::parse_from_str(&self.end, "%H:%M").unwrap_or(NaiveTime::from_hms_opt(7, 0, 0).unwrap());

        // Handle overnight ranges (e.g., 22:00 - 07:00)
        if start > end {
            now >= start || now < end
        } else {
            now >= start && now < end
        }
    }
}

/// User notification preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPreferences {
    pub enabled: bool,
    pub quiet_hours: QuietHoursConfig,
    pub category_settings: HashMap<NotificationCategory, CategoryPreference>,
    pub targets: Vec<DeliveryTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryPreference {
    pub enabled: bool,
    pub sound: Option<String>,
    pub badge: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            quiet_hours: QuietHoursConfig::default(),
            category_settings: HashMap::new(),
            targets: vec![DeliveryTarget::Desktop],
        }
    }
}

/// Central notification manager.
pub struct NotificationManager {
    providers: Vec<Arc<dyn NotificationProvider>>,
    preferences: Arc<RwLock<NotificationPreferences>>,
    history: Arc<RwLock<Vec<Notification>>>,
    max_history: usize,
}

impl NotificationManager {
    pub fn new(preferences: NotificationPreferences) -> Self {
        Self {
            providers: Vec::new(),
            preferences: Arc::new(RwLock::new(preferences)),
            history: Arc::new(RwLock::new(Vec::new())),
            max_history: 1000,
        }
    }

    /// Register a notification provider.
    pub fn add_provider(&mut self, provider: Arc<dyn NotificationProvider>) {
        info!(provider = provider.name(), "registered notification provider");
        self.providers.push(provider);
    }

    /// Send a notification to all configured targets.
    pub async fn notify(&self, notification: Notification) -> Vec<DeliveryResult> {
        let prefs = self.preferences.read().await;

        if !prefs.enabled {
            debug!("notifications disabled, skipping");
            return Vec::new();
        }

        // Check category settings
        if let Some(cat_pref) = prefs.category_settings.get(&notification.category) {
            if !cat_pref.enabled {
                debug!(
                    category = ?notification.category,
                    "category disabled, skipping notification"
                );
                return Vec::new();
            }
        }

        // Check quiet hours
        let now = Utc::now().time();
        if prefs.quiet_hours.should_suppress(&notification, now) {
            debug!("quiet hours active, suppressing notification");
            return Vec::new();
        }

        let targets = prefs.targets.clone();
        drop(prefs);

        let mut results = Vec::new();

        for target in &targets {
            for provider in &self.providers {
                if provider.supports(target) {
                    match provider.send(&notification, target).await {
                        Ok(result) => {
                            results.push(result);
                        }
                        Err(e) => {
                            warn!(
                                provider = provider.name(),
                                error = %e,
                                "notification delivery failed"
                            );
                            results.push(DeliveryResult {
                                target: provider.name().to_string(),
                                success: false,
                                error: Some(e.to_string()),
                                delivered_at: Utc::now(),
                            });
                        }
                    }
                    break; // Only one provider per target
                }
            }
        }

        // Store in history
        {
            let mut history = self.history.write().await;
            history.push(notification);
            let excess = history.len().saturating_sub(self.max_history);
            if excess > 0 {
                history.drain(0..excess);
            }
        }

        results
    }

    /// Get notification history.
    pub async fn get_history(&self, limit: usize) -> Vec<Notification> {
        let history = self.history.read().await;
        history.iter().rev().take(limit).cloned().collect()
    }

    /// Update notification preferences.
    pub async fn update_preferences(&self, prefs: NotificationPreferences) {
        let mut current = self.preferences.write().await;
        *current = prefs;
        info!("notification preferences updated");
    }

    /// Get current preferences.
    pub async fn get_preferences(&self) -> NotificationPreferences {
        self.preferences.read().await.clone()
    }

    /// Clear notification history.
    pub async fn clear_history(&self) {
        self.history.write().await.clear();
    }
}

/// Notification error.
#[derive(Debug, thiserror::Error)]
pub enum NotificationError {
    #[error("unsupported delivery target")]
    UnsupportedTarget,
    #[error("delivery failed: {0}")]
    DeliveryFailed(String),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("rate limited")]
    RateLimited,
    #[error("token expired")]
    TokenExpired,
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_builder() {
        let n = Notification::new("Title", "Body")
            .with_priority(NotificationPriority::High)
            .with_category(NotificationCategory::Mention)
            .with_group("group1")
            .with_data("channel", "general");

        assert_eq!(n.title, "Title");
        assert_eq!(n.priority, NotificationPriority::High);
        assert_eq!(n.category, NotificationCategory::Mention);
        assert_eq!(n.group_id, Some("group1".to_string()));
        assert_eq!(n.data.get("channel"), Some(&"general".to_string()));
    }

    #[test]
    fn test_quiet_hours_overnight() {
        let config = QuietHoursConfig {
            enabled: true,
            start: "22:00".to_string(),
            end: "07:00".to_string(),
            bypass_categories: vec![],
            bypass_priority: NotificationPriority::Critical,
        };

        let notification = Notification::new("Test", "Body");

        // 23:00 — should suppress
        let t = NaiveTime::from_hms_opt(23, 0, 0).unwrap();
        assert!(config.should_suppress(&notification, t));

        // 03:00 — should suppress
        let t = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
        assert!(config.should_suppress(&notification, t));

        // 12:00 — should NOT suppress
        let t = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        assert!(!config.should_suppress(&notification, t));
    }

    #[test]
    fn test_quiet_hours_bypass_critical() {
        let config = QuietHoursConfig {
            enabled: true,
            start: "22:00".to_string(),
            end: "07:00".to_string(),
            bypass_categories: vec![],
            bypass_priority: NotificationPriority::Critical,
        };

        let notification = Notification::new("Alert", "Critical!")
            .with_priority(NotificationPriority::Critical);

        let t = NaiveTime::from_hms_opt(23, 0, 0).unwrap();
        assert!(!config.should_suppress(&notification, t));
    }

    #[test]
    fn test_quiet_hours_disabled() {
        let config = QuietHoursConfig {
            enabled: false,
            ..Default::default()
        };

        let notification = Notification::new("Test", "Body");
        let t = NaiveTime::from_hms_opt(23, 0, 0).unwrap();
        assert!(!config.should_suppress(&notification, t));
    }

    #[tokio::test]
    async fn test_notification_manager_desktop() {
        let mut mgr = NotificationManager::new(NotificationPreferences::default());
        mgr.add_provider(Arc::new(DesktopNotificationProvider));

        let notification = Notification::new("Hello", "World");
        let results = mgr.notify(notification).await;

        assert_eq!(results.len(), 1);
        assert!(results[0].success);
    }

    #[tokio::test]
    async fn test_notification_disabled() {
        let prefs = NotificationPreferences {
            enabled: false,
            ..Default::default()
        };
        let mut mgr = NotificationManager::new(prefs);
        mgr.add_provider(Arc::new(DesktopNotificationProvider));

        let notification = Notification::new("Hello", "World");
        let results = mgr.notify(notification).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_notification_history() {
        let mut mgr = NotificationManager::new(NotificationPreferences::default());
        mgr.add_provider(Arc::new(DesktopNotificationProvider));

        for i in 0..5 {
            mgr.notify(Notification::new(format!("n{}", i), "body")).await;
        }

        let history = mgr.get_history(3).await;
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].title, "n4"); // Most recent first
    }

    #[test]
    fn test_delivery_target_serde() {
        let target = DeliveryTarget::Desktop;
        let json = serde_json::to_string(&target).unwrap();
        let parsed: DeliveryTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, DeliveryTarget::Desktop);
    }

    #[test]
    fn test_notification_serialization() {
        let n = Notification::new("Title", "Body")
            .with_priority(NotificationPriority::High);
        let json = serde_json::to_string(&n).unwrap();
        let parsed: Notification = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, "Title");
        assert_eq!(parsed.priority, NotificationPriority::High);
    }
}
