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

// ── CLI command implementations ──────────────────────────────

/// Agents directory inside user config.
fn agents_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    home.join(".clawdesk").join("agents")
}

/// `clawdesk agent add <id> [--from-toml path]`
///
/// Creates a new agent directory with agent.toml. If `--from-toml` is provided,
/// copies and validates the TOML. Otherwise generates a scaffold.
pub async fn cmd_agent_add(
    id: &str,
    from_toml: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dir = agents_dir().join(id);
    if dir.exists() {
        return Err(format!("Agent directory already exists: {}", dir.display()).into());
    }

    let content = if let Some(path) = from_toml {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {path}: {e}"))?;
        // Validate before saving
        let def = parse_agent_toml(&raw)?;
        let diags = validate_agent(&def);
        let errors: Vec<_> = diags.iter()
            .filter(|d| d.severity == ValidationSeverity::Error)
            .collect();
        if !errors.is_empty() {
            for e in &errors {
                eprintln!("  ✗ {}", e.message);
            }
            return Err(format!("{} validation error(s) — agent not created", errors.len()).into());
        }
        for w in diags.iter().filter(|d| d.severity == ValidationSeverity::Warning) {
            eprintln!("  ⚠ {}", w.message);
        }
        raw
    } else {
        // Generate scaffold TOML
        format!(
            r#"[agent]
id = "{id}"
display_name = "{name}"
model = "claude-sonnet-4-20250514"

[agent.persona]
soul = """
You are {name}. Describe your personality and responsibilities here.
"""

guidelines = """
- Add working guidelines here
"""

[agent.tools]
allow = ["file_read", "file_write", "web_search"]
deny = []

[agent.skills]
activate = []

# [[agent.bindings.channels]]
# channel = "telegram"
# account = "team"

[agent.subagents]
can_spawn = []
max_depth = 2
max_concurrent = 3
"#,
            id = id,
            name = id.replace('-', " ").split_whitespace()
                .map(|w| {
                    let mut c = w.chars();
                    match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().to_string() + c.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        )
    };

    std::fs::create_dir_all(&dir)?;
    let toml_path = dir.join("agent.toml");
    std::fs::write(&toml_path, &content)?;
    println!("✓ Created agent '{}' at {}", id, toml_path.display());

    // Also try to register with the running gateway
    let url = "http://127.0.0.1:18789/api/v1/admin/agents";
    if let Ok(def) = parse_agent_toml(&content) {
        let body = serde_json::json!({
            "name": def.agent.display_name,
            "icon": "🤖",
            "color": "#6366f1",
            "persona": def.agent.persona.soul,
            "skills": def.agent.skills.activate,
            "model": def.agent.model,
            "channels": def.agent.bindings.channels.iter()
                .map(|b| b.channel.clone()).collect::<Vec<_>>(),
        });
        match reqwest::Client::new().post(url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                println!("  ↳ Registered with running gateway");
            }
            _ => {
                println!("  ↳ Gateway not running — agent saved locally. Run 'clawdesk agent apply' to register later.");
            }
        }
    }

    Ok(())
}

/// `clawdesk agent validate`
///
/// Schema-validates all agent.toml files in ~/.clawdesk/agents/.
pub async fn cmd_agent_validate() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dir = agents_dir();
    if !dir.exists() {
        println!("No agents directory at {}", dir.display());
        return Ok(());
    }

    let agents = load_all_agents(&dir)?;
    if agents.is_empty() {
        println!("No agent.toml files found in {}", dir.display());
        return Ok(());
    }

    let mut total_errors = 0;
    let mut total_warnings = 0;

    for (path, def) in &agents {
        let diags = validate_agent(def);
        let errors: Vec<_> = diags.iter().filter(|d| d.severity == ValidationSeverity::Error).collect();
        let warnings: Vec<_> = diags.iter().filter(|d| d.severity == ValidationSeverity::Warning).collect();

        if errors.is_empty() {
            println!("✓ {} ({})", def.agent.id, path.display());
        } else {
            println!("✗ {} ({})", def.agent.id, path.display());
        }

        for e in &errors {
            println!("    ✗ {}", e.message);
            total_errors += 1;
        }
        for w in &warnings {
            println!("    ⚠ {}", w.message);
            total_warnings += 1;
        }
    }

    // Check for binding conflicts
    let defs: Vec<_> = agents.iter().map(|(_, d)| d.clone()).collect();
    let conflicts = detect_binding_conflicts(&defs);
    for c in &conflicts {
        println!("  ⚠ {}", c);
        total_warnings += 1;
    }

    println!();
    println!("{} agents, {} errors, {} warnings",
        agents.len(), total_errors, total_warnings);

    if total_errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// `clawdesk agent list [--bindings] [--json]`
///
/// Lists all agent definitions and their routing table.
pub async fn cmd_agent_list(
    show_bindings: bool,
    json_output: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dir = agents_dir();
    let agents = if dir.exists() {
        load_all_agents(&dir)?
    } else {
        Vec::new()
    };

    if json_output {
        let items: Vec<serde_json::Value> = agents.iter().map(|(path, def)| {
            serde_json::json!({
                "id": def.agent.id,
                "display_name": def.agent.display_name,
                "model": def.agent.model,
                "path": path.display().to_string(),
                "skills": def.agent.skills.activate,
                "bindings": def.agent.bindings.channels.iter().map(|b| {
                    serde_json::json!({
                        "channel": b.channel,
                        "account": b.account,
                        "group": b.group,
                    })
                }).collect::<Vec<_>>(),
                "can_spawn": def.agent.subagents.can_spawn,
            })
        }).collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    if agents.is_empty() {
        println!("No agents configured. Run 'clawdesk agent add <id>' to create one.");
        return Ok(());
    }

    println!("{:<16} {:<24} {:<30} {}", "ID", "NAME", "MODEL", "SKILLS");
    println!("{}", "─".repeat(90));

    for (_, def) in &agents {
        let skills = if def.agent.skills.activate.is_empty() {
            "—".to_string()
        } else {
            def.agent.skills.activate.join(", ")
        };
        println!("{:<16} {:<24} {:<30} {}",
            def.agent.id,
            def.agent.display_name,
            def.agent.model,
            skills,
        );

        if show_bindings {
            for b in &def.agent.bindings.channels {
                let group = b.group.as_deref().unwrap_or("*");
                println!("  ↳ {}:{} (group={})", b.channel, b.account, group);
            }
            if !def.agent.subagents.can_spawn.is_empty() {
                println!("  ↳ can_spawn: {:?}", def.agent.subagents.can_spawn);
            }
        }
    }

    println!();
    println!("{} agent(s)", agents.len());
    Ok(())
}

/// `clawdesk agent apply [id]`
///
/// Hot-reload agent definitions by registering them with the running gateway.
pub async fn cmd_agent_apply(
    gateway_url: &str,
    specific_id: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dir = agents_dir();
    let agents = if dir.exists() {
        load_all_agents(&dir)?
    } else {
        return Err("No agents directory found".into());
    };

    let agents_to_apply: Vec<_> = if let Some(id) = specific_id {
        agents.into_iter().filter(|(_, d)| d.agent.id == id).collect()
    } else {
        agents
    };

    if agents_to_apply.is_empty() {
        println!("No matching agents to apply.");
        return Ok(());
    }

    let client = reqwest::Client::new();
    let mut success = 0;
    let mut failed = 0;

    for (_, def) in &agents_to_apply {
        let url = format!("{}/api/v1/admin/agents", gateway_url);
        let body = serde_json::json!({
            "name": def.agent.display_name,
            "icon": "🤖",
            "color": "#6366f1",
            "persona": def.agent.persona.soul,
            "skills": def.agent.skills.activate,
            "model": def.agent.model,
            "channels": def.agent.bindings.channels.iter()
                .map(|b| b.channel.clone()).collect::<Vec<_>>(),
            "source": "agent.toml",
        });

        match client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                println!("✓ Applied '{}'", def.agent.id);
                success += 1;
            }
            Ok(resp) => {
                let body = resp.text().await.unwrap_or_default();
                eprintln!("✗ Failed to apply '{}': {}", def.agent.id, body);
                failed += 1;
            }
            Err(e) => {
                eprintln!("✗ Failed to apply '{}': {}", def.agent.id, e);
                failed += 1;
            }
        }
    }

    println!();
    println!("{} applied, {} failed", success, failed);
    Ok(())
}

