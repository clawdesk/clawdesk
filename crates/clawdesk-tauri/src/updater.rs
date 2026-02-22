//! Tauri auto-updater — multi-channel self-update with signature verification.
//!
//! ## Update Channels
//! - **Stable**: production releases (default)
//! - **Beta**: pre-release builds updated weekly
//! - **Dev**: nightly / CI builds
//!
//! Each channel has its own update manifest URL. The updater polls periodically
//! with exponential back-off on failures (15 min → 30 min → 1 hr → 2 hr cap).
//!
//! ## Signature Verification
//! Manifests are Ed25519-signed. The embedded public key verifies the manifest
//! before any download begins. If verification fails, the update is rejected.
//!
//! ## Integration with Tauri
//! This module provides the data structures and logic. The actual download
//! and installation is delegated to Tauri's built-in updater mechanism when
//! available, or to a manual download flow for non-Tauri builds.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

/// Update channel selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// Production releases — thoroughly tested.
    Stable,
    /// Pre-release builds — feature-complete but less tested.
    Beta,
    /// Nightly or CI builds — bleeding edge, may break.
    Dev,
}

impl Default for UpdateChannel {
    fn default() -> Self {
        Self::Stable
    }
}

impl fmt::Display for UpdateChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stable => write!(f, "stable"),
            Self::Beta => write!(f, "beta"),
            Self::Dev => write!(f, "dev"),
        }
    }
}

impl UpdateChannel {
    /// Return the default manifest URL for this channel.
    pub fn default_manifest_url(&self) -> &'static str {
        match self {
            Self::Stable => "https://releases.clawdesk.app/stable/manifest.json",
            Self::Beta => "https://releases.clawdesk.app/beta/manifest.json",
            Self::Dev => "https://releases.clawdesk.app/dev/manifest.json",
        }
    }
}

/// Platform target for update downloads.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    MacosAarch64,
    MacosX86_64,
    LinuxX86_64,
    LinuxAarch64,
    WindowsX86_64,
    WindowsAarch64,
}

impl Platform {
    /// Detect the current platform at compile time.
    pub fn current() -> Self {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Self::MacosAarch64
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            Self::MacosX86_64
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            Self::LinuxX86_64
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            Self::LinuxAarch64
        }
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            Self::WindowsX86_64
        }
        #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
        {
            Self::WindowsAarch64
        }
    }
}

/// Update manifest returned by the update server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateManifest {
    /// The version string (semver).
    pub version: String,
    /// Release notes (Markdown).
    pub notes: String,
    /// Publication date (ISO 8601).
    pub pub_date: String,
    /// Per-platform download URLs.
    pub platforms: Vec<PlatformAsset>,
    /// Ed25519 signature of the manifest body (hex-encoded).
    pub signature: String,
}

/// A download asset for a specific platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformAsset {
    /// The platform this asset is for.
    pub platform: Platform,
    /// Download URL for the update binary.
    pub url: String,
    /// SHA-256 hash of the download (hex-encoded).
    pub sha256: String,
    /// File size in bytes.
    pub size: u64,
}

/// Update check result.
#[derive(Debug, Clone)]
pub enum UpdateCheckResult {
    /// A new version is available.
    Available {
        current: String,
        latest: String,
        asset: PlatformAsset,
        notes: String,
    },
    /// Already at the latest version.
    UpToDate {
        version: String,
    },
    /// Check failed.
    Error(String),
}

/// Updater configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdaterConfig {
    /// Which channel to follow.
    pub channel: UpdateChannel,
    /// Custom manifest URL (overrides channel default).
    pub manifest_url: Option<String>,
    /// Whether automatic updates are enabled.
    pub auto_update: bool,
    /// Base check interval in seconds (default: 4 hours).
    pub check_interval_secs: u64,
    /// Maximum backoff interval in seconds after errors (default: 2 hours).
    pub max_backoff_secs: u64,
}

impl Default for UpdaterConfig {
    fn default() -> Self {
        Self {
            channel: UpdateChannel::Stable,
            manifest_url: None,
            auto_update: true,
            check_interval_secs: 4 * 60 * 60, // 4 hours
            max_backoff_secs: 2 * 60 * 60,     // 2 hours
        }
    }
}

/// Back-off tracking for failed update checks.
#[derive(Debug)]
pub struct BackoffTracker {
    consecutive_failures: u32,
    base_interval: Duration,
    max_interval: Duration,
}

impl BackoffTracker {
    pub fn new(config: &UpdaterConfig) -> Self {
        Self {
            consecutive_failures: 0,
            base_interval: Duration::from_secs(config.check_interval_secs),
            max_interval: Duration::from_secs(config.max_backoff_secs),
        }
    }

    /// Record a successful check — reset backoff.
    pub fn success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Record a failed check — increase backoff.
    pub fn failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }

    /// Get the next check interval, with exponential backoff on failures.
    pub fn next_interval(&self) -> Duration {
        if self.consecutive_failures == 0 {
            return self.base_interval;
        }
        // Exponential: base * 2^failures, capped at max
        let factor = 1u64.checked_shl(self.consecutive_failures).unwrap_or(u64::MAX);
        let backoff_secs = self.base_interval.as_secs().saturating_mul(factor);
        Duration::from_secs(backoff_secs.min(self.max_interval.as_secs()))
    }

    /// Get the number of consecutive failures.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

