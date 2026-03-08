//! Agent hot-reload — scan `~/.clawdesk/agents/` and build `AgentConfigRegistry`.
//!
//! ## Directory layout
//!
//! ```text
//! ~/.clawdesk/agents/
//!   ├── designer/
//!   │   └── agent.toml      → AgentDefinition
//!   ├── researcher/
//!   │   └── agent.toml
//!   └── coder/
//!       └── agent.toml
//! ```
//!
//! Each agent directory contains an `agent.toml` that defines the agent's
//! identity, model, persona, tool policy, skill activations, and channel
//! bindings. Changes to these files trigger atomic registry reload via
//! `ArcSwap`.
//!
//! ## Hot-reload protocol
//!
//! 1. `AgentLoader::load_fresh()` scans the agents directory.
//! 2. Parses each `agent.toml` into an `AgentSnapshot`.
//! 3. Builds a new `AgentConfigMap` (HashMap keyed by agent ID).
//! 4. Returns `AgentLoadResult` with the map + any errors.
//! 5. Caller stores the map via `ArcSwap` for wait-free reads.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Agent TOML schema (mirrors agent_compose.rs — kept self-contained so the
// gateway can load agents without depending on clawdesk-cli).
// ---------------------------------------------------------------------------

/// Top-level agent definition (parsed from agent.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub agent: AgentSection,
}

/// Main [agent] section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSection {
    pub id: String,
    pub display_name: String,
    #[serde(default = "default_model")]
    pub model: String,
    pub extends: Option<String>,
    #[serde(default)]
    pub persona: PersonaSection,
    #[serde(default)]
    pub tools: ToolPolicySection,
    #[serde(default)]
    pub skills: SkillsSection,
    #[serde(default)]
    pub subagents: SubagentSection,
}

