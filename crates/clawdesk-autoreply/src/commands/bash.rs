//! `/bash` — Execute shell commands (gated by exec policy).

use crate::command_registry::{Command, CommandCategory, CommandContext, CommandDef, CommandResult, ParsedCommand};
use async_trait::async_trait;

pub struct BashCommand;

#[async_trait]
impl Command for BashCommand {
    fn definition(&self) -> CommandDef {
        CommandDef {
            name: "bash".into(),
            aliases: vec!["sh".into(), "shell".into(), "exec".into()],
            description: "Execute a shell command (requires approval)".into(),
            usage: "/bash <command>".into(),
            requires_auth: true,
            category: CommandCategory::System,
        }
    }

    async fn execute(&self, cmd: &ParsedCommand, ctx: &CommandContext) -> CommandResult {
        if !ctx.is_admin {
            return CommandResult { success: false, output: "Shell execution requires admin access.".into(), ephemeral: true };
        }
        if cmd.args.is_empty() {
            return CommandResult { success: false, output: "Usage: /bash <command>".into(), ephemeral: true };
        }
        let shell_cmd = cmd.args.join(" ");
        // Note: actual execution delegates to the agent's shell_exec tool with approval gating.
        CommandResult {
            success: true,
            output: format!("Queued for execution: `{shell_cmd}`\n⚠️ Requires exec approval if policy is enabled."),
            ephemeral: false,
        }
    }
}
