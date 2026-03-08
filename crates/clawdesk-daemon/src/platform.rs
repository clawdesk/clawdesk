//! Platform-native service backend — install/start/stop/status for
//! launchd (macOS), systemd (Linux), and Windows Service.
//!
//! Each platform implementation generates its native service definition,
//! installs it to the correct location, and provides lifecycle control.

use crate::{DaemonError, HealthCheck};
use std::path::PathBuf;
use tracing::{info, warn};

/// Actions that the daemon controller can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
}

/// Current status of the daemon service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub installed: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub uptime_secs: Option<u64>,
    pub version: Option<String>,
    pub auto_start: bool,
}

use serde::{Deserialize, Serialize};

impl ServiceStatus {
    pub fn display(&self) -> String {
        let state = if self.running { "running" } else { "stopped" };
        let pid_str = self
            .pid
            .map(|p| format!(" (PID {})", p))
            .unwrap_or_default();
        let uptime = self
            .uptime_secs
            .map(|u| format!(", uptime {}s", u))
            .unwrap_or_default();
        let installed = if self.installed {
            "installed"
        } else {
            "not installed"
        };
        let auto = if self.auto_start {
            ", auto-start"
        } else {
            ""
        };

        format!(
            "ClawDesk daemon: {state}{pid_str}{uptime} [{installed}{auto}]"
        )
    }
}

/// Cross-platform daemon controller.
///
/// Dispatches to the correct platform backend based on `cfg(target_os)`.
pub struct DaemonCtl {
    binary_path: PathBuf,
    gateway_port: u16,
    log_dir: PathBuf,
    home_dir: PathBuf,
}

