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
            user_dir,
        }
    }

    /// Create with a custom user directory.
    pub fn with_user_dir(user_dir: PathBuf) -> Self {
        Self {
            integrations: DashMap::new(),
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
    // DevTools
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
"#,
    r#"
name = "jira"
description = "Jira project management"
category = "devtools"
icon = "📋"
enabled = false
[transport]
type = "api"
base_url = "https://your-domain.atlassian.net"
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
"#,
    r#"
name = "sentry"
description = "Sentry error tracking and performance monitoring"
category = "devtools"
icon = "🐛"
enabled = false
[transport]
type = "api"
base_url = "https://sentry.io/api/0"
[[credentials]]
name = "SENTRY_AUTH_TOKEN"
description = "Sentry authentication token"
env_var = "SENTRY_AUTH_TOKEN"
required = true
"#,
    r#"
name = "bitbucket"
description = "Bitbucket repositories and pull requests"
category = "devtools"
icon = "🪣"
enabled = false
[transport]
type = "api"
base_url = "https://api.bitbucket.org/2.0"
[[credentials]]
name = "BITBUCKET_APP_PASSWORD"
description = "Bitbucket app password"
env_var = "BITBUCKET_APP_PASSWORD"
required = true
"#,
    // Productivity
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
description = "Notion integration token"
env_var = "NOTION_API_TOKEN"
required = true
"#,
    r#"
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
description = "Slack bot token (xoxb-...)"
env_var = "SLACK_BOT_TOKEN"
required = true
"#,
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
client_id = ""
scopes = ["https://www.googleapis.com/auth/gmail.modify"]
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
client_id = ""
scopes = ["https://www.googleapis.com/auth/drive"]
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
client_id = ""
scopes = ["https://www.googleapis.com/auth/calendar"]
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
description = "Todoist API token"
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
client_id = ""
scopes = ["files.content.read", "files.content.write"]
"#,
    // Data
    r#"
name = "postgresql"
description = "PostgreSQL database access"
category = "data"
icon = "🐘"
enabled = false
[transport]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres"]
[[credentials]]
name = "POSTGRES_URL"
description = "PostgreSQL connection URL"
env_var = "POSTGRES_URL"
required = true
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
[[credentials]]
name = "SQLITE_DB_PATH"
description = "Path to SQLite database file"
env_var = "SQLITE_DB_PATH"
required = true
"#,
    r#"
name = "redis"
description = "Redis key-value store"
category = "data"
icon = "🔴"
enabled = false
[transport]
type = "api"
base_url = "redis://localhost:6379"
[[credentials]]
name = "REDIS_URL"
description = "Redis connection URL"
env_var = "REDIS_URL"
required = true
"#,
    r#"
name = "mongodb"
description = "MongoDB document database"
category = "data"
icon = "🍃"
enabled = false
[transport]
type = "api"
base_url = "mongodb://localhost:27017"
[[credentials]]
name = "MONGODB_URI"
description = "MongoDB connection URI"
env_var = "MONGODB_URI"
required = true
"#,
    r#"
name = "elasticsearch"
description = "Elasticsearch search and analytics"
category = "data"
icon = "🔍"
enabled = false
[transport]
type = "api"
base_url = "http://localhost:9200"
[[credentials]]
name = "ELASTICSEARCH_URL"
description = "Elasticsearch URL"
env_var = "ELASTICSEARCH_URL"
required = true
"#,
    // Cloud
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
description = "Path to GCP service account JSON"
env_var = "GOOGLE_APPLICATION_CREDENTIALS"
required = true
"#,
    // Search
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
description = "Brave Search API key"
env_var = "BRAVE_API_KEY"
required = true
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
description = "Exa API key"
env_var = "EXA_API_KEY"
required = true
"#,
    // Communication
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
description = "Discord bot token"
env_var = "DISCORD_BOT_TOKEN"
required = true
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
client_id = ""
scopes = ["https://graph.microsoft.com/.default"]
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
}
