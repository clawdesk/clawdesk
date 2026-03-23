//! Configuration as algebraic product type.
//!
//! Single Rust struct hierarchy replaces 80+ config files.
//! `ValidatedConfig` newtype ensures the compiler refuses
//! to pass raw config to business logic.

use serde::{Deserialize, Serialize};

/// Top-level ClawDesk configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClawDeskConfig {
    #[serde(default)]
    pub agents: AgentConfig,
    #[serde(default)]
    pub channels: ChannelConfigs,
    #[serde(default)]
    pub providers: ProviderConfigs,
    #[serde(default)]
    pub session: crate::session::SessionConfig,
    #[serde(default)]
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub security: SecurityConfig,
}

impl Default for ClawDeskConfig {
    fn default() -> Self {
        Self {
            agents: AgentConfig::default(),
            channels: ChannelConfigs::default(),
            providers: ProviderConfigs::default(),
            session: crate::session::SessionConfig::default(),
            gateway: GatewayConfig::default(),
            security: SecurityConfig::default(),
        }
    }
}

/// Validated config — distinct type from raw config.
/// The rest of the system only accepts `ValidatedConfig`.
#[derive(Debug, Clone)]
pub struct ValidatedConfig(ClawDeskConfig);

impl ValidatedConfig {
    /// Validate raw config. Returns errors if invalid.
    pub fn from_raw(raw: ClawDeskConfig) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();

        // Gateway validation
        if raw.gateway.port == 0 {
            errors.push("gateway.port must be > 0".to_string());
        }

        // Agent config validation
        if raw.agents.default_model.is_empty() {
            errors.push("agents.default_model must not be empty".to_string());
        }
        if raw.agents.max_tool_iterations == 0 {
            errors.push("agents.max_tool_iterations must be > 0".to_string());
        }
        if raw.agents.timeout_seconds == 0 {
            errors.push("agents.timeout_seconds must be > 0".to_string());
        }

        // Security validation
        if raw.security.max_file_size_bytes == 0 {
            errors.push("security.max_file_size_bytes must be > 0".to_string());
        }

        // Provider validation — warn if no providers, but not an error
        // (desktop mode can add keys later via UI)

        // Provider-specific validation: if configured, keys must be present
        if let Some(ref anthropic) = raw.providers.anthropic {
            if anthropic.api_key.is_none() && anthropic.api_key_ref.is_none() {
                errors.push(
                    "providers.anthropic configured but neither api_key nor api_key_ref set"
                        .to_string(),
                );
            }
        }
        if let Some(ref openai) = raw.providers.openai {
            if openai.api_key.is_none() && openai.api_key_ref.is_none() {
                errors.push(
                    "providers.openai configured but neither api_key nor api_key_ref set"
                        .to_string(),
                );
            }
        }
        if let Some(ref google) = raw.providers.google {
            if google.api_key.is_none() && google.api_key_ref.is_none() {
                errors.push(
                    "providers.google configured but neither api_key nor api_key_ref set"
                        .to_string(),
                );
            }
        }

        // Channel validation: if configured, required fields must be non-empty
        if let Some(ref telegram) = raw.channels.telegram {
            if telegram.bot_token.is_empty() {
                errors.push("channels.telegram.bot_token must not be empty".to_string());
            }
        }
        if let Some(ref discord) = raw.channels.discord {
            if discord.bot_token.is_empty() || discord.application_id.is_empty() {
                errors.push(
                    "channels.discord requires both bot_token and application_id".to_string(),
                );
            }
        }
        if let Some(ref slack) = raw.channels.slack {
            if slack.bot_token.is_empty()
                || slack.app_token.is_empty()
                || slack.signing_secret.is_empty()
            {
                errors.push(
                    "channels.slack requires bot_token, app_token, and signing_secret".to_string(),
                );
            }
        }

        if errors.is_empty() {
            Ok(ValidatedConfig(raw))
        } else {
            Err(errors)
        }
    }

    pub fn inner(&self) -> &ClawDeskConfig {
        &self.0
    }

    pub fn into_inner(self) -> ClawDeskConfig {
        self.0
    }
}

/// Agent runner configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub default_model: String,
    pub max_tool_iterations: u32,
    pub timeout_seconds: u64,
    pub enable_streaming: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_model: "auto".to_string(),
            max_tool_iterations: 10,
            timeout_seconds: 120,
            enable_streaming: true,
        }
    }
}

/// Channel configurations — only configured channels are present.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelConfigs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telegram: Option<TelegramConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discord: Option<DiscordConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slack: Option<SlackConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub whatsapp: Option<WhatsAppConfig>,
}

/// LLM provider configurations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderConfigs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<AnthropicConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai: Option<OpenAiConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google: Option<GoogleConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ollama: Option<OllamaConfig>,
}

/// Gateway server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub port: u16,
    pub bind_address: String,
    pub auth_mode: AuthMode,
    pub auth_token: Option<String>,
    pub cors_origins: Vec<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: 18789,
            bind_address: "127.0.0.1".to_string(),
            auth_mode: AuthMode::Token,
            auth_token: None,
            cors_origins: vec!["http://localhost:*".to_string()],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    None,
    Token,
    DeviceIdentity,
}

/// Security configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub enable_bash_tool: bool,
    pub allowed_commands: Vec<String>,
    pub sandbox_enabled: bool,
    pub max_file_size_bytes: u64,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enable_bash_tool: false,
            allowed_commands: vec![],
            sandbox_enabled: true,
            max_file_size_bytes: 50 * 1024 * 1024, // 50MB
        }
    }
}

// ---------------------------------------------------------------------------
// Per-channel config structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,
    #[serde(default = "default_true")]
    pub enable_groups: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    pub bot_token: String,
    pub application_id: String,
    #[serde(default)]
    pub allowed_guild_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    pub bot_token: String,
    pub app_token: String,
    pub signing_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppConfig {
    pub phone_number: String,
    pub api_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Per-provider config structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    /// Reference to keychain entry, not raw key
    pub api_key_ref: Option<String>,
    /// Direct API key (for dev/testing only)
    pub api_key: Option<String>,
    pub default_model: Option<String>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiConfig {
    pub api_key_ref: Option<String>,
    pub api_key: Option<String>,
    pub default_model: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleConfig {
    pub api_key_ref: Option<String>,
    pub api_key: Option<String>,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    #[serde(default = "default_ollama_url")]
    pub base_url: String,
    pub default_model: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_max_tokens() -> u32 {
    8192
}

fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}
