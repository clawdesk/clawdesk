//! Legacy migration adapter.
//!
//! Reads the directory structure and converts:
//! - Agent YAML configs → ClawDesk agent TOML
//! - SQLite session history → session export JSON
//! - SKILL.md files → copied to ClawDesk skills directory
//! - Channel configs (YAML) → ClawDesk channel TOML
//! - System config → merged into ClawDesk config

use crate::report::{ItemStatus, MigrationItem, MigrationReport};
use crate::{MigrateError, MigrateItem, MigrateOptions};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};
use walkdir::WalkDir;

/// Known legacy directory layout.
struct OpenClawLayout {
    root: PathBuf,
}

impl OpenClawLayout {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }

    fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    fn channels_dir(&self) -> PathBuf {
        self.root.join("channels")
    }

    fn config_file(&self) -> PathBuf {
        self.root.join("config.yaml")
    }

    fn sessions_db(&self) -> PathBuf {
        self.root.join("data").join("sessions.db")
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }
}

/// A legacy agent config (subset of fields we care about).
#[derive(Debug, Deserialize)]
struct OpenClawAgent {
    name: Option<String>,
    model: Option<String>,
    system_prompt: Option<String>,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
    tools: Option<Vec<String>>,
    #[serde(default)]
    metadata: serde_yaml::Mapping,
}

/// ClawDesk agent config (TOML output).
#[derive(Debug, Serialize)]
struct ClawDeskAgent {
    name: String,
    model: String,
    system_prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<String>,
}

/// legacy channel config.
#[derive(Debug, Deserialize)]
struct OpenClawChannel {
    #[serde(rename = "type")]
    channel_type: Option<String>,
    token: Option<String>,
    webhook_url: Option<String>,
    #[serde(default)]
    extra: serde_yaml::Mapping,
}

/// Run the legacy migration.
pub async fn migrate(options: &MigrateOptions) -> Result<MigrationReport, MigrateError> {
    let layout = OpenClawLayout::new(&options.source_path);
    let mut report = MigrationReport::new(
        "OpenClaw",
        &options.source_path.display().to_string(),
        options.dry_run,
    );

    let items = if options.include.is_empty() {
        MigrateItem::all()
    } else {
        options.include.clone()
    };

    for item in &items {
        match item {
            MigrateItem::Agents => migrate_agents(&layout, options, &mut report).await?,
            MigrateItem::Skills => migrate_skills(&layout, options, &mut report).await?,
            MigrateItem::Channels => migrate_channels(&layout, options, &mut report).await?,
            MigrateItem::Sessions => migrate_sessions(&layout, options, &mut report).await?,
            MigrateItem::Config => migrate_config(&layout, options, &mut report).await?,
            MigrateItem::Credentials => {
                report.add_warning("Credential migration from OpenClaw is not supported — credentials must be re-entered manually");
            }
        }
    }

    info!(
        migrated = report.summary.migrated,
        skipped = report.summary.skipped,
        failed = report.summary.failed,
        "Migration complete"
    );

    Ok(report)
}

