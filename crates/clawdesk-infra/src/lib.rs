//! Infrastructure crate — outbound dispatch queue, TLS, update checker, voice wake, metrics,
//! idle detection, log rotation, encrypted backup, and git sync.

pub mod backup;
pub mod circuit_breaker;
pub mod clipboard;
pub mod daemon;
pub mod dispatch;
pub mod git_sync;
pub mod idle;
pub mod log_rotation;
pub mod metrics;
pub mod notifications;
pub mod notify_overlay;
pub mod registry_client;
pub mod resource_monitor;
pub mod retry;
pub mod task_scope;
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
pub use voice_wake::{VoiceWakeRuntime, WakeRuntimeState, PttConfig, AudioStreamListener, PttMonitor};
pub use notifications::{Notification, NotificationManager, NotificationPriority, NotificationCategory};
pub use notify_overlay::{OverlayController, OverlayConfig, OverlayNotification, OverlayEvent, OverlayPriority};
pub use clipboard::{ClipboardManager, ClipboardEntry, ClipboardConfig};
pub use circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerRegistry, CircuitState as InfraCircuitState,
    CircuitStatus, DegradationStrategy, DependencyKind,
};
pub use daemon::{DaemonManager, DaemonConfig, DaemonState, HealthStatus};
pub use task_scope::{TaskScope, spawn_traced};
