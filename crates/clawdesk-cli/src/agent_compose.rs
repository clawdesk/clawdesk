//! Declarative agent composition — TOML-based agent definition with schema
//! validation, hot-reload, and CLI management.
//!
//! Replaces manual JSON config editing with validated, commentable TOML files.
//! Agents are defined in `~/.clawdesk/agents/<id>/agent.toml`.
//!
//! ## CLI Surface
//!
//! ```text
//! clawdesk agent add <id>         # Interactive wizard → generates agent.toml
//! clawdesk agent validate         # Schema-validate all agent.toml files
//! clawdesk agent list --bindings  # Show routing table
//! clawdesk agent apply            # Hot-reload without restart
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Agent TOML schema
// ---------------------------------------------------------------------------

/// Top-level agent definition (parsed from agent.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub agent: AgentSection,
}

/// Main [agent] section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSection {
    /// Unique agent identifier.
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// LLM model to use.
    #[serde(default = "default_model")]
    pub model: String,
    /// Optional base template to extend.
    pub extends: Option<String>,
    /// Persona configuration.
    #[serde(default)]
    pub persona: PersonaSection,
    /// Tool policy.
    #[serde(default)]
    pub tools: ToolPolicySection,
    /// Activated skills.
    #[serde(default)]
    pub skills: SkillsSection,
    /// Channel bindings.
    #[serde(default)]
    pub bindings: BindingsSection,
    /// Sub-agent spawn policy.
    #[serde(default)]
    pub subagents: SubagentSection,
}

fn default_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}

/// Agent persona (soul, guidelines).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonaSection {
    /// The agent's core identity/personality prompt.
    #[serde(default)]
    pub soul: String,
    /// Appended to a base template's soul (for inheritance).
    #[serde(default)]
    pub soul_append: String,
    /// Working guidelines.
    #[serde(default)]
    pub guidelines: String,
}

/// Tool allow/deny policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolPolicySection {
    /// Explicitly allowed tools.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Explicitly denied tools.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Tools to add to a base template's allow list.
    #[serde(default)]
    pub allow_append: Vec<String>,
    /// Tools to add to a base template's deny list.
    #[serde(default)]
    pub deny_append: Vec<String>,
}

/// Skill activation configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillsSection {
    /// Skills to activate for this agent.
    #[serde(default)]
    pub activate: Vec<String>,
}

/// Channel bindings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BindingsSection {
    /// Channel binding entries.
    #[serde(default)]
    pub channels: Vec<ChannelBinding>,
}

/// A single channel binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelBinding {
    /// Channel type (telegram, slack, discord, etc.)
    pub channel: String,
    /// Account/workspace identifier.
    pub account: String,
    /// Optional group filter.
    pub group: Option<String>,
    /// Optional thread filter.
    pub thread: Option<String>,
}

/// Sub-agent spawn policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentSection {
    /// Agent IDs this agent is allowed to spawn.
    #[serde(default)]
    pub can_spawn: Vec<String>,
    /// Maximum spawn depth.
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Maximum concurrent sub-agents.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
}

impl Default for SubagentSection {
    fn default() -> Self {
        Self {
            can_spawn: Vec::new(),
            max_depth: 2,
            max_concurrent: 5,
        }
    }
}

fn default_max_depth() -> u32 { 2 }
fn default_max_concurrent() -> u32 { 5 }

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// A validation diagnostic.
#[derive(Debug, Clone)]
pub struct ValidationDiagnostic {
    pub agent_id: String,
    pub severity: ValidationSeverity,
    pub message: String,
}

/// Validation severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationSeverity {
    Error,
    Warning,
}

