//! GAP-C: Dependency chain resolution for cron tasks.
//!
//! Before running a task with `depends_on` entries, the resolver checks:
//! 1. Whether all (or any, per `ChainMode`) predecessor tasks have completed
//!    with the required status since this task's own last run.
//! 2. Optionally extracts predecessor results to inject as context.
//!
//! ## Topological safety
//!
//! Cycle detection uses iterative DFS with a `visiting` set → O(V+E) time.
//! Cycles cause the dependent task to be skipped with an error log.

use crate::persistence::CronPersistence;
use chrono::{DateTime, Utc};
use clawdesk_types::cron::{ChainMode, CronRunLog, CronRunStatus, CronTask};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, warn};

/// Result of dependency resolution for a single task.
#[derive(Debug, Clone)]
pub struct DepResolution {
    /// Whether the task's dependencies are satisfied.
    pub satisfied: bool,
    /// Reason if not satisfied.
    pub reason: Option<String>,
    /// Collected predecessor results (for prompt injection).
    pub predecessor_results: Vec<PredecessorResult>,
}

/// Result from a completed predecessor task.
#[derive(Debug, Clone)]
pub struct PredecessorResult {
    pub task_id: String,
    pub task_name: String,
    pub output: String,
    pub completed_at: DateTime<Utc>,
}

/// Resolve dependencies for a task.
///
/// `own_last_run` is this task's last completed run time. Dependencies are
/// only considered "fresh" if the predecessor ran after `own_last_run`.
/// If the task has never run, all dependency runs satisfy.
pub async fn resolve_dependencies(
    task: &CronTask,
    all_tasks: &HashMap<String, Arc<CronTask>>,
    persistence: &dyn CronPersistence,
    own_last_run: Option<DateTime<Utc>>,
) -> DepResolution {
    if task.depends_on.is_empty() || task.chain_mode == ChainMode::Independent {
        return DepResolution {
            satisfied: true,
            reason: None,
            predecessor_results: vec![],
        };
    }

    let mut satisfied_count = 0;
    let mut unsatisfied_reasons = Vec::new();
    let mut predecessor_results = Vec::new();

    for dep in &task.depends_on {
        // Check that the predecessor exists.
        let pred_task = match all_tasks.get(&dep.task_id) {
            Some(t) => t,
            None => {
                unsatisfied_reasons
                    .push(format!("Predecessor '{}' not found", dep.task_id));
                continue;
            }
        };

        // Find the predecessor's last run with the required status.
        let pred_log = match persistence
            .last_run_with_status(&dep.task_id, dep.required_status)
            .await
        {
            Ok(Some(log)) => log,
            Ok(None) => {
                unsatisfied_reasons.push(format!(
                    "Predecessor '{}' has no {:?} run",
                    dep.task_id, dep.required_status
                ));
                continue;
            }
            Err(e) => {
                unsatisfied_reasons.push(format!(
                    "Failed to query predecessor '{}': {}",
                    dep.task_id, e
                ));
                continue;
            }
        };

        // Check freshness: predecessor must have run after this task's last run.
        if let Some(last_run) = own_last_run {
            let pred_time = pred_log.finished_at.unwrap_or(pred_log.started_at);
            if pred_time <= last_run {
                unsatisfied_reasons.push(format!(
                    "Predecessor '{}' last {:?} run ({}) is not newer than own last run ({})",
                    dep.task_id, dep.required_status, pred_time, last_run
                ));
                continue;
            }
        }

        satisfied_count += 1;

        // Collect result for prompt injection if requested.
        if dep.inject_result {
            if let Some(output) = &pred_log.result_preview {
                predecessor_results.push(PredecessorResult {
                    task_id: dep.task_id.clone(),
                    task_name: pred_task.name.clone(),
                    output: output.clone(),
                    completed_at: pred_log.finished_at.unwrap_or(pred_log.started_at),
                });
            }
        }
    }

    let total_deps = task.depends_on.len();
    let satisfied = match task.chain_mode {
        ChainMode::Independent => true, // Already handled above
        ChainMode::AllRequired => satisfied_count == total_deps,
        ChainMode::AnyRequired => satisfied_count > 0,
    };

    if satisfied {
        debug!(
            task_id = %task.id,
            satisfied = satisfied_count,
            total = total_deps,
            "Dependencies satisfied"
        );
        DepResolution {
            satisfied: true,
            reason: None,
            predecessor_results,
        }
    } else {
        let reason = format!(
            "{}/{} deps satisfied (need {}): {}",
            satisfied_count,
            total_deps,
            match task.chain_mode {
                ChainMode::AllRequired => "all",
                ChainMode::AnyRequired => "any",
                ChainMode::Independent => "none",
            },
            unsatisfied_reasons.join("; ")
        );
        warn!(task_id = %task.id, reason = %reason, "Dependencies not met, skipping");
        DepResolution {
            satisfied: false,
            reason: Some(reason),
            predecessor_results: vec![],
        }
    }
}

