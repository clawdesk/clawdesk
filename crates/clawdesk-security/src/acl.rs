//! Access control list manager — principal/resource/action policy engine.
//!
//! Default-deny: all access is denied unless explicitly permitted.

use clawdesk_types::security::FsAcl;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Who is requesting access.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Principal {
    User(String),
    Agent(String),
    Plugin(String),
    Role(String),
    System,
}

/// What resource is being accessed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Resource {
    Path(String),
    Tool(String),
    Channel(String),
    Config(String),
    Endpoint(String),
}

/// What action is being performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    Read,
    Write,
    Execute,
    Delete,
    Admin,
}

/// Condition for conditional access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Condition {
    TimeWindow { start_hour: u32, end_hour: u32 },
    IpRange(String),
    MfaRequired,
    RateLimit { max_per_minute: u32 },
}

/// An ACL permission entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permission {
    pub principal: Principal,
    pub resource: Resource,
    pub action: Action,
    pub effect: Effect,
    pub conditions: Vec<Condition>,
}

/// Whether the permission allows or denies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    Allow,
    Deny,
}

/// Result of an access check.
#[derive(Debug, Clone)]
pub enum AccessDecision {
    Allow,
    Deny { reason: String },
    ConditionalAllow { conditions: Vec<Condition> },
}

/// Composite key for the permission index: (principal, resource, action).
/// Enables O(1) HashMap lookup instead of O(n) linear scan.
type PermKey = (Principal, Resource, Action);

/// ACL manager with indexed permissions — O(1) lookup via HashMap.
///
/// Permissions are indexed by `(Principal, Resource, Action)` triple.
/// Each check does at most two HashMap lookups (deny-first, then allow),
/// replacing the previous two linear scans over the entire permission list.
pub struct AclManager {
    /// Indexed permissions: (principal, resource, action) → Vec<Permission>.
    /// Multiple permissions for the same key are supported (e.g., allow + deny).
    index: RwLock<HashMap<PermKey, Vec<Permission>>>,
}

impl AclManager {
    pub fn new() -> Self {
        Self {
            index: RwLock::new(HashMap::new()),
        }
    }

    /// Add a permission rule. O(1) amortized insert.
    pub async fn add_permission(&self, perm: Permission) {
        let key = (perm.principal.clone(), perm.resource.clone(), perm.action);
        self.index.write().await.entry(key).or_default().push(perm);
    }

    /// Check access for a principal performing an action on a resource.
    ///
    /// O(1) HashMap lookup replaces O(n) linear scan.
    /// Deny-first: if any matching permission has `Effect::Deny`, access is denied.
    pub async fn check(
        &self,
        principal: &Principal,
        resource: &Resource,
        action: Action,
    ) -> AccessDecision {
        let index = self.index.read().await;

        let key = (principal.clone(), resource.clone(), action);
        if let Some(perms) = index.get(&key) {
            // Check for explicit denies first (deny wins).
            for perm in perms {
                if perm.effect == Effect::Deny {
                    return AccessDecision::Deny {
                        reason: "explicitly denied".to_string(),
                    };
                }
            }

            // Check for allows.
            for perm in perms {
                if perm.effect == Effect::Allow {
                    if perm.conditions.is_empty() {
                        return AccessDecision::Allow;
                    } else {
                        return AccessDecision::ConditionalAllow {
                            conditions: perm.conditions.clone(),
                        };
                    }
                }
            }
        }

        // System principal has implicit access.
        if *principal == Principal::System {
            return AccessDecision::Allow;
        }

        // Default deny.
        AccessDecision::Deny {
            reason: "no matching permission (default deny)".to_string(),
        }
    }

    /// Import filesystem ACLs from the types crate FsAcl.
    pub async fn import_fs_acl(&self, acl: &FsAcl) {
        let resource = Resource::Path(acl.path.clone());

        for agent_id in &acl.allowed_agents {
            let principal = Principal::Agent(agent_id.clone());

            if acl.permissions.read {
                self.add_permission(Permission {
                    principal: principal.clone(),
                    resource: resource.clone(),
                    action: Action::Read,
                    effect: Effect::Allow,
                    conditions: vec![],
                })
                .await;
            }
            if acl.permissions.write {
                self.add_permission(Permission {
                    principal: principal.clone(),
                    resource: resource.clone(),
                    action: Action::Write,
                    effect: Effect::Allow,
                    conditions: vec![],
                })
                .await;
            }
            if acl.permissions.execute {
                self.add_permission(Permission {
                    principal: principal.clone(),
                    resource: resource.clone(),
                    action: Action::Execute,
                    effect: Effect::Allow,
                    conditions: vec![],
                })
                .await;
            }
        }
    }

    /// Remove all permissions for a principal.
    pub async fn revoke_all(&self, principal: &Principal) {
        let mut index = self.index.write().await;
        index.retain(|key, _| key.0 != *principal);
    }
}

impl Default for AclManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_types::security::FsPermissions;

    #[tokio::test]
    async fn test_default_deny() {
        let acl = AclManager::new();
        let decision = acl
            .check(
                &Principal::User("user-1".to_string()),
                &Resource::Tool("shell".to_string()),
                Action::Execute,
            )
            .await;
        assert!(matches!(decision, AccessDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn test_explicit_allow() {
        let acl = AclManager::new();
        acl.add_permission(Permission {
            principal: Principal::Agent("bot".to_string()),
            resource: Resource::Tool("search".to_string()),
            action: Action::Execute,
            effect: Effect::Allow,
            conditions: vec![],
        })
        .await;

        let decision = acl
            .check(
                &Principal::Agent("bot".to_string()),
                &Resource::Tool("search".to_string()),
                Action::Execute,
            )
            .await;
        assert!(matches!(decision, AccessDecision::Allow));
    }

    #[tokio::test]
    async fn test_deny_takes_priority() {
        let acl = AclManager::new();
        acl.add_permission(Permission {
            principal: Principal::User("u1".to_string()),
            resource: Resource::Config("api_key".to_string()),
            action: Action::Read,
            effect: Effect::Allow,
            conditions: vec![],
        })
        .await;
        acl.add_permission(Permission {
            principal: Principal::User("u1".to_string()),
            resource: Resource::Config("api_key".to_string()),
            action: Action::Read,
            effect: Effect::Deny,
            conditions: vec![],
        })
        .await;

        let decision = acl
            .check(
                &Principal::User("u1".to_string()),
                &Resource::Config("api_key".to_string()),
                Action::Read,
            )
            .await;
        assert!(matches!(decision, AccessDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn test_fs_acl_import() {
        let acl = AclManager::new();
        let fs_acl = FsAcl {
            path: "/data/docs".to_string(),
            allowed_agents: vec!["agent-1".to_string()],
            permissions: FsPermissions {
                read: true,
                write: false,
                execute: false,
            },
        };
        acl.import_fs_acl(&fs_acl).await;

        let read = acl
            .check(
                &Principal::Agent("agent-1".to_string()),
                &Resource::Path("/data/docs".to_string()),
                Action::Read,
            )
            .await;
        assert!(matches!(read, AccessDecision::Allow));

        let write = acl
            .check(
                &Principal::Agent("agent-1".to_string()),
                &Resource::Path("/data/docs".to_string()),
                Action::Write,
            )
            .await;
        assert!(matches!(write, AccessDecision::Deny { .. }));
    }
}
