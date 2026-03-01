//! Channel factory — configuration-driven channel construction.
//!
//! ## Design (Open-Closed Principle)
//!
//! The factory is open for extension (register new channel kinds at runtime)
//! but closed for modification (existing constructors are immutable).
//!
//! Type-theoretically, the factory is a dependent product:
//!   `Factory : Π(kind : String) → (Config(kind) → Result<Channel>)`
//!
//! Since Rust lacks dependent types, we approximate with type-erased
//! constructors over a format-agnostic `ChannelConfig`.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use clawdesk_channels::factory::{ChannelFactory, ChannelConfig};
//!
//! let factory = ChannelFactory::with_builtins();
//! let config = ChannelConfig::new("telegram", serde_json::Map::from_iter([
//!     ("bot_token".into(), serde_json::Value::String("...".into())),
//! ]));
//! let channel = factory.create(&config).unwrap();
//! ```
//!
//! ## Extensibility
//!
//! Register custom channel types before building `GatewayState`:
//! ```rust,no_run
//! # use clawdesk_channels::factory::{ChannelFactory, ChannelConfig, ChannelConfigError};
//! let mut factory = ChannelFactory::with_builtins();
//! // factory.register("my_custom_channel", |config| {
//! //     Ok(Arc::new(CustomChannel::new(config.require_string("endpoint")?)))
//! // });
//! ```

use clawdesk_channel::Channel;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tracing::info;

// ---------------------------------------------------------------------------
// Config schema — typed validation per channel kind (T-07)
// ---------------------------------------------------------------------------

/// Describes the expected type for a config field.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigFieldType {
    String,
    StringArray,
    Bool,
    Integer,
    IntegerArray,
    UnsignedArray,
}

impl std::fmt::Display for ConfigFieldType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::String => write!(f, "string"),
            Self::Bool => write!(f, "bool"),
            Self::Integer => write!(f, "integer"),
            Self::IntegerArray => write!(f, "array<integer>"),
            Self::StringArray => write!(f, "array<string>"),
            Self::UnsignedArray => write!(f, "array<unsigned>"),
        }
    }
}

/// Schema for a single config field.
#[derive(Debug, Clone)]
pub struct ConfigFieldSchema {
    pub name: String,
    pub field_type: ConfigFieldType,
    pub required: bool,
    pub description: String,
}

/// Config schema for a channel kind — describes all accepted fields.
#[derive(Debug, Clone, Default)]
pub struct ConfigSchema {
    pub kind: String,
    pub fields: Vec<ConfigFieldSchema>,
}

impl ConfigSchema {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            fields: Vec::new(),
        }
    }

    pub fn required(mut self, name: &str, ft: ConfigFieldType, desc: &str) -> Self {
        self.fields.push(ConfigFieldSchema {
            name: name.to_string(),
            field_type: ft,
            required: true,
            description: desc.to_string(),
        });
        self
    }

    pub fn optional(mut self, name: &str, ft: ConfigFieldType, desc: &str) -> Self {
        self.fields.push(ConfigFieldSchema {
            name: name.to_string(),
            field_type: ft,
            required: false,
            description: desc.to_string(),
        });
        self
    }

    /// Validate a config map against this schema.
    /// Returns a list of validation errors (empty = valid).
    pub fn validate(&self, config: &ChannelConfig) -> Vec<String> {
        let mut errors = Vec::new();

        for field in &self.fields {
            match config.inner.get(&field.name) {
                None if field.required => {
                    errors.push(format!(
                        "{}: missing required field '{}' ({})",
                        self.kind, field.name, field.description
                    ));
                }
                None => {} // optional, absent — OK
                Some(val) => {
                    let type_ok = match field.field_type {
                        ConfigFieldType::String => val.is_string(),
                        ConfigFieldType::Bool => val.is_boolean(),
                        ConfigFieldType::Integer => val.is_i64(),
                        ConfigFieldType::IntegerArray => val
                            .as_array()
                            .map(|a| a.iter().all(|v| v.is_i64()))
                            .unwrap_or(false),
                        ConfigFieldType::StringArray => val
                            .as_array()
                            .map(|a| a.iter().all(|v| v.is_string()))
                            .unwrap_or(false),
                        ConfigFieldType::UnsignedArray => val
                            .as_array()
                            .map(|a| a.iter().all(|v| v.is_u64()))
                            .unwrap_or(false),
                    };
                    if !type_ok {
                        errors.push(format!(
                            "{}: field '{}' expected {}, got {}",
                            self.kind,
                            field.name,
                            field.field_type,
                            value_type_name(val),
                        ));
                    }
                }
            }
        }
        errors
    }
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from channel factory operations.
#[derive(Debug, Error)]
pub enum ChannelConfigError {
    /// A required configuration key is missing.
    #[error("channel '{channel_kind}': missing required key '{key}'")]
    MissingKey { key: String, channel_kind: String },

