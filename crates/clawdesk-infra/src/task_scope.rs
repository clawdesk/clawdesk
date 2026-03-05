//! Structured concurrency primitives for ClawDesk.
//!
//! Provides `TaskScope` — a tracked set of spawned async tasks that ensures:
//!
//! 1. **No orphaned tasks**: All spawned tasks are collected via `JoinSet`
//!    and awaited on shutdown.
//! 2. **Panic resilience**: Each task is wrapped in `catch_unwind`, converting
//!    panics into logged errors instead of crashing the runtime.
//! 3. **Graceful shutdown**: `shutdown()` sends a cancellation signal and
//!    awaits all tasks with an optional timeout.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let scope = TaskScope::new("gateway");
//!
//! scope.spawn("ws_reader", async move {
//!     // task body
//! });
//!
//! // Later, during shutdown:
//! scope.shutdown(Duration::from_secs(5)).await;
//! ```
//!
//! ## Migration guide
//!
//! Replace fire-and-forget `tokio::spawn` calls:
//!
//! ```rust,ignore
//! // Before:
//! tokio::spawn(async move { do_work().await });
//!
//! // After:
//! scope.spawn("do_work", async move { do_work().await });
//! ```

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Tracked task scope with graceful shutdown support.
///
/// All tasks spawned through this scope are awaited on `shutdown()`.
/// Panics in individual tasks are caught and logged — they do not
/// propagate to the runtime or abort other tasks.
pub struct TaskScope {
    /// Human-readable scope name for log context.
    name: String,
    /// Tracked tasks.
    tasks: Mutex<JoinSet<()>>,
    /// Shutdown signal.
    shutdown: Arc<Notify>,
    /// Count of active tasks (atomic for lock-free reads).
    active_count: Arc<AtomicUsize>,
    /// Total tasks spawned (monotonic counter).
    total_spawned: AtomicUsize,
    /// Total panics caught.
    panic_count: AtomicUsize,
}

impl TaskScope {
    /// Create a new task scope with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tasks: Mutex::new(JoinSet::new()),
            shutdown: Arc::new(Notify::new()),
            active_count: Arc::new(AtomicUsize::new(0)),
            total_spawned: AtomicUsize::new(0),
            panic_count: AtomicUsize::new(0),
        }
    }

    /// Spawn a tracked async task with panic catching.
    ///
    /// The task name is used in log messages. If the task panics, the
    /// panic message is logged at ERROR level and the task is silently
    /// completed — no runtime abort.
    pub async fn spawn<F>(&self, task_name: &str, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let name = format!("{}::{}", self.name, task_name);
        let active = self.active_count.clone();
        let shutdown = self.shutdown.clone();

        active.fetch_add(1, Ordering::Relaxed);
        self.total_spawned.fetch_add(1, Ordering::Relaxed);

        // Wrap the actual task name for the error handler
        let task_name_owned = name.clone();

        let mut tasks = self.tasks.lock().await;
        tasks.spawn(async move {
            // Race the actual work against the shutdown signal
            let result = tokio::select! {
                biased;
                _ = shutdown.notified() => {
                    info!(task = %task_name_owned, "task cancelled by scope shutdown");
                    Ok(())
                }
                result = FutureUnwindSafe(future) => result,
            };

            active.fetch_sub(1, Ordering::Relaxed);

            if let Err(panic_info) = result {
                error!(
                    task = %task_name_owned,
                    panic = ?panic_info,
                    "task panicked — caught by TaskScope"
                );
            }
        });
    }

    /// Spawn a tracked task that returns a result.
    /// Panics and errors are both logged.
    pub async fn spawn_result<F, E>(&self, task_name: &str, future: F)
    where
        F: Future<Output = Result<(), E>> + Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        let name = format!("{}::{}", self.name, task_name);
        let active = self.active_count.clone();

        active.fetch_add(1, Ordering::Relaxed);
        self.total_spawned.fetch_add(1, Ordering::Relaxed);

        let task_name_owned = name.clone();

        let mut tasks = self.tasks.lock().await;
        tasks.spawn(async move {
            let result = ResultFutureUnwindSafe(future).await;

            active.fetch_sub(1, Ordering::Relaxed);

            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    error!(task = %task_name_owned, error = %e, "task failed");
                }
                Err(panic_info) => {
                    error!(
                        task = %task_name_owned,
                        panic = ?panic_info,
                        "task panicked — caught by TaskScope"
                    );
                }
            }
        });
    }

    /// Number of currently active tasks.
    pub fn active_count(&self) -> usize {
        self.active_count.load(Ordering::Relaxed)
    }

    /// Total tasks spawned since creation.
    pub fn total_spawned(&self) -> usize {
        self.total_spawned.load(Ordering::Relaxed)
    }

    /// Total panics caught.
    pub fn panic_count(&self) -> usize {
        self.panic_count.load(Ordering::Relaxed)
    }

    /// Signal all tasks to cancel and await completion.
    ///
    /// Tasks that check the shutdown signal will exit gracefully.
    /// Remaining tasks are aborted after the timeout.
    pub async fn shutdown(&self, timeout_dur: Duration) {
        info!(
            scope = %self.name,
            active = self.active_count(),
            total = self.total_spawned(),
            "initiating task scope shutdown"
        );

        // Signal all tasks
        self.shutdown.notify_waiters();

        // Await all tasks with timeout
        let mut tasks = self.tasks.lock().await;
        let deadline = tokio::time::sleep(timeout_dur);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                biased;
                _ = &mut deadline => {
                    let remaining = tasks.len();
                    if remaining > 0 {
                        warn!(
                            scope = %self.name,
                            remaining,
                            "shutdown timeout — aborting remaining tasks"
                        );
                        tasks.abort_all();
                    }
                    break;
                }
                result = tasks.join_next() => {
                    match result {
                        Some(Ok(())) => {}
                        Some(Err(e)) if e.is_cancelled() => {}
                        Some(Err(e)) => {
                            error!(scope = %self.name, error = %e, "task join error during shutdown");
                        }
                        None => break, // All tasks completed
                    }
                }
            }
        }

        info!(
            scope = %self.name,
            panics = self.panic_count(),
            "task scope shutdown complete"
        );
    }

    /// Get a shutdown signal that tasks can await.
    pub fn shutdown_signal(&self) -> Arc<Notify> {
        self.shutdown.clone()
    }
}