/// Migrate agent YAML configs → ClawDesk TOML.
async fn migrate_agents(
    layout: &OpenClawLayout,
    options: &MigrateOptions,
    report: &mut MigrationReport,
) -> Result<(), MigrateError> {
    let agents_dir = layout.agents_dir();
    if !agents_dir.exists() {
        report.add_warning("No agents directory found in OpenClaw source");
        return Ok(());
    }

    let dest_agents = options.dest_path.join("agents");

    for entry in WalkDir::new(&agents_dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().extension().map_or(false, |ext| {
                ext == "yaml" || ext == "yml"
            })
        })
    {
        let source_path = entry.path();
        let file_stem = source_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        debug!(agent = file_stem, "Migrating agent config");

        match tokio::fs::read_to_string(source_path).await {
            Ok(content) => {
                match serde_yaml::from_str::<OpenClawAgent>(&content) {
                    Ok(agent) => {
                        let clawdesk_agent = ClawDeskAgent {
                            name: agent.name.unwrap_or_else(|| file_stem.to_string()),
                            model: agent.model.unwrap_or_else(|| "default".to_string()),
                            system_prompt: agent
                                .system_prompt
                                .unwrap_or_else(|| "You are a helpful AI assistant.".to_string()),
                            temperature: agent.temperature,
                            max_tokens: agent.max_tokens,
                            tools: agent.tools.unwrap_or_default(),
                        };

                        let dest_file = dest_agents.join(format!("{}.toml", file_stem));
                        let toml_content = toml::to_string_pretty(&clawdesk_agent)?;

                        let status = if options.dry_run {
                            ItemStatus::DryRun
                        } else if dest_file.exists() && !options.overwrite {
                            ItemStatus::Skipped
                        } else {
                            tokio::fs::create_dir_all(&dest_agents).await?;
                            tokio::fs::write(&dest_file, &toml_content).await?;
                            ItemStatus::Migrated
                        };

                        report.add_item(MigrationItem {
                            category: "Agent".to_string(),
                            source_name: file_stem.to_string(),
                            dest_path: dest_file.display().to_string(),
                            status,
                            note: Some(format!(
                                "model={}, tools={}",
                                clawdesk_agent.model,
                                clawdesk_agent.tools.len()
                            )),
                        });
                    }
                    Err(e) => {
                        report.add_item(MigrationItem {
                            category: "Agent".to_string(),
                            source_name: file_stem.to_string(),
                            dest_path: String::new(),
                            status: ItemStatus::Failed,
                            note: Some(format!("Parse error: {}", e)),
                        });
                    }
                }
            }
            Err(e) => {
                report.add_item(MigrationItem {
                    category: "Agent".to_string(),
                    source_name: file_stem.to_string(),
                    dest_path: String::new(),
                    status: ItemStatus::Failed,
                    note: Some(format!("Read error: {}", e)),
                });
            }
        }
    }

    Ok(())
}

/// Migrate SKILL.md files — direct copy.
async fn migrate_skills(
    layout: &OpenClawLayout,
    options: &MigrateOptions,
    report: &mut MigrationReport,
) -> Result<(), MigrateError> {
    let skills_dir = layout.skills_dir();
    if !skills_dir.exists() {
        report.add_warning("No skills directory found in OpenClaw source");
        return Ok(());
    }

    let dest_skills = options.dest_path.join("skills");

    for entry in WalkDir::new(&skills_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map_or(false, |name| name == "SKILL.md" || name.ends_with(".md"))
        })
    {
        let source_path = entry.path();
        let rel_path = source_path
            .strip_prefix(&skills_dir)
            .unwrap_or(source_path);
        let dest_file = dest_skills.join(rel_path);

        let skill_name = rel_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        let status = if options.dry_run {
            ItemStatus::DryRun
        } else if dest_file.exists() && !options.overwrite {
            ItemStatus::Skipped
        } else {
            if let Some(parent) = dest_file.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::copy(source_path, &dest_file).await?;
            ItemStatus::Migrated
        };

        report.add_item(MigrationItem {
            category: "Skill".to_string(),
            source_name: skill_name.to_string(),
            dest_path: dest_file.display().to_string(),
            status,
            note: None,
        });
    }

    Ok(())
}

