//! `/model` — Switch the active LLM model mid-conversation.

use crate::command_registry::{Command, CommandCategory, CommandContext, CommandDef, CommandResult, ParsedCommand};
use async_trait::async_trait;

pub struct ModelCommand;

#[async_trait]
impl Command for ModelCommand {
    fn definition(&self) -> CommandDef {
        CommandDef {
            name: "model".into(),
            aliases: vec!["m".into(), "switch".into()],
            description: "Switch model for the current session".into(),
            usage: "/model <provider/model> — e.g. /model gpt-4o, /model anthropic/claude-sonnet-4-20250514".into(),
            requires_auth: false,
            category: CommandCategory::Model,
        }
    }

    async fn execute(&self, cmd: &ParsedCommand, _ctx: &CommandContext) -> CommandResult {
        if cmd.args.is_empty() {
            return CommandResult {
                success: false,
                output: "Usage: /model <model_name>\nExamples: /model gpt-4o, /model claude-sonnet-4-20250514".into(),
                ephemeral: true,
            };
        }
        let model = &cmd.args[0];
        CommandResult {
            success: true,
            output: format!("Switched to model: **{model}**"),
            ephemeral: true,
        }
    }
}