/// Validate an agent definition.
pub fn validate_agent(def: &AgentDefinition) -> Vec<ValidationDiagnostic> {
    let mut diags = Vec::new();
    let agent = &def.agent;

    // ID validation
    if agent.id.is_empty() {
        diags.push(ValidationDiagnostic {
            agent_id: agent.id.clone(),
            severity: ValidationSeverity::Error,
            message: "Agent ID cannot be empty".into(),
        });
    }

    if agent.id.contains(char::is_whitespace) {
        diags.push(ValidationDiagnostic {
            agent_id: agent.id.clone(),
            severity: ValidationSeverity::Error,
            message: "Agent ID cannot contain whitespace".into(),
        });
    }

    // Display name
    if agent.display_name.is_empty() {
        diags.push(ValidationDiagnostic {
            agent_id: agent.id.clone(),
            severity: ValidationSeverity::Warning,
            message: "display_name is empty — will use ID as display name".into(),
        });
    }

    // Persona
    if agent.persona.soul.is_empty() && agent.persona.soul_append.is_empty() && agent.extends.is_none() {
        diags.push(ValidationDiagnostic {
            agent_id: agent.id.clone(),
            severity: ValidationSeverity::Warning,
            message: "No persona.soul defined and no template extended — agent will have generic personality".into(),
        });
    }

    // Tool conflicts
    for tool in &agent.tools.allow {
        if agent.tools.deny.contains(tool) {
            diags.push(ValidationDiagnostic {
                agent_id: agent.id.clone(),
                severity: ValidationSeverity::Error,
                message: format!("Tool '{tool}' is in both allow and deny lists"),
            });
        }
    }

    // Binding validation
    for (i, binding) in agent.bindings.channels.iter().enumerate() {
        if binding.channel.is_empty() {
            diags.push(ValidationDiagnostic {
                agent_id: agent.id.clone(),
                severity: ValidationSeverity::Error,
                message: format!("Binding #{i}: channel type is empty"),
            });
        }
        if binding.account.is_empty() {
            diags.push(ValidationDiagnostic {
                agent_id: agent.id.clone(),
                severity: ValidationSeverity::Error,
                message: format!("Binding #{i}: account is empty"),
            });
        }
    }

    // Sub-agent policy
    if agent.subagents.max_depth > 10 {
        diags.push(ValidationDiagnostic {
            agent_id: agent.id.clone(),
            severity: ValidationSeverity::Warning,
            message: format!(
                "max_depth={} is very high — deep sub-agent chains impact latency exponentially",
                agent.subagents.max_depth
            ),
        });
    }

    diags
}

/// Parse an agent.toml file.
pub fn parse_agent_toml(content: &str) -> Result<AgentDefinition, String> {
    toml::from_str(content).map_err(|e| format!("Invalid agent.toml: {e}"))
}

/// Load all agent definitions from a directory.
pub fn load_all_agents(agents_dir: &Path) -> Result<Vec<(PathBuf, AgentDefinition)>, String> {
    let mut agents = Vec::new();

    if !agents_dir.exists() {
        return Ok(agents);
    }

    let entries = std::fs::read_dir(agents_dir)
        .map_err(|e| format!("Failed to read agents directory: {e}"))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let toml_path = path.join("agent.toml");
        if !toml_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&toml_path)
            .map_err(|e| format!("Failed to read {}: {e}", toml_path.display()))?;

        let def = parse_agent_toml(&content)?;
        agents.push((path, def));
    }

    Ok(agents)
}

