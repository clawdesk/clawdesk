//! # clawdesk-daemon — Platform-Native Daemon Lifecycle Manager
//!
//! Cross-platform service management for the ClawDesk gateway. Supports:
//!
//! - **macOS**: `launchd` via `~/Library/LaunchAgents/dev.clawdesk.gateway.plist`
//! - **Linux**: `systemd` via `~/.config/systemd/user/clawdesk.service`
//! - **Windows**: Windows Service via SCM (Service Control Manager)
//!
//! ## CLI Integration
//!
//! ```text
//! clawdesk daemon run        — Run in daemon mode (used by service manager)
//! clawdesk daemon install    — Install platform-native service
//! clawdesk daemon uninstall  — Remove platform-native service
//! clawdesk daemon start      — Start the installed service
//! clawdesk daemon stop       — Stop the installed service
//! clawdesk daemon restart    — Restart the installed service
//! clawdesk daemon status     — Show daemon status + PID + uptime
//! clawdesk daemon logs       — Tail daemon logs
//! ```
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────┐           ┌────────────────┐
//! │  DaemonCtl  │──install──│ ServiceBackend  │
//! │  (CLI ops)  │──start────│ (launchd/       │
//! │             │──stop─────│  systemd/       │
//! │             │──status───│  winservice)    │
//! └─────────────┘           └────────────────┘
//!       │
//!       │ daemon run
//!       ▼
//! ┌─────────────────┐
//! │  DaemonRunner   │
//! │  - PID file     │
//! │  - sd_notify    │
//! │  - health probe │
//! │  - graceful     │
//! │    shutdown      │
//! └─────────────────┘
//! ```

mod health;
mod pid;
mod platform;
mod runner;

pub use health::{HealthCheck, HealthStatus};
pub use pid::PidFile;
pub use platform::{DaemonCtl, ServiceAction, ServiceStatus as DaemonStatus};
pub use runner::{DaemonRunner, DaemonConfig, ShutdownPhase, ShutdownCallbacks, NoopShutdownCallbacks};

/// Errors specific to daemon lifecycle management.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("service already installed")]
    AlreadyInstalled,

    #[error("service not installed")]
    NotInstalled,

    #[error("daemon already running (PID {pid})")]
    AlreadyRunning { pid: u32 },

    #[error("daemon not running")]
    NotRunning,

    #[error("PID file error: {detail}")]
    PidFile { detail: String },

    #[error("platform error: {detail}")]
    Platform { detail: String },

    #[error("health check failed: {detail}")]
    HealthCheckFailed { detail: String },

    #[error("shutdown timeout ({secs}s elapsed)")]
    ShutdownTimeout { secs: u64 },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