impl DaemonCtl {
    /// Create a new daemon controller.
    ///
    /// `binary_path` — absolute path to the `clawdesk` binary.
    /// `gateway_port` — port the gateway listens on (for health checks).
    pub fn new(binary_path: PathBuf, gateway_port: u16) -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());
        let home_dir = PathBuf::from(&home).join(".clawdesk");
        let log_dir = home_dir.join("logs");

        Self {
            binary_path,
            gateway_port,
            log_dir,
            home_dir,
        }
    }

    /// Install the platform-native service definition.
    pub async fn install(&self) -> Result<(), DaemonError> {
        std::fs::create_dir_all(&self.log_dir)?;

        #[cfg(target_os = "macos")]
        self.install_launchd()?;

        #[cfg(target_os = "linux")]
        self.install_systemd()?;

        #[cfg(target_os = "windows")]
        self.install_windows_service()?;

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        return Err(DaemonError::Platform {
            detail: "unsupported platform".into(),
        });

        info!("service installed successfully");
        Ok(())
    }

    /// Uninstall the platform-native service definition.
    pub async fn uninstall(&self) -> Result<(), DaemonError> {
        // Stop first if running.
        let _ = self.stop().await;

        #[cfg(target_os = "macos")]
        self.uninstall_launchd()?;

        #[cfg(target_os = "linux")]
        self.uninstall_systemd()?;

        #[cfg(target_os = "windows")]
        self.uninstall_windows_service()?;

        info!("service uninstalled");
        Ok(())
    }

    /// Start the installed service.
    pub async fn start(&self) -> Result<(), DaemonError> {
        #[cfg(target_os = "macos")]
        self.start_launchd()?;

        #[cfg(target_os = "linux")]
        self.start_systemd()?;

        #[cfg(target_os = "windows")]
        self.start_windows_service()?;

        // Wait for health check.
        let hc = HealthCheck::new(&format!("http://127.0.0.1:{}", self.gateway_port));
        match hc.wait_healthy(12, 2500).await {
            Ok(status) => {
                info!(version = %status.version, uptime = status.uptime_secs, "daemon started");
                Ok(())
            }
            Err(e) => {
                warn!(%e, "daemon started but health check failed");
                Ok(()) // Service started, just health probe failed.
            }
        }
    }

    /// Stop the running service.
    pub async fn stop(&self) -> Result<(), DaemonError> {
        #[cfg(target_os = "macos")]
        self.stop_launchd()?;

        #[cfg(target_os = "linux")]
        self.stop_systemd()?;

        #[cfg(target_os = "windows")]
        self.stop_windows_service()?;

        info!("daemon stopped");
        Ok(())
    }

    /// Restart the service (stop + start).
    pub async fn restart(&self) -> Result<(), DaemonError> {
        self.stop().await?;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        self.start().await
    }

    /// Query current service status.
    pub async fn status(&self) -> Result<ServiceStatus, DaemonError> {
        let installed = self.is_installed();

        // Check for PID file.
        let pid_file = crate::PidFile::new(self.home_dir.join("clawdesk.pid"));
        let pid = pid_file.running_pid()?;
        let running = pid.is_some();

        // Probe health if running.
        let (uptime_secs, version) = if running {
            let hc = HealthCheck::new(&format!("http://127.0.0.1:{}", self.gateway_port));
            match hc.probe().await {
                Ok(status) => (Some(status.uptime_secs), Some(status.version)),
                Err(_) => (None, None),
            }
        } else {
            (None, None)
        };

        Ok(ServiceStatus {
            installed,
            running,
            pid,
            uptime_secs,
            version,
            auto_start: installed,
        })
    }

    /// Check if the service definition file exists.
    fn is_installed(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.plist_path().exists()
        }
        #[cfg(target_os = "linux")]
        {
            self.unit_path().exists()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            false
        }
    }

    /// Tail the daemon logs.
    pub async fn logs(&self, lines: usize) -> Result<String, DaemonError> {
        let log_path = self.log_dir.join("gateway.stderr.log");
        if !log_path.exists() {
            return Ok("No log file found.".into());
        }

        let content = tokio::fs::read_to_string(&log_path).await?;
        let all_lines: Vec<&str> = content.lines().collect();
        let start = all_lines.len().saturating_sub(lines);
        Ok(all_lines[start..].join("\n"))
    }

    // ────────────────────────────────────────────────────────
    // macOS — launchd
    // ────────────────────────────────────────────────────────

    #[cfg(target_os = "macos")]
    fn plist_path(&self) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join("dev.clawdesk.gateway.plist")
    }

    #[cfg(target_os = "macos")]
    fn generate_plist(&self) -> String {
        let bin = self.binary_path.display();
        let stdout = self.log_dir.join("gateway.stdout.log").display().to_string();
        let stderr = self.log_dir.join("gateway.stderr.log").display().to_string();
        let home = self.home_dir.display();

        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.clawdesk.gateway</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>daemon</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>5</integer>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>CLAWDESK_HOME</key>
        <string>{home}</string>
    </dict>
    <key>ProcessType</key>
    <string>Background</string>
    <key>LowPriorityBackgroundIO</key>
    <true/>
</dict>
</plist>"#
        )
    }

    #[cfg(target_os = "macos")]
    fn install_launchd(&self) -> Result<(), DaemonError> {
        let plist = self.plist_path();
        if plist.exists() {
            return Err(DaemonError::AlreadyInstalled);
        }

        if let Some(parent) = plist.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = self.generate_plist();
        std::fs::write(&plist, &content)?;
        info!(path = %plist.display(), "launchd plist installed");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn uninstall_launchd(&self) -> Result<(), DaemonError> {
        let plist = self.plist_path();
        if !plist.exists() {
            return Err(DaemonError::NotInstalled);
        }
        std::fs::remove_file(&plist)?;
        info!("launchd plist removed");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn start_launchd(&self) -> Result<(), DaemonError> {
        let plist = self.plist_path();
        if !plist.exists() {
            return Err(DaemonError::NotInstalled);
        }
        let status = std::process::Command::new("launchctl")
            .args(["load", "-w"])
            .arg(&plist)
            .status()?;
        if !status.success() {
            return Err(DaemonError::Platform {
                detail: format!("launchctl load failed: exit {}", status),
            });
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn stop_launchd(&self) -> Result<(), DaemonError> {
        let plist = self.plist_path();
        if !plist.exists() {
            return Err(DaemonError::NotInstalled);
        }
        let status = std::process::Command::new("launchctl")
            .args(["unload"])
            .arg(&plist)
            .status()?;
        if !status.success() {
            warn!("launchctl unload exited with non-zero (may already be unloaded)");
        }
        Ok(())
    }

    // ────────────────────────────────────────────────────────
    // Linux — systemd
    // ────────────────────────────────────────────────────────

    #[cfg(target_os = "linux")]
    fn unit_path(&self) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home)
            .join(".config")
            .join("systemd")
            .join("user")
            .join("clawdesk.service")
    }

    #[cfg(target_os = "linux")]
    fn generate_unit(&self) -> String {
        let bin = self.binary_path.display();
        let home = self.home_dir.display();

        format!(
            r#"[Unit]
Description=ClawDesk Multi-Channel AI Agent Gateway
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
ExecStart={bin} daemon run
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5s
WatchdogSec=30s
NotifyAccess=main
Environment=CLAWDESK_HOME={home}

# Security hardening
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=%h/.clawdesk
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectControlGroups=yes

# Resource limits
MemoryMax=2G
CPUQuota=200%
TasksMax=512

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=clawdesk

[Install]
WantedBy=default.target
"#
        )
    }

    #[cfg(target_os = "linux")]
    fn install_systemd(&self) -> Result<(), DaemonError> {
        let unit = self.unit_path();
        if unit.exists() {
            return Err(DaemonError::AlreadyInstalled);
        }

        if let Some(parent) = unit.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&unit, self.generate_unit())?;

        // Reload systemd user daemon.
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();

        // Enable auto-start.
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "enable", "clawdesk.service"])
            .status();

        info!(path = %unit.display(), "systemd unit installed");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn uninstall_systemd(&self) -> Result<(), DaemonError> {
        let unit = self.unit_path();
        if !unit.exists() {
            return Err(DaemonError::NotInstalled);
        }

        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "clawdesk.service"])
            .status();

        std::fs::remove_file(&unit)?;

        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();

        info!("systemd unit removed");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn start_systemd(&self) -> Result<(), DaemonError> {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "start", "clawdesk.service"])
            .status()?;
        if !status.success() {
            return Err(DaemonError::Platform {
                detail: format!("systemctl start failed: exit {}", status),
            });
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn stop_systemd(&self) -> Result<(), DaemonError> {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "stop", "clawdesk.service"])
            .status()?;
        if !status.success() {
            warn!("systemctl stop exited with non-zero (may already be stopped)");
        }
        Ok(())
    }

    // ────────────────────────────────────────────────────────
    // Windows — Service stubs
    // ────────────────────────────────────────────────────────

    #[cfg(target_os = "windows")]
    fn install_windows_service(&self) -> Result<(), DaemonError> {
        let bin = self.binary_path.display();
        let status = std::process::Command::new("sc.exe")
            .args([
                "create",
                "ClawDesk",
                &format!("binPath= \"{bin} daemon run\""),
                "start=",
                "auto",
                "DisplayName=",
                "ClawDesk AI Gateway",
            ])
            .status()?;
        if !status.success() {
            return Err(DaemonError::Platform {
                detail: "sc.exe create failed".into(),
            });
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn uninstall_windows_service(&self) -> Result<(), DaemonError> {
        let status = std::process::Command::new("sc.exe")
            .args(["delete", "ClawDesk"])
            .status()?;
        if !status.success() {
            warn!("sc.exe delete returned non-zero");
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn start_windows_service(&self) -> Result<(), DaemonError> {
        let status = std::process::Command::new("sc.exe")
            .args(["start", "ClawDesk"])
            .status()?;
        if !status.success() {
            return Err(DaemonError::Platform {
                detail: "sc.exe start failed".into(),
            });
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn stop_windows_service(&self) -> Result<(), DaemonError> {
        let status = std::process::Command::new("sc.exe")
            .args(["stop", "ClawDesk"])
            .status()?;
        if !status.success() {
            warn!("sc.exe stop returned non-zero");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_status_display() {
        let s = ServiceStatus {
            installed: true,
            running: true,
            pid: Some(12345),
            uptime_secs: Some(3600),
            version: Some("0.1.0".into()),
            auto_start: true,
        };
        let display = s.display();
        assert!(display.contains("running"));
        assert!(display.contains("12345"));
        assert!(display.contains("3600"));
        assert!(display.contains("installed"));
        assert!(display.contains("auto-start"));
    }

    #[test]
    fn service_status_stopped() {
        let s = ServiceStatus {
            installed: true,
            running: false,
            pid: None,
            uptime_secs: None,
            version: None,
            auto_start: true,
        };
        let display = s.display();
        assert!(display.contains("stopped"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_generation() {
        let ctl = DaemonCtl::new("/usr/local/bin/clawdesk".into(), 18789);
        let plist = ctl.generate_plist();
        assert!(plist.contains("dev.clawdesk.gateway"));
        assert!(plist.contains("/usr/local/bin/clawdesk"));
        assert!(plist.contains("daemon"));
        assert!(plist.contains("RunAtLoad"));
        assert!(plist.contains("KeepAlive"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unit_generation() {
        let ctl = DaemonCtl::new("/usr/local/bin/clawdesk".into(), 18789);
        let unit = ctl.generate_unit();
        assert!(unit.contains("ClawDesk"));
        assert!(unit.contains("Type=notify"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WatchdogSec=30s"));
        assert!(unit.contains("ProtectSystem=strict"));
    }
}