    /// A configuration value has the wrong type.
    #[error("channel '{channel_kind}', key '{key}': expected {expected}, got {actual}")]
    WrongType {
        key: String,
        channel_kind: String,
        expected: &'static str,
        actual: String,
    },

    /// An unknown channel kind was requested.
    #[error("unknown channel kind: '{0}' (available: telegram, discord, slack, webchat, internal)")]
    UnknownKind(String),

    /// Channel construction failed.
    #[error("failed to construct channel '{kind}': {reason}")]
    ConstructionFailed { kind: String, reason: String },

    /// Config schema validation failed.
    #[error("channel '{kind}' config validation failed:\n{errors}")]
    SchemaValidation { kind: String, errors: String },
}

// ---------------------------------------------------------------------------
// ChannelConfig — type-erased, format-agnostic configuration
// ---------------------------------------------------------------------------

/// Type-erased channel configuration.
///
/// Backed by `serde_json::Map` for universal format compatibility — can
/// be populated from TOML, JSON, YAML, or environment variables.
///
/// ## Type-theoretic view
///
/// `ChannelConfig` is a dependent record type:
///   `Π(key : String) → Option(ValueType(key))`
/// where `ValueType` is resolved at extraction time via typed accessors.
pub struct ChannelConfig {
    kind: String,
    inner: Map<String, Value>,
}

impl ChannelConfig {
    /// Create from a kind string and a JSON-equivalent map.
    pub fn new(kind: impl Into<String>, map: Map<String, Value>) -> Self {
        Self {
            kind: kind.into(),
            inner: map,
        }
    }

    /// Create from a kind string and a `serde_json::Value` (must be Object).
    pub fn from_value(kind: impl Into<String>, value: Value) -> Result<Self, ChannelConfigError> {
        let kind = kind.into();
        match value {
            Value::Object(map) => Ok(Self { kind, inner: map }),
            other => Err(ChannelConfigError::WrongType {
                key: "<root>".into(),
                channel_kind: kind,
                expected: "object/table",
                actual: format!("{}", other),
            }),
        }
    }

    /// Channel kind identifier.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Required string value.
    pub fn require_string(&self, key: &str) -> Result<String, ChannelConfigError> {
        self.inner
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ChannelConfigError::MissingKey {
                key: key.to_string(),
                channel_kind: self.kind.clone(),
            })
    }

    /// Optional string with default.
    pub fn string_or(&self, key: &str, default: &str) -> String {
        self.inner
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }

    /// Boolean with default.
    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.inner
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    }

    /// Array of i64 (e.g., Telegram allowed_chat_ids).
    pub fn i64_array(&self, key: &str) -> Vec<i64> {
        self.inner
            .get(key)
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
            .unwrap_or_default()
    }

    /// Array of strings (e.g., Discord allowed user IDs).
    pub fn string_array(&self, key: &str) -> Vec<String> {
        self.inner
            .get(key)
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    /// Array of u64 (e.g., Discord guild IDs).
    pub fn u64_array(&self, key: &str) -> Vec<u64> {
        self.inner
            .get(key)
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
            .unwrap_or_default()
    }

    /// Optional usize with default.
    pub fn usize_or(&self, key: &str, default: usize) -> usize {
        self.inner
            .get(key)
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(default)
    }

    /// Check if a key exists.
    pub fn has(&self, key: &str) -> bool {
        self.inner.contains_key(key)
    }

    /// Access the raw inner map.
    pub fn raw(&self) -> &Map<String, Value> {
        &self.inner
    }
}

// ---------------------------------------------------------------------------
// ChannelFactory — type-erased constructor registry
// ---------------------------------------------------------------------------

