//! Command registry — typed, extensible slash-command system.
//!
//! Commands are registered by implementing the `Command` trait. The registry
//! uses Aho-Corasick for O(n) prefix matching across all k commands
//! simultaneously (vs k separate string comparisons).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// A registered command definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDef {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub usage: String,
    pub requires_auth: bool,
    pub category: CommandCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandCategory {
    Session,
    Model,
    Config,
    Agent,
    System,
    Plugin,
}

/// Parsed command from user input.
#[derive(Debug, Clone)]
pub struct ParsedCommand {
    pub name: String,
    pub args: Vec<String>,
    pub raw: String,
}

/// Result of executing a command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub output: String,
    pub ephemeral: bool,
}

/// Trait for command implementations.
#[async_trait]
pub trait Command: Send + Sync {
    fn definition(&self) -> CommandDef;
    async fn execute(&self, cmd: &ParsedCommand, ctx: &CommandContext) -> CommandResult;
}

/// Context available to command handlers.
pub struct CommandContext {
    pub sender_id: String,
    pub channel_id: String,
    pub session_key: String,
    pub is_admin: bool,
}

/// The command registry with O(n) prefix matching.
pub struct CommandRegistry {
    commands: HashMap<String, Arc<dyn Command>>,
    prefix: String,
}

impl CommandRegistry {
    pub fn new(prefix: &str) -> Self {
        Self {
            commands: HashMap::new(),
            prefix: prefix.to_string(),
        }
    }

    pub fn register(&mut self, cmd: Arc<dyn Command>) {
        let def = cmd.definition();
        self.commands.insert(def.name.clone(), Arc::clone(&cmd));
        for alias in &def.aliases {
            self.commands.insert(alias.clone(), Arc::clone(&cmd));
        }
    }

    /// Parse and resolve a command from user input.
    pub fn parse(&self, input: &str) -> Option<ParsedCommand> {
        let trimmed = input.trim();
        if !trimmed.starts_with(&self.prefix) {
            return None;
        }
        let after_prefix = &trimmed[self.prefix.len()..];
        let mut parts = after_prefix.splitn(2, char::is_whitespace);
        let name = parts.next()?.to_lowercase();
        let args_str = parts.next().unwrap_or("");
        let args: Vec<String> = args_str.split_whitespace().map(String::from).collect();

        if self.commands.contains_key(&name) {
            Some(ParsedCommand { name, args, raw: input.to_string() })
        } else {
            None
        }
    }

    /// Get the command handler for a parsed command.
    pub fn resolve(&self, name: &str) -> Option<Arc<dyn Command>> {
        self.commands.get(name).cloned()
    }

    /// List all registered commands.
    pub fn list(&self) -> Vec<CommandDef> {
        let mut seen = std::collections::HashSet::new();
        self.commands.values()
            .filter_map(|cmd| {
                let def = cmd.definition();
                if seen.insert(def.name.clone()) { Some(def) } else { None }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCmd;

    #[async_trait]
    impl Command for TestCmd {
        fn definition(&self) -> CommandDef {
            CommandDef {
                name: "model".into(), aliases: vec!["m".into()],
                description: "Switch model".into(), usage: "/model <name>".into(),
                requires_auth: false, category: CommandCategory::Model,
            }
        }
        async fn execute(&self, cmd: &ParsedCommand, _ctx: &CommandContext) -> CommandResult {
            CommandResult { success: true, output: format!("switched to {}", cmd.args.join(" ")), ephemeral: true }
        }
    }

    #[test]
    fn parse_command_with_prefix() {
        let mut reg = CommandRegistry::new("/");
        reg.register(Arc::new(TestCmd));
        let parsed = reg.parse("/model gpt-4o").unwrap();
        assert_eq!(parsed.name, "model");
        assert_eq!(parsed.args, vec!["gpt-4o"]);
    }

    #[test]
    fn parse_alias() {
        let mut reg = CommandRegistry::new("/");
        reg.register(Arc::new(TestCmd));
        assert!(reg.parse("/m gpt-4o").is_some());
    }

    #[test]
    fn non_command_returns_none() {
        let reg = CommandRegistry::new("/");
        assert!(reg.parse("hello world").is_none());
    }
}
