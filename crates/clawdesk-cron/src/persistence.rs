//! GAP-C: Durable cron persistence trait.
//!
//! Defines a storage contract for cron tasks and run logs.
//! This keeps clawdesk-cron independent of any specific storage backend.
//! The concrete SochDB implementation lives in clawdesk-tauri.

use async_trait::async_trait;
use clawdesk_types::cron::{CronRunLog, CronTask};

/// Persistence backend for cron tasks and execution logs.
///
/// Implementations must be thread-safe. All methods are async to support
/// both in-memory (tests) and durable (SochDB) backends.
#[async_trait]
pub trait CronPersistence: Send + Sync + 'static {
    /// Save or update a task definition.
    async fn save_task(&self, task: &CronTask) -> Result<(), String>;

    /// Load all persisted task definitions.
    async fn load_tasks(&self) -> Result<Vec<CronTask>, String>;

    /// Delete a task definition by ID.
    async fn delete_task(&self, id: &str) -> Result<(), String>;

    /// Append an execution log entry.
    async fn save_log(&self, log: &CronRunLog) -> Result<(), String>;

    /// Load recent logs for a specific task (newest first).
    async fn load_logs(&self, task_id: &str, limit: usize) -> Result<Vec<CronRunLog>, String>;

    /// Get the most recent successful run for a task.
    /// Used for dependency chain resolution — a dependent task checks if
    /// its predecessor has succeeded since the dependent's own last run.
    async fn last_run_with_status(
        &self,
        task_id: &str,
        status: clawdesk_types::cron::CronRunStatus,
    ) -> Result<Option<CronRunLog>, String>;
}

/// In-memory persistence for testing.
#[derive(Default)]
pub struct InMemoryCronPersistence {
    tasks: tokio::sync::RwLock<Vec<CronTask>>,
    logs: tokio::sync::RwLock<Vec<CronRunLog>>,
}

impl InMemoryCronPersistence {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CronPersistence for InMemoryCronPersistence {
    async fn save_task(&self, task: &CronTask) -> Result<(), String> {
        let mut tasks = self.tasks.write().await;
        if let Some(existing) = tasks.iter_mut().find(|t| t.id == task.id) {
            *existing = task.clone();
        } else {
            tasks.push(task.clone());
        }
        Ok(())
    }

    async fn load_tasks(&self) -> Result<Vec<CronTask>, String> {
        Ok(self.tasks.read().await.clone())
    }

    async fn delete_task(&self, id: &str) -> Result<(), String> {
        self.tasks.write().await.retain(|t| t.id != id);
        Ok(())
    }

    async fn save_log(&self, log: &CronRunLog) -> Result<(), String> {
        self.logs.write().await.push(log.clone());
        Ok(())
    }

    async fn load_logs(&self, task_id: &str, limit: usize) -> Result<Vec<CronRunLog>, String> {
        let logs = self.logs.read().await;
        Ok(logs
            .iter()
            .rev()
            .filter(|l| l.task_id == task_id)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn last_run_with_status(
        &self,
        task_id: &str,
        status: clawdesk_types::cron::CronRunStatus,
    ) -> Result<Option<CronRunLog>, String> {
        let logs = self.logs.read().await;
        Ok(logs
            .iter()
            .rev()
            .find(|l| l.task_id == task_id && l.status == status)
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use clawdesk_types::cron::CronRunStatus;

    fn make_log(task_id: &str, status: CronRunStatus) -> CronRunLog {
        CronRunLog {
            task_id: task_id.to_string(),
            run_id: uuid::Uuid::new_v4().to_string(),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            status,
            result_preview: Some("test output".to_string()),
            error: None,
            tokens_used: None,
        }
    }

    #[tokio::test]
    async fn test_in_memory_persistence() {
        let store = InMemoryCronPersistence::new();
        let task = CronTask {
            id: "t1".to_string(),
            name: "Test".to_string(),
            schedule: "* * * * *".to_string(),
            prompt: "hello".to_string(),
            agent_id: None,
            delivery_targets: vec![],
            skip_if_running: false,
            timeout_secs: 60,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            depends_on: vec![],
            chain_mode: Default::default(),
            max_retained_logs: 0,
        };

        store.save_task(&task).await.unwrap();
        assert_eq!(store.load_tasks().await.unwrap().len(), 1);

        store.delete_task("t1").await.unwrap();
        assert!(store.load_tasks().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_log_persistence_and_status_query() {
        let store = InMemoryCronPersistence::new();

        store.save_log(&make_log("t1", CronRunStatus::Failed)).await.unwrap();
        store.save_log(&make_log("t1", CronRunStatus::Succeeded)).await.unwrap();
        store.save_log(&make_log("t1", CronRunStatus::Failed)).await.unwrap();

        let logs = store.load_logs("t1", 10).await.unwrap();
        assert_eq!(logs.len(), 3);

        let last_success = store
            .last_run_with_status("t1", CronRunStatus::Succeeded)
            .await
            .unwrap();
        assert!(last_success.is_some());
        assert_eq!(last_success.unwrap().status, CronRunStatus::Succeeded);

        let last_cancelled = store
            .last_run_with_status("t1", CronRunStatus::Cancelled)
            .await
            .unwrap();
        assert!(last_cancelled.is_none());
    }
}
