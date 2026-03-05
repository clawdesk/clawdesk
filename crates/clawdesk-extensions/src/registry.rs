//! Integration registry — TOML-driven integration management.
//!
//! Loads integrations from two sources:
//! 1. Bundled TOML files embedded at compile time
//! 2. User-provided TOML files from `~/.clawdesk/extensions/`

use crate::ExtensionError;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Integration category
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IntegrationCategory {
    DevTools,
    Productivity,
    Data,
    Cloud,
    Search,
    Communication,
    Custom,
}

impl std::fmt::Display for IntegrationCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevTools => write!(f, "devtools"),
            Self::Productivity => write!(f, "productivity"),
            Self::Data => write!(f, "data"),
            Self::Cloud => write!(f, "cloud"),
            Self::Search => write!(f, "search"),
            Self::Communication => write!(f, "communication"),
            Self::Custom => write!(f, "custom"),
        }
    }
}

/// MCP transport configuration for an integration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TransportConfig {
    #[serde(rename = "stdio")]
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    #[serde(rename = "sse")]
    Sse { url: String },
    #[serde(rename = "api")]
    DirectApi {
        base_url: String,
        #[serde(default)]
        headers: std::collections::HashMap<String, String>,
    },
}

/// Credential requirement for an integration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialRequirement {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub env_var: Option<String>,
    #[serde(default)]
    pub required: bool,
}

// ── Per-extension configuration schema ────────────────────────

/// Field type for extension configuration values.
///
/// Determines UI rendering (input type) and validation rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFieldType {
    /// Free-form text input.
    Text,
    /// Numeric input (integer or float).
    Number,
    /// Boolean toggle.
    Boolean,
    /// Masked secret input — stored in credential vault, NOT in plain config.
    Secret,
    /// Dropdown select from a fixed set of options.
    Select,
    /// URL input with format validation.
    Url,
    /// File system path (may trigger a file picker in the UI).
    FilePath,
    /// Port number (1–65535).
    Port,
}

impl Default for ConfigFieldType {
    fn default() -> Self {
        Self::Text
    }
}

/// A single option for a `Select`-type config field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigFieldOption {
    pub label: String,
    pub value: String,
}

/// Declarative schema for one configurable parameter of an extension.
///
/// Extensions declare their config fields in the TOML template:
/// ```toml
/// [[config_fields]]
/// key = "base_url"
/// label = "Instance URL"
/// description = "Your Jira Cloud URL"
/// field_type = "url"
/// required = true
/// placeholder = "https://mycompany.atlassian.net"
/// default = "https://your-domain.atlassian.net"
/// ```
///
/// The runtime resolves `${KEY}` placeholders in transport config from
/// user-supplied config values → vault credentials → environment variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    /// Machine-readable key (used in SochDB and `${...}` interpolation).
    pub key: String,
    /// Human-readable label for UI rendering.
    #[serde(default)]
    pub label: String,
    /// Help text / tooltip.
    #[serde(default)]
    pub description: String,
    /// Determines the input type and validation rules.
    #[serde(default)]
    pub field_type: ConfigFieldType,
    /// Default value (used if user hasn't set one).
    #[serde(default)]
    pub default: Option<String>,
    /// Whether a value is required before the extension can be enabled.
    #[serde(default)]
    pub required: bool,
    /// Placeholder text for the input field.
    #[serde(default)]
    pub placeholder: Option<String>,
    /// Regex validation pattern (applied to Text/Url/Number inputs).
    #[serde(default)]
    pub validation: Option<String>,
    /// Fixed options for `Select`-type fields.
    #[serde(default)]
    pub options: Vec<ConfigFieldOption>,
    /// Optional grouping label (e.g. "Connection", "Authentication", "Advanced").
    #[serde(default)]
    pub group: Option<String>,
}

/// OAuth configuration for an integration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConfig {
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// A single integration definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Integration {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub category: IntegrationCategory,
    #[serde(default)]
    pub icon: Option<String>,
    pub transport: TransportConfig,
    #[serde(default)]
    pub credentials: Vec<CredentialRequirement>,
    #[serde(default)]
    pub oauth: Option<OAuthConfig>,
    #[serde(default)]
    pub health_check_url: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    /// Declarative schema of user-configurable parameters.
    ///
    /// Values set by the user are stored in SochDB and resolved at runtime
    /// via `${KEY}` interpolation in transport config strings.
    #[serde(default)]
    pub config_fields: Vec<ConfigField>,
}

