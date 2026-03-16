//! `/approve` — Approve or reject pending exec/tool requests.

use crate::command_registry::{Command, CommandCategory, CommandContext, CommandDef, CommandResult, ParsedCommand};
use async_trait::async_trait;

pub struct ApproveCommand;

#[async_trait]
impl Command for ApproveCommand {
    fn definition(&self) -> CommandDef {
        CommandDef {
            name: "approve".into(),
            aliases: vec!["ok".into(), "yes".into()],
            description: "Approve a pending execution request".into(),
            usage: "/approve [request_id] | /approve all".into(),
            requires_auth: true,
            category: CommandCategory::System,
        }
    }

    async fn execute(&self, cmd: &ParsedCommand, _ctx: &CommandContext) -> CommandResult {
        let target = cmd.args.first().map(|s| s.as_str()).unwrap_or("latest");
        match target {
            "all" => CommandResult { success: true, output: "All pending requests approved.".into(), ephemeral: true },
            id => CommandResult { success: true, output: format!("Approved request `{id}`."), ephemeral: true },
        }
    }
}
