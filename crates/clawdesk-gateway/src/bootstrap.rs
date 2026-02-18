//! Bootstrap — configuration-driven gateway startup.
//!
//! Reads a TOML configuration file and constructs the plug-and-play
//! components (channels, skills) that compose into `GatewayState`.
//!
//! ## Config file format
//!
//! ```toml
//! [gateway]
//! host = "127.0.0.1"
//! port = 18789
//! cors_origins = ["http://localhost:*"]
//! admin_token = ""
//!
//! [channels.telegram]
//! bot_token = "${TELEGRAM_BOT_TOKEN}"
//! allowed_chat_ids = []
//! enable_groups = true
//!
//! [channels.webchat]
//! # no required config — always available
//!
//! [channels.discord]
//! enabled = false
//! bot_token = "${DISCORD_BOT_TOKEN}"
//! application_id = "${DISCORD_APP_ID}"
//!
//! [skills]
//! dir = "~/.clawdesk/skills"
//! auto_activate = true
//! token_budget = 4096
//! ```
//!
//! ## Environment variable expansion
//!
//! String values support `${ENV_VAR}` expansion for secrets:
//! ```toml
//! [channels.telegram]
//! bot_token = "${TELEGRAM_BOT_TOKEN}"
//! ```

use crate::GatewayConfig;
use clawdesk_channel::registry::ChannelRegistry;
use clawdesk_channels::factory::{ChannelConfig, ChannelFactory};
use clawdesk_skills::loader::SkillLoader;
use clawdesk_skills::registry::SkillRegistry;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Top-level ClawDesk configuration (deserialized from TOML).
#[derive(Debug, Clone, Deserialize)]
pub struct ClawDeskConfig {
    /// Gateway server settings.
    #[serde(default)]
    pub gateway: GatewayConfigFile,

    /// Channel configurations keyed by kind name.
    /// Each key is the channel kind (e.g., "telegram", "discord").
    /// The value contains `enabled` + channel-specific settings.
    #[serde(default)]
    pub channels: HashMap<String, ChannelEntry>,

    /// Skill system configuration.
    #[serde(default)]
    pub skills: SkillsConfig,
}

/// Gateway config as it appears in the TOML file.
///
/// Mirrors `GatewayConfig` but with serde Deserialize + defaults.
#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfigFile {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub cors_origins: Vec<String>,
    #[serde(default)]
    pub admin_token: String,
}

impl Default for GatewayConfigFile {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            cors_origins: vec!["http://localhost:*".to_string()],
            admin_token: String::new(),
        }
    }
}

impl From<GatewayConfigFile> for GatewayConfig {
    fn from(f: GatewayConfigFile) -> Self {
        Self {
            host: f.host,
            port: f.port,
            cors_origins: if f.cors_origins.is_empty() {
                vec!["http://localhost:*".to_string()]
            } else {
                f.cors_origins
            },
            admin_token: f.admin_token,
        }
    }
}

/// Per-channel configuration entry.
///
/// The `enabled` flag controls whether the channel is instantiated.
/// All other fields are channel-specific and captured via `#[serde(flatten)]`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChannelEntry {
    /// Whether this channel is enabled (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Channel-specific settings — all non-`enabled` keys are captured here.
    /// Format-agnostic: works with TOML, JSON, or any serde-compatible format.
    #[serde(flatten)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// Skill system configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct SkillsConfig {
    /// Directory to scan for skill definitions.
    #[serde(default = "default_skills_dir")]
    pub dir: String,

    /// Automatically activate all successfully loaded skills.
    #[serde(default = "default_true")]
    pub auto_activate: bool,

    /// Token budget for skill prompt fragments in the system prompt.
    #[serde(default = "default_token_budget")]
    pub token_budget: usize,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            dir: default_skills_dir(),
            auto_activate: true,
            token_budget: default_token_budget(),
        }
    }
}

// ---------------------------------------------------------------------------
// Bootstrap result types
// ---------------------------------------------------------------------------