impl Integration {
    /// Returns `true` if this integration has an MCP-connectable transport
    /// (Stdio or SSE). DirectApi integrations don't use MCP.
    pub fn is_mcp_connectable(&self) -> bool {
        matches!(self.transport, TransportConfig::Stdio { .. } | TransportConfig::Sse { .. })
    }
}

/// Integration registry — concurrent map of all known integrations.
///
/// O(1) lookup by name, O(n) filtered listing by category.
pub struct IntegrationRegistry {
    /// name → Integration
    integrations: DashMap<String, Integration>,
    /// name → user-configured values (non-secret settings).
    configs: DashMap<String, std::collections::HashMap<String, String>>,
    /// User extensions directory
    user_dir: PathBuf,
}

impl IntegrationRegistry {
    /// Create a new registry with default paths.
    pub fn new() -> Self {
        let user_dir = directories::ProjectDirs::from("dev", "clawdesk", "clawdesk")
            .map(|d| d.config_dir().join("extensions"))
            .unwrap_or_else(|| PathBuf::from("~/.clawdesk/extensions"));

        Self {
            integrations: DashMap::new(),
            configs: DashMap::new(),
            user_dir,
        }
    }

    /// Create with a custom user directory.
    pub fn with_user_dir(user_dir: PathBuf) -> Self {
        Self {
            integrations: DashMap::new(),
            configs: DashMap::new(),
            user_dir,
        }
    }

    /// Load bundled integration templates (compile-time embedded).
    pub fn load_bundled(&self) {
        for template in BUNDLED_INTEGRATIONS {
            match toml::from_str::<Integration>(template) {
                Ok(integration) => {
                    debug!(name = %integration.name, "loaded bundled integration");
                    self.integrations
                        .insert(integration.name.clone(), integration);
                }
                Err(e) => {
                    warn!(error = %e, "failed to parse bundled integration");
                }
            }
        }
        info!(count = self.integrations.len(), "loaded bundled integrations");
    }