fn default_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonaSection {
    #[serde(default)]
    pub soul: String,
    #[serde(default)]
    pub soul_append: String,
    #[serde(default)]
    pub guidelines: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolPolicySection {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub allow_append: Vec<String>,
    #[serde(default)]
    pub deny_append: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillsSection {
    #[serde(default)]
    pub activate: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubagentSection {
    #[serde(default)]
    pub can_spawn: Vec<String>,
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
}

fn default_max_depth() -> u32 {
    2
}

// ---------------------------------------------------------------------------
// Materialised agent snapshot
// ---------------------------------------------------------------------------

/// A materialised agent configuration ready for use at runtime.
///
/// This is the hot-swappable unit: changes to `agent.toml` produce a
/// new `AgentSnapshot` that is compared (by `config_hash`) to detect
/// actual changes before triggering the swap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSnapshot {
    /// Agent identifier (from TOML).
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// LLM model identifier.
    pub model: String,
    /// System prompt (assembled from persona soul + guidelines).
    pub system_prompt: String,
    /// Tool allow-list (empty = all tools allowed).
    pub tools_allow: Vec<String>,
    /// Tool deny-list.
    pub tools_deny: Vec<String>,
    /// Activated skills.
    pub active_skills: Vec<String>,
    /// Sub-agent spawn policy.
    pub can_spawn: Vec<String>,
    /// Max sub-agent depth.
    pub max_depth: u32,
    /// Source file path for diagnostics.
    pub source_path: PathBuf,
    /// SHA-256 of the raw TOML content — used for change detection.
    pub config_hash: String,
}

/// Map of agent ID → snapshot. This is the unit of atomic swap.
pub type AgentConfigMap = HashMap<String, AgentSnapshot>;

/// Result of a full agent scan.
#[derive(Debug)]
pub struct AgentLoadResult {
    /// Successfully loaded agent snapshots.
    pub agents: AgentConfigMap,
    /// Errors encountered during loading (agent_id, error message).
    pub errors: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Agent loader
// ---------------------------------------------------------------------------

/// Filesystem-based agent definition loader.
///
/// Scans `~/.clawdesk/agents/` for agent.toml files and materialises them
/// into `AgentSnapshot` instances for the runtime registry.
pub struct AgentLoader {
    agents_dir: PathBuf,
}

impl AgentLoader {
    pub fn new(agents_dir: PathBuf) -> Self {
        Self { agents_dir }
    }

    /// Scan the agents directory and load all valid agent definitions.
    ///
    /// Returns `(loaded_snapshots, errors)`. Errors in individual agents
    /// do not prevent others from loading.
    pub fn load_fresh(&self) -> AgentLoadResult {
        let mut agents = HashMap::new();
        let mut errors = Vec::new();

        if !self.agents_dir.exists() {
            debug!(dir = %self.agents_dir.display(), "agents directory does not exist");
            return AgentLoadResult { agents, errors };
        }

        let entries = match std::fs::read_dir(&self.agents_dir) {
            Ok(e) => e,
            Err(e) => {
                warn!(dir = %self.agents_dir.display(), %e, "failed to read agents directory");
                errors.push(("_dir".into(), e.to_string()));
                return AgentLoadResult { agents, errors };
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let toml_path = path.join("agent.toml");
            if !toml_path.exists() {
                continue;
            }

            match self.load_agent(&toml_path) {
                Ok(snapshot) => {
                    info!(
                        id = %snapshot.id,
                        model = %snapshot.model,
                        skills = snapshot.active_skills.len(),
                        "loaded agent definition"
                    );
                    agents.insert(snapshot.id.clone(), snapshot);
                }
                Err(e) => {
                    let dir_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    warn!(agent = %dir_name, %e, "failed to load agent definition");
                    errors.push((dir_name, e));
                }
            }
        }

        info!(
            loaded = agents.len(),
            errors = errors.len(),
            "agent definitions scanned"
        );

        AgentLoadResult { agents, errors }
    }

    /// Load a single agent.toml file.
    fn load_agent(&self, toml_path: &Path) -> Result<AgentSnapshot, String> {
        let content = std::fs::read_to_string(toml_path)
            .map_err(|e| format!("read failed: {e}"))?;

        let def: AgentDefinition = toml::from_str(&content)
            .map_err(|e| format!("TOML parse error: {e}"))?;

        let agent = &def.agent;

        // Build system prompt from persona.
        let mut system_prompt = agent.persona.soul.clone();
        if !agent.persona.guidelines.is_empty() {
            if !system_prompt.is_empty() {
                system_prompt.push_str("\n\n");
            }
            system_prompt.push_str(&agent.persona.guidelines);
        }

        // Compute config hash for change detection.
        let config_hash = sha256_hex(content.as_bytes());

        Ok(AgentSnapshot {
            id: agent.id.clone(),
            display_name: agent.display_name.clone(),
            model: agent.model.clone(),
            system_prompt,
            tools_allow: agent.tools.allow.clone(),
            tools_deny: agent.tools.deny.clone(),
            active_skills: agent.skills.activate.clone(),
            can_spawn: agent.subagents.can_spawn.clone(),
            max_depth: agent.subagents.max_depth,
            source_path: toml_path.to_path_buf(),
            config_hash,
        })
    }

    /// The directory being scanned.
    pub fn agents_dir(&self) -> &Path {
        &self.agents_dir
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(data);
    hex::encode(hash)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn load_agent_from_toml() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("test-agent");
        fs::create_dir_all(&agent_dir).unwrap();

        fs::write(
            agent_dir.join("agent.toml"),
            r#"
[agent]
id = "test-agent"
display_name = "Test Agent"
model = "claude-sonnet-4-20250514"

[agent.persona]
soul = "You are a helpful test agent."
guidelines = "Be concise."

[agent.tools]
allow = ["file_read", "exec"]
deny = ["browser"]

[agent.skills]
activate = ["core/summarize"]

[agent.subagents]
can_spawn = ["researcher"]
max_depth = 3
"#,
        )
        .unwrap();

        let loader = AgentLoader::new(dir.path().to_path_buf());
        let result = loader.load_fresh();

        assert_eq!(result.agents.len(), 1);
        assert!(result.errors.is_empty());

        let snap = &result.agents["test-agent"];
        assert_eq!(snap.display_name, "Test Agent");
        assert_eq!(snap.model, "claude-sonnet-4-20250514");
        assert!(snap.system_prompt.contains("helpful test agent"));
        assert!(snap.system_prompt.contains("Be concise."));
        assert_eq!(snap.tools_allow, vec!["file_read", "exec"]);
        assert_eq!(snap.tools_deny, vec!["browser"]);
        assert_eq!(snap.active_skills, vec!["core/summarize"]);
        assert_eq!(snap.can_spawn, vec!["researcher"]);
        assert_eq!(snap.max_depth, 3);
        assert!(!snap.config_hash.is_empty());
    }

    #[test]
    fn load_skips_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("bad-agent");
        fs::create_dir_all(&agent_dir).unwrap();
        fs::write(agent_dir.join("agent.toml"), "this is not valid toml {{{}").unwrap();

        let loader = AgentLoader::new(dir.path().to_path_buf());
        let result = loader.load_fresh();

        assert!(result.agents.is_empty());
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn load_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let loader = AgentLoader::new(dir.path().to_path_buf());
        let result = loader.load_fresh();

        assert!(result.agents.is_empty());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn load_multiple_agents() {
        let dir = tempfile::tempdir().unwrap();
        for name in &["alpha", "beta", "gamma"] {
            let agent_dir = dir.path().join(name);
            fs::create_dir_all(&agent_dir).unwrap();
            fs::write(
                agent_dir.join("agent.toml"),
                format!(
                    r#"
[agent]
id = "{name}"
display_name = "Agent {name}"
"#
                ),
            )
            .unwrap();
        }

        let loader = AgentLoader::new(dir.path().to_path_buf());
        let result = loader.load_fresh();

        assert_eq!(result.agents.len(), 3);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn config_hash_changes_on_content_change() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("hash-test");
        fs::create_dir_all(&agent_dir).unwrap();

        let toml_v1 = r#"
[agent]
id = "hash-test"
display_name = "Hash Test v1"
"#;
        fs::write(agent_dir.join("agent.toml"), toml_v1).unwrap();
        let loader = AgentLoader::new(dir.path().to_path_buf());
        let r1 = loader.load_fresh();
        let hash1 = r1.agents["hash-test"].config_hash.clone();

        let toml_v2 = r#"
[agent]
id = "hash-test"
display_name = "Hash Test v2"
model = "gpt-4o"
"#;
        fs::write(agent_dir.join("agent.toml"), toml_v2).unwrap();
        let r2 = loader.load_fresh();
        let hash2 = r2.agents["hash-test"].config_hash.clone();

        assert_ne!(hash1, hash2);
    }
}