/// `clawdesk agent export <id> [-o path]`
///
/// Export an agent to agent.toml format.
pub async fn cmd_agent_export(
    id: &str,
    output: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dir = agents_dir();
    let agents = if dir.exists() {
        load_all_agents(&dir)?
    } else {
        Vec::new()
    };

    if let Some((_, def)) = agents.iter().find(|(_, d)| d.agent.id == id) {
        let toml_str = toml::to_string_pretty(def)
            .map_err(|e| format!("Failed to serialize: {e}"))?;

        if let Some(path) = output {
            std::fs::write(path, &toml_str)?;
            println!("Exported '{}' to {}", id, path);
        } else {
            println!("{}", toml_str);
        }
    } else {
        return Err(format!("Agent '{}' not found in {}", id, dir.display()).into());
    }

    Ok(())
}

/// Delete an agent by marking it in the SochDB deletion blacklist.
///
/// This prevents the agent from being re-discovered from folder scanning.
/// Works even when the desktop app has the SochDB locked (records deletion
/// in a local marker file as fallback).
pub async fn cmd_agent_delete(
    id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let sochdb_dir = clawdesk_types::dirs::sochdb();
    std::fs::create_dir_all(&sochdb_dir)?;

    // Try to open SochDB directly
    match clawdesk_sochdb::SochStore::open(sochdb_dir.to_str().unwrap_or(".")) {
        Ok(store) => {
            // Delete the agent entry
            let agent_key = format!("agents/{}", id);
            let _ = store.delete_durable(&agent_key);

            // Record in deletion blacklist
            let del_key = format!("deleted_agents/{}", id);
            let ts = chrono::Utc::now().to_rfc3339();
            store.put_durable(&del_key, ts.as_bytes())
                .map_err(|e| format!("Failed to record deletion: {e}"))?;
            println!("✓ Deleted agent '{}' (won't reappear on restart)", id);
        }
        Err(_) => {
            // SochDB locked by desktop — use HTTP API if gateway is running
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()?;
            match client.delete(format!("http://127.0.0.1:18789/api/v1/agents/{}", id))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    println!("✓ Deleted agent '{}' via gateway API", id);
                }
                _ => {
                    eprintln!("⚠ SochDB is locked (desktop running) and gateway is not reachable.");
                    eprintln!("  Delete the agent from the desktop app, or stop the desktop first.");
                    return Err("Cannot delete: SochDB locked and gateway unreachable".into());
                }
            }
        }
    }

    Ok(())
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