    /// Load user-provided TOML files from the extensions directory.
    pub async fn load_user_extensions(&self) -> Result<usize, ExtensionError> {
        let dir = &self.user_dir;
        if !dir.exists() {
            debug!(path = %dir.display(), "user extensions directory not found");
            return Ok(0);
        }

        let mut count = 0;
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map(|e| e == "toml").unwrap_or(false) {
                match tokio::fs::read_to_string(&path).await {
                    Ok(content) => match toml::from_str::<Integration>(&content) {
                        Ok(integration) => {
                            info!(name = %integration.name, path = %path.display(), "loaded user integration");
                            self.integrations
                                .insert(integration.name.clone(), integration);
                            count += 1;
                        }
                        Err(e) => {
                            warn!(path = %path.display(), error = %e, "failed to parse integration TOML");
                        }
                    },
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "failed to read integration file");
                    }
                }
            }
        }

        info!(count, "loaded user extensions");
        Ok(count)
    }

    /// Get an integration by name.
    pub fn get(&self, name: &str) -> Option<Integration> {
        self.integrations.get(name).map(|r| r.clone())
    }

    /// List all integrations.
    pub fn list(&self) -> Vec<Integration> {
        self.integrations.iter().map(|r| r.value().clone()).collect()
    }

    /// List integrations by category.
    pub fn list_by_category(&self, category: &IntegrationCategory) -> Vec<Integration> {
        self.integrations
            .iter()
            .filter(|r| &r.value().category == category)
            .map(|r| r.value().clone())
            .collect()
    }

    /// Enable an integration.
    pub fn enable(&self, name: &str) -> Result<(), ExtensionError> {
        let mut entry = self
            .integrations
            .get_mut(name)
            .ok_or_else(|| ExtensionError::NotFound(name.into()))?;
        entry.enabled = true;
        Ok(())
    }

    /// Disable an integration.
    pub fn disable(&self, name: &str) -> Result<(), ExtensionError> {
        let mut entry = self
            .integrations
            .get_mut(name)
            .ok_or_else(|| ExtensionError::NotFound(name.into()))?;
        entry.enabled = false;
        Ok(())
    }

    /// Get all enabled integrations.
    pub fn enabled(&self) -> Vec<Integration> {
        self.integrations
            .iter()
            .filter(|r| r.value().enabled)
            .map(|r| r.value().clone())
            .collect()
    }

    /// Get names of all enabled integrations (for persistence).
    pub fn enabled_names(&self) -> Vec<String> {
        self.integrations
            .iter()
            .filter(|r| r.value().enabled)
            .map(|r| r.key().clone())
            .collect()
    }

    /// Restore previously enabled integrations from a list of names.
    ///
    /// Silently skips names that don't exist in the registry (e.g. a
    /// user-defined extension that was removed between restarts).
    pub fn restore_enabled(&self, names: &[String]) {
        for name in names {
            if let Some(mut entry) = self.integrations.get_mut(name) {
                entry.enabled = true;
                debug!(name = %name, "restored integration enabled state");
            } else {
                warn!(name = %name, "skipping unknown integration from saved state");
            }
        }
    }

    /// Count of registered integrations.
    pub fn count(&self) -> usize {
        self.integrations.len()
    }

    /// Get the stored user configuration for an integration.
    pub fn get_config(&self, name: &str) -> Option<std::collections::HashMap<String, String>> {
        self.configs.get(name).map(|r| r.value().clone())
    }

    /// Set (merge) user configuration for an integration.
    ///
    /// Only keys that appear in the integration's `config_fields` or
    /// `credentials` schema are accepted — unknown keys are silently dropped.
    pub fn set_config(
        &self,
        name: &str,
        values: std::collections::HashMap<String, String>,
    ) -> Result<(), ExtensionError> {
        let integration = self
            .integrations
            .get(name)
            .ok_or_else(|| ExtensionError::NotFound(name.into()))?;

        // Build set of valid keys from config_fields + credential env_vars.
        let valid_keys: std::collections::HashSet<&str> = integration
            .config_fields
            .iter()
            .map(|f| f.key.as_str())
            .chain(
                integration
                    .credentials
                    .iter()
                    .filter_map(|c| c.env_var.as_deref()),
            )
            .chain(integration.credentials.iter().map(|c| c.name.as_str()))
            .collect();

        let filtered: std::collections::HashMap<String, String> = values
            .into_iter()
            .filter(|(k, _)| valid_keys.contains(k.as_str()))
            .collect();

        self.configs.insert(name.to_string(), filtered);
        Ok(())
    }

    /// Validate that all required config fields have values.
    ///
    /// Returns a list of missing-required-field keys, or empty if valid.
    pub fn validate_config(&self, name: &str) -> Result<Vec<String>, ExtensionError> {
        let integration = self
            .integrations
            .get(name)
            .ok_or_else(|| ExtensionError::NotFound(name.into()))?;

        let config = self
            .configs
            .get(name)
            .map(|r| r.value().clone())
            .unwrap_or_default();

        let missing: Vec<String> = integration
            .config_fields
            .iter()
            .filter(|f| f.required)
            .filter(|f| {
                let val = config.get(&f.key).or(f.default.as_ref());
                val.map(|v| v.trim().is_empty()).unwrap_or(true)
            })
            .map(|f| f.key.clone())
            .collect();

        Ok(missing)
    }

    /// Resolve a transport config by interpolating `${KEY}` placeholders.
    ///
    /// Resolution order (first match wins):
    /// 1. User config values from `set_config()`
    /// 2. Credential env map (vault + env vars)
    /// 3. Config field defaults
    /// 4. Leave the literal `${KEY}` in place (logged as warning)
    pub fn resolve_transport(
        &self,
        name: &str,
        credential_env: &std::collections::HashMap<String, String>,
    ) -> Option<TransportConfig> {
        let integration = self.integrations.get(name)?;
        let config = self
            .configs
            .get(name)
            .map(|r| r.value().clone())
            .unwrap_or_default();

        // Build lookup: config values > credential env > config field defaults
        let defaults: std::collections::HashMap<&str, &str> = integration
            .config_fields
            .iter()
            .filter_map(|f| f.default.as_deref().map(|d| (f.key.as_str(), d)))
            .collect();

        let resolve = |s: &str| -> String {
            let mut result = s.to_string();
            // Find all ${KEY} patterns and replace
            while let Some(start) = result.find("${") {
                if let Some(end) = result[start..].find('}') {
                    let key = &result[start + 2..start + end];
                    let replacement = config
                        .get(key)
                        .map(|v| v.as_str())
                        .or_else(|| credential_env.get(key).map(|v| v.as_str()))
                        .or_else(|| defaults.get(key).copied());

                    if let Some(val) = replacement {
                        result = format!("{}{}{}", &result[..start], val, &result[start + end + 1..]);
                    } else {
                        warn!(key, integration = %name, "unresolved config variable in transport");
                        // Break to avoid infinite loop on unresolvable vars
                        break;
                    }
                } else {
                    break;
                }
            }
            result
        };

        let resolved = match &integration.transport {
            TransportConfig::Stdio { command, args } => TransportConfig::Stdio {
                command: resolve(command),
                args: args.iter().map(|a| resolve(a)).collect(),
            },
            TransportConfig::Sse { url } => TransportConfig::Sse {
                url: resolve(url),
            },
            TransportConfig::DirectApi { base_url, headers } => TransportConfig::DirectApi {
                base_url: resolve(base_url),
                headers: headers
                    .iter()
                    .map(|(k, v)| (k.clone(), resolve(v)))
                    .collect(),
            },
        };

        Some(resolved)
    }
}