/// Type-erased channel constructor function.
type ChannelConstructor =
    Arc<dyn Fn(&ChannelConfig) -> Result<Arc<dyn Channel>, ChannelConfigError> + Send + Sync>;

/// Channel factory — maps `(kind, config) → Arc<dyn Channel>`.
///
/// ## Thread safety
///
/// The factory is immutable after construction (`Send + Sync`).
/// Channel constructors run synchronously; any async initialization
/// (e.g., verifying bot tokens) should happen in the channel's
/// `start()` method, not in the constructor.
///
/// ## Extensibility
///
/// Register custom channel types before building `GatewayState`:
/// ```rust,no_run
/// # use clawdesk_channels::factory::{ChannelFactory, ChannelConfig, ChannelConfigError};
/// # use std::sync::Arc;
/// let mut factory = ChannelFactory::with_builtins();
/// factory.register("custom", |config| {
///     let token = config.require_string("token")?;
///     // Ok(Arc::new(CustomChannel::new(token)))
///     todo!()
/// });
/// ```
pub struct ChannelFactory {
    constructors: HashMap<String, ChannelConstructor>,
    schemas: HashMap<String, ConfigSchema>,
}

impl ChannelFactory {
    /// Create an empty factory (no built-in channels).
    pub fn new() -> Self {
        Self {
            constructors: HashMap::new(),
            schemas: HashMap::new(),
        }
    }

    /// Register a constructor for a channel kind.
    ///
    /// Overwrites any existing constructor for the same kind.
    pub fn register<F>(&mut self, kind: &str, constructor: F)
    where
        F: Fn(&ChannelConfig) -> Result<Arc<dyn Channel>, ChannelConfigError> + Send + Sync + 'static,
    {
        info!(kind, "registered channel constructor");
        self.constructors
            .insert(kind.to_string(), Arc::new(constructor));
    }

    /// Register a constructor **with** a typed config schema.
    pub fn register_with_schema<F>(&mut self, kind: &str, schema: ConfigSchema, constructor: F)
    where
        F: Fn(&ChannelConfig) -> Result<Arc<dyn Channel>, ChannelConfigError> + Send + Sync + 'static,
    {
        info!(kind, "registered channel constructor with schema");
        self.schemas.insert(kind.to_string(), schema);
        self.constructors
            .insert(kind.to_string(), Arc::new(constructor));
    }

    /// Create a channel instance from configuration.
    ///
    /// If a schema is registered for the kind, validates config first.
    pub fn create(&self, config: &ChannelConfig) -> Result<Arc<dyn Channel>, ChannelConfigError> {
        // Schema validation gate.
        if let Some(schema) = self.schemas.get(config.kind()) {
            let errors = schema.validate(config);
            if !errors.is_empty() {
                return Err(ChannelConfigError::SchemaValidation {
                    kind: config.kind().to_string(),
                    errors: errors.join("\n"),
                });
            }
        }

        let ctor = self
            .constructors
            .get(config.kind())
            .ok_or_else(|| ChannelConfigError::UnknownKind(config.kind().to_string()))?;
        ctor(config)
    }

    /// Get the config schema for a channel kind (if registered).
    pub fn schema(&self, kind: &str) -> Option<&ConfigSchema> {
        self.schemas.get(kind)
    }

    /// List all registered channel kinds.
    pub fn available_kinds(&self) -> Vec<&str> {
        self.constructors.keys().map(|s| s.as_str()).collect()
    }

