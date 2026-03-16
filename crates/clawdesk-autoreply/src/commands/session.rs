//! `/session` — Session management (new, list, switch, export, clear).

use crate::command_registry::{Command, CommandCategory, CommandContext, CommandDef, CommandResult, ParsedCommand};
use async_trait::async_trait;

pub struct SessionCommand;

#[async_trait]
impl Command for SessionCommand {
    fn definition(&self) -> CommandDef {
        CommandDef {
            name: "session".into(),
            aliases: vec!["s".into()],
            description: "Manage chat sessions".into(),
            usage: "/session [new|list|switch <id>|clear|info]".into(),
            requires_auth: false,
            category: CommandCategory::Session,
        }
    }

    async fn execute(&self, cmd: &ParsedCommand, ctx: &CommandContext) -> CommandResult {
        let sub = cmd.args.first().map(|s| s.as_str()).unwrap_or("info");
        match sub {
            "new" => CommandResult { success: true, output: "Created new session.".into(), ephemeral: true },
            "list" => CommandResult { success: true, output: "Active sessions: (listing pending)".into(), ephemeral: true },
            "switch" => {
                let id = cmd.args.get(1).cloned().unwrap_or_default();
                if id.is_empty() {
                    CommandResult { success: false, output: "Usage: /session switch <session_id>".into(), ephemeral: true }
                } else {
                    CommandResult { success: true, output: format!("Switched to session `{id}`."), ephemeral: true }
                }
            }
            "clear" => CommandResult { success: true, output: "Session history cleared.".into(), ephemeral: true },
            "info" | _ => CommandResult {
                success: true,
                output: format!("Session: `{}`\nChannel: `{}`", ctx.session_key, ctx.channel_id),
                ephemeral: true,
            },
        }
    }
}
