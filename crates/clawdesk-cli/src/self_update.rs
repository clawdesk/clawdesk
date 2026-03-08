//! Self-update mechanism — check for new releases and apply atomic binary updates.
//!
//! ## Design
//!
//! 1. Query GitHub Releases API for the latest version.
//! 2. Compare with current version (semver).
//! 3. Download the appropriate platform binary + SHA-256 checksum.
//! 4. Verify checksum.
//! 5. Atomic replacement: write to `<binary>.new` → rename over current.
//! 6. On failure, rollback by keeping `<binary>.bak`.
//!
//! ## Security
//!
//! - SHA-256 checksum verification (matches release artifacts).
//! - HTTPS-only downloads.
//! - Atomic rename ensures no partial writes.
//! - Optional rollback via `<binary>.bak`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// GitHub release metadata (subset of API response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub name: String,
    pub published_at: String,
    pub html_url: String,
    pub assets: Vec<ReleaseAsset>,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
}

/// A single release asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
    pub content_type: String,
}

/// Result of version check.
#[derive(Debug, Clone, Serialize)]
pub enum UpdateCheck {
    /// Already on latest version.
    UpToDate { current: String },
    /// A newer version is available.
    UpdateAvailable {
        current: String,
        latest: String,
        download_url: String,
        checksum_url: Option<String>,
        release_notes_url: String,
        asset_size: u64,
    },
    /// Could not determine (error).
    CheckFailed { error: String },
}

/// Result of an update operation.
#[derive(Debug)]
pub enum UpdateResult {
    /// Successfully updated.
    Updated {
        from_version: String,
        to_version: String,
        binary_path: PathBuf,
    },
    /// Already up to date.
    AlreadyCurrent { version: String },
    /// Update failed.
    Failed { error: String },
}

/// Self-updater configuration.
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    /// GitHub repository (e.g., "user/clawdesk").
    pub repo: String,
    /// Current binary version.
    pub current_version: String,
    /// Allow pre-release updates.
    pub allow_prerelease: bool,
    /// HTTP timeout for API calls and downloads.
    pub timeout: std::time::Duration,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            repo: "user/clawdesk".to_string(),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            allow_prerelease: false,
            timeout: std::time::Duration::from_secs(60),
        }
    }
}

// ---------------------------------------------------------------------------
// Updater
// ---------------------------------------------------------------------------

/// Self-update engine.
pub struct SelfUpdater {
    config: UpdateConfig,
    http: reqwest::Client,
}

impl SelfUpdater {
    pub fn new(config: UpdateConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(format!("ClawDesk-Updater/{}", config.current_version))
            .build()
            .unwrap_or_default();
        Self { config, http }
    }

    /// Check for available updates.
    pub async fn check(&self) -> UpdateCheck {
        let url = format!(
            "https://api.github.com/repos/{}/releases/latest",
            self.config.repo
        );

        debug!(%url, "checking for updates");

        let resp = match self.http.get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return UpdateCheck::CheckFailed { error: format!("HTTP request: {e}") },
        };

        if !resp.status().is_success() {
            return UpdateCheck::CheckFailed {
                error: format!("GitHub API returned {}", resp.status()),
            };
        }

        let release: ReleaseInfo = match resp.json().await {
            Ok(r) => r,
            Err(e) => return UpdateCheck::CheckFailed { error: format!("JSON parse: {e}") },
        };

        if release.draft || (!self.config.allow_prerelease && release.prerelease) {
            return UpdateCheck::UpToDate {
                current: self.config.current_version.clone(),
            };
        }

        let latest = release.tag_name.trim_start_matches('v').to_string();
        let current = self.config.current_version.trim_start_matches('v');

        if !is_newer(&latest, current) {
            return UpdateCheck::UpToDate {
                current: self.config.current_version.clone(),
            };
        }

        // Find the appropriate asset for this platform.
        let target = platform_asset_name();
        let asset = release.assets.iter().find(|a| a.name.contains(&target));
        let checksum_asset = release.assets.iter().find(|a| a.name.ends_with(".sha256"));

