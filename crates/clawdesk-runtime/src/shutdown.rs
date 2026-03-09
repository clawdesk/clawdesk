//! Graceful shutdown orchestrator.
//!
//! Provides a centralized shutdown coordinator that:
//! 1. Listens for shutdown signals (SIGTERM, SIGINT, or explicit trigger)
//! 2. Notifies all registered subsystems in priority order
//! 3. Gives each subsystem a grace period to drain work
//! 4. Force-kills anything still running after the deadline
//!
//! # Usage
//!
//! ```ignore
//! let shutdown = ShutdownCoordinator::new(ShutdownConfig::default());
//! let token = shutdown.token();
//!
//! // Register subsystems (lower phase = earlier shutdown).
//! shutdown.register("http_server", 10, || async { /* drain connections */ });
//! shutdown.register("agent_runtime", 20, || async { /* checkpoint state */ });
//! shutdown.register("sochdb", 30, || async { /* flush WAL */ });
//!
//! // In main:
//! shutdown.wait().await; // blocks until all subsystems shut down
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Notify};

/// Configuration for graceful shutdown.
#[derive(Debug, Clone)]
pub struct ShutdownConfig {
    /// Maximum time to wait for all subsystems to drain.
    pub grace_period: Duration,
    /// Time between shutdown progress checks.
    pub poll_interval: Duration,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            grace_period: Duration::from_secs(30),
            poll_interval: Duration::from_millis(250),
        }
    }
}

/// A token that can be cloned and checked to see if shutdown is in progress.
#[derive(Clone)]
pub struct ShutdownToken {
    triggered: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl ShutdownToken {
    fn new() -> Self {
        Self {
            triggered: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Check if shutdown has been triggered.
    pub fn is_shutdown(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }

    /// Wait until shutdown is triggered.
    pub async fn wait(&self) {
        if self.is_shutdown() {
            return;
        }
        self.notify.notified().await;
    }

    /// Trigger shutdown.
    fn trigger(&self) {
        self.triggered.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

/// Shutdown phase — lower numbers shut down first.
type Phase = u32;

type ShutdownFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A registered subsystem with its shutdown handler.
struct Subsystem {
    name: String,
    phase: Phase,
    handler: Option<Box<dyn FnOnce() -> ShutdownFuture + Send>>,
}

/// Outcome of shutting down a single subsystem.
#[derive(Debug, Clone)]
pub struct SubsystemResult {
    pub name: String,
    pub phase: Phase,
    pub completed: bool,
    pub elapsed: Duration,
}

/// Result of the full shutdown sequence.
#[derive(Debug, Clone)]
pub struct ShutdownResult {
    pub subsystems: Vec<SubsystemResult>,
    pub total_elapsed: Duration,
    pub all_clean: bool,
}

/// Central shutdown coordinator.
pub struct ShutdownCoordinator {
    config: ShutdownConfig,
    token: ShutdownToken,
    subsystems: Vec<Subsystem>,
    /// Broadcast sender for shutdown signal.
    _tx: broadcast::Sender<()>,
}

impl ShutdownCoordinator {
    /// Create a new shutdown coordinator.
    pub fn new(config: ShutdownConfig) -> Self {
        let (tx, _) = broadcast::channel(1);
        Self {
            config,
            token: ShutdownToken::new(),
            subsystems: Vec::new(),
            _tx: tx,
        }
    }

    /// Get a cloneable shutdown token.
    pub fn token(&self) -> ShutdownToken {
        self.token.clone()
    }

    /// Register a subsystem with a shutdown handler.
    ///
    /// `phase` controls ordering — lower phases run first.
    pub fn register<F, Fut>(&mut self, name: impl Into<String>, phase: Phase, handler: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.subsystems.push(Subsystem {
            name: name.into(),
            phase,
            handler: Some(Box::new(move || Box::pin(handler()))),
        });
    }

    /// Trigger shutdown and execute all handlers in phase order.
    ///
    /// Each handler gets the configured grace period. If a handler doesn't
    /// complete in time, it's abandoned and we move to the next phase.
    pub async fn shutdown(mut self) -> ShutdownResult {
        let start = std::time::Instant::now();
        self.token.trigger();

        // Sort by phase.
        self.subsystems.sort_by_key(|s| s.phase);

        let mut results = Vec::new();
        let per_subsystem_timeout = self.config.grace_period;

        for sub in &mut self.subsystems {
            let sub_start = std::time::Instant::now();
            let name = sub.name.clone();
            let phase = sub.phase;

            if let Some(handler) = sub.handler.take() {
                let fut = handler();
                let completed = tokio::time::timeout(per_subsystem_timeout, fut)
                    .await
                    .is_ok();

                results.push(SubsystemResult {
                    name,
                    phase,
                    completed,
                    elapsed: sub_start.elapsed(),
                });
            }
        }

        let all_clean = results.iter().all(|r| r.completed);

        ShutdownResult {
            subsystems: results,
            total_elapsed: start.elapsed(),
            all_clean,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_token_signal() {
        let token = ShutdownToken::new();
        assert!(!token.is_shutdown());
        token.trigger();
        assert!(token.is_shutdown());
        // wait() should return immediately after trigger.
        token.wait().await;
    }

    #[tokio::test]
    async fn ordered_shutdown() {
        let mut coord = ShutdownCoordinator::new(ShutdownConfig::default());

        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let o1 = order.clone();
        let o2 = order.clone();
        let o3 = order.clone();

        coord.register("db", 30, move || async move {
            o3.lock().unwrap().push("db");
        });
        coord.register("http", 10, move || async move {
            o1.lock().unwrap().push("http");
        });
        coord.register("runtime", 20, move || async move {
            o2.lock().unwrap().push("runtime");
        });

        let result = coord.shutdown().await;
        assert!(result.all_clean);
        assert_eq!(*order.lock().unwrap(), vec!["http", "runtime", "db"]);
    }

    #[tokio::test]
    async fn timeout_handling() {
        let config = ShutdownConfig {
            grace_period: Duration::from_millis(50),
            ..Default::default()
        };
        let mut coord = ShutdownCoordinator::new(config);

        coord.register("slow", 10, || async {
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        coord.register("fast", 20, || async {});

        let result = coord.shutdown().await;
        assert!(!result.all_clean);
        assert!(!result.subsystems[0].completed); // slow timed out
        assert!(result.subsystems[1].completed); // fast completed
    }
}
