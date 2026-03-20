//! Hook policy — allow/deny rules and slot-based mutual exclusion.
//!
//! ## Design
//!
//! Two orthogonal policy mechanisms:
//!
//! 1. **Slot competition**: multiple plugins can declare hooks for the same
//!    category (e.g., "memory-provider"), but only one is active at a time.
//!    Higher-priority hooks evict lower-priority ones (CAS semantics).
//!
//! 2. **Policy rules**: server-level allow/deny lists for hooks.
//!    Deny takes precedence over allow (fail-closed).
//!
//! Slot competition is a lattice selection: among competing plugins for
//! a slot, we pick the one with highest priority:
//!   `select : P(Plugin) → Plugin`
//!   where `select(S) = argmax_{p ∈ S} priority(p)`
//!
//! The deny list is a filter morphism:
//!   `policy : Hook → Bool`
//!   applied as a pre-condition before dispatch.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Hook policy configuration.
///
/// Controls which hooks are allowed to execute and which slots
/// have mutual exclusion constraints.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookPolicy {
    /// Hooks explicitly denied by name (admin override).
    #[serde(default)]
    deny_hooks: HashSet<String>,
    /// Hooks explicitly allowed (if non-empty, acts as allowlist).
    #[serde(default)]
    allow_hooks: HashSet<String>,
    /// Slot occupancy — only one hook per slot is active.
    /// Key: slot name, Value: occupying hook name.
    #[serde(default)]
    slots: HashMap<String, SlotOccupant>,
}

/// A slot occupant — which plugin/hook currently owns a slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotOccupant {
    /// Hook name that occupies this slot.
    pub hook_name: String,
    /// Plugin ID that registered the hook.
    pub plugin_id: String,
    /// Priority used for slot competition (higher = wins).
    pub priority: i32,
}

/// Result of a policy check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Hook is allowed to execute.
    Allow,
    /// Hook is denied.
    Deny { reason: String },
}

impl HookPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if a hook is allowed to execute.
    ///
    /// Resolution order:
    /// 1. Explicit deny list → Deny
    /// 2. Non-empty allow list and hook not in it → Deny
    /// 3. Otherwise → Allow
    pub fn check(&self, hook_name: &str) -> PolicyDecision {
        if self.deny_hooks.contains(hook_name) {
            return PolicyDecision::Deny {
                reason: format!("hook '{hook_name}' is in the deny list"),
            };
        }
        if !self.allow_hooks.is_empty() && !self.allow_hooks.contains(hook_name) {
            return PolicyDecision::Deny {
                reason: format!("hook '{hook_name}' is not in the allow list"),
            };
        }
        PolicyDecision::Allow
    }

    /// Add a hook to the deny list.
    pub fn deny(&mut self, hook_name: impl Into<String>) {
        self.deny_hooks.insert(hook_name.into());
    }

    /// Add a hook to the allow list.
    pub fn allow(&mut self, hook_name: impl Into<String>) {
        self.allow_hooks.insert(hook_name.into());
    }

    /// Attempt to claim a slot for a hook.
    ///
    /// If the slot is unoccupied, the hook claims it.
    /// If occupied by a lower-priority hook, the new hook evicts it.
    /// Returns the evicted hook name (if any) or `None`.
    ///
    /// This implements the _CAS (compare-and-swap) slot swap_ pattern
    /// from ClawDesk's plugin registry, applied to hooks.
    pub fn claim_slot(
        &mut self,
        slot: &str,
        hook_name: &str,
        plugin_id: &str,
        priority: i32,
    ) -> SlotClaimResult {
        if let Some(current) = self.slots.get(slot) {
            if current.hook_name == hook_name {
                // Already occupying
                return SlotClaimResult::AlreadyOwned;
            }
            if priority > current.priority {
                let evicted = current.hook_name.clone();
                self.slots.insert(
                    slot.to_string(),
                    SlotOccupant {
                        hook_name: hook_name.to_string(),
                        plugin_id: plugin_id.to_string(),
                        priority,
                    },
                );
                SlotClaimResult::Claimed { evicted: Some(evicted) }
            } else {
                SlotClaimResult::Rejected {
                    current_occupant: current.hook_name.clone(),
                    current_priority: current.priority,
                }
            }
        } else {
            self.slots.insert(
                slot.to_string(),
                SlotOccupant {
                    hook_name: hook_name.to_string(),
                    plugin_id: plugin_id.to_string(),
                    priority,
                },
            );
            SlotClaimResult::Claimed { evicted: None }
        }
    }

    /// Release a slot.
    pub fn release_slot(&mut self, slot: &str, hook_name: &str) -> bool {
        if let Some(current) = self.slots.get(slot) {
            if current.hook_name == hook_name {
                self.slots.remove(slot);
                return true;
            }
        }
        false
    }

    /// Get the current occupant of a slot.
    pub fn slot_occupant(&self, slot: &str) -> Option<&SlotOccupant> {
        self.slots.get(slot)
    }

    /// List all slot occupancies.
    pub fn all_slots(&self) -> &HashMap<String, SlotOccupant> {
        &self.slots
    }
}

