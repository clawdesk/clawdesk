//! # clawdesk-cron
//!
//! Cron scheduling system — schedule parsing, isolated execution,
//! overlap prevention, and delivery queue.
//!
//! ## Architecture
//! - **ScheduleParser**: Parses cron expressions into next-run times
//! - **CronExecutor**: Runs tasks in isolated contexts with timeout
//! - **CronManager**: Manages task lifecycle, overlap prevention, and delivery

pub mod executor;
pub mod heartbeat;
pub mod parser;
pub mod manager;
pub mod proactive;
pub mod webhook;

pub use executor::CronExecutor;
pub use heartbeat::{Heartbeat, HeartbeatConfig, HeartbeatDecision, HEARTBEAT_SKIP};
pub use parser::{parse_cron_expression, matches_cron};
pub use manager::CronManager;
pub use proactive::{ProactiveOrchestrator, NotificationType, SelectedNotification, SystemContext};
