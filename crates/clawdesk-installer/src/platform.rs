//! Platform-specific installer operations.
//!
//! Handles platform detection, data directory creation, and OS integration
//! (launchd/systemd service registration, PATH configuration).

use crate::InstallerError;
use std::path::PathBuf;
use tracing::info;

/// Platform-specific paths and configuration.
#[derive(Debug, Clone)]
pub struct PlatformPaths {
    /// Application data directory (~/.clawdesk or platform equivalent)
    pub data_dir: PathBuf,
    /// Configuration directory
    pub config_dir: PathBuf,
    /// Log directory
    pub log_dir: PathBuf,
    /// Model storage directory
    pub models_dir: PathBuf,
    /// Skills directory
    pub skills_dir: PathBuf,
}

impl PlatformPaths {
    /// Detect platform-appropriate paths.
    pub fn detect() -> Self {
        #[cfg(target_os = "macos")]
        {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
            let data = home.join(".clawdesk");
            Self {
                config_dir: data.join("config"),
                log_dir: data.join("logs"),
                models_dir: data.join("models"),
                skills_dir: data.join("skills"),
                data_dir: data,
            }
        }

        #[cfg(target_os = "linux")]
        {
            let data = dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("clawdesk");
            let config = dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("clawdesk");
            Self {
                data_dir: data.clone(),
                config_dir: config,
                log_dir: data.join("logs"),
                models_dir: data.join("models"),
                skills_dir: data.join("skills"),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let data = dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"))
                .join("ClawDesk");
            Self {
                config_dir: data.join("config"),
                log_dir: data.join("logs"),
                models_dir: data.join("models"),
                skills_dir: data.join("skills"),
                data_dir: data,
            }
        }
    }

    /// Create all required directories.
    pub fn ensure_dirs(&self) -> Result<(), InstallerError> {
        for dir in [&self.data_dir, &self.config_dir, &self.log_dir,
                     &self.models_dir, &self.skills_dir] {
            std::fs::create_dir_all(dir)?;
        }
        info!(data_dir = %self.data_dir.display(), "platform directories created");
        Ok(())
    }
}

/// Detected platform characteristics.
#[derive(Debug, Clone)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
    pub paths: PlatformPaths,
}

impl PlatformInfo {
    pub fn detect() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            paths: PlatformPaths::detect(),
        }
    }
}
