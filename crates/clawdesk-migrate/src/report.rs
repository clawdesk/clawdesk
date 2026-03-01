//! Migration report — tracks what was migrated, skipped, and failed.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Overall migration report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    pub source: String,
    pub source_path: String,
    pub dry_run: bool,
    pub items: Vec<MigrationItem>,
    pub summary: MigrationSummary,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

impl MigrationReport {
    pub fn new(source: &str, source_path: &str, dry_run: bool) -> Self {
        Self {
            source: source.to_string(),
            source_path: source_path.to_string(),
            dry_run,
            items: Vec::new(),
            summary: MigrationSummary::default(),
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn add_item(&mut self, item: MigrationItem) {
        match item.status {
            ItemStatus::Migrated => self.summary.migrated += 1,
            ItemStatus::Skipped => self.summary.skipped += 1,
            ItemStatus::Failed => self.summary.failed += 1,
            ItemStatus::DryRun => self.summary.dry_run += 1,
        }
        self.summary.total += 1;
        self.items.push(item);
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    pub fn add_error(&mut self, error: impl Into<String>) {
        self.errors.push(error.into());
    }

    pub fn is_success(&self) -> bool {
        self.summary.failed == 0 && self.errors.is_empty()
    }
}

impl fmt::Display for MigrationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Migration Report")?;
        writeln!(f, "================")?;
        writeln!(f, "Source:     {} ({})", self.source, self.source_path)?;
        writeln!(f, "Dry Run:    {}", self.dry_run)?;
        writeln!(f)?;
        writeln!(f, "Summary:")?;
        writeln!(f, "  Total:    {}", self.summary.total)?;
        writeln!(f, "  Migrated: {}", self.summary.migrated)?;
        writeln!(f, "  Skipped:  {}", self.summary.skipped)?;
        writeln!(f, "  Failed:   {}", self.summary.failed)?;
        if self.summary.dry_run > 0 {
            writeln!(f, "  Dry-run:  {}", self.summary.dry_run)?;
        }
        writeln!(f)?;

        if !self.items.is_empty() {
            writeln!(f, "Items:")?;
            for item in &self.items {
                writeln!(
                    f,
                    "  [{}] {} — {} → {}",
                    item.status.icon(),
                    item.category,
                    item.source_name,
                    item.dest_path
                )?;
                if let Some(note) = &item.note {
                    writeln!(f, "       {}", note)?;
                }
            }
            writeln!(f)?;
        }

        if !self.warnings.is_empty() {
            writeln!(f, "Warnings:")?;
            for w in &self.warnings {
                writeln!(f, "  ⚠ {}", w)?;
            }
            writeln!(f)?;
        }

        if !self.errors.is_empty() {
            writeln!(f, "Errors:")?;
            for e in &self.errors {
                writeln!(f, "  ✖ {}", e)?;
            }
        }

        Ok(())
    }
}

/// A single migrated item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationItem {
    pub category: String,
    pub source_name: String,
    pub dest_path: String,
    pub status: ItemStatus,
    pub note: Option<String>,
}

/// Status of a migration item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemStatus {
    Migrated,
    Skipped,
    Failed,
    DryRun,
}

impl ItemStatus {
    pub fn icon(&self) -> &'static str {
        match self {
            ItemStatus::Migrated => "✓",
            ItemStatus::Skipped => "→",
            ItemStatus::Failed => "✖",
            ItemStatus::DryRun => "~",
        }
    }
}

/// Summary counters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MigrationSummary {
    pub total: u32,
    pub migrated: u32,
    pub skipped: u32,
    pub failed: u32,
    pub dry_run: u32,
}
