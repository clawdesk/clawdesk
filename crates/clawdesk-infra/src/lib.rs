//! Infrastructure crate — outbound dispatch queue, TLS, update checker, voice wake, metrics,
//! idle detection, log rotation, encrypted backup, and git sync.

pub mod backup;
pub mod clipboard;
pub mod daemon;
pub mod dispatch;
pub mod git_sync;
pub mod idle;
pub mod log_rotation;
pub mod metrics;
pub mod notifications;
pub mod registry_client;
pub mod retry;
pub mod tls;
pub mod updater;
pub mod voice_wake;

pub use backup::{BackupConfig, BackupManager, BackupRecord, BackupType, BackupStatus, RetentionPolicy};
pub use dispatch::{DispatchQueue, OutboundItem, OutboundPriority};
pub use retry::{
    RetryConfig, RetryClassifier, RetryDecision, RetryRunner, JitterStrategy,
    retry_async, compute_delay, classify_http_status, parse_retry_after,
    telegram_retry_config, discord_retry_config, provider_retry_config,
    AlwaysRetry, FnClassifier,
};
pub use git_sync::{GitSyncConfig, GitSyncManager, SyncState, SyncRecord, SyncOperation, ConflictStrategy};
pub use idle::{IdleConfig, IdleDetector};
pub use log_rotation::{LogRotationConfig, RotatingFileWriter};
pub use metrics::{MetricsCollector, MetricsSnapshot};
pub use tls::TlsManager;
pub use updater::UpdateChecker;
pub use voice_wake::VoiceWakeManager;
pub use notifications::{Notification, NotificationManager, NotificationPriority, NotificationCategory};
pub use clipboard::{ClipboardManager, ClipboardEntry, ClipboardConfig};
pub use daemon::{DaemonManager, DaemonConfig, DaemonState, HealthStatus};