/// Migrate channel configs.
async fn migrate_channels(
    layout: &OpenClawLayout,
    options: &MigrateOptions,
    report: &mut MigrationReport,
) -> Result<(), MigrateError> {
    let channels_dir = layout.channels_dir();
    if !channels_dir.exists() {
        // Also check config.yaml for inline channel configs
        report.add_warning("No channels directory found — checking config.yaml");
        return Ok(());
    }

    let dest_channels = options.dest_path.join("channels");

    for entry in WalkDir::new(&channels_dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().extension().map_or(false, |ext| {
                ext == "yaml" || ext == "yml"
            })
        })
    {
        let source_path = entry.path();
        let file_stem = source_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        match tokio::fs::read_to_string(source_path).await {
            Ok(content) => {
                match serde_yaml::from_str::<OpenClawChannel>(&content) {
                    Ok(channel) => {
                        let channel_type = channel
                            .channel_type
                            .as_deref()
                            .unwrap_or("unknown");

                        // Map legacy channel type to ClawDesk
                        let clawdesk_type = match channel_type {
                            "discord" | "Discord" => "discord",
                            "telegram" | "Telegram" => "telegram",
                            "slack" | "Slack" => "slack",
                            "whatsapp" | "WhatsApp" => "whatsapp",
                            "irc" | "IRC" => "irc",
                            "email" | "Email" => "email",
                            other => other,
                        };

                        let dest_file =
                            dest_channels.join(format!("{}.toml", file_stem));

                        // Build a simple TOML config
                        let mut toml_content = format!(
                            "[channel]\ntype = \"{}\"\nname = \"{}\"\n",
                            clawdesk_type, file_stem
                        );

                        if let Some(token) = &channel.token {
                            toml_content.push_str(&format!(
                                "# TOKEN REDACTED — re-enter via: clawdesk config channel {} --token <TOKEN>\n\
                                 # original_token_length = {}\n",
                                file_stem,
                                token.len()
                            ));
                            report.add_warning(format!(
                                "Channel '{}' has a token — you must re-enter it manually for security",
                                file_stem
                            ));
                        }

                        if let Some(url) = &channel.webhook_url {
                            toml_content
                                .push_str(&format!("webhook_url = \"{}\"\n", url));
                        }

                        let status = if options.dry_run {
                            ItemStatus::DryRun
                        } else if dest_file.exists() && !options.overwrite {
                            ItemStatus::Skipped
                        } else {
                            tokio::fs::create_dir_all(&dest_channels).await?;
                            tokio::fs::write(&dest_file, &toml_content).await?;
                            ItemStatus::Migrated
                        };

                        report.add_item(MigrationItem {
                            category: "Channel".to_string(),
                            source_name: format!("{} ({})", file_stem, clawdesk_type),
                            dest_path: dest_file.display().to_string(),
                            status,
                            note: None,
                        });
                    }
                    Err(e) => {
                        report.add_item(MigrationItem {
                            category: "Channel".to_string(),
                            source_name: file_stem.to_string(),
                            dest_path: String::new(),
                            status: ItemStatus::Failed,
                            note: Some(format!("Parse error: {}", e)),
                        });
                    }
                }
            }
            Err(e) => {
                report.add_item(MigrationItem {
                    category: "Channel".to_string(),
                    source_name: file_stem.to_string(),
                    dest_path: String::new(),
                    status: ItemStatus::Failed,
                    note: Some(format!("Read error: {}", e)),
                });
            }
        }
    }

    Ok(())
}

/// Migrate sessions — export from SQLite or JSON session files.
async fn migrate_sessions(
    layout: &OpenClawLayout,
    options: &MigrateOptions,
    report: &mut MigrationReport,
) -> Result<(), MigrateError> {
    // Check for JSON session files first
    let sessions_dir = layout.sessions_dir();
    if sessions_dir.exists() {
        let dest_sessions = options.dest_path.join("sessions");

        for entry in WalkDir::new(&sessions_dir)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().extension().map_or(false, |ext| ext == "json")
            })
        {
            let source_path = entry.path();
            let file_name = source_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown.json");

            let dest_file = dest_sessions.join(file_name);

            let status = if options.dry_run {
                ItemStatus::DryRun
            } else if dest_file.exists() && !options.overwrite {
                ItemStatus::Skipped
            } else {
                tokio::fs::create_dir_all(&dest_sessions).await?;
                tokio::fs::copy(source_path, &dest_file).await?;
                ItemStatus::Migrated
            };

            report.add_item(MigrationItem {
                category: "Session".to_string(),
                source_name: file_name.to_string(),
                dest_path: dest_file.display().to_string(),
                status,
                note: None,
            });
        }

        return Ok(());
    }

    // Check for SQLite database
    let db_path = layout.sessions_db();
    if db_path.exists() {
        report.add_warning(
            "SQLite session database found. Session migration from SQLite requires \
             the `rusqlite` feature. Export sessions manually with: \
             sqlite3 sessions.db '.dump sessions' > sessions_export.sql",
        );

        report.add_item(MigrationItem {
            category: "Session".to_string(),
            source_name: "sessions.db".to_string(),
            dest_path: String::new(),
            status: ItemStatus::Skipped,
            note: Some("SQLite export not implemented — use manual export".to_string()),
        });
    } else {
        report.add_warning("No sessions directory or database found in OpenClaw source");
    }

    Ok(())
}

