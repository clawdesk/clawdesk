//! Browser profiles — persistent user-data directories for session continuity.
//!
//! Profiles allow agents to maintain logins, cookies, and preferences across
//! sessions. Each profile maps to a Chrome `--user-data-dir` with isolated
//! storage.
//!
//! ## Storage layout
//! ```text
//! ~/.clawdesk/browser/profiles/
//! ├── default/
//! │   ├── profile.json       ← metadata
//! │   └── user-data/         ← Chrome user-data-dir
//! ├── work/
//! │   ├── profile.json
//! │   └── user-data/
//! └── shopping/
//!     ├── profile.json
//!     └── user-data/
//! ```

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// A named browser profile with persistent user-data directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserProfile {
    /// Unique profile name (alphanumeric + hyphens).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Whether to launch headless (overrides global config when set).
    pub headless: Option<bool>,
    /// Viewport width override.
    pub viewport_width: Option<u32>,
    /// Viewport height override.
    pub viewport_height: Option<u32>,
    /// Chrome startup arguments (appended to defaults).
    pub extra_args: Vec<String>,
    /// Profile color for visual identification (hex, e.g. "#4A90D9").
    pub color: Option<String>,
    /// Whether the profile is the default.
    pub is_default: bool,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
    /// Last used timestamp (RFC 3339).
    pub last_used: Option<String>,
}

impl Default for BrowserProfile {
    fn default() -> Self {
        Self {
            name: "default".into(),
            description: "Default browser profile".into(),
            headless: None,
            viewport_width: None,
            viewport_height: None,
            extra_args: vec![],
            color: None,
            is_default: true,
            created_at: now_rfc3339(),
            last_used: None,
        }
    }
}

/// Profile manager — CRUD for browser profiles on disk.
pub struct ProfileManager {
    base_dir: PathBuf,
}

impl ProfileManager {
    /// Create a new profile manager.
    ///
    /// `base_dir` is typically `~/.clawdesk/browser/profiles`.
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Create with the default base directory (`~/.clawdesk/browser/profiles`).
    pub fn default_location() -> Result<Self, String> {
        let home = dirs_path()?;
        let base = home.join(".clawdesk").join("browser").join("profiles");
        Ok(Self::new(base))
    }

    /// Ensure the base directory and default profile exist.
    pub fn ensure_defaults(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.base_dir)
            .map_err(|e| format!("create profile base dir: {e}"))?;

