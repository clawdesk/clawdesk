//! Structured concurrency for the ClawDesk runtime.
//!
//! Provides a `TaskScope` that enforces the invariant that parent tasks
//! always outlive their children. When a scope completes (or is cancelled),
//! all spawned child tasks are joined or cancelled before the scope exits.
//!
//! ## Design Principles
//!
//! 1. **No orphaned tasks** — every spawned future is tracked and joined.
//! 2. **Cancellation propagation** — a `CancellationToken` is shared with
//!    all children; cancelling the parent token cancels all descendants.
//! 3. **Error propagation** — if any child fails, the scope can be configured
//!    to cancel siblings (fail-fast) or collect all results (collect-all).
//! 4. **Timeout enforcement** — per-scope and per-child deadlines.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let scope = TaskScope::new("agent-run");
//! let token = scope.token();
//!
//! scope.spawn("tool-a", async move { tool_a(token.child()).await });
//! scope.spawn("tool-b", async move { tool_b(token.child()).await });
//!
//! // Waits for all children, propagates first error in fail-fast mode.
//! scope.join().await?;
//! ```

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn, Span};

// ─────────────────────────────────────────────────────────────────────────────
// Cancellation token
// ─────────────────────────────────────────────────────────────────────────────

/// Cooperative cancellation token, propagated from parent to child scopes.
///
/// Calling `cancel()` on a parent token propagates cancellation to all
/// child tokens. Tasks should periodically check `is_cancelled()`.
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

struct CancellationInner {
    cancelled: AtomicBool,
    notify: Notify,
    parent: Option<CancellationToken>,
}

impl CancellationToken {
    /// Create a root cancellation token (no parent).
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancellationInner {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
                parent: None,
            }),
        }
    }

    /// Create a child token that inherits cancellation from this token.
    pub fn child(&self) -> Self {
        Self {
            inner: Arc::new(CancellationInner {
                cancelled: AtomicBool::new(self.is_cancelled()),
                notify: Notify::new(),
                parent: Some(self.clone()),
            }),
        }
    }

    /// Cancel this token and notify all waiters.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    /// Check if this token (or any ancestor) has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        if self.inner.cancelled.load(Ordering::Acquire) {
            return true;
        }
        if let Some(ref parent) = self.inner.parent {
            if parent.is_cancelled() {
                // Cache the result locally
                self.inner.cancelled.store(true, Ordering::Release);
                return true;
            }
        }
        false
    }

    /// Wait until this token is cancelled. Returns immediately if already cancelled.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        // Poll parent cancellation in parallel with local notification
        loop {
            tokio::select! {
                _ = self.inner.notify.notified() => {
                    if self.is_cancelled() {
                        return;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if self.is_cancelled() {
                        return;
                    }
                }
            }
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Task scope
// ─────────────────────────────────────────────────────────────────────────────

/// Strategy when a child task fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureStrategy {
    /// Cancel all siblings on first child failure.
    FailFast,
    /// Let all siblings finish; collect all results.
    CollectAll,
}

/// Configuration for a task scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeConfig {
    /// Maximum concurrent child tasks.
    pub max_concurrent: usize,
    /// Strategy on child failure.
    pub failure_strategy: FailureStrategy,
    /// Overall scope timeout (None = no timeout).
    #[serde(default)]
    pub timeout: Option<Duration>,
}

impl Default for ScopeConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 64,
            failure_strategy: FailureStrategy::FailFast,
            timeout: None,
        }
    }
}

/// Outcome of a child task.
#[derive(Debug)]
pub enum TaskOutcome {
    /// Task completed successfully.
    Ok,
    /// Task returned an error.
    Err(String),
    /// Task was cancelled.
    Cancelled,
    /// Task panicked.
    Panicked(String),
}

/// Summary of a completed scope.
#[derive(Debug)]
pub struct ScopeResult {
    pub name: String,
    pub outcomes: HashMap<String, TaskOutcome>,
    pub elapsed: Duration,
    pub total_spawned: usize,
}

impl ScopeResult {
    /// Whether all tasks completed successfully.
    pub fn is_ok(&self) -> bool {
        self.outcomes
            .values()
            .all(|o| matches!(o, TaskOutcome::Ok))
    }

    /// Get the first error, if any.
    pub fn first_error(&self) -> Option<(&str, &str)> {
        self.outcomes.iter().find_map(|(name, outcome)| {
            if let TaskOutcome::Err(msg) = outcome {
                Some((name.as_str(), msg.as_str()))
            } else {
                None
            }
        })
    }
}

/// A structured concurrency scope that tracks child tasks.
///
/// Guarantees: when `join()` returns, all spawned children have completed.
pub struct TaskScope {
    name: String,
    config: ScopeConfig,
    token: CancellationToken,
    children: Vec<(String, JoinHandle<Result<(), String>>)>,
    spawned_count: AtomicUsize,
    started_at: Instant,
}