/// Convenience function for one-off spawns with panic catching.
///
/// Use this as a drop-in replacement for `tokio::spawn` when the task
/// is not associated with a `TaskScope`:
///
/// ```rust,ignore
/// // Before:
/// tokio::spawn(async move { risky_work().await });
///
/// // After:
/// spawn_traced("risky_work", async move { risky_work().await });
/// ```
pub fn spawn_traced<F>(name: &str, future: F) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let name = name.to_string();
    tokio::spawn(async move {
        match FutureUnwindSafe(future).await {
            Ok(()) => {}
            Err(panic_info) => {
                error!(
                    task = %name,
                    panic = ?panic_info,
                    "spawned task panicked"
                );
            }
        }
    })
}

/// Wrapper that makes a future implement UnwindSafe and catches panics.
struct FutureUnwindSafe<F>(F);

impl<F: Future<Output = ()>> Future for FutureUnwindSafe<F> {
    type Output = Result<(), Box<dyn std::any::Any + Send>>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // SAFETY: We only project to the inner future, maintaining pinning.
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };

        // catch_unwind requires AssertUnwindSafe for the Poll call
        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
            Ok(std::task::Poll::Ready(())) => std::task::Poll::Ready(Ok(())),
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Err(panic) => std::task::Poll::Ready(Err(panic)),
        }
    }
}

// SAFETY: FutureUnwindSafe delegates Send to the inner future.
unsafe impl<F: Send> Send for FutureUnwindSafe<F> {}

/// Like `FutureUnwindSafe` but for futures returning `Result<(), E>`.
struct ResultFutureUnwindSafe<F>(F);

impl<F, E> Future for ResultFutureUnwindSafe<F>
where
    F: Future<Output = Result<(), E>>,
{
    type Output = Result<Result<(), E>, Box<dyn std::any::Any + Send>>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };

        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
            Ok(std::task::Poll::Ready(result)) => std::task::Poll::Ready(Ok(result)),
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Err(panic) => std::task::Poll::Ready(Err(panic)),
        }
    }
}

unsafe impl<F: Send> Send for ResultFutureUnwindSafe<F> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn test_spawn_and_shutdown() {
        let scope = TaskScope::new("test");
        let completed = Arc::new(AtomicBool::new(false));
        let completed2 = completed.clone();

        scope
            .spawn("worker", async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                completed2.store(true, Ordering::SeqCst);
            })
            .await;

        scope.shutdown(Duration::from_secs(5)).await;
        assert!(completed.load(Ordering::SeqCst));
        assert_eq!(scope.total_spawned(), 1);
    }

    #[tokio::test]
    async fn test_panic_caught() {
        let scope = TaskScope::new("test");

        scope
            .spawn("panicker", async move {
                panic!("intentional test panic");
            })
            .await;

        // Should not panic — the panic is caught
        scope.shutdown(Duration::from_secs(5)).await;
        assert_eq!(scope.total_spawned(), 1);
    }

    #[tokio::test]
    async fn test_spawn_traced() {
        let done = Arc::new(AtomicBool::new(false));
        let done2 = done.clone();

        let handle = spawn_traced("test_task", async move {
            done2.store(true, Ordering::SeqCst);
        });

        handle.await.unwrap();
        assert!(done.load(Ordering::SeqCst));
    }
}
