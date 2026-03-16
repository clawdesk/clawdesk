//! `/help` — List available commands.

use crate::command_registry::{Command, CommandCategory, CommandContext, CommandDef, CommandResult, ParsedCommand};
use async_trait::async_trait;

pub struct HelpCommand;

#[async_trait]
impl Command for HelpCommand {
    fn definition(&self) -> CommandDef {
        CommandDef {
            name: "help".into(),
            aliases: vec!["h".into(), "?".into(), "commands".into()],
            description: "Show available commands".into(),
            usage: "/help [command_name]".into(),
            requires_auth: false,
            category: CommandCategory::System,
        }
    }

    async fn execute(&self, cmd: &ParsedCommand, _ctx: &CommandContext) -> CommandResult {
        // When no specific command is requested, show summary.
        if cmd.args.is_empty() {
            let help_text = "\
**Available Commands**

**Session**: /session, /export, /compact
**Model**: /model
**Config**: /config
**Agent**: /context, /status
**System**: /bash, /approve, /help

Type `/help <command>` for details.";
            return CommandResult { success: true, output: help_text.into(), ephemeral: true };
        }
        // Specific command help.
        let target = &cmd.args[0];
        CommandResult {
            success: true,
            output: format!("Help for `/{target}`: (details pending)"),
            ephemeral: true,
        }
    }
}
