//! Update checker — checks for new ClawDesk versions and notifies the user.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Version info from the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
    pub release_notes_url: Option<String>,
    pub checked_at: chrono::DateTime<chrono::Utc>,
}

/// Update check configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// Whether to check for updates automatically.
    pub auto_check: bool,
    /// How often to check (seconds).
    pub check_interval_secs: u64,
    /// URL to check for the latest version.
    pub registry_url: String,
    /// Current version string.
    pub current_version: String,
    /// Whether to include pre-release versions.
    pub include_prerelease: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            auto_check: true,
            check_interval_secs: 86_400, // Daily.
            registry_url: "https://registry.npmjs.org/openclaw/latest".to_string(),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            include_prerelease: false,
        }
    }
}

/// Checks for new versions of ClawDesk.
pub struct UpdateChecker {
    config: UpdateConfig,
    client: reqwest::Client,
    last_check: std::sync::Mutex<Option<VersionInfo>>,
}

impl UpdateChecker {
    /// Create with a dedicated reqwest client.
    pub fn new(config: UpdateConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self::with_client(config, client)
    }

    /// Create with a shared reqwest client (avoids per-instance connection pools).
    pub fn with_client(config: UpdateConfig, client: reqwest::Client) -> Self {
        Self {
            config,
            client,
            last_check: std::sync::Mutex::new(None),
        }
    }

    /// Perform a one-shot version check.
    pub async fn check_now(&self) -> Result<VersionInfo, String> {
        debug!(
            url = %self.config.registry_url,
            "checking for updates"
        );

        let resp = self
            .client
            .get(&self.config.registry_url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("registry returned {}", resp.status()));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("parse failed: {e}"))?;

        let latest = body["version"]
            .as_str()
            .unwrap_or(&self.config.current_version)
            .to_string();

        let update_available = Self::is_newer(&self.config.current_version, &latest);

        let info = VersionInfo {
            current: self.config.current_version.clone(),
            latest,
            update_available,
            release_notes_url: body["homepage"].as_str().map(String::from),
            checked_at: chrono::Utc::now(),
        };

        if info.update_available {
            info!(
                current = %info.current,
                latest = %info.latest,
                "update available"
            );
        } else {
            debug!("running latest version");
        }

        *self.last_check.lock().unwrap() = Some(info.clone());
        Ok(info)
    }

    /// Get the last check result without performing a new check.
    pub fn last_check(&self) -> Option<VersionInfo> {
        self.last_check.lock().unwrap().clone()
    }

    /// Simple semver comparison (major.minor.patch).
    fn is_newer(current: &str, latest: &str) -> bool {
        let parse = |v: &str| -> (u32, u32, u32) {
            let parts: Vec<&str> = v.trim_start_matches('v').split('.').collect();
            (
                parts.first().and_then(|p| p.parse().ok()).unwrap_or(0),
                parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0),
                parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0),
            )
        };
        let c = parse(current);
        let l = parse(latest);
        l > c
    }

    /// Run periodic update checks.
    pub async fn run_loop(&self, cancel: tokio_util::sync::CancellationToken) {
        if !self.config.auto_check {
            debug!("automatic update checks disabled");
            return;
        }

        let interval = Duration::from_secs(self.config.check_interval_secs);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("update checker shutting down");
                    break;
                }
                _ = tokio::time::sleep(interval) => {
                    match self.check_now().await {
                        Ok(info) => {
                            if info.update_available {
                                info!(latest = %info.latest, "new version available");
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "update check failed");
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_comparison() {
        assert!(UpdateChecker::is_newer("1.0.0", "1.0.1"));
        assert!(UpdateChecker::is_newer("1.0.0", "1.1.0"));
        assert!(UpdateChecker::is_newer("1.0.0", "2.0.0"));
        assert!(!UpdateChecker::is_newer("1.0.1", "1.0.0"));
        assert!(!UpdateChecker::is_newer("1.0.0", "1.0.0"));
        assert!(UpdateChecker::is_newer("v0.1.0", "v0.2.0"));
    }

    #[test]
    fn test_default_config() {
        let config = UpdateConfig::default();
        assert!(config.auto_check);
        assert_eq!(config.check_interval_secs, 86_400);
        assert!(!config.include_prerelease);
    }
}
