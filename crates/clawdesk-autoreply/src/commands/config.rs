//! `/config` — View or set runtime configuration values.

use crate::command_registry::{Command, CommandCategory, CommandContext, CommandDef, CommandResult, ParsedCommand};
use async_trait::async_trait;

pub struct ConfigCommand;

#[async_trait]
impl Command for ConfigCommand {
    fn definition(&self) -> CommandDef {
        CommandDef {
            name: "config".into(),
            aliases: vec!["cfg".into(), "set".into()],
            description: "Get or set configuration values".into(),
            usage: "/config get <key> | /config set <key> <value>".into(),
            requires_auth: true,
            category: CommandCategory::Config,
        }
    }

    async fn execute(&self, cmd: &ParsedCommand, ctx: &CommandContext) -> CommandResult {
        if !ctx.is_admin {
            return CommandResult { success: false, output: "Config changes require admin access.".into(), ephemeral: true };
        }
        match cmd.args.first().map(|s| s.as_str()) {
            Some("get") => {
                let key = cmd.args.get(1).map(|s| s.as_str()).unwrap_or("*");
                CommandResult { success: true, output: format!("Config `{key}`: (value lookup pending)"), ephemeral: true }
            }
            Some("set") => {
                let key = cmd.args.get(1).cloned().unwrap_or_default();
                let value = cmd.args.get(2).cloned().unwrap_or_default();
                if key.is_empty() || value.is_empty() {
                    return CommandResult { success: false, output: "Usage: /config set <key> <value>".into(), ephemeral: true };
                }
                CommandResult { success: true, output: format!("Set `{key}` = `{value}`"), ephemeral: true }
            }
            _ => CommandResult { success: false, output: "Usage: /config get <key> | /config set <key> <value>".into(), ephemeral: true },
        }
    }
}
