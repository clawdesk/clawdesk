//! `/context` — Show context window usage, token budget, and active skills.

use crate::command_registry::{Command, CommandCategory, CommandContext, CommandDef, CommandResult, ParsedCommand};
use async_trait::async_trait;

pub struct ContextCommand;

#[async_trait]
impl Command for ContextCommand {
    fn definition(&self) -> CommandDef {
        CommandDef {
            name: "context".into(),
            aliases: vec!["ctx".into()],
            description: "Show context window usage and active skills".into(),
            usage: "/context [report|compact|skills]".into(),
            requires_auth: false,
            category: CommandCategory::Agent,
        }
    }

    async fn execute(&self, cmd: &ParsedCommand, _ctx: &CommandContext) -> CommandResult {
        let sub = cmd.args.first().map(|s| s.as_str()).unwrap_or("report");
        match sub {
            "compact" => CommandResult { success: true, output: "Context compaction triggered.".into(), ephemeral: true },
            "skills" => CommandResult { success: true, output: "Active skills: (listing pending)".into(), ephemeral: true },
            "report" | _ => CommandResult {
                success: true,
                output: "**Context Report**\n• Tokens used: (pending)\n• Budget remaining: (pending)\n• Active skills: (pending)\n• Memory entries: (pending)".into(),
                ephemeral: true,
            },
        }
    }
}
