//! Cron manager — task CRUD, scheduling loop, dependency chaining, persistence.

use crate::dep_resolver;
use crate::executor::{AgentExecutor, CronExecutor, DeliveryHandler};
use crate::parser;
use crate::persistence::CronPersistence;
use chrono::Utc;
use clawdesk_types::cron::{ChainMode, CronRunLog, CronRunStatus, CronTask};
use clawdesk_types::error::CronError;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Manages scheduled tasks: CRUD operations, scheduling loop, dependency
/// resolution, and optional durable persistence.
///
/// Tasks are stored as `Arc<CronTask>` to avoid deep cloning in the tick loop.
/// The two-phase tick evaluates cron expressions under a read lock, then only
/// clones `Arc` pointers (not task data) for spawned execution futures.
pub struct CronManager {
    tasks: RwLock<HashMap<String, Arc<CronTask>>>,
    executor: Arc<CronExecutor>,
    cancel: CancellationToken,
    /// Optional durable persistence backend (GAP-C).
    persistence: Option<Arc<dyn CronPersistence>>,
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
            persistence: None,
        }
    }

    /// Create a CronManager with durable persistence (GAP-C).
    pub fn with_persistence(
        agent: Arc<dyn AgentExecutor>,
        delivery: Arc<dyn DeliveryHandler>,
        persistence: Arc<dyn CronPersistence>,
    ) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            executor: Arc::new(CronExecutor::new(agent, delivery)),
            cancel: CancellationToken::new(),
            persistence: Some(persistence),
        }
    }

    /// Load persisted tasks from the durable backend (call on startup).
    pub async fn load_persisted(&self) -> Result<usize, String> {
        let Some(persistence) = &self.persistence else {
            return Ok(0);
        };

        let persisted = persistence.load_tasks().await?;
        let count = persisted.len();
        let mut tasks = self.tasks.write().await;
        for task in persisted {
            tasks.insert(task.id.clone(), Arc::new(task));
        }
        info!(count, "Loaded persisted cron tasks");
        Ok(count)
    }

    /// Add or update a task.
    pub async fn upsert_task(&self, task: CronTask) -> Result<(), CronError> {
        // Validate the cron expression.
        parser::parse_cron_expression(&task.schedule)?;

        // Check for dependency cycles before inserting.
        if !task.depends_on.is_empty() {
            let mut preview = self.tasks.read().await.clone();
            preview.insert(task.id.clone(), Arc::new(task.clone()));
            if let Some(cycle) = dep_resolver::detect_cycle(&task.id, &preview) {
                return Err(CronError::InvalidExpression {
                    expr: format!("Dependency cycle detected: {}", cycle.join(" → ")),
                });
            }
        }

        // Persist before inserting in memory.
        if let Some(persistence) = &self.persistence {
            persistence.save_task(&task).await.map_err(|e| CronError::InvalidExpression {
                expr: format!("Persistence error: {}", e),
            })?;
        }

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

        // Remove from persistence.
        if let Some(persistence) = &self.persistence {
            if let Err(e) = persistence.delete_task(id).await {
                warn!(task_id = %id, error = %e, "Failed to delete task from persistence");
            }
        }

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
        let task_snapshot = {
            let mut tasks = self.tasks.write().await;
            let arc_task = tasks.get_mut(id).ok_or_else(|| CronError::InvalidExpression {
                expr: format!("Task '{}' not found", id),
            })?;
            // Must replace the Arc since CronTask fields aren't mutable through Arc.
            let mut task = (**arc_task).clone();
            task.enabled = enabled;
            task.updated_at = Utc::now();
            let snapshot = task.clone();
            *arc_task = Arc::new(task);
            snapshot
        };

        // Persist the change outside the write lock to avoid holding it during I/O.
        if let Some(persistence) = &self.persistence {
            persistence.save_task(&task_snapshot).await.map_err(|e| CronError::InvalidExpression {
                expr: format!("Persistence error: {}", e),
            })?;
        }
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

    /// Process one scheduling tick — two-phase evaluation with dependency resolution.
    ///
    /// Phase 1: under read lock, evaluate cron expressions and collect Arc
    /// pointers for matched tasks (no deep clones).
    /// Phase 2: check dependencies and spawn execution futures.
    async fn tick(&self) {
        // Use local time for cron evaluation — users configure schedules
        // in their local timezone (e.g. "06 23 * * 0" means 11:06 PM local).
        // We create a DateTime<Utc> with local-time components so the parser
        // matches against the user's intended hours/minutes/days.
        let local_now = chrono::Local::now();
        let now = chrono::DateTime::<Utc>::from_naive_utc_and_offset(
            local_now.naive_local(),
            Utc,
        );

        // Phase 1: collect matched task Arcs under read lock.
        let (matched, all_tasks): (Vec<Arc<CronTask>>, HashMap<String, Arc<CronTask>>) = {
            let tasks = self.tasks.read().await;
            let all = tasks.clone();
            let matched = tasks
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
                .collect();
            (matched, all)
        };

        // Phase 2: resolve dependencies and spawn execution (lock released).
        for task in matched {
            let executor = self.executor.clone();
            let persistence = self.persistence.clone();
            let all_for_dep = all_tasks.clone();

            tokio::spawn(async move {
                // Check dependencies (GAP-C).
                if !task.depends_on.is_empty() && task.chain_mode != ChainMode::Independent {
                    if let Some(ref p) = persistence {
                        // Find this task's last run time.
                        let own_last = p
                            .last_run_with_status(&task.id, CronRunStatus::Succeeded)
                            .await
                            .ok()
                            .flatten()
                            .and_then(|l| l.finished_at);

                        let resolution = dep_resolver::resolve_dependencies(
                            &task,
                            &all_for_dep,
                            p.as_ref(),
                            own_last,
                        )
                        .await;

                        if !resolution.satisfied {
                            debug!(
                                task_id = %task.id,
                                reason = ?resolution.reason,
                                "Dependencies not met, skipping"
                            );
                            // Log as skipped.
                            let log = CronRunLog {
                                task_id: task.id.clone(),
                                run_id: uuid::Uuid::new_v4().to_string(),
                                started_at: Utc::now(),
                                finished_at: Some(Utc::now()),
                                status: CronRunStatus::Skipped,
                                result_preview: None,
                                error: resolution.reason,
                                tokens_used: None,
                            };
                            let _ = p.save_log(&log).await;
                            return;
                        }

                        // Inject predecessor results into prompt if available.
                        if !resolution.predecessor_results.is_empty() {
                            let context = dep_resolver::format_dep_context(
                                &resolution.predecessor_results,
                            );
                            // Create a modified task with augmented prompt.
                            let mut augmented = (*task).clone();
                            augmented.prompt = format!("{}\n\n{}", context, augmented.prompt);
                            if let Err(e) = executor.execute_task(&augmented).await {
                                warn!(task_id = %task.id, error = %e, "Cron execution failed");
                            } else if let Some(ref p) = persistence {
                                // Save latest log to persistence.
                                if let Some(log) = executor.task_logs(&task.id, 1).await.first() {
                                    let _ = p.save_log(log).await;
                                }
                            }
                            return;
                        }
                    }
                }

                // Standard execution (no deps or deps satisfied without injection).
                debug!(task_id = %task.id, "Cron matched, executing");
                if let Err(e) = executor.execute_task(&task).await {
                    warn!(task_id = %task.id, error = %e, "Cron execution failed");
                }
                // Persist execution log.
                if let Some(ref p) = persistence {
                    if let Some(log) = executor.task_logs(&task.id, 1).await.first() {
                        let _ = p.save_log(log).await;
                    }
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
        let log = self.executor.execute_task(&task).await?;
        // Persist run log.
        if let Some(persistence) = &self.persistence {
            let _ = persistence.save_log(&log).await;
        }
        Ok(log)
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
            depends_on: vec![],
            chain_mode: Default::default(),
            max_retained_logs: 0,
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