        // Create default profile if missing
        if !self.profile_dir("default").join("profile.json").exists() {
            self.create(BrowserProfile::default())?;
            info!("created default browser profile");
        }
        Ok(())
    }

    /// Create a new profile.
    pub fn create(&self, profile: BrowserProfile) -> Result<BrowserProfile, String> {
        validate_profile_name(&profile.name)?;

        let dir = self.profile_dir(&profile.name);
        if dir.join("profile.json").exists() {
            return Err(format!("profile '{}' already exists", profile.name));
        }

        // Create directory structure
        std::fs::create_dir_all(dir.join("user-data"))
            .map_err(|e| format!("create profile dir: {e}"))?;

        // Write metadata
        self.write_metadata(&profile)?;

        // Write Chrome preferences with profile color if set
        if let Some(ref color) = profile.color {
            self.write_chrome_color(&profile.name, color)?;
        }

        info!(name = %profile.name, "browser profile created");
        Ok(profile)
    }

    /// Get a profile by name.
    pub fn get(&self, name: &str) -> Result<BrowserProfile, String> {
        let path = self.profile_dir(name).join("profile.json");
        let data = std::fs::read_to_string(&path)
            .map_err(|e| format!("read profile '{}': {e}", name))?;
        serde_json::from_str(&data)
            .map_err(|e| format!("parse profile '{}': {e}", name))
    }

    /// List all profiles.
    pub fn list(&self) -> Result<Vec<BrowserProfile>, String> {
        let mut profiles = Vec::new();

        if !self.base_dir.exists() {
            return Ok(profiles);
        }

        let entries = std::fs::read_dir(&self.base_dir)
            .map_err(|e| format!("read profile dir: {e}"))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let meta_path = path.join("profile.json");
                if meta_path.exists() {
                    match std::fs::read_to_string(&meta_path) {
                        Ok(data) => match serde_json::from_str::<BrowserProfile>(&data) {
                            Ok(p) => profiles.push(p),
                            Err(e) => warn!(path = %meta_path.display(), "bad profile: {e}"),
                        },
                        Err(e) => warn!(path = %meta_path.display(), "read error: {e}"),
                    }
                }
            }
        }

        // Sort: default first, then alphabetical
        profiles.sort_by(|a, b| {
            b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name))
        });

        Ok(profiles)
    }

    /// Update a profile's metadata.
    pub fn update(&self, profile: BrowserProfile) -> Result<BrowserProfile, String> {
        validate_profile_name(&profile.name)?;

        let dir = self.profile_dir(&profile.name);
        if !dir.join("profile.json").exists() {
            return Err(format!("profile '{}' not found", profile.name));
        }

        self.write_metadata(&profile)?;

        if let Some(ref color) = profile.color {
            self.write_chrome_color(&profile.name, color)?;
        }

        debug!(name = %profile.name, "browser profile updated");
        Ok(profile)
    }

    /// Delete a profile and its user-data directory.
    pub fn delete(&self, name: &str) -> Result<(), String> {
        if name == "default" {
            return Err("cannot delete the default profile".into());
        }

        let dir = self.profile_dir(name);
        if !dir.exists() {
            return Err(format!("profile '{}' not found", name));
        }

        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("delete profile '{}': {e}", name))?;

        info!(name, "browser profile deleted");
        Ok(())
    }

    /// Touch the last_used timestamp on a profile.
    pub fn touch(&self, name: &str) -> Result<(), String> {
        if let Ok(mut profile) = self.get(name) {
            profile.last_used = Some(now_rfc3339());
            self.write_metadata(&profile)?;
        }
        Ok(())
    }

    /// Get the Chrome `--user-data-dir` path for a profile.
    pub fn user_data_dir(&self, name: &str) -> PathBuf {
        self.profile_dir(name).join("user-data")
    }

    /// Get the profile directory.
    fn profile_dir(&self, name: &str) -> PathBuf {
        self.base_dir.join(name)
    }

    /// Write profile metadata to disk.
    fn write_metadata(&self, profile: &BrowserProfile) -> Result<(), String> {
        let path = self.profile_dir(&profile.name).join("profile.json");
        let json = serde_json::to_string_pretty(profile)
            .map_err(|e| format!("serialize profile: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("write profile: {e}"))
    }

    /// Write Chrome Preferences with profile color (toolbar theme).
    fn write_chrome_color(&self, name: &str, hex_color: &str) -> Result<(), String> {
        let prefs_dir = self.user_data_dir(name).join("Default");
        std::fs::create_dir_all(&prefs_dir)
            .map_err(|e| format!("create prefs dir: {e}"))?;

        let (r, g, b) = parse_hex_color(hex_color)?;

        let prefs = serde_json::json!({
            "browser": {
                "theme": {
                    "color_type": 1,
                    "user_color": format!("#{:02x}{:02x}{:02x}", r, g, b)
                }
            }
        });

        let prefs_path = prefs_dir.join("Preferences");

        // Merge with existing preferences if they exist
        let merged = if prefs_path.exists() {
            if let Ok(existing) = std::fs::read_to_string(&prefs_path) {
                if let Ok(mut existing_val) = serde_json::from_str::<serde_json::Value>(&existing) {
                    merge_json(&mut existing_val, &prefs);
                    existing_val
                } else {
                    prefs
                }
            } else {
                prefs
            }
        } else {
            prefs
        };

        let json = serde_json::to_string_pretty(&merged)
            .map_err(|e| format!("serialize prefs: {e}"))?;
        std::fs::write(&prefs_path, json).map_err(|e| format!("write prefs: {e}"))
    }
}

// ── Helpers ──────────────────────────────────────────────────

/// Validate a profile name: alphanumeric, hyphens, underscores, 1-64 chars.
fn validate_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("profile name must be 1-64 characters".into());
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err("profile name may only contain alphanumeric, hyphens, underscores".into());
    }
    // Prevent path traversal
    if name.contains("..") || name.starts_with('.') {
        return Err("profile name cannot contain '..' or start with '.'".into());
    }
    Ok(())
}

