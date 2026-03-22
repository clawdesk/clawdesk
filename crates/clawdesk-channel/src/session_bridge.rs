//! Session bridging — cross-channel identity linking and session mirroring.
//!
//! ## Problem
//!
//! Sessions are keyed by `{channel}:{identifier}` (e.g., `telegram:12345`,
//! `internal:desktop-main`). This means the same user has **separate** sessions
//! on each channel with no shared context.
//!
//! ## Solution: User Alias Registry
//!
//! A `UserAlias` maps a user identity to a canonical user ID, enabling:
//!
//! 1. **Session federation**: Telegram message → look up alias → find linked
//!    desktop session → mirror the request there.
//! 2. **Progress forwarding**: Desktop agent events → look up linked channels
//!    → push updates to all subscribed channels (e.g., Telegram).
//! 3. **Context continuity**: Resume a desktop conversation from Telegram
//!    with full history preserved.
//!
//! ```text
//!   Telegram:12345 ──┐                   ┌── internal:desktop-main
//!   Discord:67890  ──┼── user:sushanth ──┤
//!   Slack:U09XY    ──┘                   └── webchat:session-abc
//! ```
//!
//! The bridge does NOT merge sessions. It creates a **linked view** — each
//! channel retains its own session, but the bridge knows they belong to the
//! same user and can route events between them.

use clawdesk_types::channel::ChannelId;
use clawdesk_types::session::SessionKey;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info};

/// A channel-specific identity for a user.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelIdentity {
    pub channel: ChannelId,
    pub identifier: String,
}

impl ChannelIdentity {
    pub fn new(channel: ChannelId, identifier: impl Into<String>) -> Self {
        Self {
            channel,
            identifier: identifier.into(),
        }
    }

    /// Convert to a SessionKey.
    pub fn to_session_key(&self) -> SessionKey {
        SessionKey::new(self.channel, &self.identifier)
    }
}

/// A user alias — links multiple channel identities to a canonical user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAlias {
    /// Canonical user ID (opaque, user-chosen or generated).
    pub user_id: String,
    /// All linked channel identities for this user.
    pub identities: Vec<ChannelIdentity>,
    /// The "primary" session to bridge to (typically the desktop session).
    /// When a message arrives on a secondary channel, it can be forwarded
    /// to this session for processing.
    pub primary_session: Option<SessionKey>,
    /// Channels that should receive progress updates when the primary
    /// session is processing. Enables "watch from phone" scenario.
    pub notify_channels: Vec<ChannelIdentity>,
}

impl UserAlias {
    /// Create a new alias with a single identity.
    pub fn new(user_id: impl Into<String>, identity: ChannelIdentity) -> Self {
        Self {
            user_id: user_id.into(),
            identities: vec![identity],
            primary_session: None,
            notify_channels: Vec::new(),
        }
    }

    /// Link an additional channel identity.
    pub fn link(&mut self, identity: ChannelIdentity) {
        if !self.identities.contains(&identity) {
            info!(
                user = %self.user_id,
                channel = %identity.channel,
                id = %identity.identifier,
                "Linked new channel identity"
            );
            self.identities.push(identity);
        }
    }

    /// Set the primary session (typically the desktop session).
    pub fn set_primary(&mut self, session: SessionKey) {
        self.primary_session = Some(session);
    }

    /// Subscribe a channel identity for progress notifications.
    pub fn subscribe_notifications(&mut self, identity: ChannelIdentity) {
        if !self.notify_channels.contains(&identity) {
            self.notify_channels.push(identity);
        }
    }

    /// Unsubscribe a channel from notifications.
    pub fn unsubscribe_notifications(&mut self, identity: &ChannelIdentity) {
        self.notify_channels.retain(|c| c != identity);
    }

    /// Find the identity for a specific channel.
    pub fn identity_for(&self, channel: ChannelId) -> Option<&ChannelIdentity> {
        self.identities.iter().find(|i| i.channel == channel)
    }

    /// Get all session keys for this user (one per linked channel).
    pub fn session_keys(&self) -> Vec<SessionKey> {
        self.identities.iter().map(|i| i.to_session_key()).collect()
    }
}

/// Registry of user aliases — maps channel identities to canonical users.
///
/// Two indexes for O(1) lookup in both directions:
/// - `by_identity`: ChannelIdentity → user_id
/// - `by_user`: user_id → UserAlias
pub struct SessionBridge {
    /// Identity → canonical user_id (O(1) reverse lookup).
    by_identity: DashMap<ChannelIdentity, String>,
    /// user_id → full alias record.
    by_user: DashMap<String, UserAlias>,
}