    /// Create a factory pre-loaded with all built-in channel constructors.
    ///
    /// Built-in channels:
    /// - `telegram` — Telegram Bot API (long-polling)
    /// - `discord` — Discord Bot Gateway (WebSocket v10)
    /// - `slack` — Slack Socket Mode
    /// - `webchat` — Gateway WebSocket bridge
    /// - `internal` — In-process testing channel
    pub fn with_builtins() -> Self {
        let mut f = Self::new();

        // --- Telegram ---
        // Required: bot_token
        // Optional: allowed_chat_ids (default: []), enable_groups (default: false)
        let telegram_schema = ConfigSchema::new("telegram")
            .required("bot_token", ConfigFieldType::String, "Telegram Bot API token")
            .optional("allowed_chat_ids", ConfigFieldType::IntegerArray, "Restrict to these chat IDs")
            .optional("enable_groups", ConfigFieldType::Bool, "Allow group chats");
        f.register_with_schema("telegram", telegram_schema, |config| {
            let bot_token = config.require_string("bot_token")?;
            let allowed_chat_ids = config.i64_array("allowed_chat_ids");
            let enable_groups = config.bool_or("enable_groups", false);
            Ok(Arc::new(crate::telegram::TelegramChannel::new(
                bot_token,
                allowed_chat_ids,
                enable_groups,
            )))
        });

        // --- Discord ---
        // Required: bot_token, application_id
        // Optional: allowed_guild_ids (default: [])
        let discord_schema = ConfigSchema::new("discord")
            .required("bot_token", ConfigFieldType::String, "Discord bot token")
            .required("application_id", ConfigFieldType::String, "Discord application ID")
            .optional("allowed_guild_ids", ConfigFieldType::UnsignedArray, "Restrict to these guild IDs")
            .optional("allowed_users", ConfigFieldType::StringArray, "Allowed user IDs (\"*\" = everyone)")
            .optional("listen_to_bots", ConfigFieldType::Bool, "Process messages from other bots")
            .optional("mention_only", ConfigFieldType::Bool, "Only respond when @mentioned")
            .optional("default_channel_id", ConfigFieldType::String, "Default Discord channel ID for cross-channel sends");
        f.register_with_schema("discord", discord_schema, |config| {
            let bot_token = config.require_string("bot_token")?;
            let application_id = config.require_string("application_id")?;
            let allowed_guild_ids = config.u64_array("allowed_guild_ids");
            let allowed_users = {
                let users = config.string_array("allowed_users");
                if users.is_empty() { vec!["*".to_string()] } else { users }
            };
            let listen_to_bots = config.bool_or("listen_to_bots", false);
            let mention_only = config.bool_or("mention_only", false);
            let default_channel_id = {
                let s = config.string_or("default_channel_id", "");
                if s.is_empty() { None } else { s.parse::<u64>().ok() }
            };
            Ok(Arc::new(crate::discord::DiscordChannel::new(
                bot_token,
                application_id,
                allowed_guild_ids,
                allowed_users,
                listen_to_bots,
                mention_only,
                default_channel_id,
            )))
        });

        // --- Slack ---
        // Required: bot_token, app_token, signing_secret
        let slack_schema = ConfigSchema::new("slack")
            .required("bot_token", ConfigFieldType::String, "Slack bot token")
            .required("app_token", ConfigFieldType::String, "Slack app-level token")
            .required("signing_secret", ConfigFieldType::String, "Slack signing secret");
        f.register_with_schema("slack", slack_schema, |config| {
            let bot_token = config.require_string("bot_token")?;
            let app_token = config.require_string("app_token")?;
            let signing_secret = config.require_string("signing_secret")?;
            Ok(Arc::new(crate::slack::SlackChannel::new(
                bot_token,
                app_token,
                signing_secret,
            )))
        });

        // --- WebChat ---
        // No required config. The initial broadcast receiver is dropped;
        // consumers should call WebChatChannel::subscribe() to get a receiver.
        f.register("webchat", |_config| {
            let (ch, _initial_rx) = crate::webchat::WebChatChannel::new();
            Ok(Arc::new(ch))
        });

        // --- Internal (testing) ---
        f.register("internal", |_config| {
            Ok(Arc::new(crate::internal::InternalChannel::new()))
        });

        // --- WhatsApp ---
        // Required: phone_number_id, access_token, verify_token
        let whatsapp_schema = ConfigSchema::new("whatsapp")
            .required("phone_number_id", ConfigFieldType::String, "WhatsApp phone number ID")
            .required("access_token", ConfigFieldType::String, "WhatsApp Cloud API access token")
            .optional("verify_token", ConfigFieldType::String, "Webhook verify token");
        f.register_with_schema("whatsapp", whatsapp_schema, |config| {
            let phone_number_id = config.require_string("phone_number_id")?;
            let access_token = config.require_string("access_token")?;
            let verify_token = config.string_or("verify_token", "clawdesk");
            Ok(Arc::new(crate::whatsapp::WhatsAppChannel::new(
                phone_number_id,
                access_token,
                verify_token,
            )))
        });

        // --- Email ---
        // Required: imap_host, smtp_host, email, password
        let email_schema = ConfigSchema::new("email")
            .required("imap_host", ConfigFieldType::String, "IMAP server hostname")
            .required("smtp_host", ConfigFieldType::String, "SMTP server hostname")
            .required("email", ConfigFieldType::String, "Email address")
            .required("password", ConfigFieldType::String, "Email password or app password");
        f.register_with_schema("email", email_schema, |config| {
            let imap_host = config.require_string("imap_host")?;
            let smtp_host = config.require_string("smtp_host")?;
            let email_addr = config.require_string("email")?;
            let password = config.require_string("password")?;
            Ok(Arc::new(crate::email::EmailChannel::new(
                crate::email::EmailConfig {
                    imap: crate::email::ImapConfig {
                        host: imap_host,
                        port: 993,
                        username: email_addr.clone(),
                        password: password.clone(),
                        use_tls: true,
                    },
                    smtp: crate::email::SmtpConfig {
                        host: smtp_host,
                        port: 587,
                        username: email_addr.clone(),
                        password,
                        use_tls: true,
                    },
                    from_address: email_addr,
                    from_name: "ClawDesk".into(),
                    mailbox: "INBOX".into(),
                    poll_interval_secs: 30,
                },
            )))
        });

        // --- iMessage (macOS only) ---
        // Optional: allowed_contacts (default: ["*"]), poll_interval_secs (default: 3)
        let imessage_schema = ConfigSchema::new("imessage")
            .optional("allowed_contacts", ConfigFieldType::StringArray, "Allowed contacts (phone/email). [\"*\"] = everyone")
            .optional("poll_interval_secs", ConfigFieldType::Integer, "Polling interval in seconds (default: 3)");
        f.register_with_schema("imessage", imessage_schema, |config| {
            let allowed_contacts = {
                let contacts = config.string_array("allowed_contacts");
                if contacts.is_empty() { vec!["*".to_string()] } else { contacts }
            };
            let poll_interval = config.usize_or("poll_interval_secs", 3) as u64;
            Ok(Arc::new(crate::imessage::IMessageChannel::new(
                allowed_contacts,
                poll_interval,
            )))
        });

        // --- IRC ---
        // Required: server, nickname
        // Optional: port, username, channels, allowed_users, passwords, verify_tls
        let irc_schema = ConfigSchema::new("irc")
            .required("server", ConfigFieldType::String, "IRC server hostname")
            .required("nickname", ConfigFieldType::String, "Bot nickname")
            .optional("port", ConfigFieldType::Integer, "Server port (default: 6697)")
            .optional("username", ConfigFieldType::String, "IRC username (default: nickname)")
            .optional("channels", ConfigFieldType::StringArray, "Channels to join (e.g. [\"#general\"])")
            .optional("allowed_users", ConfigFieldType::StringArray, "Allowed user nicks (\"*\" = everyone)")
            .optional("server_password", ConfigFieldType::String, "Server password")
            .optional("nickserv_password", ConfigFieldType::String, "NickServ IDENTIFY password")
            .optional("sasl_password", ConfigFieldType::String, "SASL PLAIN password")
            .optional("verify_tls", ConfigFieldType::Bool, "Verify TLS certificates (default: true)");
        f.register_with_schema("irc", irc_schema, |config| {
            let server = config.require_string("server")?;
            let nickname = config.require_string("nickname")?;
            let port = config.usize_or("port", 6697) as u16;
            let username = config.raw().get("username").and_then(|v| v.as_str()).map(String::from);
            let channels = config.string_array("channels");
            let allowed_users = {
                let users = config.string_array("allowed_users");
                if users.is_empty() { vec!["*".to_string()] } else { users }
            };
            let server_password = config.raw().get("server_password").and_then(|v| v.as_str()).map(String::from);
            let nickserv_password = config.raw().get("nickserv_password").and_then(|v| v.as_str()).map(String::from);
            let sasl_password = config.raw().get("sasl_password").and_then(|v| v.as_str()).map(String::from);
            let verify_tls = config.bool_or("verify_tls", true);
            Ok(Arc::new(crate::irc::IrcChannel::new(
                crate::irc::IrcChannelConfig {
                    server,
                    port,
                    nickname,
                    username,
                    channels,
                    allowed_users,
                    server_password,
                    nickserv_password,
                    sasl_password,
                    verify_tls,
                },
            )))
        });

        f
    }
}

