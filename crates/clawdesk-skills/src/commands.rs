//! Slash command registry for skill invocation.
//!
//! ## Slash Commands (P3)
//!
//! Some legacy skills are `user_invocable: true` — the user can trigger
//! them with `/skill-name` in the chat input. This module provides:
//!
//! 1. **SkillCommandRegistry** — maps `/command` → skill ID
//! 2. **Command parsing** — extracts `/command args` from user input
//! 3. **Autocomplete** — prefix-matched command suggestions for UI
//! 4. **Deterministic dispatch** — bypasses LLM skill selection for explicit commands
//!
//! ## Flow
//!
//! ```text
//! User types: /weather London
//!     → parse_command() → Some(Command { name: "weather", args: "London" })
//!     → registry.resolve("weather") → Some(skill_id)
//!     → force-include skill, inject args into prompt
//! ```

use crate::definition::SkillId;
use std::collections::HashMap;
use tracing::debug;

/// A parsed slash command from user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    /// Command name (without the leading `/`).
    pub name: String,
    /// Arguments after the command name.
    pub args: String,
}

impl SlashCommand {
    pub fn new(name: impl Into<String>, args: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: args.into(),
        }
    }
}

impl std::fmt::Display for SlashCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.args.is_empty() {
            write!(f, "/{}", self.name)
        } else {
            write!(f, "/{} {}", self.name, self.args)
        }
    }
}

/// Registration entry for a slash command.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    /// The skill this command invokes.
    pub skill_id: SkillId,
    /// Human-readable description for autocomplete.
    pub description: String,
    /// Optional emoji for UI display.
    pub emoji: Option<String>,
    /// Whether this command is currently available (based on eligibility).
    pub available: bool,
}

/// Registry of slash commands mapped to skills.
pub struct SkillCommandRegistry {
    /// Map: command name → entry.
    commands: HashMap<String, CommandEntry>,
    /// Map: skill_id → command name (reverse lookup).
    skill_to_command: HashMap<String, String>,
}

impl SkillCommandRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
            skill_to_command: HashMap::new(),
        }
    }

    /// Register a slash command for a skill.
    pub fn register(
        &mut self,
        command_name: impl Into<String>,
        skill_id: SkillId,
        description: impl Into<String>,
        emoji: Option<String>,
    ) {
        let name = command_name.into();
        let skill_str = skill_id.as_str().to_string();

        self.skill_to_command
            .insert(skill_str, name.clone());

        self.commands.insert(
            name,
            CommandEntry {
                skill_id,
                description: description.into(),
                emoji,
                available: true,
            },
        );
    }

    /// Resolve a command name to its skill.
    pub fn resolve(&self, command_name: &str) -> Option<&CommandEntry> {
        self.commands.get(command_name)
    }

    /// Get the command name for a skill (if registered).
    pub fn command_for_skill(&self, skill_id: &str) -> Option<&str> {
        self.skill_to_command.get(skill_id).map(|s| s.as_str())
    }

    /// Get all registered commands.
    pub fn all_commands(&self) -> Vec<(&str, &CommandEntry)> {
        let mut cmds: Vec<_> = self.commands.iter().map(|(k, v)| (k.as_str(), v)).collect();
        cmds.sort_by_key(|(name, _)| *name);
        cmds
    }

    /// Get available commands only.
    pub fn available_commands(&self) -> Vec<(&str, &CommandEntry)> {
        self.all_commands()
            .into_iter()
            .filter(|(_, e)| e.available)
            .collect()
    }

    /// Autocomplete: find commands matching a prefix.
    pub fn autocomplete(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let prefix_lower = prefix.to_lowercase();
        let mut matches: Vec<AutocompleteItem> = self
            .commands
            .iter()
            .filter(|(name, entry)| {
                entry.available && name.to_lowercase().starts_with(&prefix_lower)
            })
            .map(|(name, entry)| AutocompleteItem {
                command: format!("/{}", name),
                description: entry.description.clone(),
                emoji: entry.emoji.clone(),
            })
            .collect();

        matches.sort_by(|a, b| a.command.cmp(&b.command));
        matches
    }

    /// Set availability for a command (e.g., after eligibility check).
    pub fn set_available(&mut self, command_name: &str, available: bool) {
        if let Some(entry) = self.commands.get_mut(command_name) {
            entry.available = available;
        }
    }

    /// Number of registered commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl Default for SkillCommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Item returned by autocomplete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    /// The full command string (e.g., "/weather").
    pub command: String,
    /// Description text.
    pub description: String,
    /// Optional emoji.
    pub emoji: Option<String>,
}

/// Parse a slash command from user input.
///
/// Returns `None` if the input doesn't start with `/` or is empty.
///
/// # Examples
/// - `"/weather London"` → `Some(SlashCommand { name: "weather", args: "London" })`
/// - `"/help"` → `Some(SlashCommand { name: "help", args: "" })`
/// - `"hello"` → `None`
/// - `"/"` → `None`
pub fn parse_command(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();

    if !trimmed.starts_with('/') {
        return None;
    }

    let without_slash = &trimmed[1..];
    if without_slash.is_empty() {
        return None;
    }

    // Split on first whitespace
    let (name, args) = match without_slash.find(char::is_whitespace) {
        Some(pos) => (&without_slash[..pos], without_slash[pos..].trim()),
        None => (without_slash, ""),
    };

    // Command names must be alphanumeric + hyphens
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }

    Some(SlashCommand {
        name: name.to_lowercase(),
        args: args.to_string(),
    })
}

