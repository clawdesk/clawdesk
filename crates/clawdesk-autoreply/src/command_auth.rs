//! Command authorization gate — enforces role-based access control between
//! command parsing and execution.
//!
//! This module bridges `CommandRegistry` (parsing + dispatch) with
//! `CommandPolicyEngine` (risk classification) and adds role-based
//! authorization that checks the `requires_auth` flag on `CommandDef`.

use crate::command_registry::{CommandContext, CommandRegistry, CommandResult, ParsedCommand};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::{info, warn};

/// Role level for authorization decisions (ordered by privilege).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleLevel {
    /// Default — unauthenticated or unrecognized sender.
    Guest = 0,
    /// Explicitly allowed user (in the allowlist).
    Allowlisted = 1,
    /// Group/channel administrator (contributed by channel adapter).
    ChannelAdmin = 2,
    /// Bot owner (DM sender or configured owner identity).
    Owner = 3,
}

/// Result of an authorization check.
#[derive(Debug, Clone)]
pub struct CommandAuthResult {
    pub allowed: bool,
    pub reason: Option<String>,
    pub resolved_role: RoleLevel,
}

/// Configuration for the command authorization gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandAuthConfig {
    /// Identities that are always treated as owners.
    pub owner_ids: Vec<String>,
    /// Identities in the allowlist (Allowlisted role).
    pub allowlisted_ids: Vec<String>,
    /// Whether DM senders are treated as owner-equivalent.
    pub dm_is_owner: bool,
}

impl Default for CommandAuthConfig {
    fn default() -> Self {
        Self {
            owner_ids: vec![],
            allowlisted_ids: vec![],
            dm_is_owner: true,
        }
    }
}

/// Authorization gate that sits between command parsing and execution.
pub struct CommandAuthGate {
    owner_ids: HashSet<String>,
    allowlisted_ids: HashSet<String>,
    dm_is_owner: bool,
}

impl CommandAuthGate {
    pub fn new(config: CommandAuthConfig) -> Self {
        Self {
            owner_ids: config.owner_ids.into_iter().collect(),
            allowlisted_ids: config.allowlisted_ids.into_iter().collect(),
            dm_is_owner: config.dm_is_owner,
        }
    }

    /// Resolve the sender's role based on identity and channel context.
    ///
    /// Priority: DM owner equiv > explicit owner > channel admin claim > allowlist > guest.
    pub fn resolve_role(
        &self,
        sender_id: &str,
        is_dm: bool,
        is_channel_admin: bool,
    ) -> RoleLevel {
        // DM senders are owner-equivalent when configured.
        if is_dm && self.dm_is_owner {
            return RoleLevel::Owner;
        }
        // Explicit owner identity.
        if self.owner_ids.contains(sender_id) {
            return RoleLevel::Owner;
        }
        // Channel admin claim (contributed by channel adapter).
        if is_channel_admin {
            return RoleLevel::ChannelAdmin;
        }
        // Allowlisted user.
        if self.allowlisted_ids.contains(sender_id) {
            return RoleLevel::Allowlisted;
        }
        RoleLevel::Guest
    }

    /// Check whether a parsed command is authorized for the given context.
    ///
    /// Commands with `requires_auth: true` require at least `Allowlisted` role.
    /// Commands with `requires_auth: false` are open to all.
    pub fn authorize(
        &self,
        registry: &CommandRegistry,
        cmd: &ParsedCommand,
        ctx: &CommandContext,
        is_dm: bool,
    ) -> CommandAuthResult {
        let resolved_role = self.resolve_role(&ctx.sender_id, is_dm, ctx.is_admin);

        let handler = match registry.resolve(&cmd.name) {
            Some(h) => h,
            None => {
                return CommandAuthResult {
                    allowed: false,
                    reason: Some(format!("unknown command: {}", cmd.name)),
                    resolved_role,
                };
            }
        };

        let def = handler.definition();

        if def.requires_auth && resolved_role < RoleLevel::Allowlisted {
            warn!(
                command = %cmd.name,
                sender = %ctx.sender_id,
                role = ?resolved_role,
                "command authorization denied"
            );
            return CommandAuthResult {
                allowed: false,
                reason: Some(format!(
                    "/{} requires authorization (your role: {:?})",
                    cmd.name, resolved_role
                )),
                resolved_role,
            };
        }

        info!(
            command = %cmd.name,
            sender = %ctx.sender_id,
            role = ?resolved_role,
            "command authorized"
        );
        CommandAuthResult {
            allowed: true,
            reason: None,
            resolved_role,
        }
    }

