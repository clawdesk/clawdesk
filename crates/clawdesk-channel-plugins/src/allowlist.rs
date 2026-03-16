//! Hierarchical allowlist — 3-level resolution with denial propagation.
//!
//! Levels: Global → Channel → Conversation.
//! `allow(r) = V(r) ∪ (C(r) \ V_deny(r)) ∪ (G(r) \ C_deny(r) \ V_deny(r))`

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Allowlist level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowlistLevel {
    Global,
    Channel,
    Conversation,
}

/// Decision from allowlist resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowDecision {
    Allow,
    Deny { level: AllowlistLevel, reason: String },
}

/// A single allowlist level with allow and deny sets.
#[derive(Debug, Clone, Default)]
struct LevelList {
    allow: HashSet<String>,
    deny: HashSet<String>,
}

/// Hierarchical 3-level allowlist.
pub struct HierarchicalAllowlist {
    global: LevelList,
    channel: LevelList,
    conversation: LevelList,
}

impl HierarchicalAllowlist {
    pub fn new() -> Self {
        Self {
            global: LevelList::default(),
            channel: LevelList::default(),
            conversation: LevelList::default(),
        }
    }

    pub fn global_allow(&mut self, pattern: &str) {
        self.global.allow.insert(pattern.to_string());
    }

    pub fn global_deny(&mut self, pattern: &str) {
        self.global.deny.insert(pattern.to_string());
    }

    pub fn channel_allow(&mut self, pattern: &str) {
        self.channel.allow.insert(pattern.to_string());
    }

    pub fn channel_deny(&mut self, pattern: &str) {
        self.channel.deny.insert(pattern.to_string());
    }

    pub fn conversation_allow(&mut self, pattern: &str) {
        self.conversation.allow.insert(pattern.to_string());
    }

    pub fn conversation_deny(&mut self, pattern: &str) {
        self.conversation.deny.insert(pattern.to_string());
    }

    /// Resolve whether an identifier is allowed.
    ///
    /// Resolution: O(3) per request — constant time.
    pub fn check(&self, identifier: &str) -> AllowDecision {
        // Check conversation level first (most specific).
        if self.conversation.deny.contains(identifier) {
            return AllowDecision::Deny {
                level: AllowlistLevel::Conversation,
                reason: "denied at conversation level".into(),
            };
        }
        if self.conversation.allow.contains(identifier) {
            return AllowDecision::Allow;
        }

        // Check channel level.
        if self.channel.deny.contains(identifier) {
            return AllowDecision::Deny {
                level: AllowlistLevel::Channel,
                reason: "denied at channel level".into(),
            };
        }
        if self.channel.allow.contains(identifier) {
            return AllowDecision::Allow;
        }

        // Check global level.
        if self.global.deny.contains(identifier) {
            return AllowDecision::Deny {
                level: AllowlistLevel::Global,
                reason: "denied at global level".into(),
            };
        }
        if self.global.allow.contains(identifier) {
            return AllowDecision::Allow;
        }

        // Default: deny (allowlist is opt-in).
        AllowDecision::Deny {
            level: AllowlistLevel::Global,
            reason: "not in any allowlist".into(),
        }
    }
}

impl Default for HierarchicalAllowlist {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_allow() {
        let mut al = HierarchicalAllowlist::new();
        al.global_allow("user123");
        assert_eq!(al.check("user123"), AllowDecision::Allow);
    }

    #[test]
    fn channel_deny_overrides_global_allow() {
        let mut al = HierarchicalAllowlist::new();
        al.global_allow("user123");
        al.channel_deny("user123");
        assert!(matches!(al.check("user123"), AllowDecision::Deny { level: AllowlistLevel::Channel, .. }));
    }

    #[test]
    fn conversation_allow_overrides_channel_deny() {
        let mut al = HierarchicalAllowlist::new();
        al.channel_deny("user123");
        al.conversation_allow("user123");
        assert_eq!(al.check("user123"), AllowDecision::Allow);
    }

    #[test]
    fn unknown_user_denied() {
        let al = HierarchicalAllowlist::new();
        assert!(matches!(al.check("unknown"), AllowDecision::Deny { .. }));
    }
}