/// Check if user input starts with a slash command.
pub fn is_command(input: &str) -> bool {
    parse_command(input).is_some()
}

/// Built-in commands that are always available (not skill-backed).
pub const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("help", "Show available commands"),
    ("skills", "List active skills"),
    ("clear", "Clear conversation history"),
    ("compact", "Compact conversation context"),
    ("config", "Open configuration"),
];

/// Check if a command name is a built-in.
pub fn is_builtin(command_name: &str) -> bool {
    BUILTIN_COMMANDS
        .iter()
        .any(|(name, _)| *name == command_name)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_command() {
        let cmd = parse_command("/weather London").unwrap();
        assert_eq!(cmd.name, "weather");
        assert_eq!(cmd.args, "London");
    }

    #[test]
    fn parse_command_no_args() {
        let cmd = parse_command("/help").unwrap();
        assert_eq!(cmd.name, "help");
        assert_eq!(cmd.args, "");
    }

    #[test]
    fn parse_command_with_extra_spaces() {
        let cmd = parse_command("  /search   some query  ").unwrap();
        assert_eq!(cmd.name, "search");
        assert_eq!(cmd.args, "some query");
    }

    #[test]
    fn parse_not_a_command() {
        assert!(parse_command("hello world").is_none());
        assert!(parse_command("").is_none());
        assert!(parse_command("/").is_none());
    }

    #[test]
    fn parse_hyphenated_command() {
        let cmd = parse_command("/coding-agent fix this").unwrap();
        assert_eq!(cmd.name, "coding-agent");
        assert_eq!(cmd.args, "fix this");
    }

    #[test]
    fn parse_command_lowercases() {
        let cmd = parse_command("/Weather London").unwrap();
        assert_eq!(cmd.name, "weather");
    }

    #[test]
    fn is_command_check() {
        assert!(is_command("/help"));
        assert!(is_command("/weather London"));
        assert!(!is_command("hello"));
        assert!(!is_command("/"));
    }

    #[test]
    fn registry_register_and_resolve() {
        let mut registry = SkillCommandRegistry::new();
        registry.register("weather", SkillId::from("weather-skill"), "Get weather", Some("🌤️".to_string()));

        let entry = registry.resolve("weather").unwrap();
        assert_eq!(entry.skill_id.as_str(), "weather-skill");
        assert_eq!(entry.description, "Get weather");
        assert!(entry.available);
    }

    #[test]
    fn registry_reverse_lookup() {
        let mut registry = SkillCommandRegistry::new();
        registry.register("wx", SkillId::from("weather-skill"), "Weather", None);

        assert_eq!(registry.command_for_skill("weather-skill"), Some("wx"));
        assert_eq!(registry.command_for_skill("nonexistent"), None);
    }

    #[test]
    fn autocomplete_prefix() {
        let mut registry = SkillCommandRegistry::new();
        registry.register("weather", SkillId::from("weather"), "Weather", None);
        registry.register("web-search", SkillId::from("web"), "Web search", None);
        registry.register("git-commit", SkillId::from("git"), "Git commit", None);

        let results = registry.autocomplete("we");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].command, "/weather");
        assert_eq!(results[1].command, "/web-search");
    }

    #[test]
    fn autocomplete_empty_prefix_returns_all() {
        let mut registry = SkillCommandRegistry::new();
        registry.register("a", SkillId::from("a"), "A", None);
        registry.register("b", SkillId::from("b"), "B", None);

        let results = registry.autocomplete("");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn autocomplete_excludes_unavailable() {
        let mut registry = SkillCommandRegistry::new();
        registry.register("avail", SkillId::from("a"), "Available", None);
        registry.register("unavail", SkillId::from("u"), "Unavailable", None);
        registry.set_available("unavail", false);

        let results = registry.autocomplete("");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].command, "/avail");
    }

    #[test]
    fn available_commands_filter() {
        let mut registry = SkillCommandRegistry::new();
        registry.register("a", SkillId::from("a"), "A", None);
        registry.register("b", SkillId::from("b"), "B", None);
        registry.set_available("b", false);

        assert_eq!(registry.all_commands().len(), 2);
        assert_eq!(registry.available_commands().len(), 1);
    }

    #[test]
    fn slash_command_display() {
        let cmd = SlashCommand::new("weather", "London");
        assert_eq!(cmd.to_string(), "/weather London");

        let cmd = SlashCommand::new("help", "");
        assert_eq!(cmd.to_string(), "/help");
    }

    #[test]
    fn is_builtin_check() {
        assert!(is_builtin("help"));
        assert!(is_builtin("skills"));
        assert!(is_builtin("clear"));
        assert!(!is_builtin("weather"));
    }

    #[test]
    fn registry_len() {
        let mut registry = SkillCommandRegistry::new();
        assert!(registry.is_empty());
        registry.register("a", SkillId::from("a"), "A", None);
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
    }
}
