//! `/compact` — Trigger context window compaction.

use crate::command_registry::{Command, CommandCategory, CommandContext, CommandDef, CommandResult, ParsedCommand};
use async_trait::async_trait;

pub struct CompactCommand;

#[async_trait]
impl Command for CompactCommand {
    fn definition(&self) -> CommandDef {
        CommandDef {
            name: "compact".into(),
            aliases: vec!["compress".into()],
            description: "Compact the context window to free tokens".into(),
            usage: "/compact [aggressive]".into(),
            requires_auth: false,
            category: CommandCategory::Agent,
        }
    }

    async fn execute(&self, cmd: &ParsedCommand, _ctx: &CommandContext) -> CommandResult {
        let aggressive = cmd.args.first().map(|s| s == "aggressive").unwrap_or(false);
        let mode = if aggressive { "aggressive" } else { "standard" };
        CommandResult {
            success: true,
            output: format!("Context compaction ({mode}) triggered. Freed tokens: (pending)"),
            ephemeral: true,
        }
    }
}
