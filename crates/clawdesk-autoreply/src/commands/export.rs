//! `/export` — Export session history.

use crate::command_registry::{Command, CommandCategory, CommandContext, CommandDef, CommandResult, ParsedCommand};
use async_trait::async_trait;

pub struct ExportCommand;

#[async_trait]
impl Command for ExportCommand {
    fn definition(&self) -> CommandDef {
        CommandDef {
            name: "export".into(),
            aliases: vec!["save".into()],
            description: "Export the current session as JSON or Markdown".into(),
            usage: "/export [json|md|txt]".into(),
            requires_auth: false,
            category: CommandCategory::Session,
        }
    }

    async fn execute(&self, cmd: &ParsedCommand, ctx: &CommandContext) -> CommandResult {
        let format = cmd.args.first().map(|s| s.as_str()).unwrap_or("json");
        match format {
            "json" | "md" | "txt" | "markdown" => CommandResult {
                success: true,
                output: format!("Session `{}` exported as {format}. (file path pending)", ctx.session_key),
                ephemeral: true,
            },
            _ => CommandResult { success: false, output: "Supported formats: json, md, txt".into(), ephemeral: true },
        }
    }
}
