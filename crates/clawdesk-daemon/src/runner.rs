//! Daemon runner — the actual `clawdesk daemon run` execution mode.
//!
//! Differs from `gateway run` in three ways:
//! 1. Writes/manages a PID file
//! 2. Integrates with sd_notify (systemd readiness + watchdog)
//! 3. Implements 6-phase graceful shutdown with state checkpointing
//!
//! ## Shutdown Protocol
//!
//! ```text
//! Phase 1: Stop accepting new connections
//! Phase 2: Drain in-flight agent runs (max 30s)
//! Phase 3: Checkpoint active sessions
//! Phase 4: Flush event bus to dead letter queue
//! Phase 5: Persist cron next-fire times
//! Phase 6: Close storage (SochDB WAL fsync)
//! ```

use crate::{DaemonError, HealthCheck, PidFile};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Instant;
use tracing::{error, info, warn};

/// Phases of the graceful shutdown protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShutdownPhase {
    /// Phase 1: Stop accepting new inbound connections.
    StopAccepting,
    /// Phase 2: Drain in-flight agent runs (wait up to timeout).
    DrainInFlight,
    /// Phase 3: Checkpoint all active session state.
    CheckpointSessions,
    /// Phase 4: Flush event bus pending items to dead letter queue.
    FlushEventBus,
    /// Phase 5: Persist cron schedule next-fire times.
    PersistCronState,
    /// Phase 6: Close storage cleanly (fsync WAL).
    CloseStorage,
}

impl ShutdownPhase {
    /// All phases in execution order.
    pub fn all() -> &'static [ShutdownPhase] {
        &[
            ShutdownPhase::StopAccepting,
            ShutdownPhase::DrainInFlight,
            ShutdownPhase::CheckpointSessions,
            ShutdownPhase::FlushEventBus,
            ShutdownPhase::PersistCronState,
            ShutdownPhase::CloseStorage,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            ShutdownPhase::StopAccepting => "stop_accepting",
            ShutdownPhase::DrainInFlight => "drain_in_flight",
            ShutdownPhase::CheckpointSessions => "checkpoint_sessions",
            ShutdownPhase::FlushEventBus => "flush_event_bus",
            ShutdownPhase::PersistCronState => "persist_cron_state",
            ShutdownPhase::CloseStorage => "close_storage",
        }
    }
}

/// Configuration for the daemon runner.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Port the gateway listens on.
    pub port: u16,
    /// Bind address.
    pub bind: String,
    /// Maximum time to wait for in-flight requests to drain.
    pub drain_timeout_secs: u64,
    /// PID file path (default: `~/.clawdesk/clawdesk.pid`).
    pub pid_path: PathBuf,
    /// Log directory.
    pub log_dir: PathBuf,
    /// Systemd watchdog interval (0 = disabled).
    pub watchdog_interval_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let home = PidFile::default_path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        Self {
            port: 18789,
            bind: "127.0.0.1".into(),
            drain_timeout_secs: 30,
            pid_path: PidFile::default_path(),
            log_dir: home.join("logs"),
            watchdog_interval_secs: 0,
        }
    }
}

/// The daemon runner manages the full lifecycle of `clawdesk daemon run`.
///
/// It wraps the gateway server with daemon-specific functionality:
/// PID file management, sd_notify integration, watchdog heartbeats,
/// and the 6-phase graceful shutdown protocol.
pub struct DaemonRunner {
    config: DaemonConfig,
    pid_file: PidFile,
    started_at: Instant,
}

impl DaemonRunner {
    /// Create a new daemon runner.
    pub fn new(config: DaemonConfig) -> Self {
        let pid_file = PidFile::new(&config.pid_path);
        Self {
            config,
            pid_file,
            started_at: Instant::now(),
        }
    }

    /// Acquire the PID file — prevents multiple daemon instances.
    pub fn acquire_pid(&self) -> Result<(), DaemonError> {
        self.pid_file.acquire()
    }

    /// Release the PID file on shutdown.
    pub fn release_pid(&self) -> Result<(), DaemonError> {
        self.pid_file.release()
    }