/// Result of a slot claim attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotClaimResult {
    /// Successfully claimed the slot.
    Claimed {
        /// Previously evicted hook, if any.
        evicted: Option<String>,
    },
    /// Already owns this slot.
    AlreadyOwned,
    /// Rejected — current occupant has equal or higher priority.
    Rejected {
        current_occupant: String,
        current_priority: i32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_allows_all() {
        let policy = HookPolicy::new();
        assert_eq!(policy.check("any-hook"), PolicyDecision::Allow);
    }

    #[test]
    fn deny_list_blocks_hook() {
        let mut policy = HookPolicy::new();
        policy.deny("bad-hook");
        assert_eq!(
            policy.check("bad-hook"),
            PolicyDecision::Deny { reason: "hook 'bad-hook' is in the deny list".into() }
        );
        assert_eq!(policy.check("good-hook"), PolicyDecision::Allow);
    }

    #[test]
    fn allow_list_blocks_unlisted() {
        let mut policy = HookPolicy::new();
        policy.allow("approved-hook");
        assert_eq!(policy.check("approved-hook"), PolicyDecision::Allow);
        assert!(matches!(policy.check("random-hook"), PolicyDecision::Deny { .. }));
    }

    #[test]
    fn deny_takes_precedence_over_allow() {
        let mut policy = HookPolicy::new();
        policy.allow("hook-a");
        policy.deny("hook-a");
        assert!(matches!(policy.check("hook-a"), PolicyDecision::Deny { .. }));
    }

    #[test]
    fn slot_claim_empty() {
        let mut policy = HookPolicy::new();
        let result = policy.claim_slot("memory", "mem-hook", "plugin-a", 10);
        assert_eq!(result, SlotClaimResult::Claimed { evicted: None });
        assert_eq!(policy.slot_occupant("memory").unwrap().hook_name, "mem-hook");
    }

    #[test]
    fn slot_claim_eviction() {
        let mut policy = HookPolicy::new();
        policy.claim_slot("memory", "old-hook", "plugin-a", 10);
        let result = policy.claim_slot("memory", "new-hook", "plugin-b", 20);
        assert_eq!(result, SlotClaimResult::Claimed { evicted: Some("old-hook".into()) });
        assert_eq!(policy.slot_occupant("memory").unwrap().hook_name, "new-hook");
    }

    #[test]
    fn slot_claim_rejected() {
        let mut policy = HookPolicy::new();
        policy.claim_slot("memory", "strong-hook", "plugin-a", 100);
        let result = policy.claim_slot("memory", "weak-hook", "plugin-b", 10);
        assert_eq!(
            result,
            SlotClaimResult::Rejected {
                current_occupant: "strong-hook".into(),
                current_priority: 100,
            }
        );
    }

    #[test]
    fn slot_release() {
        let mut policy = HookPolicy::new();
        policy.claim_slot("memory", "hook-a", "plugin-a", 10);
        assert!(policy.release_slot("memory", "hook-a"));
        assert!(policy.slot_occupant("memory").is_none());
    }

    #[test]
    fn slot_release_wrong_owner() {
        let mut policy = HookPolicy::new();
        policy.claim_slot("memory", "hook-a", "plugin-a", 10);
        assert!(!policy.release_slot("memory", "hook-b"));
        assert!(policy.slot_occupant("memory").is_some());
    }
}