/// Result of the bootstrap process — everything needed for plug-and-play
/// components of `GatewayState`.
pub struct BootstrapResult {
    /// Constructed channel registry with all enabled channels.
    pub channels: ChannelRegistry,
    /// Loaded skill registry (optionally auto-activated).
    pub skills: SkillRegistry,
    /// Skill loader — retained for hot-reload.
    pub skill_loader: SkillLoader,
    /// Channel factory — retained for hot-reload and runtime extension.
    pub channel_factory: ChannelFactory,
    /// Parsed gateway config.
    pub gateway_config: GatewayConfig,
    /// Skills config — retained for reload operations.
    pub skills_config: SkillsConfig,
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

impl ClawDeskConfig {
    /// Load configuration from a TOML file with environment variable expansion.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read config '{}': {}", path.display(), e))?;
        let expanded = expand_env_vars(&content);
        toml::from_str(&expanded)
            .map_err(|e| format!("failed to parse config '{}': {}", path.display(), e))
    }

    /// Default config file path.
    ///
    /// Checks `CLAWDESK_CONFIG` env var, falls back to `~/.clawdesk/config.toml`.
    pub fn default_path() -> PathBuf {
        std::env::var("CLAWDESK_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join(".clawdesk").join("config.toml"))
    }

    /// Load from the default path, returning a default config if the file doesn't exist.
    pub fn load_or_default() -> Self {
        let path = Self::default_path();
        if path.exists() {
            match Self::load(&path) {
                Ok(config) => {
                    info!(path = %path.display(), "loaded configuration");
                    config
                }
                Err(e) => {
                    error!(%e, "failed to load config, using defaults");
                    Self::default()
                }
            }
        } else {
            info!("no config file found, using defaults");
            Self::default()
        }
    }
}

impl Default for ClawDeskConfig {
    fn default() -> Self {
        Self {
            gateway: GatewayConfigFile::default(),
            channels: HashMap::new(),
            skills: SkillsConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Bootstrap functions
// ---------------------------------------------------------------------------

/// Bootstrap channels and skills from configuration.
///
/// This is the main entry point for plug-and-play startup:
/// 1. Creates a `ChannelFactory` with built-in constructors
/// 2. Constructs enabled channels from config
/// 3. Loads skills from the filesystem
/// 4. Returns everything needed for `GatewayState`
///
/// The caller (CLI or Tauri binary) composes the result with
/// other dependencies (providers, tools, store, etc.) to build
/// the full `GatewayState`.
pub async fn bootstrap(config: &ClawDeskConfig) -> BootstrapResult {
    let factory = ChannelFactory::with_builtins();
    let mut channel_errors: Vec<String> = Vec::new();

    // --- Channels: config-driven construction ---
    let mut registry = ChannelRegistry::new();
    for (kind, entry) in &config.channels {
        if !entry.enabled {
            info!(kind, "channel disabled, skipping");
            continue;
        }

        let channel_config = ChannelConfig::new(kind.as_str(), entry.settings.clone());
        match factory.create(&channel_config) {
            Ok(ch) => {
                info!(kind, "channel created from config");
                match registry.register(ch) {
                    clawdesk_channel::registry::RegistrationResult::Ok { id, .. } => {
                        info!(%id, "channel registered with attestation");
                    }
                    clawdesk_channel::registry::RegistrationResult::Rejected { reason } => {
                        let msg = format!("channel '{}' registration rejected: {}", kind, reason);
                        error!(%msg);
                        channel_errors.push(msg);
                    }
                }
            }
            Err(e) => {
                let msg = format!("{}", e);
                error!(kind, error = %msg, "failed to create channel");
                channel_errors.push(msg);
            }
        }
    }

    // --- Skills: filesystem discovery ---
    let skills_dir = expand_tilde(&config.skills.dir);
    let skill_loader = SkillLoader::new(PathBuf::from(&skills_dir));
    let load_result = skill_loader.load_fresh(config.skills.auto_activate).await;

    if !load_result.errors.is_empty() {
        for e in &load_result.errors {
            warn!(error = %e, "skill load warning");
        }
    }

    info!(
        channels = registry.len(),
        channel_errors = channel_errors.len(),
        skills = load_result.registry.len(),
        skill_errors = load_result.errors.len(),
        "bootstrap complete"
    );

    BootstrapResult {
        channels: registry,
        skills: load_result.registry,
        skill_loader,
        channel_factory: factory,
        gateway_config: config.gateway.clone().into(),
        skills_config: config.skills.clone(),
    }
}

/// Bootstrap only channels from a factory and config map.
///
/// Useful for hot-reloading channels without touching skills.
pub fn bootstrap_channels(
    factory: &ChannelFactory,
    channels_config: &HashMap<String, ChannelEntry>,
) -> (ChannelRegistry, Vec<String>) {
    let mut registry = ChannelRegistry::new();
    let mut errors = Vec::new();

    for (kind, entry) in channels_config {
        if !entry.enabled {
            continue;
        }
        let config = ChannelConfig::new(kind.as_str(), entry.settings.clone());
        match factory.create(&config) {
            Ok(ch) => {
                if let clawdesk_channel::registry::RegistrationResult::Rejected { reason } = registry.register(ch) {
                    errors.push(format!("channel '{}' rejected: {}", kind, reason));
                }
            }
            Err(e) => errors.push(format!("{}", e)),
        }
    }

    (registry, errors)
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Expand `${ENV_VAR}` patterns in a string.
///
/// This enables secrets to be injected via environment variables
/// without storing them in the config file.
fn expand_env_vars(s: &str) -> String {
    let mut result = s.to_string();
    // Simple stateful scan: find ${...} patterns and replace.
    while let Some(start) = result.find("${") {
        if let Some(end) = result[start..].find('}') {
            let var_name = &result[start + 2..start + end];
            let value = std::env::var(var_name).unwrap_or_default();
            result = format!(
                "{}{}{}",
                &result[..start],
                value,
                &result[start + end + 1..]
            );
        } else {
            break; // malformed, stop
        }
    }
    result
}

/// Expand `~` prefix to the user's home directory.
fn expand_tilde(s: &str) -> String {
    if s.starts_with('~') {
        let home = home_dir();
        format!("{}{}", home.display(), &s[1..])
    } else {
        s.to_string()
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    18789
}

fn default_true() -> bool {
    true
}

fn default_skills_dir() -> String {
    "~/.clawdesk/skills".to_string()
}

fn default_token_budget() -> usize {
    4096
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_expansion() {
        std::env::set_var("TEST_CLAWDESK_TOKEN", "secret123");
        let input = "bot_token = \"${TEST_CLAWDESK_TOKEN}\"";
        let result = expand_env_vars(input);
        assert_eq!(result, "bot_token = \"secret123\"");
        std::env::remove_var("TEST_CLAWDESK_TOKEN");
    }

    #[test]
    fn tilde_expansion() {
        let result = expand_tilde("~/.clawdesk/skills");
        assert!(result.ends_with("/.clawdesk/skills"));
        assert!(!result.starts_with('~'));
    }

    #[test]
    fn default_config_is_valid() {
        let config = ClawDeskConfig::default();
        assert_eq!(config.gateway.port, 18789);
        assert!(config.channels.is_empty());
        assert!(config.skills.auto_activate);
        assert_eq!(config.skills.token_budget, 4096);
    }

    #[test]
    fn parse_minimal_toml() {
        let toml_str = r#"
[gateway]
port = 9999

[channels.webchat]

[skills]
token_budget = 2048
"#;
        let config: ClawDeskConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.gateway.port, 9999);
        assert!(config.channels.contains_key("webchat"));
        assert_eq!(config.skills.token_budget, 2048);
    }

    #[test]
    fn parse_channel_with_settings() {
        let toml_str = r#"
[channels.telegram]
enabled = true
bot_token = "test_token_123"
allowed_chat_ids = [111, 222]
enable_groups = true
"#;
        let config: ClawDeskConfig = toml::from_str(toml_str).unwrap();
        let tg = &config.channels["telegram"];
        assert!(tg.enabled);
        assert_eq!(
            tg.settings.get("bot_token").unwrap().as_str().unwrap(),
            "test_token_123"
        );
    }

    #[test]
    fn disabled_channel() {
        let toml_str = r#"
[channels.discord]
enabled = false
bot_token = "test"
application_id = "app123"
"#;
        let config: ClawDeskConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.channels["discord"].enabled);
    }
}