/// Get the user's home directory.
fn dirs_path() -> Result<PathBuf, String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .map_err(|_| "HOME not set".into())
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .map_err(|_| "USERPROFILE not set".into())
    }
}

/// Parse a hex color string (#RRGGBB or RRGGBB) to (r, g, b).
fn parse_hex_color(hex: &str) -> Result<(u8, u8, u8), String> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return Err(format!("invalid hex color: expected 6 chars, got {}", hex.len()));
    }
    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| format!("bad red: {e}"))?;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| format!("bad green: {e}"))?;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| format!("bad blue: {e}"))?;
    Ok((r, g, b))
}

/// Deep-merge two serde_json::Value objects.
fn merge_json(base: &mut serde_json::Value, overlay: &serde_json::Value) {
    if let (serde_json::Value::Object(base_map), serde_json::Value::Object(overlay_map)) =
        (base, overlay)
    {
        for (key, value) in overlay_map {
            let entry = base_map
                .entry(key.clone())
                .or_insert(serde_json::Value::Null);
            if entry.is_object() && value.is_object() {
                merge_json(entry, value);
            } else {
                *entry = value.clone();
            }
        }
    }
}

/// Current time as RFC 3339 string.
fn now_rfc3339() -> String {
    // Use a simple approach without chrono dependency
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Format as ISO 8601 / RFC 3339 (UTC)
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Simple year/month/day from days since epoch (1970-01-01)
    let (year, month, day) = days_to_ymd(days_since_epoch);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_profile_names() {
        assert!(validate_profile_name("default").is_ok());
        assert!(validate_profile_name("my-profile").is_ok());
        assert!(validate_profile_name("work_2024").is_ok());
        assert!(validate_profile_name("A").is_ok());
    }

    #[test]
    fn invalid_profile_names() {
        assert!(validate_profile_name("").is_err());
        assert!(validate_profile_name("../escape").is_err());
        assert!(validate_profile_name(".hidden").is_err());
        assert!(validate_profile_name("has spaces").is_err());
        assert!(validate_profile_name("a/b").is_err());
        let long = "a".repeat(65);
        assert!(validate_profile_name(&long).is_err());
    }

    #[test]
    fn hex_color_parsing() {
        assert_eq!(parse_hex_color("#FF0000").unwrap(), (255, 0, 0));
        assert_eq!(parse_hex_color("00FF00").unwrap(), (0, 255, 0));
        assert_eq!(parse_hex_color("#4A90D9").unwrap(), (74, 144, 217));
        assert!(parse_hex_color("ZZZ").is_err());
    }

    #[test]
    fn merge_json_deep() {
        let mut base = serde_json::json!({"a": {"b": 1, "c": 2}});
        let overlay = serde_json::json!({"a": {"b": 99, "d": 3}});
        merge_json(&mut base, &overlay);
        assert_eq!(base["a"]["b"], 99);
        assert_eq!(base["a"]["c"], 2);
        assert_eq!(base["a"]["d"], 3);
    }

    #[test]
    fn default_profile() {
        let p = BrowserProfile::default();
        assert_eq!(p.name, "default");
        assert!(p.is_default);
        assert!(p.extra_args.is_empty());
    }

    #[test]
    fn rfc3339_format() {
        let ts = now_rfc3339();
        // Should look like "2024-01-15T12:30:45Z"
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
    }
}
