//! Device capabilities — camera, screen capture, location, device info.
//!
//! Provides cross-platform device capability abstractions.
//! Platform-specific implementations behind feature flags.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;


// ═══════════════════════════════════════════════════════════════
// Device info
// ═══════════════════════════════════════════════════════════════

/// Structured device information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub hostname: String,
    pub cpu_cores: usize,
    pub memory_total_mb: u64,
    pub display_count: u32,
    pub is_desktop: bool,
}

impl DeviceInfo {
    /// Gather device info from the current system.
    pub fn gather() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            os_version: os_version(),
            arch: std::env::consts::ARCH.to_string(),
            hostname: hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown".into()),
            cpu_cores: num_cpus::get(),
            memory_total_mb: sys_memory_mb(),
            display_count: 1, // Tauri layer can override
            is_desktop: true,
        }
    }
}

/// Device status (dynamic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub battery_level: Option<f64>,
    pub battery_charging: Option<bool>,
    pub uptime_secs: u64,
    pub memory_used_mb: u64,
    pub memory_total_mb: u64,
}

impl DeviceStatus {
    /// Gather current device status.
    pub fn gather() -> Self {
        Self {
            battery_level: None, // Would need platform API
            battery_charging: None,
            uptime_secs: uptime_secs(),
            memory_used_mb: 0, // Placeholder
            memory_total_mb: sys_memory_mb(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Location
// ═══════════════════════════════════════════════════════════════

/// GPS/location data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationData {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy_m: Option<f64>,
    pub altitude_m: Option<f64>,
    pub heading: Option<f64>,
    pub speed_mps: Option<f64>,
    pub timestamp: String,
    pub source: LocationSource,
}

/// Source of the location data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationSource {
    Gps,
    Network,
    Ip,
    Manual,
    Unknown,
}

/// Location provider trait.
#[async_trait::async_trait]
pub trait LocationProvider: Send + Sync {
    /// Get current location.
    async fn get_location(&self) -> Result<LocationData, String>;
    /// Check if location access is authorized.
    async fn is_authorized(&self) -> bool;
    /// Request location authorization.
    async fn request_authorization(&self) -> Result<bool, String>;
}

/// IP-based location fallback (uses external API).
pub struct IpLocationProvider;

#[async_trait::async_trait]
impl LocationProvider for IpLocationProvider {
    async fn get_location(&self) -> Result<LocationData, String> {
        // Use a free IP geolocation API as fallback
        let resp = reqwest::get("http://ip-api.com/json/?fields=lat,lon,city,query")
            .await
            .map_err(|e| format!("location request failed: {e}"))?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("location parse failed: {e}"))?;

        Ok(LocationData {
            latitude: json["lat"].as_f64().unwrap_or(0.0),
            longitude: json["lon"].as_f64().unwrap_or(0.0),
            accuracy_m: Some(5000.0), // IP-based, low accuracy
            altitude_m: None,
            heading: None,
            speed_mps: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
            source: LocationSource::Ip,
        })
    }

    async fn is_authorized(&self) -> bool {
        true // IP-based doesn't need auth
    }

    async fn request_authorization(&self) -> Result<bool, String> {
        Ok(true)
    }
}

// ═══════════════════════════════════════════════════════════════
// Camera
// ═══════════════════════════════════════════════════════════════

/// Camera capture result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraCapture {
    pub format: String,
    pub base64: String,
    pub width: u32,
    pub height: u32,
    pub source: String,
    pub timestamp: String,
}

/// Screen recording result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenRecording {
    pub format: String,
    pub base64: String,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
    pub timestamp: String,
}

/// Camera/media provider trait.
#[async_trait::async_trait]
pub trait MediaProvider: Send + Sync {
    /// Take a photo from the camera.
    async fn camera_snap(
        &self,
        camera_id: Option<&str>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<CameraCapture, String>;

    /// Record a short video clip from the camera.
    async fn camera_clip(
        &self,
        camera_id: Option<&str>,
        duration_ms: u64,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<CameraCapture, String>;

    /// Record the screen.
    async fn screen_record(
        &self,
        duration_ms: u64,
        display_id: Option<u32>,
    ) -> Result<ScreenRecording, String>;

    /// Take a screenshot.
    async fn screen_snap(
        &self,
        display_id: Option<u32>,
    ) -> Result<CameraCapture, String>;

    /// List available cameras.
    async fn list_cameras(&self) -> Result<Vec<CameraInfo>, String>;
}

/// Camera device information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

// ═══════════════════════════════════════════════════════════════
// Device capabilities aggregator
// ═══════════════════════════════════════════════════════════════

/// Available device capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    pub camera: bool,
    pub screen_capture: bool,
    pub location: bool,
    pub notifications: bool,
    pub microphone: bool,
    pub clipboard: bool,
}

impl DeviceCapabilities {
    /// Check what capabilities are available on this device.
    pub fn detect() -> Self {
        Self {
            camera: cfg!(any(target_os = "macos", target_os = "windows", target_os = "linux")),
            screen_capture: cfg!(any(target_os = "macos", target_os = "windows", target_os = "linux")),
            location: true, // IP-based always available
            notifications: true,
            microphone: cfg!(any(target_os = "macos", target_os = "windows", target_os = "linux")),
            clipboard: true,
        }
    }
}

/// Device capability manager — coordinates device operations.
pub struct DeviceManager {
    pub location: Box<dyn LocationProvider>,
    pub media: Option<Box<dyn MediaProvider>>,
    pub info: DeviceInfo,
}

impl DeviceManager {
    /// Create a new device manager with defaults.
    pub fn new() -> Self {
        Self {
            location: Box::new(IpLocationProvider),
            media: None,
            info: DeviceInfo::gather(),
        }
    }

