//! Schema migration — forward-only versioned migrations for SochDB.
//!
//! Each migration M_i is O(|data| × complexity(M_i)). Forward-only guarantees:
//! once applied, never rolled back — avoids NP-hard rollback sequences.

use serde::{Deserialize, Serialize};
use tracing::info;

/// A single migration step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Migration {
    pub version: u32,
    pub name: String,
    pub description: String,
    /// SQL-like or structured migration command.
    pub up: String,
}

/// Current schema version tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaVersion {
    pub current: u32,
    pub applied: Vec<AppliedMigration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedMigration {
    pub version: u32,
    pub name: String,
    pub applied_at: String,
    pub duration_ms: u64,
}

impl Default for SchemaVersion {
    fn default() -> Self {
        Self { current: 0, applied: Vec::new() }
    }
}

/// Migration runner — applies pending migrations in order.
pub struct MigrationRunner {
    migrations: Vec<Migration>,
}

impl MigrationRunner {
    pub fn new() -> Self {
        Self { migrations: Vec::new() }
    }

    /// Register a migration.
    pub fn add(&mut self, migration: Migration) {
        self.migrations.push(migration);
        self.migrations.sort_by_key(|m| m.version);
    }

    /// Get migrations pending from current version.
    pub fn pending(&self, current_version: u32) -> Vec<&Migration> {
        self.migrations.iter()
            .filter(|m| m.version > current_version)
            .collect()
    }

    /// Simulate applying migrations (dry run).
    pub fn plan(&self, current_version: u32) -> Vec<String> {
        self.pending(current_version)
            .iter()
            .map(|m| format!("v{}: {} — {}", m.version, m.name, m.description))
            .collect()
    }

    /// Apply all pending migrations.
    pub fn apply(&self, schema: &mut SchemaVersion) -> Result<Vec<AppliedMigration>, String> {
        let pending = self.pending(schema.current);
        let mut applied = Vec::new();

        for migration in pending {
            info!(version = migration.version, name = %migration.name, "applying migration");
            let start = std::time::Instant::now();

            // In a real implementation, this would execute against SochDB.
            // For now, we just record the application.
            let record = AppliedMigration {
                version: migration.version,
                name: migration.name.clone(),
                applied_at: chrono::Utc::now().to_rfc3339(),
                duration_ms: start.elapsed().as_millis() as u64,
            };

            schema.current = migration.version;
            schema.applied.push(record.clone());
            applied.push(record);
        }

        Ok(applied)
    }

    /// Number of registered migrations.
    pub fn count(&self) -> usize {
        self.migrations.len()
    }
}

impl Default for MigrationRunner {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_from_zero() {
        let mut runner = MigrationRunner::new();
        runner.add(Migration { version: 1, name: "init".into(), description: "initial".into(), up: "".into() });
        runner.add(Migration { version: 2, name: "add_field".into(), description: "add field".into(), up: "".into() });
        assert_eq!(runner.pending(0).len(), 2);
        assert_eq!(runner.pending(1).len(), 1);
        assert_eq!(runner.pending(2).len(), 0);
    }

    #[test]
    fn apply_updates_version() {
        let mut runner = MigrationRunner::new();
        runner.add(Migration { version: 1, name: "v1".into(), description: "".into(), up: "".into() });
        runner.add(Migration { version: 2, name: "v2".into(), description: "".into(), up: "".into() });

        let mut schema = SchemaVersion::default();
        let applied = runner.apply(&mut schema).unwrap();
        assert_eq!(applied.len(), 2);
        assert_eq!(schema.current, 2);
    }

    #[test]
    fn plan_shows_pending() {
        let mut runner = MigrationRunner::new();
        runner.add(Migration { version: 1, name: "init".into(), description: "setup tables".into(), up: "".into() });
        let plan = runner.plan(0);
        assert_eq!(plan.len(), 1);
        assert!(plan[0].contains("init"));
    }
}