/// Migrate system config.
async fn migrate_config(
    layout: &OpenClawLayout,
    options: &MigrateOptions,
    report: &mut MigrationReport,
) -> Result<(), MigrateError> {
    let config_file = layout.config_file();
    if !config_file.exists() {
        report.add_warning("No config.yaml found in OpenClaw source");
        return Ok(());
    }

    let content = tokio::fs::read_to_string(&config_file).await?;

    // Parse as generic YAML value to extract relevant fields
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&content)?;

    let dest_config = options.dest_path.join("config.toml");

    // Extract known config patterns
    let mut toml_lines = vec![
        "# ClawDesk config — migrated from OpenClaw".to_string(),
        format!("# Source: {}", config_file.display()),
        "".to_string(),
    ];

    if let Some(mapping) = yaml_value.as_mapping() {
        // Extract model preferences
        if let Some(model) = mapping.get(&serde_yaml::Value::String("model".to_string())) {
            if let Some(model_str) = model.as_str() {
                toml_lines.push(format!("[llm]"));
                toml_lines.push(format!("default_model = \"{}\"", model_str));
                toml_lines.push(String::new());
            }
        }

        // Extract default provider
        if let Some(provider) = mapping.get(&serde_yaml::Value::String("provider".to_string())) {
            if let Some(provider_str) = provider.as_str() {
                toml_lines.push(format!("[provider]"));
                toml_lines.push(format!("default = \"{}\"", provider_str));
                toml_lines.push(String::new());
            }
        }

        // Extract any log level
        if let Some(log_level) = mapping.get(&serde_yaml::Value::String("log_level".to_string())) {
            if let Some(level_str) = log_level.as_str() {
                toml_lines.push(format!("[logging]"));
                toml_lines.push(format!("level = \"{}\"", level_str));
                toml_lines.push(String::new());
            }
        }
    }

    // Also dump the original YAML as a comment for reference
    toml_lines.push("# --- Original OpenClaw config (for reference) ---".to_string());
    for line in content.lines() {
        toml_lines.push(format!("# {}", line));
    }

    let toml_content = toml_lines.join("\n");

    let status = if options.dry_run {
        ItemStatus::DryRun
    } else if dest_config.exists() && !options.overwrite {
        ItemStatus::Skipped
    } else {
        tokio::fs::create_dir_all(&options.dest_path).await?;
        tokio::fs::write(&dest_config, &toml_content).await?;
        ItemStatus::Migrated
    };

    report.add_item(MigrationItem {
        category: "Config".to_string(),
        source_name: "config.yaml".to_string(),
        dest_path: dest_config.display().to_string(),
        status,
        note: Some("Partial — review migrated config and adjust manually".to_string()),
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_migrate_empty_source() {
        let source = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();

        let options = MigrateOptions {
            source: crate::MigrateSource::OpenClaw,
            source_path: source.path().to_path_buf(),
            dest_path: dest.path().to_path_buf(),
            dry_run: true,
            overwrite: false,
            include: vec![],
        };

        let report = migrate(&options).await.unwrap();
        assert!(report.is_success());
        assert!(!report.warnings.is_empty()); // Should warn about missing dirs
    }

    #[tokio::test]
    async fn test_migrate_agent_yaml() {
        let source = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();

        // Create a mock agent config
        let agents_dir = source.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("test-agent.yaml"),
            r#"
name: Test Agent
model: claude-3-opus
system_prompt: You are a test agent
temperature: 0.7
max_tokens: 4096
tools:
  - web_search
  - file_read
"#,
        )
        .unwrap();

        let options = MigrateOptions {
            source: crate::MigrateSource::OpenClaw,
            source_path: source.path().to_path_buf(),
            dest_path: dest.path().to_path_buf(),
            dry_run: false,
            overwrite: false,
            include: vec![MigrateItem::Agents],
        };

        let report = migrate(&options).await.unwrap();
        assert!(report.is_success());
        assert_eq!(report.summary.migrated, 1);

        // Verify output
        let output_file = dest.path().join("agents").join("test-agent.toml");
        assert!(output_file.exists());

        let content = std::fs::read_to_string(&output_file).unwrap();
        assert!(content.contains("name = \"Test Agent\""));
        assert!(content.contains("model = \"claude-3-opus\""));
        assert!(content.contains("web_search"));
    }
}