    /// Authorize and execute a command in one step.
    ///
    /// Returns a denial `CommandResult` if unauthorized, or the command's
    /// execution result if authorized.
    pub async fn authorize_and_execute(
        &self,
        registry: &CommandRegistry,
        cmd: &ParsedCommand,
        ctx: &CommandContext,
        is_dm: bool,
    ) -> CommandResult {
        let auth = self.authorize(registry, cmd, ctx, is_dm);

        if !auth.allowed {
            return CommandResult {
                success: false,
                output: auth.reason.unwrap_or_else(|| "unauthorized".to_string()),
                ephemeral: true,
            };
        }

        match registry.resolve(&cmd.name) {
            Some(handler) => handler.execute(cmd, ctx).await,
            None => CommandResult {
                success: false,
                output: format!("command '{}' not found", cmd.name),
                ephemeral: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_registry::{
        Command, CommandCategory, CommandDef, CommandRegistry, ParsedCommand,
    };
    use async_trait::async_trait;
    use std::sync::Arc;

    struct AuthTestCmd {
        requires_auth: bool,
    }

    #[async_trait]
    impl Command for AuthTestCmd {
        fn definition(&self) -> CommandDef {
            CommandDef {
                name: "config".into(),
                aliases: vec![],
                description: "Configure bot".into(),
                usage: "/config <key> <value>".into(),
                requires_auth: self.requires_auth,
                category: CommandCategory::Config,
            }
        }
        async fn execute(&self, _cmd: &ParsedCommand, _ctx: &CommandContext) -> CommandResult {
            CommandResult {
                success: true,
                output: "configured".into(),
                ephemeral: true,
            }
        }
    }

    fn test_ctx(sender: &str, is_admin: bool) -> CommandContext {
        CommandContext {
            sender_id: sender.to_string(),
            channel_id: "chan-1".to_string(),
            session_key: "sess-1".to_string(),
            is_admin,
        }
    }

    #[test]
    fn dm_sender_is_owner_equivalent() {
        let gate = CommandAuthGate::new(CommandAuthConfig::default());
        let role = gate.resolve_role("unknown-user", true, false);
        assert_eq!(role, RoleLevel::Owner);
    }

    #[test]
    fn explicit_owner_recognized() {
        let gate = CommandAuthGate::new(CommandAuthConfig {
            owner_ids: vec!["admin-1".to_string()],
            ..Default::default()
        });
        let role = gate.resolve_role("admin-1", false, false);
        assert_eq!(role, RoleLevel::Owner);
    }

    #[test]
    fn unknown_group_user_is_guest() {
        let gate = CommandAuthGate::new(CommandAuthConfig::default());
        let role = gate.resolve_role("random-user", false, false);
        assert_eq!(role, RoleLevel::Guest);
    }

    #[tokio::test]
    async fn auth_required_blocks_guest() {
        let gate = CommandAuthGate::new(CommandAuthConfig::default());
        let mut reg = CommandRegistry::new("/");
        reg.register(Arc::new(AuthTestCmd { requires_auth: true }));

        let cmd = reg.parse("/config test").unwrap();
        let ctx = test_ctx("random-user", false);
        let result = gate.authorize_and_execute(&reg, &cmd, &ctx, false).await;
        assert!(!result.success);
        assert!(result.output.contains("authorization"));
    }

    #[tokio::test]
    async fn auth_required_allows_dm_owner() {
        let gate = CommandAuthGate::new(CommandAuthConfig::default());
        let mut reg = CommandRegistry::new("/");
        reg.register(Arc::new(AuthTestCmd { requires_auth: true }));

        let cmd = reg.parse("/config test").unwrap();
        let ctx = test_ctx("dm-user", false);
        let result = gate.authorize_and_execute(&reg, &cmd, &ctx, true).await;
        assert!(result.success);
    }

    #[tokio::test]
    async fn open_command_allows_guest() {
        let gate = CommandAuthGate::new(CommandAuthConfig::default());
        let mut reg = CommandRegistry::new("/");
        reg.register(Arc::new(AuthTestCmd {
            requires_auth: false,
        }));

        let cmd = reg.parse("/config test").unwrap();
        let ctx = test_ctx("random-user", false);
        let result = gate.authorize_and_execute(&reg, &cmd, &ctx, false).await;
        assert!(result.success);
    }

    #[test]
    fn allowlisted_user_passes_auth() {
        let gate = CommandAuthGate::new(CommandAuthConfig {
            allowlisted_ids: vec!["trusted-user".to_string()],
            ..Default::default()
        });
        let mut reg = CommandRegistry::new("/");
        reg.register(Arc::new(AuthTestCmd { requires_auth: true }));

        let cmd = reg.parse("/config test").unwrap();
        let ctx = test_ctx("trusted-user", false);
        let auth = gate.authorize(&reg, &cmd, &ctx, false);
        assert!(auth.allowed);
        assert_eq!(auth.resolved_role, RoleLevel::Allowlisted);
    }
}
