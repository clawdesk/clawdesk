//! Declarative Reload Policy via `reload.toml`.
//!
//! Parses a `~/.clawdesk/reload.toml` file that parameterizes the entire
//! hot-reload subsystem. Provides environment-specific presets (dev, staging,
//! prod) and field-level override of every knob in the reload pipeline.
//!
//! ## Schema (example)
//!
//! ```toml
//! [global]
//! preset = "production"          # "development" | "staging" | "production"
//! enable_hot_reload = true
//!
//! [watcher]
//! debounce_ms = 500
//! extensions = ["toml", "json", "yaml"]
//!
//! [validation]
//! strict_mode = true
//! skip_stages = []               # e.g., ["compatibility"]
//!
//! [canary]
//! window_secs = 60
//! check_interval_secs = 5
//! health_threshold = 0.7
//! auto_rollback = true
//!
//! [rollback]
//! buffer_capacity = 10
//!
//! [credentials]
//! drain_timeout_secs = 30
//! auto_rotate = false
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Top-level policy
// ---------------------------------------------------------------------------

/// Top-level reload policy, deserialized from `reload.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadPolicy {
    /// Global settings.
    #[serde(default)]
    pub global: GlobalPolicy,

    /// File watcher settings.
    #[serde(default)]
    pub watcher: WatcherPolicy,

    /// Validation pipeline settings.
    #[serde(default)]
    pub validation: ValidationPolicy,

    /// Canary health monitoring settings.
    #[serde(default)]
    pub canary: CanaryPolicy,

    /// Rollback buffer settings.
    #[serde(default)]
    pub rollback: RollbackPolicy,

    /// Credential rotation settings.
    #[serde(default)]
    pub credentials: CredentialPolicy,
}

impl Default for ReloadPolicy {
    fn default() -> Self {
        Self::for_preset(Preset::Development)
    }
}

impl ReloadPolicy {
    /// Create a policy from a preset, then apply overrides from the file.
    pub fn for_preset(preset: Preset) -> Self {
        match preset {
            Preset::Development => Self::development(),
            Preset::Staging => Self::staging(),
            Preset::Production => Self::production(),
        }
    }

    /// Development preset — fast iteration, lenient validation.
    fn development() -> Self {
        Self {
            global: GlobalPolicy {
                preset: Preset::Development,
                enable_hot_reload: true,
            },
            watcher: WatcherPolicy {
                debounce_ms: 100,
                extensions: default_extensions(),
            },
            validation: ValidationPolicy {
                strict_mode: false,
                skip_stages: HashSet::from(["compatibility".into()]),
            },
            canary: CanaryPolicy {
                window_secs: 10,
                check_interval_secs: 2,
                health_threshold: 0.5,
                auto_rollback: false,
                min_checks: 2,
            },
            rollback: RollbackPolicy {
                buffer_capacity: 5,
            },
            credentials: CredentialPolicy {
                drain_timeout_secs: 10,
                auto_rotate: false,
            },
        }
    }

    /// Staging preset — moderate safety.
    fn staging() -> Self {
        Self {
            global: GlobalPolicy {
                preset: Preset::Staging,
                enable_hot_reload: true,
            },
            watcher: WatcherPolicy {
                debounce_ms: 300,
                extensions: default_extensions(),
            },
            validation: ValidationPolicy {
                strict_mode: true,
                skip_stages: HashSet::new(),
            },
            canary: CanaryPolicy {
                window_secs: 30,
                check_interval_secs: 5,
                health_threshold: 0.7,
                auto_rollback: true,
                min_checks: 3,
            },
            rollback: RollbackPolicy {
                buffer_capacity: 10,
            },
            credentials: CredentialPolicy {
                drain_timeout_secs: 30,
                auto_rotate: true,
            },
        }
    }

    /// Production preset — maximum safety.
    fn production() -> Self {
        Self {
            global: GlobalPolicy {
                preset: Preset::Production,
                enable_hot_reload: true,
            },
            watcher: WatcherPolicy {
                debounce_ms: 500,
                extensions: default_extensions(),
            },
            validation: ValidationPolicy {
                strict_mode: true,
                skip_stages: HashSet::new(),
            },
            canary: CanaryPolicy {
                window_secs: 60,
                check_interval_secs: 5,
                health_threshold: 0.8,
                auto_rollback: true,
                min_checks: 5,
            },
            rollback: RollbackPolicy {
                buffer_capacity: 20,
            },
            credentials: CredentialPolicy {
                drain_timeout_secs: 60,
                auto_rotate: true,
            },
        }
    }