    /// Notify systemd that the daemon is ready (sd_notify READY=1).
    ///
    /// On non-systemd platforms, this is a no-op.
    pub fn notify_ready(&self) {
        #[cfg(target_os = "linux")]
        {
            if let Ok(sock) = std::env::var("NOTIFY_SOCKET") {
                let msg = b"READY=1\nSTATUS=ClawDesk gateway ready\n";
                if let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() {
                    let _ = socket.send_to(msg, &sock);
                    info!("sd_notify: READY=1 sent");
                }
            }
        }
        let _ = &self; // suppress unused on non-linux
    }

    /// Send a watchdog heartbeat to systemd (WATCHDOG=1).
    ///
    /// Should be called at `WatchdogSec/2` intervals.
    pub fn notify_watchdog(&self) {
        #[cfg(target_os = "linux")]
        {
            if let Ok(sock) = std::env::var("NOTIFY_SOCKET") {
                let msg = b"WATCHDOG=1\n";
                if let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() {
                    let _ = socket.send_to(msg, &sock);
                }
            }
        }
        let _ = &self; // suppress unused on non-linux
    }

    /// Notify systemd of a status message.
    pub fn notify_status(&self, status: &str) {
        #[cfg(target_os = "linux")]
        {
            if let Ok(sock) = std::env::var("NOTIFY_SOCKET") {
                let msg = format!("STATUS={status}\n");
                if let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() {
                    let _ = socket.send_to(msg.as_bytes(), &sock);
                }
            }
        }
        let _ = (self, status); // suppress unused
    }

    /// Start the watchdog heartbeat task.
    ///
    /// Returns a `JoinHandle` that sends heartbeats every `interval` seconds.
    /// Cancel via the provided `CancellationToken`.
    pub fn spawn_watchdog(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let interval = self.config.watchdog_interval_secs;
        if interval == 0 {
            // Auto-detect from systemd WATCHDOG_USEC.
            #[cfg(target_os = "linux")]
            {
                if let Ok(usec) = std::env::var("WATCHDOG_USEC") {
                    if let Ok(val) = usec.parse::<u64>() {
                        let half_secs = val / 2_000_000; // WATCHDOG_USEC/2 in seconds
                        if half_secs > 0 {
                            return Some(Self::start_heartbeat(half_secs, cancel));
                        }
                    }
                }
            }
            return None;
        }

        Some(Self::start_heartbeat(interval, cancel))
    }