impl Default for ChannelFactory {
    /// Default factory includes all built-in channels.
    fn default() -> Self {
        Self::with_builtins()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_creates_internal_channel() {
        let factory = ChannelFactory::with_builtins();
        let config = ChannelConfig::new("internal", Map::new());
        let ch = factory.create(&config);
        assert!(ch.is_ok());
    }

    #[test]
    fn factory_rejects_unknown_kind() {
        let factory = ChannelFactory::with_builtins();
        let config = ChannelConfig::new("carrier_pigeon", Map::new());
        match factory.create(&config) {
            Err(err) => assert!(matches!(err, ChannelConfigError::UnknownKind(_))),
            Ok(_) => panic!("expected UnknownKind error"),
        }
    }

    #[test]
    fn telegram_requires_bot_token() {
        let factory = ChannelFactory::with_builtins();
        let config = ChannelConfig::new("telegram", Map::new());
        match factory.create(&config) {
            Err(err) => assert!(
                matches!(err, ChannelConfigError::MissingKey { .. } | ChannelConfigError::SchemaValidation { .. }),
                "expected MissingKey or SchemaValidation error, got: {err}"
            ),
            Ok(_) => panic!("expected error for missing bot_token"),
        }
    }

    #[test]
    fn config_typed_extraction() {
        let mut map = Map::new();
        map.insert("name".into(), Value::String("test".into()));
        map.insert("count".into(), Value::Number(42.into()));
        map.insert("enabled".into(), Value::Bool(true));
        map.insert(
            "ids".into(),
            Value::Array(vec![Value::Number(1.into()), Value::Number(2.into())]),
        );

        let config = ChannelConfig::new("test", map);
        assert_eq!(config.require_string("name").unwrap(), "test");
        assert_eq!(config.bool_or("enabled", false), true);
        assert_eq!(config.bool_or("missing", false), false);
        assert_eq!(config.i64_array("ids"), vec![1i64, 2]);
        assert_eq!(config.usize_or("count", 0), 42);
        assert!(config.has("name"));
        assert!(!config.has("nonexistent"));
    }

    #[test]
    fn available_kinds_lists_builtins() {
        let factory = ChannelFactory::with_builtins();
        let kinds = factory.available_kinds();
        assert!(kinds.contains(&"telegram"));
        assert!(kinds.contains(&"discord"));
        assert!(kinds.contains(&"slack"));
        assert!(kinds.contains(&"webchat"));
        assert!(kinds.contains(&"internal"));
    }

    #[test]
    fn schema_validation_catches_wrong_type() {
        let schema = ConfigSchema::new("test")
            .required("token", ConfigFieldType::String, "API token")
            .optional("retries", ConfigFieldType::Integer, "Retry count");

        let mut map = Map::new();
        map.insert("token".into(), Value::Number(123.into())); // wrong type
        map.insert("retries".into(), Value::String("not_a_number".into())); // wrong type

        let config = ChannelConfig::new("test", map);
        let errors = schema.validate(&config);
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("expected string"));
        assert!(errors[1].contains("expected integer"));
    }

    #[test]
    fn schema_validation_catches_missing_required() {
        let schema = ConfigSchema::new("test")
            .required("token", ConfigFieldType::String, "API token");

        let config = ChannelConfig::new("test", Map::new());
        let errors = schema.validate(&config);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("missing required field 'token'"));
    }

    #[test]
    fn schema_validation_passes_valid_config() {
        let schema = ConfigSchema::new("test")
            .required("token", ConfigFieldType::String, "API token")
            .optional("debug", ConfigFieldType::Bool, "Debug mode");

        let mut map = Map::new();
        map.insert("token".into(), Value::String("abc".into()));
        let config = ChannelConfig::new("test", map);
        let errors = schema.validate(&config);
        assert!(errors.is_empty());
    }

    #[test]
    fn factory_schema_query() {
        let factory = ChannelFactory::with_builtins();
        let schema = factory.schema("telegram").expect("telegram schema");
        assert_eq!(schema.kind, "telegram");
        let required_fields: Vec<&str> = schema
            .fields
            .iter()
            .filter(|f| f.required)
            .map(|f| f.name.as_str())
            .collect();
        assert!(required_fields.contains(&"bot_token"));
    }
}
