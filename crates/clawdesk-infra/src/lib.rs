//! Infrastructure crate — outbound dispatch queue, TLS, update checker, voice wake, metrics, idle detection, log rotation.

pub mod clipboard;
pub mod daemon;
pub mod dispatch;
pub mod idle;
pub mod log_rotation;
pub mod metrics;
pub mod notifications;
pub mod registry_client;
pub mod tls;
pub mod updater;
pub mod voice_wake;

pub use dispatch::{DispatchQueue, OutboundItem, OutboundPriority};
pub use idle::{IdleConfig, IdleDetector};
pub use log_rotation::{LogRotationConfig, RotatingFileWriter};
pub use metrics::{MetricsCollector, MetricsSnapshot};
pub use tls::TlsManager;
pub use updater::UpdateChecker;
pub use voice_wake::VoiceWakeManager;
pub use notifications::{Notification, NotificationManager, NotificationPriority, NotificationCategory};
pub use clipboard::{ClipboardManager, ClipboardEntry, ClipboardConfig};
pub use daemon::{DaemonManager, DaemonConfig, DaemonState, HealthStatus};