impl Default for IntegrationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for IntegrationRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntegrationRegistry")
            .field("count", &self.integrations.len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Bundled integration TOML templates
// ---------------------------------------------------------------------------

const BUNDLED_INTEGRATIONS: &[&str] = &[
    // ── DevTools ──────────────────────────────────────────────────
    r#"
name = "github"
description = "GitHub repositories, issues, pull requests, and code search"
category = "devtools"
icon = "🐙"
enabled = false
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
[[credentials]]
name = "GITHUB_PERSONAL_ACCESS_TOKEN"
description = "GitHub personal access token with repo scope"
env_var = "GITHUB_PERSONAL_ACCESS_TOKEN"
required = true
[[config_fields]]
key = "GITHUB_API_URL"
label = "API URL"
description = "GitHub API endpoint (change for GitHub Enterprise)"
field_type = "url"
required = false
default = "https://api.github.com"
placeholder = "https://api.github.com"
group = "Connection"
"#,
    r#"
name = "gitlab"
description = "GitLab projects, issues, and merge requests"
category = "devtools"
icon = "🦊"
enabled = false
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-gitlab"]
[[credentials]]
name = "GITLAB_TOKEN"
description = "GitLab personal access token"
env_var = "GITLAB_TOKEN"
required = true
[[config_fields]]
key = "GITLAB_URL"
label = "GitLab URL"
description = "GitLab instance URL (change for self-hosted)"
field_type = "url"
required = false
default = "https://gitlab.com"
placeholder = "https://gitlab.example.com"
group = "Connection"
"#,
    r#"
name = "jira"
description = "Jira project management"
category = "devtools"
icon = "📋"
enabled = false
[transport]
type = "api"
base_url = "${JIRA_BASE_URL}"
[[credentials]]
name = "JIRA_API_TOKEN"
description = "Jira API token"
env_var = "JIRA_API_TOKEN"
required = true
[[credentials]]
name = "JIRA_EMAIL"
description = "Jira account email"
env_var = "JIRA_EMAIL"
required = true
[[config_fields]]
key = "JIRA_BASE_URL"
label = "Jira URL"
description = "Your Jira Cloud instance URL"
field_type = "url"
required = true
placeholder = "https://your-company.atlassian.net"
group = "Connection"
[[config_fields]]
key = "JIRA_PROJECT_KEY"
label = "Default Project"
description = "Default Jira project key (e.g. PROJ)"
field_type = "text"
required = false
placeholder = "PROJ"
group = "Settings"
"#,
    r#"
name = "linear"
description = "Linear issue tracking and project management"
category = "devtools"
icon = "📐"
enabled = false
[transport]
type = "api"
base_url = "https://api.linear.app"
[[credentials]]
name = "LINEAR_API_KEY"
description = "Linear API key"
env_var = "LINEAR_API_KEY"
required = true
[[config_fields]]
key = "LINEAR_TEAM_ID"
label = "Team ID"
description = "Default Linear team ID for issue creation"
field_type = "text"
required = false
placeholder = "TEAM-123"
group = "Settings"
"#,
    r#"
name = "sentry"
description = "Sentry error tracking and performance monitoring"
category = "devtools"
icon = "🐛"
enabled = false
[transport]
type = "api"
base_url = "${SENTRY_BASE_URL}"
[[credentials]]
name = "SENTRY_AUTH_TOKEN"
description = "Sentry authentication token"
env_var = "SENTRY_AUTH_TOKEN"
required = true
[[config_fields]]
key = "SENTRY_BASE_URL"
label = "Sentry URL"
description = "Sentry API endpoint (change for self-hosted)"
field_type = "url"
required = false
default = "https://sentry.io/api/0"
placeholder = "https://sentry.io/api/0"
group = "Connection"
[[config_fields]]
key = "SENTRY_ORG"
label = "Organization"
description = "Sentry organization slug"
field_type = "text"
required = false
placeholder = "my-org"
group = "Settings"
[[config_fields]]
key = "SENTRY_PROJECT"
label = "Project"
description = "Sentry project slug"
field_type = "text"
required = false
placeholder = "my-project"
group = "Settings"
"#,
    r#"
name = "bitbucket"
description = "Bitbucket repositories and pull requests"
category = "devtools"
icon = "🪣"
enabled = false
[transport]
type = "api"
base_url = "${BITBUCKET_BASE_URL}"
[[credentials]]
name = "BITBUCKET_APP_PASSWORD"
description = "Bitbucket app password"
env_var = "BITBUCKET_APP_PASSWORD"
required = true
[[config_fields]]
key = "BITBUCKET_BASE_URL"
label = "API URL"
description = "Bitbucket API endpoint (change for Bitbucket Server)"
field_type = "url"
required = false
default = "https://api.bitbucket.org/2.0"
placeholder = "https://api.bitbucket.org/2.0"
group = "Connection"
[[config_fields]]
key = "BITBUCKET_WORKSPACE"
label = "Workspace"
description = "Bitbucket workspace slug"
field_type = "text"
required = false
placeholder = "my-workspace"
group = "Settings"
"#,
    // ── Productivity ──────────────────────────────────────────────
    r#"
name = "notion"
description = "Notion workspace pages and databases"
category = "productivity"
icon = "📝"
enabled = false
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-notion"]
[[credentials]]
name = "NOTION_API_TOKEN"
description = "Notion integration token (from https://www.notion.so/my-integrations)"
env_var = "NOTION_API_TOKEN"
required = true
"#,
    r##"
name = "slack"
description = "Slack workspace messaging and channels"
category = "productivity"
icon = "💬"
enabled = false
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-slack"]
[[credentials]]
name = "SLACK_BOT_TOKEN"
description = "Slack bot token (starts with xoxb-)"
env_var = "SLACK_BOT_TOKEN"
required = true
[[config_fields]]
key = "SLACK_DEFAULT_CHANNEL"
label = "Default Channel"
description = "Default Slack channel for messages"
field_type = "text"
required = false
placeholder = "#general"
group = "Settings"
"##,
    r#"
name = "gmail"
description = "Gmail email reading and sending"
category = "productivity"
icon = "📧"
enabled = false
[transport]
type = "api"
base_url = "https://gmail.googleapis.com/gmail/v1"
[oauth]
auth_url = "https://accounts.google.com/o/oauth2/v2/auth"
token_url = "https://oauth2.googleapis.com/token"
client_id = "${GMAIL_CLIENT_ID}"
scopes = ["https://www.googleapis.com/auth/gmail.modify"]
[[config_fields]]
key = "GMAIL_CLIENT_ID"
label = "OAuth Client ID"
description = "Google Cloud OAuth 2.0 client ID (from Cloud Console)"
field_type = "text"
required = true
placeholder = "xxxxxxxxx.apps.googleusercontent.com"
group = "Authentication"
[[config_fields]]
key = "GMAIL_MAX_RESULTS"
label = "Max Results"
description = "Maximum number of emails to return per query"
field_type = "number"
required = false
default = "25"
placeholder = "25"
group = "Settings"
"#,
    r#"
name = "google-drive"
description = "Google Drive file management"
category = "productivity"
icon = "📁"
enabled = false
[transport]
type = "api"
base_url = "https://www.googleapis.com/drive/v3"
[oauth]
auth_url = "https://accounts.google.com/o/oauth2/v2/auth"
token_url = "https://oauth2.googleapis.com/token"
client_id = "${GDRIVE_CLIENT_ID}"
scopes = ["https://www.googleapis.com/auth/drive"]
[[config_fields]]
key = "GDRIVE_CLIENT_ID"
label = "OAuth Client ID"
description = "Google Cloud OAuth 2.0 client ID"
field_type = "text"
required = true
placeholder = "xxxxxxxxx.apps.googleusercontent.com"
group = "Authentication"
"#,
    r#"
name = "google-calendar"
description = "Google Calendar events and scheduling"
category = "productivity"
icon = "📅"
enabled = false
[transport]
type = "api"
base_url = "https://www.googleapis.com/calendar/v3"
[oauth]
auth_url = "https://accounts.google.com/o/oauth2/v2/auth"
token_url = "https://oauth2.googleapis.com/token"
client_id = "${GCAL_CLIENT_ID}"
scopes = ["https://www.googleapis.com/auth/calendar"]
[[config_fields]]
key = "GCAL_CLIENT_ID"
label = "OAuth Client ID"
description = "Google Cloud OAuth 2.0 client ID"
field_type = "text"
required = true
placeholder = "xxxxxxxxx.apps.googleusercontent.com"
group = "Authentication"
"#,
    r#"
name = "todoist"
description = "Todoist task management"
category = "productivity"
icon = "✅"
enabled = false
[transport]
type = "api"
base_url = "https://api.todoist.com/rest/v2"
[[credentials]]
name = "TODOIST_API_TOKEN"
description = "Todoist API token (from Settings > Integrations)"
env_var = "TODOIST_API_TOKEN"
required = true
"#,
    r#"
name = "dropbox"
description = "Dropbox file storage and sharing"
category = "productivity"
icon = "📦"
enabled = false
[transport]
type = "api"
base_url = "https://api.dropboxapi.com/2"
[oauth]
auth_url = "https://www.dropbox.com/oauth2/authorize"
token_url = "https://api.dropboxapi.com/oauth2/token"
client_id = "${DROPBOX_CLIENT_ID}"
scopes = ["files.content.read", "files.content.write"]
[[config_fields]]
key = "DROPBOX_CLIENT_ID"
label = "App Key"
description = "Dropbox app key (from App Console)"
field_type = "text"
required = true
placeholder = "your-app-key"
group = "Authentication"
"#,
    // ── Data ──────────────────────────────────────────────────────
    r#"
name = "postgresql"
description = "PostgreSQL database access"
category = "data"
icon = "🐘"
enabled = false
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "${POSTGRES_URL}"]
[[credentials]]
name = "POSTGRES_URL"
description = "PostgreSQL connection URL"
env_var = "POSTGRES_URL"
required = true
[[config_fields]]
key = "POSTGRES_URL"
label = "Connection URL"
description = "PostgreSQL connection string"
field_type = "url"
required = true
placeholder = "postgresql://user:pass@localhost:5432/mydb"
group = "Connection"
[[config_fields]]
key = "POSTGRES_SCHEMA"
label = "Schema"
description = "Default database schema"
field_type = "text"
required = false
default = "public"
placeholder = "public"
group = "Settings"
"#,
    r#"
name = "sqlite"
description = "SQLite database access"
category = "data"
icon = "💾"
enabled = false
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-sqlite", "--db-path", "${SQLITE_DB_PATH}"]
[[config_fields]]
key = "SQLITE_DB_PATH"
label = "Database Path"
description = "Path to the SQLite database file"
field_type = "filepath"
required = true
placeholder = "/path/to/database.db"
group = "Connection"
"#,
    r#"
name = "redis"
description = "Redis key-value store"
category = "data"
icon = "🔴"
enabled = false
[transport]
type = "api"
base_url = "${REDIS_URL}"
[[credentials]]
name = "REDIS_PASSWORD"
description = "Redis password (if authentication required)"
env_var = "REDIS_PASSWORD"
required = false
[[config_fields]]
key = "REDIS_URL"
label = "Connection URL"
description = "Redis connection URL"
field_type = "url"
required = true
default = "redis://localhost:6379"
placeholder = "redis://localhost:6379"
group = "Connection"
[[config_fields]]
key = "REDIS_DB"
label = "Database Number"
description = "Redis database index (0-15)"
field_type = "number"
required = false
default = "0"
placeholder = "0"
group = "Settings"
"#,
    r#"
name = "mongodb"
description = "MongoDB document database"
category = "data"
icon = "🍃"
enabled = false
[transport]
type = "api"
base_url = "${MONGODB_URI}"
[[config_fields]]
key = "MONGODB_URI"
label = "Connection URI"
description = "MongoDB connection string"
field_type = "url"
required = true
default = "mongodb://localhost:27017"
placeholder = "mongodb://user:pass@localhost:27017/mydb"
group = "Connection"
[[config_fields]]
key = "MONGODB_DATABASE"
label = "Database"
description = "Default database name"
field_type = "text"
required = false
placeholder = "mydb"
group = "Settings"
"#,
    r#"
name = "elasticsearch"
description = "Elasticsearch search and analytics"
category = "data"
icon = "🔍"
enabled = false
[transport]
type = "api"
base_url = "${ELASTICSEARCH_URL}"
[[credentials]]
name = "ELASTICSEARCH_API_KEY"
description = "Elasticsearch API key (if authentication required)"
env_var = "ELASTICSEARCH_API_KEY"
required = false
[[config_fields]]
key = "ELASTICSEARCH_URL"
label = "Cluster URL"
description = "Elasticsearch cluster endpoint"
field_type = "url"
required = true
default = "http://localhost:9200"
placeholder = "http://localhost:9200"
group = "Connection"
[[config_fields]]
key = "ELASTICSEARCH_INDEX"
label = "Default Index"
description = "Default index pattern for queries"
field_type = "text"
required = false
placeholder = "my-index-*"
group = "Settings"
"#,
    // ── Cloud ─────────────────────────────────────────────────────
    r#"
name = "aws"
description = "Amazon Web Services"
category = "cloud"
icon = "☁️"
enabled = false
[transport]
type = "api"
base_url = "https://aws.amazon.com"
[[credentials]]
name = "AWS_ACCESS_KEY_ID"
description = "AWS access key ID"
env_var = "AWS_ACCESS_KEY_ID"
required = true
[[credentials]]
name = "AWS_SECRET_ACCESS_KEY"
description = "AWS secret access key"
env_var = "AWS_SECRET_ACCESS_KEY"
required = true
[[config_fields]]
key = "AWS_REGION"
label = "Region"
description = "AWS region for API calls"
field_type = "select"
required = false
default = "us-east-1"
group = "Connection"
[[config_fields.options]]
label = "US East (N. Virginia)"
value = "us-east-1"
[[config_fields.options]]
label = "US West (Oregon)"
value = "us-west-2"
[[config_fields.options]]
label = "EU (Ireland)"
value = "eu-west-1"
[[config_fields.options]]
label = "EU (Frankfurt)"
value = "eu-central-1"
[[config_fields.options]]
label = "Asia Pacific (Tokyo)"
value = "ap-northeast-1"
[[config_fields.options]]
label = "Asia Pacific (Singapore)"
value = "ap-southeast-1"
[[config_fields]]
key = "AWS_PROFILE"
label = "Profile"
description = "AWS CLI profile name (from ~/.aws/credentials)"
field_type = "text"
required = false
default = "default"
placeholder = "default"
group = "Connection"
"#,
    r#"
name = "azure"
description = "Microsoft Azure cloud services"
category = "cloud"
icon = "🔷"
enabled = false
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-azure"]
[[credentials]]
name = "AZURE_SUBSCRIPTION_ID"
description = "Azure subscription ID"
env_var = "AZURE_SUBSCRIPTION_ID"
required = true
[[config_fields]]
key = "AZURE_TENANT_ID"
label = "Tenant ID"
description = "Azure Active Directory tenant ID"
field_type = "text"
required = false
placeholder = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
group = "Connection"
[[config_fields]]
key = "AZURE_RESOURCE_GROUP"
label = "Resource Group"
description = "Default Azure resource group"
field_type = "text"
required = false
placeholder = "my-resource-group"
group = "Settings"
"#,
    r#"
name = "gcp"
description = "Google Cloud Platform"
category = "cloud"
icon = "🌐"
enabled = false
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-gcp"]
[[credentials]]
name = "GOOGLE_APPLICATION_CREDENTIALS"
description = "Path to GCP service account JSON key file"
env_var = "GOOGLE_APPLICATION_CREDENTIALS"
required = true
[[config_fields]]
key = "GCP_PROJECT_ID"
label = "Project ID"
description = "Google Cloud project ID"
field_type = "text"
required = false
placeholder = "my-project-id"
group = "Connection"
[[config_fields]]
key = "GCP_REGION"
label = "Region"
description = "Default GCP region"
field_type = "text"
required = false
default = "us-central1"
placeholder = "us-central1"
group = "Settings"
"#,
    // ── Search ────────────────────────────────────────────────────
    r#"
name = "brave-search"
description = "Brave Search web search engine"
category = "search"
icon = "🦁"
enabled = false
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-brave-search"]
[[credentials]]
name = "BRAVE_API_KEY"
description = "Brave Search API key (from https://brave.com/search/api/)"
env_var = "BRAVE_API_KEY"
required = true
[[config_fields]]
key = "BRAVE_SEARCH_COUNT"
label = "Results Count"
description = "Number of search results to return"
field_type = "number"
required = false
default = "10"
placeholder = "10"
group = "Settings"
"#,
    r#"
name = "exa-search"
description = "Exa AI-powered web search"
category = "search"
icon = "🔎"
enabled = false
[transport]
type = "api"
base_url = "https://api.exa.ai"
[[credentials]]
name = "EXA_API_KEY"
description = "Exa API key (from https://dashboard.exa.ai)"
env_var = "EXA_API_KEY"
required = true
[[config_fields]]
key = "EXA_NUM_RESULTS"
label = "Results Count"
description = "Number of search results to return"
field_type = "number"
required = false
default = "10"
placeholder = "10"
group = "Settings"
"#,
    // ── Communication ─────────────────────────────────────────────
    r#"
name = "discord-mcp"
description = "Discord bot integration via MCP"
category = "communication"
icon = "🎮"
enabled = false
[transport]
type = "api"
base_url = "https://discord.com/api/v10"
[[credentials]]
name = "DISCORD_BOT_TOKEN"
description = "Discord bot token (from Discord Developer Portal)"
env_var = "DISCORD_BOT_TOKEN"
required = true
[[config_fields]]
key = "DISCORD_GUILD_ID"
label = "Server ID"
description = "Default Discord server (guild) ID"
field_type = "text"
required = false
placeholder = "123456789012345678"
group = "Settings"
"#,
    r#"
name = "teams-mcp"
description = "Microsoft Teams integration via MCP"
category = "communication"
icon = "👥"
enabled = false
[transport]
type = "api"
base_url = "https://graph.microsoft.com/v1.0"
[oauth]
auth_url = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
token_url = "https://login.microsoftonline.com/common/oauth2/v2.0/token"
client_id = "${TEAMS_CLIENT_ID}"
scopes = ["https://graph.microsoft.com/.default"]
[[config_fields]]
key = "TEAMS_CLIENT_ID"
label = "App Client ID"
description = "Azure AD application client ID"
field_type = "text"
required = true
placeholder = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
group = "Authentication"
[[config_fields]]
key = "TEAMS_TENANT_ID"
label = "Tenant ID"
description = "Azure AD tenant ID (use 'common' for multi-tenant)"
field_type = "text"
required = false
default = "common"
placeholder = "common"
group = "Authentication"
"#,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_integrations_count() {
        let registry = IntegrationRegistry::new();
        registry.load_bundled();
        assert!(registry.count() >= 25, "Expected 25+ integrations, got {}", registry.count());
    }

    #[test]
    fn get_github_integration() {
        let registry = IntegrationRegistry::new();
        registry.load_bundled();
        let gh = registry.get("github").unwrap();
        assert_eq!(gh.category, IntegrationCategory::DevTools);
        assert!(!gh.config_fields.is_empty(), "GitHub should have config fields");
    }

    #[test]
    fn list_by_category() {
        let registry = IntegrationRegistry::new();
        registry.load_bundled();
        let devtools = registry.list_by_category(&IntegrationCategory::DevTools);
        assert!(!devtools.is_empty());
    }

    #[test]
    fn enable_disable() {
        let registry = IntegrationRegistry::new();
        registry.load_bundled();
        assert!(!registry.get("github").unwrap().enabled);
        registry.enable("github").unwrap();
        assert!(registry.get("github").unwrap().enabled);
        registry.disable("github").unwrap();
        assert!(!registry.get("github").unwrap().enabled);
    }

    #[test]
    fn config_set_get_validate() {
        let registry = IntegrationRegistry::new();
        registry.load_bundled();

        // Jira has required config_field "JIRA_BASE_URL"
        let missing = registry.validate_config("jira").unwrap();
        assert!(missing.contains(&"JIRA_BASE_URL".to_string()));

        // Set it
        let mut values = std::collections::HashMap::new();
        values.insert("JIRA_BASE_URL".to_string(), "https://myco.atlassian.net".to_string());
        registry.set_config("jira", values).unwrap();

        // Now validate should pass
        let missing = registry.validate_config("jira").unwrap();
        assert!(missing.is_empty(), "Expected no missing fields after setting JIRA_BASE_URL, got: {:?}", missing);

        // Get config
        let config = registry.get_config("jira").unwrap();
        assert_eq!(config.get("JIRA_BASE_URL").unwrap(), "https://myco.atlassian.net");
    }

    #[test]
    fn resolve_transport_interpolation() {
        let registry = IntegrationRegistry::new();
        registry.load_bundled();

        // Set Jira base URL via config
        let mut values = std::collections::HashMap::new();
        values.insert("JIRA_BASE_URL".to_string(), "https://test.atlassian.net".to_string());
        registry.set_config("jira", values).unwrap();

        let cred_env = std::collections::HashMap::new();
        let resolved = registry.resolve_transport("jira", &cred_env).unwrap();

        match resolved {
            TransportConfig::DirectApi { base_url, .. } => {
                assert_eq!(base_url, "https://test.atlassian.net");
            }
            _ => panic!("Expected DirectApi transport"),
        }
    }

    #[test]
    fn config_rejects_unknown_keys() {
        let registry = IntegrationRegistry::new();
        registry.load_bundled();

        let mut values = std::collections::HashMap::new();
        values.insert("UNKNOWN_KEY".to_string(), "value".to_string());
        values.insert("JIRA_BASE_URL".to_string(), "https://test.atlassian.net".to_string());
        registry.set_config("jira", values).unwrap();

        let config = registry.get_config("jira").unwrap();
        assert!(!config.contains_key("UNKNOWN_KEY"), "Unknown keys should be filtered");
        assert!(config.contains_key("JIRA_BASE_URL"));
    }
}