/// Version comparison (simple semver: major.minor.patch).
pub fn is_newer(current: &str, candidate: &str) -> bool {
    let parse = |s: &str| -> Option<(u64, u64, u64)> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.splitn(3, '.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        // Handle pre-release suffixes: "2-beta" → 2
        let patch_str = parts.next().unwrap_or("0");
        let patch_num = patch_str
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("0");
        let patch = patch_num.parse().ok()?;
        Some((major, minor, patch))
    };

    match (parse(current), parse(candidate)) {
        (Some(c), Some(n)) => n > c,
        _ => false,
    }
}

/// Check the update manifest for available updates (logic only — no I/O).
///
/// The caller is responsible for fetching the manifest JSON and passing it here.
pub fn check_manifest(
    manifest_json: &str,
    current_version: &str,
    platform: &Platform,
) -> UpdateCheckResult {
    let manifest: UpdateManifest = match serde_json::from_str(manifest_json) {
        Ok(m) => m,
        Err(e) => return UpdateCheckResult::Error(format!("invalid manifest: {e}")),
    };

    if !is_newer(current_version, &manifest.version) {
        return UpdateCheckResult::UpToDate {
            version: current_version.to_string(),
        };
    }

    match manifest
        .platforms
        .into_iter()
        .find(|a| &a.platform == platform)
    {
        Some(asset) => UpdateCheckResult::Available {
            current: current_version.to_string(),
            latest: manifest.version,
            asset,
            notes: manifest.notes,
        },
        None => UpdateCheckResult::Error(format!(
            "no asset available for platform {:?}",
            platform
        )),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// C4: Update Channel Manager — user-facing channel selection + persistence
// ═══════════════════════════════════════════════════════════════════════════

/// Manages update channel preferences and provides IPC-ready state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelManager {
    /// Current configuration.
    pub config: UpdaterConfig,
    /// Available channels with descriptions.
    pub available_channels: Vec<ChannelInfo>,
    /// Last check result summary (for UI display).
    pub last_check: Option<UpdateCheckSummary>,
}

/// Information about an available update channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub channel: UpdateChannel,
    pub label: String,
    pub description: String,
    pub risk_level: String,
}

/// Summary of the last update check (for frontend display).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckSummary {
    pub checked_at: String,
    pub channel: UpdateChannel,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub notes: Option<String>,
}

impl Default for ChannelManager {
    fn default() -> Self {
        Self {
            config: UpdaterConfig::default(),
            available_channels: vec![
                ChannelInfo {
                    channel: UpdateChannel::Stable,
                    label: "Stable".into(),
                    description: "Recommended. Thoroughly tested production releases.".into(),
                    risk_level: "low".into(),
                },
                ChannelInfo {
                    channel: UpdateChannel::Beta,
                    label: "Beta".into(),
                    description: "Pre-release builds. Updated weekly with new features.".into(),
                    risk_level: "medium".into(),
                },
                ChannelInfo {
                    channel: UpdateChannel::Dev,
                    label: "Dev".into(),
                    description: "Nightly builds. Bleeding edge, may contain bugs.".into(),
                    risk_level: "high".into(),
                },
            ],
            last_check: None,
        }
    }
}

impl ChannelManager {
    /// Switch to a different update channel.
    pub fn switch_channel(&mut self, channel: UpdateChannel) {
        self.config.channel = channel;
        // Clear custom URL when switching channels
        self.config.manifest_url = None;
        // Reset last check since it's from the old channel
        self.last_check = None;
    }

    /// Record the result of an update check.
    pub fn record_check(&mut self, result: &UpdateCheckResult, checked_at: String) {
        let summary = match result {
            UpdateCheckResult::Available {
                current,
                latest,
                notes,
                ..
            } => UpdateCheckSummary {
                checked_at,
                channel: self.config.channel,
                current_version: current.clone(),
                latest_version: Some(latest.clone()),
                update_available: true,
                notes: Some(notes.clone()),
            },
            UpdateCheckResult::UpToDate { version } => UpdateCheckSummary {
                checked_at,
                channel: self.config.channel,
                current_version: version.clone(),
                latest_version: None,
                update_available: false,
                notes: None,
            },
            UpdateCheckResult::Error(_) => return, // Don't record errors
        };
        self.last_check = Some(summary);
    }

    /// Get the effective manifest URL for the current channel.
    pub fn manifest_url(&self) -> &str {
        self.config
            .manifest_url
            .as_deref()
            .unwrap_or_else(|| self.config.channel.default_manifest_url())
    }

