//! # clawdesk-migrate
//!
//! Platform migration engine for ClawDesk.
//!
//! Supports importing configuration, sessions, skills, and channel configs
//! from competing platforms:
//!
//! - **Legacy**: Agent YAML configs, SQLite sessions, SKILL.md files, channel mappings
//! - **Claude Desktop**: config.json, conversation history
//!
//! ## Usage
//! ```text
//! clawdesk migrate --from openclaw --source /path/to/openclaw [--dry-run]
//! ```

pub mod openclaw;
pub mod report;

use std::path::{Path, PathBuf};
use thiserror::Error;

/// Migration source platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateSource {
    /// legacy platform
    OpenClaw,
    /// Claude Desktop app
    ClaudeDesktop,
}

impl MigrateSource {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "openclaw" | "open-claw" => Some(MigrateSource::OpenClaw),
            "claude" | "claude-desktop" | "claudedesktop" => Some(MigrateSource::ClaudeDesktop),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            MigrateSource::OpenClaw => "OpenClaw",
            MigrateSource::ClaudeDesktop => "Claude Desktop",
        }
    }
}

/// Migration options.
#[derive(Debug, Clone)]
pub struct MigrateOptions {
    /// Source platform to migrate from
    pub source: MigrateSource,
    /// Path to the source platform's data directory
    pub source_path: PathBuf,
    /// Destination path for ClawDesk config (defaults to ~/.clawdesk)
    pub dest_path: PathBuf,
    /// If true, only report what would be migrated without writing
    pub dry_run: bool,
    /// Overwrite existing files
    pub overwrite: bool,
    /// Specific items to migrate (empty = all)
    pub include: Vec<MigrateItem>,
}

/// Types of items that can be migrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateItem {
    /// Agent configurations
    Agents,
    /// Conversation sessions / history
    Sessions,
    /// Skills / tool definitions
    Skills,
    /// Channel configurations
    Channels,
    /// Credentials (if exportable)
    Credentials,
    /// System configuration
    Config,
}

impl MigrateItem {
    pub fn all() -> Vec<MigrateItem> {
        vec![
            MigrateItem::Agents,
            MigrateItem::Sessions,
            MigrateItem::Skills,
            MigrateItem::Channels,
            MigrateItem::Credentials,
            MigrateItem::Config,
        ]
    }
}

/// Migration error.
#[derive(Debug, Error)]
pub enum MigrateError {
    #[error("Source directory not found: {0}")]
    SourceNotFound(PathBuf),

    #[error("Unsupported source platform: {0}")]
    UnsupportedSource(String),

    #[error("Parse error in {file}: {message}")]
    ParseError { file: String, message: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yml::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML serialize error: {0}")]
    Toml(#[from] toml::ser::Error),

    #[error("Migration failed: {0}")]
    Failed(String),
}

/// Run a migration with the given options.
pub async fn run_migration(options: &MigrateOptions) -> Result<report::MigrationReport, MigrateError> {
    // Validate source exists
    if !options.source_path.exists() {
        return Err(MigrateError::SourceNotFound(options.source_path.clone()));
    }

    tracing::info!(
        source = options.source.name(),
        path = %options.source_path.display(),
        dry_run = options.dry_run,
        "Starting migration"
    );

    match options.source {
        MigrateSource::OpenClaw => openclaw::migrate(options).await,
        MigrateSource::ClaudeDesktop => {
            Err(MigrateError::UnsupportedSource(
                "Claude Desktop migration not yet implemented".to_string(),
            ))
        }
    }
}