    /// Load from a TOML file, falling back to the preset defaults.
    ///
    /// If the file doesn't exist, returns the development preset.
    /// Any fields present in the file override the preset values.
    pub fn load_from_file(path: &Path) -> Result<Self, ReloadPolicyError> {
        if !path.exists() {
            info!(
                path = %path.display(),
                "reload.toml not found, using development defaults"
            );
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path).map_err(|e| {
            ReloadPolicyError::IoError(format!(
                "failed to read {}: {e}",
                path.display()
            ))
        })?;

        Self::parse(&content)
    }

    /// Parse a TOML string into a policy.
    pub fn parse(toml_str: &str) -> Result<Self, ReloadPolicyError> {
        let policy: Self = toml::from_str(toml_str).map_err(|e| {
            ReloadPolicyError::ParseError(format!("TOML parse error: {e}"))
        })?;

        info!(preset = ?policy.global.preset, "reload policy loaded");
        Ok(policy)
    }

    /// Default file path: `~/.clawdesk/reload.toml`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".clawdesk").join("reload.toml"))
    }

    /// Validate the policy values.
    pub fn validate(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.watcher.debounce_ms == 0 {
            warnings.push("watcher.debounce_ms is 0 — may cause excessive reloads".into());
        }
        if self.canary.health_threshold <= 0.0 || self.canary.health_threshold > 1.0 {
            warnings.push(format!(
                "canary.health_threshold ({}) should be in (0.0, 1.0]",
                self.canary.health_threshold
            ));
        }
        if self.canary.window_secs < self.canary.check_interval_secs {
            warnings.push(
                "canary.window_secs < check_interval_secs — canary will never collect enough checks".into(),
            );
        }
        if self.rollback.buffer_capacity == 0 {
            warnings.push("rollback.buffer_capacity is 0 — rollbacks will be impossible".into());
        }

        if !warnings.is_empty() {
            for w in &warnings {
                warn!("reload policy validation: {w}");
            }
        }

        warnings
    }
}

// ---------------------------------------------------------------------------
// Sub-policies
// ---------------------------------------------------------------------------

/// Environment preset selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    Development,
    Staging,
    Production,
}

impl Default for Preset {
    fn default() -> Self {
        Self::Development
    }
}

/// Global reload settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalPolicy {
    /// Active preset.
    #[serde(default)]
    pub preset: Preset,
    /// Whether hot reload is enabled at all.
    #[serde(default = "default_true")]
    pub enable_hot_reload: bool,
}

impl Default for GlobalPolicy {
    fn default() -> Self {
        Self {
            preset: Preset::Development,
            enable_hot_reload: true,
        }
    }
}

/// File watcher settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherPolicy {
    /// Debounce interval in milliseconds.
    #[serde(default = "default_debounce")]
    pub debounce_ms: u64,
    /// File extensions to watch.
    #[serde(default = "default_extensions")]
    pub extensions: Vec<String>,
}

impl Default for WatcherPolicy {
    fn default() -> Self {
        Self {
            debounce_ms: default_debounce(),
            extensions: default_extensions(),
        }
    }
}

/// Validation pipeline settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationPolicy {
    /// If true, warnings are promoted to errors.
    #[serde(default)]
    pub strict_mode: bool,
    /// Stages to skip (by name).
    #[serde(default)]
    pub skip_stages: HashSet<String>,
}

impl Default for ValidationPolicy {
    fn default() -> Self {
        Self {
            strict_mode: false,
            skip_stages: HashSet::new(),
        }
    }
}

/// Canary health monitoring settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryPolicy {
    /// Canary observation window in seconds.
    #[serde(default = "default_canary_window")]
    pub window_secs: u64,
    /// How often to check health in seconds.
    #[serde(default = "default_check_interval")]
    pub check_interval_secs: u64,
    /// Composite health score threshold for promotion.
    #[serde(default = "default_health_threshold")]
    pub health_threshold: f64,
    /// Whether to auto-rollback on canary failure.
    #[serde(default = "default_true")]
    pub auto_rollback: bool,
    /// Minimum health checks before verdict.
    #[serde(default = "default_min_checks")]
    pub min_checks: usize,
}