impl SessionBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            by_identity: DashMap::new(),
            by_user: DashMap::new(),
        })
    }

    /// Register or update a user alias.
    pub fn register(&self, alias: UserAlias) {
        let user_id = alias.user_id.clone();
        for identity in &alias.identities {
            self.by_identity.insert(identity.clone(), user_id.clone());
        }
        info!(user = %user_id, identities = alias.identities.len(), "User alias registered");
        self.by_user.insert(user_id, alias);
    }

    /// Link a channel identity to an existing user.
    pub fn link_identity(&self, user_id: &str, identity: ChannelIdentity) {
        self.by_identity.insert(identity.clone(), user_id.to_string());
        if let Some(mut alias) = self.by_user.get_mut(user_id) {
            alias.link(identity);
        }
    }

    /// Look up the user alias for a channel identity.
    pub fn resolve(&self, identity: &ChannelIdentity) -> Option<UserAlias> {
        let user_id = self.by_identity.get(identity)?;
        self.by_user.get(user_id.value()).map(|a| a.clone())
    }

    /// Look up by user ID.
    pub fn get_user(&self, user_id: &str) -> Option<UserAlias> {
        self.by_user.get(user_id).map(|a| a.clone())
    }

    /// Find the primary session for a channel identity.
    ///
    /// This is the key method for the Telegram → Desktop bridge:
    /// 1. Telegram message arrives with `ChannelIdentity { Telegram, "12345" }`
    /// 2. Resolve → finds `user:sushanth`
    /// 3. Return primary session → `internal:desktop-main`
    /// 4. Forward message to that session
    pub fn find_primary_session(&self, identity: &ChannelIdentity) -> Option<SessionKey> {
        let alias = self.resolve(identity)?;
        alias.primary_session
    }

    /// Find all channels that should be notified for a session's events.
    ///
    /// This is the key method for Desktop → Telegram progress forwarding:
    /// 1. Agent on `internal:desktop-main` emits progress events
    /// 2. Look up which user owns this session
    /// 3. Return their `notify_channels` → `[Telegram:12345]`
    /// 4. Forward events to those channels
    pub fn notification_channels_for_session(
        &self,
        session_key: &SessionKey,
    ) -> Vec<ChannelIdentity> {
        // Find the user who owns this session
        let identity = ChannelIdentity::new(session_key.channel(), session_key.identifier());
        let alias = match self.resolve(&identity) {
            Some(a) => a,
            None => return Vec::new(),
        };

        // Also check if this is the primary session
        if let Some(ref primary) = alias.primary_session {
            if primary == session_key {
                return alias.notify_channels.clone();
            }
        }

        // Check if this session belongs to the user at all
        if alias.identities.iter().any(|i| i.to_session_key() == *session_key) {
            return alias.notify_channels.clone();
        }

        Vec::new()
    }

    /// Set the primary session for a user.
    pub fn set_primary_session(&self, user_id: &str, session: SessionKey) {
        if let Some(mut alias) = self.by_user.get_mut(user_id) {
            debug!(user = %user_id, session = %session, "Set primary session");
            alias.set_primary(session);
        }
    }

    /// Subscribe a channel for progress notifications on a user's sessions.
    pub fn subscribe_channel(&self, user_id: &str, identity: ChannelIdentity) {
        if let Some(mut alias) = self.by_user.get_mut(user_id) {
            alias.subscribe_notifications(identity);
        }
    }

    /// List all registered users.
    pub fn list_users(&self) -> Vec<String> {
        self.by_user.iter().map(|e| e.key().clone()).collect()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_resolve() {
        let bridge = SessionBridge::new();
        let tg = ChannelIdentity::new(ChannelId::Telegram, "12345");
        let alias = UserAlias::new("sushanth", tg.clone());
        bridge.register(alias);

        let resolved = bridge.resolve(&tg).unwrap();
        assert_eq!(resolved.user_id, "sushanth");
    }

    #[test]
    fn link_multiple_channels() {
        let bridge = SessionBridge::new();
        let tg = ChannelIdentity::new(ChannelId::Telegram, "12345");
        let desktop = ChannelIdentity::new(ChannelId::Internal, "desktop-main");

        let alias = UserAlias::new("sushanth", tg.clone());
        bridge.register(alias);
        bridge.link_identity("sushanth", desktop.clone());

        // Both identities resolve to the same user
        assert_eq!(
            bridge.resolve(&tg).unwrap().user_id,
            bridge.resolve(&desktop).unwrap().user_id
        );
    }

    #[test]
    fn primary_session_lookup() {
        let bridge = SessionBridge::new();
        let tg = ChannelIdentity::new(ChannelId::Telegram, "12345");
        let desktop = ChannelIdentity::new(ChannelId::Internal, "desktop-main");
        let desktop_session = SessionKey::new(ChannelId::Internal, "desktop-main");

        let mut alias = UserAlias::new("sushanth", tg.clone());
        alias.link(desktop.clone());
        alias.set_primary(desktop_session.clone());
        bridge.register(alias);

        // From Telegram, find the desktop session
        let primary = bridge.find_primary_session(&tg).unwrap();
        assert_eq!(primary, desktop_session);
    }

    #[test]
    fn notification_channels_for_desktop() {
        let bridge = SessionBridge::new();
        let tg = ChannelIdentity::new(ChannelId::Telegram, "12345");
        let desktop = ChannelIdentity::new(ChannelId::Internal, "desktop-main");
        let desktop_session = SessionKey::new(ChannelId::Internal, "desktop-main");

        let mut alias = UserAlias::new("sushanth", desktop.clone());
        alias.link(tg.clone());
        alias.set_primary(desktop_session.clone());
        alias.subscribe_notifications(tg.clone());
        bridge.register(alias);

        // Desktop session → should notify Telegram
        let channels = bridge.notification_channels_for_session(&desktop_session);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].channel, ChannelId::Telegram);
    }

    #[test]
    fn no_notifications_for_unknown_session() {
        let bridge = SessionBridge::new();
        let unknown = SessionKey::new(ChannelId::Internal, "unknown");
        let channels = bridge.notification_channels_for_session(&unknown);
        assert!(channels.is_empty());
    }
}
