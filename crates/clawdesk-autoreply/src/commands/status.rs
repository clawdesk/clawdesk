//! `/status` — Show system status and health.

use crate::command_registry::{Command, CommandCategory, CommandContext, CommandDef, CommandResult, ParsedCommand};
use async_trait::async_trait;

pub struct StatusCommand;

#[async_trait]
impl Command for StatusCommand {
    fn definition(&self) -> CommandDef {
        CommandDef {
            name: "status".into(),
            aliases: vec!["health".into(), "info".into()],
            description: "Show agent status, provider health, and resource usage".into(),
            usage: "/status [providers|channels|memory]".into(),
            requires_auth: false,
            category: CommandCategory::System,
        }
    }

    async fn execute(&self, cmd: &ParsedCommand, _ctx: &CommandContext) -> CommandResult {
        let sub = cmd.args.first().map(|s| s.as_str()).unwrap_or("all");
        match sub {
            "providers" => CommandResult { success: true, output: "Provider health: (pending)".into(), ephemeral: true },
            "channels" => CommandResult { success: true, output: "Channel status: (pending)".into(), ephemeral: true },
            "memory" => CommandResult { success: true, output: "Memory usage: (pending)".into(), ephemeral: true },
            "all" | _ => CommandResult {
                success: true,
                output: "**System Status**\n• Providers: (pending)\n• Channels: (pending)\n• Memory: (pending)\n• Uptime: (pending)".into(),
                ephemeral: true,
            },
        }
    }
}