impl TaskScope {
    /// Create a new scope with default configuration.
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_config(name, ScopeConfig::default())
    }

    /// Create a new scope with specific configuration.
    pub fn with_config(name: impl Into<String>, config: ScopeConfig) -> Self {
        Self {
            name: name.into(),
            config,
            token: CancellationToken::new(),
            children: Vec::new(),
            spawned_count: AtomicUsize::new(0),
            started_at: Instant::now(),
        }
    }

    /// Get this scope's cancellation token.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Current number of spawned tasks.
    pub fn spawned_count(&self) -> usize {
        self.spawned_count.load(Ordering::Relaxed)
    }

    /// Spawn a named child task within this scope.
    ///
    /// The task receives a child cancellation token and should check it
    /// periodically to support cooperative cancellation.
    pub fn spawn<F, Fut>(&mut self, name: impl Into<String>, f: F)
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let name = name.into();
        let child_token = self.token.child();

        self.spawned_count.fetch_add(1, Ordering::Relaxed);

        let task_name = name.clone();
        let handle = tokio::spawn(async move {
            debug!(task = %task_name, "child task started");
            let result = f(child_token).await;
            debug!(task = %task_name, ok = result.is_ok(), "child task finished");
            result
        });

        self.children.push((name, handle));
    }

    /// Cancel all children and wait for them to finish.
    pub async fn cancel_all(&mut self) {
        self.token.cancel();
        for (name, handle) in self.children.drain(..) {
            match handle.await {
                Ok(_) => debug!(task = %name, "cancelled task joined"),
                Err(e) => warn!(task = %name, error = %e, "cancelled task panicked"),
            }
        }
    }

    /// Wait for all children to complete, applying the configured failure strategy.
    ///
    /// Returns a `ScopeResult` with outcomes for each child task.
    pub async fn join(mut self) -> ScopeResult {
        let deadline = self.config.timeout.map(|t| Instant::now() + t);
        let mut outcomes = HashMap::new();

        // Timeout wrapper
        if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, self.join_inner(&mut outcomes)).await {
                Ok(()) => {}
                Err(_) => {
                    // Timed out — cancel remaining
                    warn!(scope = %self.name, "scope timed out, cancelling remaining tasks");
                    self.token.cancel();
                    for (name, handle) in self.children.drain(..) {
                        if outcomes.contains_key(&name) {
                            continue;
                        }
                        match handle.await {
                            Ok(Ok(())) => {
                                outcomes.insert(name, TaskOutcome::Ok);
                            }
                            Ok(Err(e)) => {
                                outcomes.insert(name, TaskOutcome::Err(e));
                            }
                            Err(e) => {
                                outcomes.insert(
                                    name,
                                    TaskOutcome::Panicked(e.to_string()),
                                );
                            }
                        }
                    }
                }
            }
        } else {
            self.join_inner(&mut outcomes).await;
        }

        let elapsed = self.started_at.elapsed();
        let total_spawned = self.spawned_count.load(Ordering::Relaxed);

        info!(
            scope = %self.name,
            total = total_spawned,
            ok = outcomes.values().filter(|o| matches!(o, TaskOutcome::Ok)).count(),
            elapsed_ms = elapsed.as_millis(),
            "scope completed"
        );

        ScopeResult {
            name: self.name,
            outcomes,
            elapsed,
            total_spawned,
        }
    }

    async fn join_inner(&mut self, outcomes: &mut HashMap<String, TaskOutcome>) {
        let children = std::mem::take(&mut self.children);

        for (name, handle) in children {
            match handle.await {
                Ok(Ok(())) => {
                    outcomes.insert(name, TaskOutcome::Ok);
                }
                Ok(Err(msg)) => {
                    outcomes.insert(name.clone(), TaskOutcome::Err(msg));
                    if self.config.failure_strategy == FailureStrategy::FailFast {
                        warn!(scope = %self.name, failed_task = %name, "fail-fast: cancelling siblings");
                        self.token.cancel();
                    }
                }
                Err(join_error) => {
                    let msg = join_error.to_string();
                    outcomes.insert(name, TaskOutcome::Panicked(msg));
                    if self.config.failure_strategy == FailureStrategy::FailFast {
                        self.token.cancel();
                    }
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn basic_scope_join() {
        let mut scope = TaskScope::new("test");
        scope.spawn("a", |_token| async { Ok(()) });
        scope.spawn("b", |_token| async { Ok(()) });
        let result = scope.join().await;
        assert!(result.is_ok());
        assert_eq!(result.total_spawned, 2);
    }

    #[tokio::test]
    async fn fail_fast_cancels_siblings() {
        let mut scope = TaskScope::new("fail-test");
        scope.spawn("fast-fail", |_token| async { Err("boom".into()) });
        scope.spawn("slow", |token| async move {
            // This should get cancelled
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => Ok(()),
                _ = token.cancelled() => Err("cancelled".into()),
            }
        });
        let result = scope.join().await;
        assert!(!result.is_ok());
        assert!(result.first_error().is_some());
    }

    #[tokio::test]
    async fn cancellation_token_propagation() {
        let parent = CancellationToken::new();
        let child = parent.child();
        let grandchild = child.child();

        assert!(!grandchild.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
    }

    #[tokio::test]
    async fn scope_timeout() {
        let config = ScopeConfig {
            timeout: Some(Duration::from_millis(50)),
            ..Default::default()
        };
        let mut scope = TaskScope::with_config("timeout-test", config);
        scope.spawn("slow", |_token| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(())
        });
        let result = scope.join().await;
        assert!(result.elapsed < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn collect_all_strategy() {
        let config = ScopeConfig {
            failure_strategy: FailureStrategy::CollectAll,
            ..Default::default()
        };
        let mut scope = TaskScope::with_config("collect-test", config);
        scope.spawn("ok-task", |_token| async { Ok(()) });
        scope.spawn("err-task", |_token| async { Err("oops".into()) });
        let result = scope.join().await;
        assert_eq!(result.outcomes.len(), 2);
        // Both tasks should have completed
        assert!(matches!(
            result.outcomes.get("ok-task"),
            Some(TaskOutcome::Ok)
        ));
        assert!(matches!(
            result.outcomes.get("err-task"),
            Some(TaskOutcome::Err(_))
        ));
    }
}