/// Resolve template inheritance.
///
/// Uses a prototype chain model: `agent.field ?? template.field ?? default`.
/// Chain depth is bounded at 3 (agent → template → base).
pub fn resolve_inheritance(
    agent: &AgentDefinition,
    templates: &HashMap<String, AgentDefinition>,
) -> AgentDefinition {
    let mut resolved = agent.clone();

    if let Some(ref extends) = agent.agent.extends {
        if let Some(template) = templates.get(extends.trim_start_matches("builtin:")) {
            let tmpl = &template.agent;

            // Soul: append semantics
            if resolved.agent.persona.soul.is_empty() {
                resolved.agent.persona.soul = tmpl.persona.soul.clone();
            }
            if !resolved.agent.persona.soul_append.is_empty() {
                resolved.agent.persona.soul = format!(
                    "{}\n\n{}",
                    resolved.agent.persona.soul,
                    resolved.agent.persona.soul_append
                );
            }

            // Guidelines: inherit if empty
            if resolved.agent.persona.guidelines.is_empty() {
                resolved.agent.persona.guidelines = tmpl.persona.guidelines.clone();
            }

            // Model: inherit if default
            if resolved.agent.model == default_model() && tmpl.model != default_model() {
                resolved.agent.model = tmpl.model.clone();
            }

            // Tools: set union for appends
            if resolved.agent.tools.allow.is_empty() && !tmpl.tools.allow.is_empty() {
                resolved.agent.tools.allow = tmpl.tools.allow.clone();
            }
            for tool in &resolved.agent.tools.allow_append {
                if !resolved.agent.tools.allow.contains(tool) {
                    resolved.agent.tools.allow.push(tool.clone());
                }
            }

            if resolved.agent.tools.deny.is_empty() && !tmpl.tools.deny.is_empty() {
                resolved.agent.tools.deny = tmpl.tools.deny.clone();
            }
            for tool in &resolved.agent.tools.deny_append {
                if !resolved.agent.tools.deny.contains(tool) {
                    resolved.agent.tools.deny.push(tool.clone());
                }
            }

            // Skills: union
            for skill in &tmpl.skills.activate {
                if !resolved.agent.skills.activate.contains(skill) {
                    resolved.agent.skills.activate.push(skill.clone());
                }
            }
        }
    }

    resolved
}

/// Detect binding conflicts between agents.
///
/// O(B²) where B = total bindings, but B < 100 in practice.
pub fn detect_binding_conflicts(agents: &[AgentDefinition]) -> Vec<String> {
    let mut conflicts = Vec::new();
    let mut bindings: Vec<(String, &ChannelBinding)> = Vec::new();

    for agent in agents {
        for binding in &agent.agent.bindings.channels {
            bindings.push((agent.agent.id.clone(), binding));
        }
    }

    for i in 0..bindings.len() {
        for j in (i + 1)..bindings.len() {
            let (agent_a, bind_a) = &bindings[i];
            let (agent_b, bind_b) = &bindings[j];

            if bind_a.channel == bind_b.channel
                && bind_a.account == bind_b.account
                && bind_a.group == bind_b.group
                && bind_a.thread == bind_b.thread
            {
                conflicts.push(format!(
                    "Agents '{agent_a}' and '{agent_b}' have overlapping binding: {}:{}",
                    bind_a.channel, bind_a.account
                ));
            }
        }
    }

    conflicts
}