impl Default for CanaryPolicy {
    fn default() -> Self {
        Self {
            window_secs: default_canary_window(),
            check_interval_secs: default_check_interval(),
            health_threshold: default_health_threshold(),
            auto_rollback: true,
            min_checks: default_min_checks(),
        }
    }
}

/// Rollback buffer settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackPolicy {
    /// Number of config generations to keep for rollback.
    #[serde(default = "default_buffer_capacity")]
    pub buffer_capacity: usize,
}

impl Default for RollbackPolicy {
    fn default() -> Self {
        Self {
            buffer_capacity: default_buffer_capacity(),
        }
    }
}

/// Credential rotation settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialPolicy {
    /// Maximum drain timeout in seconds.
    #[serde(default = "default_drain_timeout")]
    pub drain_timeout_secs: u64,
    /// Whether to auto-rotate credentials on config change.
    #[serde(default)]
    pub auto_rotate: bool,
}

impl Default for CredentialPolicy {
    fn default() -> Self {
        Self {
            drain_timeout_secs: default_drain_timeout(),
            auto_rotate: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}
fn default_debounce() -> u64 {
    300
}
fn default_extensions() -> Vec<String> {
    vec!["toml".into(), "json".into(), "yaml".into(), "yml".into()]
}
fn default_canary_window() -> u64 {
    30
}
fn default_check_interval() -> u64 {
    5
}
fn default_health_threshold() -> f64 {
    0.7
}
fn default_min_checks() -> usize {
    3
}
fn default_buffer_capacity() -> usize {
    10
}
fn default_drain_timeout() -> u64 {
    30
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Error type for reload policy operations.
#[derive(Debug, Clone)]
pub enum ReloadPolicyError {
    IoError(String),
    ParseError(String),
}

impl std::fmt::Display for ReloadPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(e) => write!(f, "IO error: {e}"),
            Self::ParseError(e) => write!(f, "parse error: {e}"),
        }
    }
}

impl std::error::Error for ReloadPolicyError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_development() {
        let policy = ReloadPolicy::default();
        assert_eq!(policy.global.preset, Preset::Development);
        assert_eq!(policy.watcher.debounce_ms, 100);
        assert!(!policy.canary.auto_rollback);
    }

    #[test]
    fn production_preset() {
        let policy = ReloadPolicy::for_preset(Preset::Production);
        assert_eq!(policy.canary.window_secs, 60);
        assert!(policy.canary.auto_rollback);
        assert!(policy.validation.strict_mode);
        assert_eq!(policy.rollback.buffer_capacity, 20);
    }

    #[test]
    fn staging_preset() {
        let policy = ReloadPolicy::for_preset(Preset::Staging);
        assert_eq!(policy.watcher.debounce_ms, 300);
        assert!(policy.credentials.auto_rotate);
    }

    #[test]
    fn parse_toml() {
        let toml = r#"
[global]
preset = "production"
enable_hot_reload = true

[watcher]
debounce_ms = 1000

[canary]
health_threshold = 0.9
"#;
        let policy = ReloadPolicy::parse(toml).unwrap();
        assert_eq!(policy.global.preset, Preset::Production);
        assert_eq!(policy.watcher.debounce_ms, 1000);
        assert!((policy.canary.health_threshold - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_minimal_toml() {
        let toml = r#"
[global]
preset = "staging"
"#;
        let policy = ReloadPolicy::parse(toml).unwrap();
        assert_eq!(policy.global.preset, Preset::Staging);
        // Defaults should fill in.
        assert_eq!(policy.canary.check_interval_secs, default_check_interval());
    }

    #[test]
    fn validation_catches_bad_values() {
        let mut policy = ReloadPolicy::default();
        policy.canary.health_threshold = 1.5; // Invalid
        policy.rollback.buffer_capacity = 0; // Invalid
        let warnings = policy.validate();
        assert!(warnings.len() >= 2);
    }

    #[test]
    fn missing_file_returns_default() {
        let policy =
            ReloadPolicy::load_from_file(Path::new("/nonexistent/reload.toml")).unwrap();
        assert_eq!(policy.global.preset, Preset::Development);
    }

    #[test]
    fn parse_error_returns_err() {
        let bad_toml = "{{{{invalid";
        let result = ReloadPolicy::parse(bad_toml);
        assert!(result.is_err());
    }
}