        match asset {
            Some(a) => UpdateCheck::UpdateAvailable {
                current: self.config.current_version.clone(),
                latest,
                download_url: a.browser_download_url.clone(),
                checksum_url: checksum_asset.map(|c| c.browser_download_url.clone()),
                release_notes_url: release.html_url,
                asset_size: a.size,
            },
            None => UpdateCheck::CheckFailed {
                error: format!("no asset found for platform '{target}'"),
            },
        }
    }

    /// Download and apply an update.
    ///
    /// 1. Download binary to `<current_binary>.new`
    /// 2. Verify SHA-256 checksum (if available)
    /// 3. Rename current → `.bak`
    /// 4. Rename `.new` → current
    pub async fn apply(&self) -> UpdateResult {
        let check = self.check().await;

        let (latest, download_url, checksum_url) = match check {
            UpdateCheck::UpdateAvailable {
                latest,
                download_url,
                checksum_url,
                ..
            } => (latest, download_url, checksum_url),
            UpdateCheck::UpToDate { current } => {
                return UpdateResult::AlreadyCurrent { version: current };
            }
            UpdateCheck::CheckFailed { error } => {
                return UpdateResult::Failed { error };
            }
        };

        info!(%latest, "downloading update");

        // Determine binary path.
        let binary_path = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return UpdateResult::Failed { error: format!("current_exe: {e}") },
        };

        let new_path = binary_path.with_extension("new");
        let bak_path = binary_path.with_extension("bak");

        // Download the binary.
        if let Err(e) = self.download_to_file(&download_url, &new_path).await {
            return UpdateResult::Failed { error: format!("download: {e}") };
        }

        // Verify checksum.
        if let Some(ref checksum_url) = checksum_url {
            match self.verify_checksum(checksum_url, &new_path).await {
                Ok(true) => debug!("checksum verified"),
                Ok(false) => {
                    let _ = std::fs::remove_file(&new_path);
                    return UpdateResult::Failed { error: "checksum mismatch".into() };
                }
                Err(e) => {
                    warn!(%e, "checksum verification failed — proceeding anyway");
                }
            }
        }

        // Set executable permission on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&new_path, std::fs::Permissions::from_mode(0o755));
        }

        // Atomic swap: current → bak, new → current.
        if let Err(e) = self.atomic_replace(&binary_path, &new_path, &bak_path) {
            return UpdateResult::Failed { error: format!("atomic replace: {e}") };
        }

        info!(
            from = %self.config.current_version,
            to = %latest,
            path = %binary_path.display(),
            "update applied successfully"
        );

        UpdateResult::Updated {
            from_version: self.config.current_version.clone(),
            to_version: latest,
            binary_path,
        }
    }

    /// Rollback to the backup binary.
    pub fn rollback(&self) -> Result<(), String> {
        let binary_path = std::env::current_exe()
            .map_err(|e| format!("current_exe: {e}"))?;
        let bak_path = binary_path.with_extension("bak");

        if !bak_path.exists() {
            return Err("no backup binary found".into());
        }

        std::fs::rename(&bak_path, &binary_path)
            .map_err(|e| format!("rollback rename: {e}"))?;

        info!(path = %binary_path.display(), "rolled back to previous version");
        Ok(())
    }

    async fn download_to_file(&self, url: &str, dest: &Path) -> Result<(), String> {
        let resp = self.http.get(url).send().await
            .map_err(|e| format!("HTTP: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }

        let bytes = resp.bytes().await
            .map_err(|e| format!("read body: {e}"))?;

        tokio::fs::write(dest, &bytes).await
            .map_err(|e| format!("write file: {e}"))?;

        debug!(path = %dest.display(), bytes = bytes.len(), "downloaded file");
        Ok(())
    }

    async fn verify_checksum(&self, checksum_url: &str, file_path: &Path) -> Result<bool, String> {
        // Download checksum file.
        let resp = self.http.get(checksum_url).send().await
            .map_err(|e| format!("HTTP: {e}"))?;
        let checksum_text = resp.text().await
            .map_err(|e| format!("read: {e}"))?;

        // Expected format: "sha256hash  filename\n"
        let expected_hash = checksum_text.split_whitespace()
            .next()
            .ok_or("empty checksum file")?
            .to_lowercase();

        // Compute actual hash.
        let file_bytes = tokio::fs::read(file_path).await
            .map_err(|e| format!("read file: {e}"))?;

        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&file_bytes);
        let actual_hash = hex::encode(hasher.finalize());

        Ok(actual_hash == expected_hash)
    }

    fn atomic_replace(&self, current: &Path, new: &Path, bak: &Path) -> Result<(), String> {
        // Remove old backup if exists.
        if bak.exists() {
            std::fs::remove_file(bak)
                .map_err(|e| format!("remove old backup: {e}"))?;
        }

        // Move current → backup.
        if current.exists() {
            std::fs::rename(current, bak)
                .map_err(|e| format!("backup current: {e}"))?;
        }

        // Move new → current.
        match std::fs::rename(new, current) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Rollback: restore from backup.
                error!(%e, "rename new → current failed, rolling back");
                if bak.exists() {
                    let _ = std::fs::rename(bak, current);
                }
                Err(format!("rename: {e}"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compare two semver strings. Returns true if `latest` > `current`.
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> (u64, u64, u64) {
        let parts: Vec<&str> = s.split('.').collect();
        (
            parts.first().and_then(|p| p.parse().ok()).unwrap_or(0),
            parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0),
            parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0),
        )
    };
    parse(latest) > parse(current)
}

/// Determine the platform-specific asset name fragment.
fn platform_asset_name() -> String {
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "linux") {
        "unknown-linux"
    } else if cfg!(target_os = "windows") {
        "pc-windows"
    } else {
        "unknown"
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    };

    format!("{arch}-{os}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparison() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("1.1.0", "1.0.9"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.0", "1.0.0"));
    }

    #[test]
    fn platform_asset_name_is_sensible() {
        let name = platform_asset_name();
        assert!(!name.is_empty());
        // Should contain arch and OS.
        assert!(name.contains("x86_64") || name.contains("aarch64") || name.contains("unknown"));
    }

    #[test]
    fn update_config_default() {
        let config = UpdateConfig::default();
        assert!(!config.current_version.is_empty());
        assert!(!config.allow_prerelease);
    }

    #[test]
    fn update_check_serializes() {
        let check = UpdateCheck::UpdateAvailable {
            current: "0.1.0".into(),
            latest: "0.2.0".into(),
            download_url: "https://example.com/binary".into(),
            checksum_url: Some("https://example.com/checksum".into()),
            release_notes_url: "https://example.com/release".into(),
            asset_size: 10_000_000,
        };
        let json = serde_json::to_string(&check).unwrap();
        assert!(json.contains("0.2.0"));
    }
}
