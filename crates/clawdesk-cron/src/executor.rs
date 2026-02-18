//! Cron executor — runs scheduled agent tasks with overlap prevention.

use async_trait::async_trait;
use chrono::Utc;
use clawdesk_types::cron::{CronRunLog, CronRunStatus, CronTask, DeliveryTarget};
use clawdesk_types::error::CronError;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Trait for executing an agent prompt and returning the response text.
#[async_trait]
pub trait AgentExecutor: Send + Sync + 'static {
    async fn execute(&self, prompt: &str, agent_id: Option<&str>) -> Result<String, String>;
}

/// Trait for delivering results to targets.
#[async_trait]
pub trait DeliveryHandler: Send + Sync + 'static {
    async fn deliver(
        &self,
        target: &DeliveryTarget,
        content: &str,
    ) -> Result<(), String>;
}

/// Cron task executor with overlap prevention and timeout.
pub struct CronExecutor {
    agent: Arc<dyn AgentExecutor>,
    delivery: Arc<dyn DeliveryHandler>,
    /// Currently running task IDs (for overlap detection).
    running: RwLock<HashSet<String>>,
    /// Execution logs — O(1) append/evict ring buffer.
    logs: RwLock<VecDeque<CronRunLog>>,
    max_log_entries: usize,
}

impl CronExecutor {
    pub fn new(
        agent: Arc<dyn AgentExecutor>,
        delivery: Arc<dyn DeliveryHandler>,
    ) -> Self {
        Self {
            agent,
            delivery,
            running: RwLock::new(HashSet::new()),
            logs: RwLock::new(VecDeque::new()),
            max_log_entries: 1000,
        }
    }

    /// Execute a single cron task.
    pub async fn execute_task(&self, task: &CronTask) -> Result<CronRunLog, CronError> {
        // Check overlap.
        if task.skip_if_running {
            let running = self.running.read().await;
            if running.contains(&task.id) {
                let log = CronRunLog {
                    task_id: task.id.clone(),
                    run_id: uuid::Uuid::new_v4().to_string(),
                    started_at: Utc::now(),
                    finished_at: Some(Utc::now()),
                    status: CronRunStatus::Skipped,
                    result_preview: None,
                    error: Some("Overlapping with running instance".to_string()),
                    tokens_used: None,
                };
                self.append_log(log.clone()).await;
                return Err(CronError::Overlapping {
                    id: task.id.clone(),
                });
            }
        }

        // Mark as running.
        {
            let mut running = self.running.write().await;
            running.insert(task.id.clone());
        }

        let run_id = uuid::Uuid::new_v4().to_string();
        let started_at = Utc::now();

        info!(
            task_id = %task.id,
            run_id = %run_id,
            prompt_len = task.prompt.len(),
            "Executing cron task"
        );

        // Run with timeout.
        let timeout = tokio::time::Duration::from_secs(task.timeout_secs);
        let result = tokio::time::timeout(
            timeout,
            self.agent.execute(&task.prompt, task.agent_id.as_deref()),
        )
        .await;

        let finished_at = Utc::now();

        // Remove from running set.
        {
            let mut running = self.running.write().await;
            running.remove(&task.id);
        }

        let log = match result {
            Ok(Ok(response)) => {
                // Deliver results.
                for target in &task.delivery_targets {
                    if let Err(e) = self.delivery.deliver(target, &response).await {
                        warn!(
                            task_id = %task.id,
                            error = %e,
                            "Delivery failed"
                        );
                    }
                }

                let preview = if response.len() > 200 {
                    format!("{}...", &response[..200])
                } else {
                    response.clone()
                };

                CronRunLog {
                    task_id: task.id.clone(),
                    run_id,
                    started_at,
                    finished_at: Some(finished_at),
                    status: CronRunStatus::Succeeded,
                    result_preview: Some(preview),
                    error: None,
                    tokens_used: None,
                }
            }
            Ok(Err(e)) => {
                error!(task_id = %task.id, error = %e, "Cron task failed");
                CronRunLog {
                    task_id: task.id.clone(),
                    run_id,
                    started_at,
                    finished_at: Some(finished_at),
                    status: CronRunStatus::Failed,
                    result_preview: None,
                    error: Some(e),
                    tokens_used: None,
                }
            }
            Err(_) => {
                error!(task_id = %task.id, timeout_secs = task.timeout_secs, "Cron task timed out");
                CronRunLog {
                    task_id: task.id.clone(),
                    run_id,
                    started_at,
                    finished_at: Some(finished_at),
                    status: CronRunStatus::TimedOut,
                    result_preview: None,
                    error: Some(format!("Timed out after {}s", task.timeout_secs)),
                    tokens_used: None,
                }
            }
        };

        self.append_log(log.clone()).await;

        if log.status == CronRunStatus::TimedOut {
            return Err(CronError::TaskTimeout {
                id: task.id.clone(),
                timeout_secs: task.timeout_secs,
            });
        }

        Ok(log)
    }

