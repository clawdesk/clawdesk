//! Cron manager — task CRUD and scheduling loop.

use crate::executor::{AgentExecutor, CronExecutor, DeliveryHandler};
use crate::parser;
use chrono::Utc;
use clawdesk_types::cron::{CronRunLog, CronTask};
use clawdesk_types::error::CronError;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Manages scheduled tasks: CRUD operations and the scheduling loop.
///
/// Tasks are stored as `Arc<CronTask>` to avoid deep cloning in the tick loop.
/// The two-phase tick evaluates cron expressions under a read lock, then only
/// clones `Arc` pointers (not task data) for spawned execution futures.
pub struct CronManager {
    tasks: RwLock<HashMap<String, Arc<CronTask>>>,
    executor: Arc<CronExecutor>,
    cancel: CancellationToken,
}

impl CronManager {
    pub fn new(
        agent: Arc<dyn AgentExecutor>,
        delivery: Arc<dyn DeliveryHandler>,
    ) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            executor: Arc::new(CronExecutor::new(agent, delivery)),
            cancel: CancellationToken::new(),
        }
    }

    /// Add or update a task.
    pub async fn upsert_task(&self, task: CronTask) -> Result<(), CronError> {
        // Validate the cron expression.
        parser::parse_cron_expression(&task.schedule)?;

        let id = task.id.clone();
        let mut tasks = self.tasks.write().await;
        tasks.insert(id.clone(), Arc::new(task));
        info!(task_id = %id, "Task upserted");
        Ok(())
    }

    /// Remove a task.
    pub async fn remove_task(&self, id: &str) -> Result<CronTask, CronError> {
        let mut tasks = self.tasks.write().await;
        let arc_task = tasks.remove(id).ok_or_else(|| CronError::InvalidExpression {
            expr: format!("Task '{}' not found", id),
        })?;
        // Unwrap Arc or clone the inner value.
        Ok(Arc::try_unwrap(arc_task).unwrap_or_else(|a| (*a).clone()))
    }

    /// Get a task by ID.
    pub async fn get_task(&self, id: &str) -> Option<CronTask> {
        self.tasks.read().await.get(id).map(|t| (**t).clone())
    }

    /// List all tasks.
    pub async fn list_tasks(&self) -> Vec<CronTask> {
        self.tasks.read().await.values().map(|t| (**t).clone()).collect()
    }

    /// Enable/disable a task.
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), CronError> {
        let mut tasks = self.tasks.write().await;
        let arc_task = tasks.get_mut(id).ok_or_else(|| CronError::InvalidExpression {
            expr: format!("Task '{}' not found", id),
        })?;
        // Must replace the Arc since CronTask fields aren't mutable through Arc.
        let mut task = (**arc_task).clone();
        task.enabled = enabled;
        task.updated_at = Utc::now();
        *arc_task = Arc::new(task);
        Ok(())
    }

    /// Run the scheduling loop. Call this from a spawned task.
    pub async fn run(&self) {
        info!("Cron scheduler started");

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    info!("Cron scheduler stopping");
                    break;
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(60)) => {
                    self.tick().await;
                }
            }
        }
    }

    /// Process one scheduling tick — two-phase evaluation.
    ///
    /// Phase 1: under read lock, evaluate cron expressions and collect Arc
    /// pointers for matched tasks (no deep clones).
    /// Phase 2: spawn execution futures with the Arc pointers.
    async fn tick(&self) {
        let now = Utc::now();

        // Phase 1: collect matched task Arcs under read lock.
        let matched: Vec<Arc<CronTask>> = {
            let tasks = self.tasks.read().await;
            tasks
                .values()
                .filter(|t| t.enabled)
                .filter(|t| {
                    match parser::matches_cron(&t.schedule, &now) {
                        Ok(true) => true,
                        Ok(false) => false,
                        Err(e) => {
                            error!(task_id = %t.id, error = %e, "Failed to evaluate cron");
                            false
                        }
                    }
                })
                .cloned() // Arc clone, not CronTask clone.
                .collect()
        };

        // Phase 2: spawn execution (lock released).
        for task in matched {
            debug!(task_id = %task.id, "Cron matched, executing");
            let executor = self.executor.clone();
            tokio::spawn(async move {
                if let Err(e) = executor.execute_task(&task).await {
                    warn!(task_id = %task.id, error = %e, "Cron execution failed");
                }
            });
        }
    }

    /// Stop the scheduling loop.
    pub fn stop(&self) {
        self.cancel.cancel();
    }

    /// Get recent logs.
    pub async fn recent_logs(&self, limit: usize) -> Vec<CronRunLog> {
        self.executor.recent_logs(limit).await
    }

    /// Get logs for a specific task.
    pub async fn task_logs(&self, task_id: &str, limit: usize) -> Vec<CronRunLog> {
        self.executor.task_logs(task_id, limit).await
    }

    /// Manually trigger a task (ignoring schedule).
    pub async fn trigger(&self, id: &str) -> Result<CronRunLog, CronError> {
        let task = {
            let tasks = self.tasks.read().await;
            tasks.get(id).cloned().ok_or_else(|| CronError::InvalidExpression {
                expr: format!("Task '{}' not found", id),
            })?
        };
        self.executor.execute_task(&task).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{AgentExecutor, DeliveryHandler};
    use async_trait::async_trait;
    use clawdesk_types::cron::DeliveryTarget;

    struct MockAgent;

    #[async_trait]
    impl AgentExecutor for MockAgent {
        async fn execute(&self, _prompt: &str, _agent_id: Option<&str>) -> Result<String, String> {
            Ok("done".to_string())
        }
    }

    struct MockDelivery;

    #[async_trait]
    impl DeliveryHandler for MockDelivery {
        async fn deliver(&self, _target: &DeliveryTarget, _content: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn make_task(id: &str, schedule: &str) -> CronTask {
        CronTask {
            id: id.to_string(),
            name: format!("Task {id}"),
            schedule: schedule.to_string(),
            prompt: "Hello".to_string(),
            agent_id: None,
            delivery_targets: vec![],
            skip_if_running: false,
            timeout_secs: 60,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_upsert_and_list() {
        let mgr = CronManager::new(Arc::new(MockAgent), Arc::new(MockDelivery));
        mgr.upsert_task(make_task("t1", "* * * * *")).await.unwrap();
        mgr.upsert_task(make_task("t2", "*/5 * * * *")).await.unwrap();
        let tasks = mgr.list_tasks().await;
        assert_eq!(tasks.len(), 2);
    }

    #[tokio::test]
    async fn test_remove_task() {
        let mgr = CronManager::new(Arc::new(MockAgent), Arc::new(MockDelivery));
        mgr.upsert_task(make_task("t1", "* * * * *")).await.unwrap();
        let removed = mgr.remove_task("t1").await.unwrap();
        assert_eq!(removed.id, "t1");
        assert!(mgr.list_tasks().await.is_empty());
    }

    #[tokio::test]
    async fn test_trigger() {
        let mgr = CronManager::new(Arc::new(MockAgent), Arc::new(MockDelivery));
        mgr.upsert_task(make_task("t1", "* * * * *")).await.unwrap();
        let log = mgr.trigger("t1").await.unwrap();
        assert_eq!(log.task_id, "t1");
    }

    #[tokio::test]
    async fn test_invalid_schedule() {
        let mgr = CronManager::new(Arc::new(MockAgent), Arc::new(MockDelivery));
        let err = mgr.upsert_task(make_task("t1", "bad")).await.unwrap_err();
        assert!(matches!(err, CronError::InvalidExpression { .. }));
    }
}
