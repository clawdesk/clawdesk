//! Load agent configs from TOML files on disk.
//!
//! Scans a directory for `*.toml` files, parses each as an `AgentConfig`,
//! and registers them in the `AgentRegistry`. Complexity: O(n) in file count.

use crate::error::AgentConfigError;
use crate::registry::AgentRegistry;
use crate::schema::AgentConfig;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

/// Loads agent TOML files from a directory into a registry.
pub struct AgentLoader;

impl AgentLoader {
    /// Load all `*.toml` files from a directory into the registry.
    ///
    /// Skips files that fail to parse, logging warnings.
    /// Returns the number of successfully loaded agents.
    pub fn load_dir(dir: &Path, registry: &AgentRegistry) -> Result<usize, AgentConfigError> {
        if !dir.exists() {
            return Err(AgentConfigError::DirectoryNotFound(dir.to_path_buf()));
        }

        let mut count = 0;

        let entries = std::fs::read_dir(dir)
            .map_err(|e| AgentConfigError::IoError(e.to_string()))?;

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to read directory entry: {}", e);
                    continue;
                }
            };

            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }

            match Self::load_file(&path) {
                Ok(config) => {
                    let name = config.agent.name.clone();
                    registry.upsert(config);
                    info!(agent = %name, path = %path.display(), "Loaded agent config");
                    count += 1;
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "Failed to load agent config");
                }
            }
        }

        Ok(count)
    }

    /// Load a single TOML file as an `AgentConfig`.
    pub fn load_file(path: &Path) -> Result<AgentConfig, AgentConfigError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| AgentConfigError::IoError(format!("{}: {}", path.display(), e)))?;

        let config = AgentConfig::from_toml(&content)
            .map_err(|e| AgentConfigError::ParseError {
                path: path.to_path_buf(),
                detail: e.to_string(),
            })?;

        validate(&config)?;
        Ok(config)
    }

    /// Scaffold a new agent TOML file from a template.
    pub fn scaffold(name: &str, dir: &Path) -> Result<std::path::PathBuf, AgentConfigError> {
        let path = dir.join(format!("{}.toml", name));
        if path.exists() {
            return Err(AgentConfigError::AlreadyExists(path));
        }

        let template = format!(
            r#"[agent]
name = "{name}"
description = "TODO: Describe what this agent does"
version = "1.0.0"
tags = []

[model]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
fallback = ["openai:gpt-4o"]
temperature = 0.7
max_tokens = 4096

[system_prompt]
content = """
You are a helpful assistant specialized in TODO.
"""

[capabilities]
tools = ["read_file", "web_search"]
network = ["*"]
memory_write = ["self.*"]

[resources]
max_tokens_per_hour = 500000
max_tool_iterations = 25
timeout_seconds = 300
"#
        );

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentConfigError::IoError(e.to_string()))?;
        }

        std::fs::write(&path, template)
            .map_err(|e| AgentConfigError::IoError(e.to_string()))?;

        Ok(path)
    }
}

fn validate(config: &AgentConfig) -> Result<(), AgentConfigError> {
    if config.agent.name.is_empty() {
        return Err(AgentConfigError::ValidationError(
            "agent.name must not be empty".into(),
        ));
    }

    if config.agent.name.contains(char::is_whitespace) {
        return Err(AgentConfigError::ValidationError(
            "agent.name must not contain whitespace (use kebab-case)".into(),
        ));
    }

    if config.model.model.is_empty() {
        return Err(AgentConfigError::ValidationError(
            "model.model must not be empty".into(),
        ));
    }

    if config.model.provider.is_empty() {
        return Err(AgentConfigError::ValidationError(
            "model.provider must not be empty".into(),
        ));
    }

    if config.model.temperature < 0.0 || config.model.temperature > 2.0 {
        return Err(AgentConfigError::ValidationError(
            "model.temperature must be between 0.0 and 2.0".into(),
        ));
    }

    if config.resources.max_tool_iterations == 0 {
        return Err(AgentConfigError::ValidationError(
            "resources.max_tool_iterations must be > 0".into(),
        ));
    }

    if config.resources.timeout_seconds == 0 {
        return Err(AgentConfigError::ValidationError(
            "resources.timeout_seconds must be > 0".into(),
        ));
    }

    // Validate trait counts (max 3 per category as per TraitLibrary constraint)
    if config.traits.persona.len() > 3 {
        return Err(AgentConfigError::ValidationError(
            "traits.persona allows max 3 entries".into(),
        ));
    }
    if config.traits.methodology.len() > 3 {
        return Err(AgentConfigError::ValidationError(
            "traits.methodology allows max 3 entries".into(),
        ));
    }

    Ok(())
}