/// Format predecessor results as context for injection into the task prompt.
pub fn format_dep_context(results: &[PredecessorResult]) -> String {
    if results.is_empty() {
        return String::new();
    }

    let mut ctx = String::from("<predecessor_results>\n");
    for r in results {
        ctx.push_str(&format!(
            "<task name=\"{}\" id=\"{}\" completed=\"{}\">\n{}\n</task>\n",
            r.task_name, r.task_id, r.completed_at, r.output
        ));
    }
    ctx.push_str("</predecessor_results>\n");
    ctx
}

/// Detect cycles in the dependency graph.
///
/// Returns `Some(cycle_path)` if a cycle is found, `None` otherwise.
/// Uses iterative DFS with O(V+E) time complexity.
pub fn detect_cycle(
    task_id: &str,
    all_tasks: &HashMap<String, Arc<CronTask>>,
) -> Option<Vec<String>> {
    let mut visiting: HashSet<String> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<(String, Vec<String>)> = vec![(task_id.to_string(), vec![task_id.to_string()])];

    while let Some((current, path)) = stack.pop() {
        if visited.contains(&current) {
            continue;
        }

        if visiting.contains(&current) {
            visited.insert(current);
            visiting.remove(path.first().unwrap_or(&String::new()));
            continue;
        }

        visiting.insert(current.clone());

        if let Some(task) = all_tasks.get(&current) {
            for dep in &task.depends_on {
                if visiting.contains(&dep.task_id) {
                    let mut cycle = path.clone();
                    cycle.push(dep.task_id.clone());
                    return Some(cycle);
                }
                if !visited.contains(&dep.task_id) {
                    let mut new_path = path.clone();
                    new_path.push(dep.task_id.clone());
                    stack.push((dep.task_id.clone(), new_path));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::InMemoryCronPersistence;
    use clawdesk_types::cron::{ChainMode, TaskDependency};

    fn make_task(id: &str, deps: Vec<TaskDependency>, mode: ChainMode) -> CronTask {
        CronTask {
            id: id.to_string(),
            name: format!("Task {id}"),
            schedule: "* * * * *".to_string(),
            prompt: "hello".to_string(),
            agent_id: None,
            delivery_targets: vec![],
            skip_if_running: false,
            timeout_secs: 60,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            depends_on: deps,
            chain_mode: mode,
            max_retained_logs: 0,
        }
    }

    fn make_log(task_id: &str, status: CronRunStatus) -> CronRunLog {
        CronRunLog {
            task_id: task_id.to_string(),
            run_id: uuid::Uuid::new_v4().to_string(),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            status,
            result_preview: Some("output data".to_string()),
            error: None,
            tokens_used: None,
        }
    }

    #[tokio::test]
    async fn test_no_deps_always_satisfied() {
        let task = make_task("t1", vec![], ChainMode::Independent);
        let store = InMemoryCronPersistence::new();
        let all: HashMap<String, Arc<CronTask>> = HashMap::new();
        let res = resolve_dependencies(&task, &all, &store, None).await;
        assert!(res.satisfied);
    }

    #[tokio::test]
    async fn test_all_required_satisfied() {
        let store = InMemoryCronPersistence::new();
        store.save_log(&make_log("dep1", CronRunStatus::Succeeded)).await.unwrap();
        store.save_log(&make_log("dep2", CronRunStatus::Succeeded)).await.unwrap();

        let dep1 = make_task("dep1", vec![], ChainMode::Independent);
        let dep2 = make_task("dep2", vec![], ChainMode::Independent);

        let task = make_task(
            "t1",
            vec![
                TaskDependency {
                    task_id: "dep1".to_string(),
                    required_status: CronRunStatus::Succeeded,
                    inject_result: true,
                },
                TaskDependency {
                    task_id: "dep2".to_string(),
                    required_status: CronRunStatus::Succeeded,
                    inject_result: false,
                },
            ],
            ChainMode::AllRequired,
        );

        let mut all = HashMap::new();
        all.insert("dep1".to_string(), Arc::new(dep1));
        all.insert("dep2".to_string(), Arc::new(dep2));

        let res = resolve_dependencies(&task, &all, &store, None).await;
        assert!(res.satisfied);
        assert_eq!(res.predecessor_results.len(), 1); // only dep1 has inject_result
    }

    #[tokio::test]
    async fn test_all_required_not_satisfied() {
        let store = InMemoryCronPersistence::new();
        store.save_log(&make_log("dep1", CronRunStatus::Succeeded)).await.unwrap();
        // dep2 has no successful run

        let dep1 = make_task("dep1", vec![], ChainMode::Independent);
        let dep2 = make_task("dep2", vec![], ChainMode::Independent);

        let task = make_task(
            "t1",
            vec![
                TaskDependency {
                    task_id: "dep1".to_string(),
                    required_status: CronRunStatus::Succeeded,
                    inject_result: false,
                },
                TaskDependency {
                    task_id: "dep2".to_string(),
                    required_status: CronRunStatus::Succeeded,
                    inject_result: false,
                },
            ],
            ChainMode::AllRequired,
        );

        let mut all = HashMap::new();
        all.insert("dep1".to_string(), Arc::new(dep1));
        all.insert("dep2".to_string(), Arc::new(dep2));

        let res = resolve_dependencies(&task, &all, &store, None).await;
        assert!(!res.satisfied);
    }

    #[tokio::test]
    async fn test_any_required_partial_ok() {
        let store = InMemoryCronPersistence::new();
        store.save_log(&make_log("dep1", CronRunStatus::Succeeded)).await.unwrap();

        let dep1 = make_task("dep1", vec![], ChainMode::Independent);
        let dep2 = make_task("dep2", vec![], ChainMode::Independent);

        let task = make_task(
            "t1",
            vec![
                TaskDependency {
                    task_id: "dep1".to_string(),
                    required_status: CronRunStatus::Succeeded,
                    inject_result: false,
                },
                TaskDependency {
                    task_id: "dep2".to_string(),
                    required_status: CronRunStatus::Succeeded,
                    inject_result: false,
                },
            ],
            ChainMode::AnyRequired,
        );

        let mut all = HashMap::new();
        all.insert("dep1".to_string(), Arc::new(dep1));
        all.insert("dep2".to_string(), Arc::new(dep2));

        let res = resolve_dependencies(&task, &all, &store, None).await;
        assert!(res.satisfied);
    }

    #[test]
    fn test_cycle_detection_no_cycle() {
        let t1 = make_task("t1", vec![], ChainMode::Independent);
        let t2 = make_task(
            "t2",
            vec![TaskDependency {
                task_id: "t1".to_string(),
                required_status: CronRunStatus::Succeeded,
                inject_result: false,
            }],
            ChainMode::AllRequired,
        );

        let mut all = HashMap::new();
        all.insert("t1".to_string(), Arc::new(t1));
        all.insert("t2".to_string(), Arc::new(t2));

        assert!(detect_cycle("t2", &all).is_none());
    }

    #[test]
    fn test_cycle_detection_with_cycle() {
        let t1 = make_task(
            "t1",
            vec![TaskDependency {
                task_id: "t2".to_string(),
                required_status: CronRunStatus::Succeeded,
                inject_result: false,
            }],
            ChainMode::AllRequired,
        );
        let t2 = make_task(
            "t2",
            vec![TaskDependency {
                task_id: "t1".to_string(),
                required_status: CronRunStatus::Succeeded,
                inject_result: false,
            }],
            ChainMode::AllRequired,
        );

        let mut all = HashMap::new();
        all.insert("t1".to_string(), Arc::new(t1));
        all.insert("t2".to_string(), Arc::new(t2));

        assert!(detect_cycle("t1", &all).is_some());
    }

    #[test]
    fn test_format_dep_context_empty() {
        assert!(format_dep_context(&[]).is_empty());
    }

    #[test]
    fn test_format_dep_context_with_results() {
        let results = vec![PredecessorResult {
            task_id: "dep1".to_string(),
            task_name: "Daily Report".to_string(),
            output: "Sales: $50k".to_string(),
            completed_at: Utc::now(),
        }];
        let ctx = format_dep_context(&results);
        assert!(ctx.contains("<predecessor_results>"));
        assert!(ctx.contains("Daily Report"));
        assert!(ctx.contains("Sales: $50k"));
    }
}