/// Compute binding specificity: |{channel, account, group, thread} ∩ specified_fields|.
pub fn binding_specificity(binding: &ChannelBinding) -> usize {
    let mut specificity = 0;
    if !binding.channel.is_empty() { specificity += 1; }
    if !binding.account.is_empty() { specificity += 1; }
    if binding.group.is_some() { specificity += 1; }
    if binding.thread.is_some() { specificity += 1; }
    specificity
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_toml() -> &'static str {
        r#"
[agent]
id = "designer"
display_name = "UI/UX Designer"
model = "claude-sonnet-4-20250514"

[agent.persona]
soul = "You are a senior product designer with 15 years of experience."

[agent.tools]
allow = ["browser", "canvas", "file_read"]
deny = ["exec", "file_write"]

[agent.skills]
activate = ["core/image-description"]

[[agent.bindings.channels]]
channel = "telegram"
account = "design-team"

[[agent.bindings.channels]]
channel = "slack"
account = "design-channel"

[agent.subagents]
can_spawn = ["researcher"]
max_depth = 1
max_concurrent = 3
"#
    }

    #[test]
    fn test_parse_agent_toml() {
        let def = parse_agent_toml(sample_toml()).unwrap();
        assert_eq!(def.agent.id, "designer");
        assert_eq!(def.agent.display_name, "UI/UX Designer");
        assert_eq!(def.agent.tools.allow.len(), 3);
        assert_eq!(def.agent.tools.deny.len(), 2);
        assert_eq!(def.agent.bindings.channels.len(), 2);
        assert_eq!(def.agent.subagents.can_spawn, vec!["researcher"]);
    }

    #[test]
    fn test_validate_agent_ok() {
        let def = parse_agent_toml(sample_toml()).unwrap();
        let diags = validate_agent(&def);
        assert!(diags.iter().all(|d| d.severity != ValidationSeverity::Error));
    }

    #[test]
    fn test_validate_empty_id() {
        let toml = r#"
[agent]
id = ""
display_name = "Test"
"#;
        let def = parse_agent_toml(toml).unwrap();
        let diags = validate_agent(&def);
        assert!(diags.iter().any(|d| d.message.contains("cannot be empty")));
    }

    #[test]
    fn test_validate_tool_conflict() {
        let toml = r#"
[agent]
id = "test"
display_name = "Test"
[agent.tools]
allow = ["exec"]
deny = ["exec"]
"#;
        let def = parse_agent_toml(toml).unwrap();
        let diags = validate_agent(&def);
        assert!(diags.iter().any(|d| d.message.contains("both allow and deny")));
    }

    #[test]
    fn test_binding_specificity() {
        let binding = ChannelBinding {
            channel: "telegram".into(),
            account: "team".into(),
            group: Some("design".into()),
            thread: None,
        };
        assert_eq!(binding_specificity(&binding), 3);
    }

    #[test]
    fn test_detect_binding_conflicts() {
        let a = parse_agent_toml(r#"
[agent]
id = "a"
display_name = "A"
[[agent.bindings.channels]]
channel = "slack"
account = "general"
"#).unwrap();

        let b = parse_agent_toml(r#"
[agent]
id = "b"
display_name = "B"
[[agent.bindings.channels]]
channel = "slack"
account = "general"
"#).unwrap();

        let conflicts = detect_binding_conflicts(&[a, b]);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].contains("overlapping"));
    }

    #[test]
    fn test_resolve_inheritance() {
        let template = parse_agent_toml(r#"
[agent]
id = "base-designer"
display_name = "Base Designer"
[agent.persona]
soul = "You are a designer."
[agent.tools]
allow = ["browser", "canvas"]
deny = ["exec"]
[agent.skills]
activate = ["core/image-description"]
"#).unwrap();

        let agent = parse_agent_toml(r#"
[agent]
id = "my-designer"
display_name = "My Designer"
extends = "base-designer"
[agent.persona]
soul_append = "You specialize in mobile design."
[agent.tools]
allow_append = ["figma_export"]
"#).unwrap();

        let mut templates = HashMap::new();
        templates.insert("base-designer".to_string(), template);

        let resolved = resolve_inheritance(&agent, &templates);
        assert!(resolved.agent.persona.soul.contains("You are a designer"));
        assert!(resolved.agent.persona.soul.contains("mobile design"));
        assert!(resolved.agent.tools.allow.contains(&"figma_export".to_string()));
        assert!(resolved.agent.tools.deny.contains(&"exec".to_string()));
        assert!(resolved.agent.skills.activate.contains(&"core/image-description".to_string()));
    }

    #[test]
    fn test_no_conflicts_different_channels() {
        let a = parse_agent_toml(r#"
[agent]
id = "a"
display_name = "A"
[[agent.bindings.channels]]
channel = "telegram"
account = "team"
"#).unwrap();

        let b = parse_agent_toml(r#"
[agent]
id = "b"
display_name = "B"
[[agent.bindings.channels]]
channel = "slack"
account = "team"
"#).unwrap();

        let conflicts = detect_binding_conflicts(&[a, b]);
        assert!(conflicts.is_empty());
    }
}