    /// Set a custom location provider.
    pub fn set_location_provider(&mut self, provider: Box<dyn LocationProvider>) {
        self.location = provider;
    }

    /// Set a media provider.
    pub fn set_media_provider(&mut self, provider: Box<dyn MediaProvider>) {
        self.media = Some(provider);
    }

    /// Get device info.
    pub fn device_info(&self) -> &DeviceInfo {
        &self.info
    }

    /// Get device status.
    pub fn device_status(&self) -> DeviceStatus {
        DeviceStatus::gather()
    }

    /// Get current location.
    pub async fn get_location(&self) -> Result<LocationData, String> {
        self.location.get_location().await
    }

    /// Take a camera snapshot.
    pub async fn camera_snap(
        &self,
        camera_id: Option<&str>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<CameraCapture, String> {
        match &self.media {
            Some(m) => m.camera_snap(camera_id, width, height).await,
            None => Err("media provider not configured".into()),
        }
    }

    /// Record a camera clip.
    pub async fn camera_clip(
        &self,
        camera_id: Option<&str>,
        duration_ms: u64,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<CameraCapture, String> {
        match &self.media {
            Some(m) => m.camera_clip(camera_id, duration_ms, width, height).await,
            None => Err("media provider not configured".into()),
        }
    }

    /// Record the screen.
    pub async fn screen_record(
        &self,
        duration_ms: u64,
        display_id: Option<u32>,
    ) -> Result<ScreenRecording, String> {
        match &self.media {
            Some(m) => m.screen_record(duration_ms, display_id).await,
            None => Err("media provider not configured".into()),
        }
    }

    /// Take a screenshot.
    pub async fn screen_snap(
        &self,
        display_id: Option<u32>,
    ) -> Result<CameraCapture, String> {
        match &self.media {
            Some(m) => m.screen_snap(display_id).await,
            None => Err("media provider not configured".into()),
        }
    }

    /// List cameras.
    pub async fn list_cameras(&self) -> Result<Vec<CameraInfo>, String> {
        match &self.media {
            Some(m) => m.list_cameras().await,
            None => Ok(vec![]),
        }
    }

    /// Get all device capabilities.
    pub fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities::detect()
    }
}

// ═══════════════════════════════════════════════════════════════
// Platform helpers
// ═══════════════════════════════════════════════════════════════

fn os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".into())
    }
    #[cfg(target_os = "windows")]
    {
        "windows".into()
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|c| {
                c.lines()
                    .find(|l| l.starts_with("PRETTY_NAME="))
                    .map(|l| l.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "linux".into())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        "unknown".into()
    }
}

fn sys_memory_mb() -> u64 {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        Command::new("sysctl")
            .arg("-n")
            .arg("hw.memsize")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|b| b / (1024 * 1024))
            .unwrap_or(0)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|c| {
                c.lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .and_then(|l| {
                        l.split_whitespace()
                            .nth(1)
                            .and_then(|s| s.parse::<u64>().ok())
                    })
            })
            .map(|kb| kb / 1024)
            .unwrap_or(0)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

fn uptime_secs() -> u64 {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        // sysctl kern.boottime returns something like:
        // kern.boottime: { sec = 1234567890, usec = 0 } ...
        Command::new("sysctl")
            .arg("-n")
            .arg("kern.boottime")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| {
                s.split("sec = ")
                    .nth(1)
                    .and_then(|r| r.split(',').next())
                    .and_then(|n| n.trim().parse::<u64>().ok())
            })
            .and_then(|boot| {
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .ok()
                    .map(|now| now.as_secs().saturating_sub(boot))
            })
            .unwrap_or(0)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|n| n.parse::<f64>().ok()))
            .map(|f| f as u64)
            .unwrap_or(0)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_info_gathers() {
        let info = DeviceInfo::gather();
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
        assert!(info.cpu_cores > 0);
        assert!(info.is_desktop);
    }

    #[test]
    fn device_capabilities_detect() {
        let caps = DeviceCapabilities::detect();
        assert!(caps.location); // IP-based always true
        assert!(caps.notifications);
        assert!(caps.clipboard);
    }

    #[test]
    fn device_manager_new() {
        let mgr = DeviceManager::new();
        assert!(!mgr.device_info().os.is_empty());
        assert!(mgr.media.is_none());
    }

    #[test]
    fn device_status_gathers() {
        let status = DeviceStatus::gather();
        // uptime should be > 0 on a running system
        // (but might be 0 on unsupported platforms in CI)
        assert!(status.memory_total_mb > 0 || cfg!(target_os = "windows"));
    }
}