    /// Append a log entry. O(1) amortized: pop_front is a pointer
    /// increment (no element shifting), push_back is a simple write.
    async fn append_log(&self, log: CronRunLog) {
        let mut logs = self.logs.write().await;
        if logs.len() >= self.max_log_entries {
            logs.pop_front(); // O(1) — ring buffer head advance
        }
        logs.push_back(log);
    }

    /// Get recent logs for a specific task.
    pub async fn task_logs(&self, task_id: &str, limit: usize) -> Vec<CronRunLog> {
        let logs = self.logs.read().await;
        logs.iter()
            .rev()
            .filter(|l| l.task_id == task_id)
            .take(limit)
            .cloned()
            .collect()
    }

    /// Get all recent logs.
    pub async fn recent_logs(&self, limit: usize) -> Vec<CronRunLog> {
        let logs = self.logs.read().await;
        logs.iter().rev().take(limit).cloned().collect()
    }

    /// Check if a task is currently running.
    pub async fn is_running(&self, task_id: &str) -> bool {
        self.running.read().await.contains(task_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockAgent;

    #[async_trait]
    impl AgentExecutor for MockAgent {
        async fn execute(&self, prompt: &str, _agent_id: Option<&str>) -> Result<String, String> {
            Ok(format!("Response to: {prompt}"))
        }
    }

    struct MockDelivery;

    #[async_trait]
    impl DeliveryHandler for MockDelivery {
        async fn deliver(&self, _target: &DeliveryTarget, _content: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn test_task(id: &str) -> CronTask {
        CronTask {
            id: id.to_string(),
            name: format!("Task {id}"),
            schedule: "* * * * *".to_string(),
            prompt: "What's the weather?".to_string(),
            agent_id: None,
            delivery_targets: vec![],
            skip_if_running: true,
            timeout_secs: 30,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_execute_task() {
        let executor = CronExecutor::new(
            Arc::new(MockAgent),
            Arc::new(MockDelivery),
        );
        let task = test_task("t1");
        let log = executor.execute_task(&task).await.unwrap();
        assert_eq!(log.status, CronRunStatus::Succeeded);
        assert!(log.result_preview.is_some());
    }

    #[tokio::test]
    async fn test_task_logs() {
        let executor = CronExecutor::new(
            Arc::new(MockAgent),
            Arc::new(MockDelivery),
        );
        let task = test_task("t2");
        executor.execute_task(&task).await.unwrap();
        executor.execute_task(&task).await.unwrap();

        let logs = executor.task_logs("t2", 10).await;
        assert_eq!(logs.len(), 2);
    }

    #[tokio::test]
    async fn test_is_not_running_after_complete() {
        let executor = CronExecutor::new(
            Arc::new(MockAgent),
            Arc::new(MockDelivery),
        );
        let task = test_task("t3");
        executor.execute_task(&task).await.unwrap();
        assert!(!executor.is_running("t3").await);
    }
}