    /// Spawn a SIGHUP handler that calls the provided reload callback.
    ///
    /// On Unix systems, `SIGHUP` triggers a full hot-reload of agents,
    /// skills, and config. The caller provides a closure that performs
    /// the actual reload logic (typically calling `GatewayState::reload_*`).
    ///
    /// On non-Unix platforms, this is a no-op.
    #[cfg(unix)]
    pub fn spawn_sighup_handler<F>(
        cancel: tokio_util::sync::CancellationToken,
        reload_fn: F,
    ) -> tokio::task::JoinHandle<()>
    where
        F: Fn() + Send + Sync + 'static,
    {
        tokio::spawn(async move {
            let mut signal = match tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::hangup(),
            ) {
                Ok(s) => s,
                Err(e) => {
                    warn!(%e, "failed to register SIGHUP handler");
                    return;
                }
            };

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = signal.recv() => {
                        info!("SIGHUP received — triggering hot-reload");
                        reload_fn();
                    }
                }
            }
        })
    }

    /// No-op SIGHUP handler on non-Unix platforms.
    #[cfg(not(unix))]
    pub fn spawn_sighup_handler<F>(
        _cancel: tokio_util::sync::CancellationToken,
        _reload_fn: F,
    ) -> tokio::task::JoinHandle<()>
    where
        F: Fn() + Send + Sync + 'static,
    {
        tokio::spawn(async {})
    }

    fn start_heartbeat(
        interval_secs: u64,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {
                        // Send WATCHDOG=1.
                        #[cfg(target_os = "linux")]
                        {
                            if let Ok(sock) = std::env::var("NOTIFY_SOCKET") {
                                if let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() {
                                    let _ = socket.send_to(b"WATCHDOG=1\n", &sock);
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    /// Execute the 6-phase graceful shutdown protocol.
    ///
    /// Each phase is executed in order. The `callbacks` provide the actual
    /// implementation for each phase (wired by the CLI to gateway internals).
    pub async fn graceful_shutdown(&self, callbacks: &dyn ShutdownCallbacks) {
        let start = Instant::now();
        info!("initiating graceful shutdown");

        self.notify_status("shutting down: stop accepting");
        for phase in ShutdownPhase::all() {
            let phase_start = Instant::now();
            info!(phase = phase.label(), "shutdown phase started");

            let timeout = match phase {
                ShutdownPhase::DrainInFlight => {
                    std::time::Duration::from_secs(self.config.drain_timeout_secs)
                }
                _ => std::time::Duration::from_secs(10),
            };

            let result = tokio::time::timeout(timeout, callbacks.execute_phase(*phase)).await;

            match result {
                Ok(Ok(())) => {
                    info!(
                        phase = phase.label(),
                        elapsed_ms = phase_start.elapsed().as_millis() as u64,
                        "shutdown phase completed"
                    );
                }
                Ok(Err(e)) => {
                    warn!(
                        phase = phase.label(),
                        error = %e,
                        "shutdown phase failed, continuing"
                    );
                }
                Err(_) => {
                    warn!(
                        phase = phase.label(),
                        timeout_secs = timeout.as_secs(),
                        "shutdown phase timed out, continuing"
                    );
                }
            }

            self.notify_status(&format!("shutting down: {}", phase.label()));
        }

        // Release PID file.
        if let Err(e) = self.release_pid() {
            warn!(%e, "failed to release PID file");
        }

        info!(
            total_ms = start.elapsed().as_millis() as u64,
            "graceful shutdown complete"
        );
    }

    /// Gateway port for health checking.
    pub fn port(&self) -> u16 {
        self.config.port
    }

    /// How long the daemon has been running.
    pub fn uptime(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }

    /// Access the daemon config.
    pub fn config(&self) -> &DaemonConfig {
        &self.config
    }
}

/// Trait for shutdown phase implementation callbacks.
///
/// The CLI wires these to the actual gateway state operations:
/// - `StopAccepting` → close the HTTP listener
/// - `DrainInFlight` → wait for active agent runs to complete
/// - `CheckpointSessions` → save session state via CheckpointStore
/// - `FlushEventBus` → drain EventBus WFQ to DeadLetterQueue
/// - `PersistCronState` → save cron next-fire timestamps
/// - `CloseStorage` → SochDB fsync + close
#[async_trait::async_trait]
pub trait ShutdownCallbacks: Send + Sync {
    async fn execute_phase(&self, phase: ShutdownPhase) -> Result<(), String>;
}

/// No-op shutdown callbacks for testing.
pub struct NoopShutdownCallbacks;

#[async_trait::async_trait]
impl ShutdownCallbacks for NoopShutdownCallbacks {
    async fn execute_phase(&self, phase: ShutdownPhase) -> Result<(), String> {
        info!(phase = phase.label(), "no-op shutdown phase");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_phases_ordered() {
        let phases = ShutdownPhase::all();
        assert_eq!(phases.len(), 6);
        assert_eq!(phases[0], ShutdownPhase::StopAccepting);
        assert_eq!(phases[5], ShutdownPhase::CloseStorage);
    }

    #[test]
    fn default_config() {
        let config = DaemonConfig::default();
        assert_eq!(config.port, 18789);
        assert_eq!(config.drain_timeout_secs, 30);
        assert!(config.pid_path.ends_with("clawdesk.pid"));
    }

    #[tokio::test]
    async fn noop_shutdown() {
        let config = DaemonConfig::default();
        let runner = DaemonRunner::new(config);
        let callbacks = NoopShutdownCallbacks;
        runner.graceful_shutdown(&callbacks).await;
        // Should complete without error.
    }

    #[test]
    fn phase_labels() {
        for phase in ShutdownPhase::all() {
            assert!(!phase.label().is_empty());
        }
    }
}
