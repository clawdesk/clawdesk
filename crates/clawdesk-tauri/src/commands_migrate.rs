//! Migration commands — import data, Claude Desktop, etc.
//!
//! Wraps clawdesk-migrate to let users import agents, sessions, skills,
//! channels, and config from other AI desktop apps via the Tauri frontend.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

// ── Response types ────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct MigrationSourceInfo {
    pub name: String,
    pub label: String,
    pub supported_items: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct MigrationReportInfo {
    pub source: String,
    pub source_path: String,
    pub dry_run: bool,
    pub success: bool,
    pub summary: MigrationSummaryInfo,
    pub items: Vec<MigrationItemInfo>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct MigrationSummaryInfo {
    pub total: usize,
    pub migrated: usize,
    pub skipped: usize,
    pub failed: usize,
    pub dry_run: usize,
}

#[derive(Debug, Serialize)]
pub struct MigrationItemInfo {
    pub category: String,
    pub source_name: String,
    pub dest_path: String,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct MigrationRequest {
    pub source: String,
    pub source_path: String,
    pub dest_path: Option<String>,
    pub dry_run: bool,
    pub overwrite: bool,
    pub include: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ValidateSourceResult {
    pub valid: bool,
    pub source: String,
    pub found_items: Vec<String>,
    pub error: Option<String>,
}

// ── Commands ──────────────────────────────────────────────────

/// List supported migration sources.
#[tauri::command]
pub async fn list_migration_sources() -> Result<Vec<MigrationSourceInfo>, String> {
    let items: Vec<String> = clawdesk_migrate::MigrateItem::all()
        .iter()
        .map(|i| format!("{:?}", i))
        .collect();

    Ok(vec![
        MigrationSourceInfo {
            name: "openclaw".into(),
            label: "OpenClaw".into(),
            supported_items: items.clone(),
        },
        MigrationSourceInfo {
            name: "claude_desktop".into(),
            label: "Claude Desktop".into(),
            supported_items: items,
        },
    ])
}

/// Validate a migration source directory (check it exists and has expected structure).
#[tauri::command]
pub async fn validate_migration_source(
    source: String,
    source_path: String,
) -> Result<ValidateSourceResult, String> {
    let migrate_source = match source.as_str() {
        "openclaw" => clawdesk_migrate::MigrateSource::OpenClaw,
        "claude_desktop" => clawdesk_migrate::MigrateSource::ClaudeDesktop,
        other => return Err(format!("Unknown migration source: {}", other)),
    };

    let path = std::path::Path::new(&source_path);
    if !path.exists() {
        return Ok(ValidateSourceResult {
            valid: false,
            source: source.clone(),
            found_items: vec![],
            error: Some(format!("Path does not exist: {}", source_path)),
        });
    }

    // Check for expected subdirectories/files based on source type
    let mut found_items = Vec::new();
    match migrate_source {
        clawdesk_migrate::MigrateSource::OpenClaw => {
            if path.join("agents").exists() { found_items.push("Agents".into()); }
            if path.join("skills").exists() { found_items.push("Skills".into()); }
            if path.join("channels").exists() { found_items.push("Channels".into()); }
            if path.join("sessions").exists() { found_items.push("Sessions".into()); }
            if path.join("config.yaml").exists() || path.join("config.toml").exists() {
                found_items.push("Config".into());
            }
        }
        clawdesk_migrate::MigrateSource::ClaudeDesktop => {
            if path.join("claude_desktop_config.json").exists() {
                found_items.push("Config".into());
            }
        }
    }

    Ok(ValidateSourceResult {
        valid: !found_items.is_empty(),
        source,
        found_items,
        error: None,
    })
}

/// Run a migration from a source to ClawDesk's data directory.
#[tauri::command]
pub async fn run_migration(
    request: MigrationRequest,
    state: State<'_, AppState>,
) -> Result<MigrationReportInfo, String> {
    let migrate_source = match request.source.as_str() {
        "openclaw" => clawdesk_migrate::MigrateSource::OpenClaw,
        "claude_desktop" => clawdesk_migrate::MigrateSource::ClaudeDesktop,
        other => return Err(format!("Unknown migration source: {}", other)),
    };

    let dest_path = request.dest_path.unwrap_or_else(|| {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());
        format!("{}/.clawdesk", home)
    });

    let include = request.include.map(|items| {
        items
            .iter()
            .filter_map(|i| match i.to_lowercase().as_str() {
                "agents" => Some(clawdesk_migrate::MigrateItem::Agents),
                "sessions" => Some(clawdesk_migrate::MigrateItem::Sessions),
                "skills" => Some(clawdesk_migrate::MigrateItem::Skills),
                "channels" => Some(clawdesk_migrate::MigrateItem::Channels),
                "credentials" => Some(clawdesk_migrate::MigrateItem::Credentials),
                "config" => Some(clawdesk_migrate::MigrateItem::Config),
                _ => None,
            })
            .collect()
    });

    let options = clawdesk_migrate::MigrateOptions {
        source: migrate_source,
        source_path: std::path::PathBuf::from(&request.source_path),
        dest_path: std::path::PathBuf::from(&dest_path),
        dry_run: request.dry_run,
        overwrite: request.overwrite,
        include: include.unwrap_or_default(),
    };

    let report = clawdesk_migrate::run_migration(&options)
        .await
        .map_err(|e| format!("{:?}", e))?;

    Ok(MigrationReportInfo {
        source: report.source.clone(),
        source_path: report.source_path.clone(),
        dry_run: report.dry_run,
        success: report.is_success(),
        summary: MigrationSummaryInfo {
            total: report.summary.total as usize,
            migrated: report.summary.migrated as usize,
            skipped: report.summary.skipped as usize,
            failed: report.summary.failed as usize,
            dry_run: report.summary.dry_run as usize,
        },
        items: report
            .items
            .iter()
            .map(|item| MigrationItemInfo {
                category: format!("{:?}", item.category),
                source_name: item.source_name.clone(),
                dest_path: item.dest_path.clone(),
                status: format!("{:?}", item.status),
                note: item.note.clone().unwrap_or_default(),
            })
            .collect(),
        warnings: report.warnings.clone(),
        errors: report.errors.clone(),
    })
}

/// Run a dry-run migration (preview what would be imported without writing anything).
#[tauri::command]
pub async fn preview_migration(
    source: String,
    source_path: String,
    state: State<'_, AppState>,
) -> Result<MigrationReportInfo, String> {
    run_migration(
        MigrationRequest {
            source,
            source_path,
            dest_path: None,
            dry_run: true,
            overwrite: false,
            include: None,
        },
        state,
    )
    .await
}