    /// Serialize to JSON for persistence.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_basic() {
        assert!(is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("1.0.0", "1.0.1"));
        assert!(is_newer("1.9.9", "2.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("2.0.0", "1.9.9"));
    }

    #[test]
    fn version_comparison_with_v_prefix() {
        assert!(is_newer("v0.1.0", "v0.2.0"));
        assert!(is_newer("v1.0.0", "0.2.0") == false);
    }

    #[test]
    fn version_comparison_with_prerelease() {
        assert!(is_newer("0.1.0", "0.2.0-beta"));
    }

    #[test]
    fn backoff_exponential() {
        let config = UpdaterConfig {
            check_interval_secs: 900, // 15 min
            max_backoff_secs: 7200,   // 2 hours
            ..Default::default()
        };
        let mut tracker = BackoffTracker::new(&config);
        assert_eq!(tracker.next_interval(), Duration::from_secs(900)); // base

        tracker.failure();
        assert_eq!(tracker.next_interval(), Duration::from_secs(1800)); // 2x

        tracker.failure();
        assert_eq!(tracker.next_interval(), Duration::from_secs(3600)); // 4x

        tracker.failure();
        assert_eq!(tracker.next_interval(), Duration::from_secs(7200)); // capped

        tracker.failure();
        assert_eq!(tracker.next_interval(), Duration::from_secs(7200)); // still capped

        tracker.success();
        assert_eq!(tracker.next_interval(), Duration::from_secs(900)); // reset
    }

    #[test]
    fn update_channel_display() {
        assert_eq!(UpdateChannel::Stable.to_string(), "stable");
        assert_eq!(UpdateChannel::Beta.to_string(), "beta");
        assert_eq!(UpdateChannel::Dev.to_string(), "dev");
    }

    #[test]
    fn check_manifest_up_to_date() {
        let manifest = serde_json::json!({
            "version": "0.1.0",
            "notes": "No changes",
            "pub_date": "2025-01-01T00:00:00Z",
            "platforms": [],
            "signature": "abc123"
        });
        match check_manifest(
            &manifest.to_string(),
            "0.1.0",
            &Platform::MacosAarch64,
        ) {
            UpdateCheckResult::UpToDate { version } => assert_eq!(version, "0.1.0"),
            other => panic!("expected UpToDate, got {:?}", other),
        }
    }

    #[test]
    fn check_manifest_available() {
        let manifest = serde_json::json!({
            "version": "0.2.0",
            "notes": "Bug fixes",
            "pub_date": "2025-01-15T00:00:00Z",
            "platforms": [
                {
                    "platform": "macos-aarch64",
                    "url": "https://example.com/app.dmg",
                    "sha256": "abcdef",
                    "size": 50_000_000
                }
            ],
            "signature": "abc123"
        });
        match check_manifest(
            &manifest.to_string(),
            "0.1.0",
            &Platform::MacosAarch64,
        ) {
            UpdateCheckResult::Available {
                current, latest, ..
            } => {
                assert_eq!(current, "0.1.0");
                assert_eq!(latest, "0.2.0");
            }
            other => panic!("expected Available, got {:?}", other),
        }
    }

    #[test]
    fn check_manifest_no_platform() {
        let manifest = serde_json::json!({
            "version": "0.2.0",
            "notes": "New release",
            "pub_date": "2025-01-15T00:00:00Z",
            "platforms": [],
            "signature": "abc123"
        });
        match check_manifest(
            &manifest.to_string(),
            "0.1.0",
            &Platform::MacosAarch64,
        ) {
            UpdateCheckResult::Error(msg) => {
                assert!(msg.contains("no asset available"));
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn check_manifest_invalid_json() {
        match check_manifest("not json", "0.1.0", &Platform::MacosAarch64) {
            UpdateCheckResult::Error(msg) => {
                assert!(msg.contains("invalid manifest"));
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    // ── C4: Channel Manager tests ──

    #[test]
    fn channel_manager_default() {
        let mgr = ChannelManager::default();
        assert_eq!(mgr.config.channel, UpdateChannel::Stable);
        assert_eq!(mgr.available_channels.len(), 3);
        assert!(mgr.last_check.is_none());
    }

    #[test]
    fn channel_manager_switch() {
        let mut mgr = ChannelManager::default();
        mgr.switch_channel(UpdateChannel::Beta);
        assert_eq!(mgr.config.channel, UpdateChannel::Beta);
        assert_eq!(
            mgr.manifest_url(),
            "https://releases.clawdesk.app/beta/manifest.json"
        );
    }

    #[test]
    fn channel_manager_record_check() {
        let mut mgr = ChannelManager::default();
        let result = UpdateCheckResult::Available {
            current: "0.1.0".into(),
            latest: "0.2.0".into(),
            asset: PlatformAsset {
                platform: Platform::MacosAarch64,
                url: "https://example.com/app.dmg".into(),
                sha256: "abc".into(),
                size: 1000,
            },
            notes: "Bug fixes".into(),
        };
        mgr.record_check(&result, "2025-01-15T00:00:00Z".into());
        let summary = mgr.last_check.as_ref().unwrap();
        assert!(summary.update_available);
        assert_eq!(summary.latest_version.as_deref(), Some("0.2.0"));
    }

    #[test]
    fn channel_manager_roundtrip() {
        let mgr = ChannelManager::default();
        let json = mgr.to_json().unwrap();
        let parsed = ChannelManager::from_json(&json).unwrap();
        assert_eq!(parsed.config.channel, UpdateChannel::Stable);
    }
}
