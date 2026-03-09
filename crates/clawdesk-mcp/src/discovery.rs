//! MCP auto-discovery — finds MCP servers on the local network and via
//! well-known HTTP endpoints.
//!
//! ## Discovery Methods
//!
//! 1. **`.well-known/mcp.json`**: HTTP GET to `{base_url}/.well-known/mcp.json`
//!    returns a JSON document describing available MCP servers.
//!
//! 2. **Local config files**: Scans `~/.clawdesk/mcp-servers/` and
//!    `$XDG_CONFIG_HOME/clawdesk/mcp-servers/` for `*.json` server configs.
//!
//! 3. **Environment variable**: `CLAWDESK_MCP_SERVERS` can point to a JSON
//!    file listing additional MCP servers.
//!
//! ## Discovery Document Format
//!
//! ```json
//! {
//!   "servers": [
//!     {
//!       "name": "example-tools",
//!       "transport": "sse",
//!       "url": "https://example.com/mcp",
//!       "version": "1.0",
//!       "capabilities": ["tools/list", "tools/call"]
//!     }
//!   ]
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// A discovered MCP server endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredServer {
    /// Server name.
    pub name: String,
    /// Transport type: "stdio" or "sse".
    pub transport: String,
    /// For SSE: the URL endpoint. For stdio: the command to spawn.
    pub endpoint: String,
    /// Optional: command-line arguments for stdio transport.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Optional: environment variables for stdio transport.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// Server version.
    pub version: Option<String>,
    /// Declared capabilities (e.g., "tools/list", "tools/call", "resources/list").
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Discovery source: "well-known", "config-file", "env", "manual".
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    "manual".into()
}

/// Discovery document returned by `.well-known/mcp.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryDocument {
    pub servers: Vec<DiscoveredServer>,
}

/// MCP server auto-discovery engine.
pub struct McpDiscovery {
    /// HTTP client for well-known endpoint fetching.
    http_client: reqwest::Client,
    /// Timeout for HTTP discovery requests.
    timeout: std::time::Duration,
}

impl McpDiscovery {
    pub fn new() -> Self {
        Self {
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            timeout: std::time::Duration::from_secs(10),
        }
    }

    /// Discover all MCP servers from all sources.
    pub async fn discover_all(&self) -> Vec<DiscoveredServer> {
        let mut servers = Vec::new();

        // 1. Local config files
        servers.extend(self.discover_local_configs().await);

        // 2. Environment variable
        servers.extend(self.discover_from_env().await);

        // Deduplicate by name
        let mut seen = std::collections::HashSet::new();
        servers.retain(|s| seen.insert(s.name.clone()));

        info!(count = servers.len(), "MCP discovery complete");
        servers
    }

    /// Discover MCP servers from a `.well-known/mcp.json` endpoint.
    pub async fn discover_well_known(&self, base_url: &str) -> Result<Vec<DiscoveredServer>, String> {
        let url = format!("{}/.well-known/mcp.json", base_url.trim_end_matches('/'));
        debug!(url = %url, "fetching MCP discovery document");

        let response = self
            .http_client
            .get(&url)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }

        let doc: DiscoveryDocument = response
            .json()
            .await
            .map_err(|e| format!("JSON parse failed: {e}"))?;

        let mut servers = doc.servers;
        for s in &mut servers {
            s.source = "well-known".into();
        }

        Ok(servers)
    }

    /// Scan local config directories for MCP server definitions.
    async fn discover_local_configs(&self) -> Vec<DiscoveredServer> {
        let mut servers = Vec::new();

        for dir in self.config_dirs() {
            if !dir.exists() {
                continue;
            }
            debug!(dir = %dir.display(), "scanning for MCP server configs");
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("json") {
                            match self.load_config_file(&path) {
                                Ok(mut svrs) => {
                                    for s in &mut svrs {
                                        s.source = "config-file".into();
                                    }
                                    servers.extend(svrs);
                                }
                                Err(e) => warn!(path = %path.display(), error = %e, "failed to load MCP config"),
                            }
                        }
                    }
                }
                Err(e) => warn!(dir = %dir.display(), error = %e, "failed to read config directory"),
            }
        }

        servers
    }

    /// Discover from `CLAWDESK_MCP_SERVERS` environment variable.
    async fn discover_from_env(&self) -> Vec<DiscoveredServer> {
        let path = match std::env::var("CLAWDESK_MCP_SERVERS") {
            Ok(p) => PathBuf::from(p),
            Err(_) => return Vec::new(),
        };

        match self.load_config_file(&path) {
            Ok(mut servers) => {
                for s in &mut servers {
                    s.source = "env".into();
                }
                servers
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "failed to load MCP servers from env");
                Vec::new()
            }
        }
    }

    /// Load a config file containing a discovery document or single server.
    fn load_config_file(&self, path: &Path) -> Result<Vec<DiscoveredServer>, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("read failed: {e}"))?;

        // Try as discovery document first
        if let Ok(doc) = serde_json::from_str::<DiscoveryDocument>(&content) {
            return Ok(doc.servers);
        }

        // Try as single server
        if let Ok(server) = serde_json::from_str::<DiscoveredServer>(&content) {
            return Ok(vec![server]);
        }

        // Try as array of servers
        if let Ok(servers) = serde_json::from_str::<Vec<DiscoveredServer>>(&content) {
            return Ok(servers);
        }

        Err("unrecognized config format".into())
    }

    /// Platform-aware config directories for MCP server definitions.
    fn config_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        // ~/.clawdesk/mcp-servers/
        if let Some(home) = home_dir() {
            dirs.push(home.join(".clawdesk").join("mcp-servers"));
        }

        // XDG_CONFIG_HOME/clawdesk/mcp-servers/
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            dirs.push(PathBuf::from(xdg).join("clawdesk").join("mcp-servers"));
        } else if let Some(home) = home_dir() {
            dirs.push(home.join(".config").join("clawdesk").join("mcp-servers"));
        }

        dirs
    }
}

impl Default for McpDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

/// Cross-platform home directory.
fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_discovery_document() {
        let json = r#"{
            "servers": [
                {
                    "name": "test-tools",
                    "transport": "sse",
                    "endpoint": "https://example.com/mcp",
                    "version": "1.0",
                    "capabilities": ["tools/list", "tools/call"]
                },
                {
                    "name": "local-db",
                    "transport": "stdio",
                    "endpoint": "mcp-db-server",
                    "args": ["--port", "5000"],
                    "capabilities": ["resources/list"]
                }
            ]
        }"#;

        let doc: DiscoveryDocument = serde_json::from_str(json).expect("parse");
        assert_eq!(doc.servers.len(), 2);
        assert_eq!(doc.servers[0].name, "test-tools");
        assert_eq!(doc.servers[0].transport, "sse");
        assert_eq!(doc.servers[1].args, vec!["--port", "5000"]);
    }

    #[test]
    fn parse_single_server() {
        let json = r#"{
            "name": "standalone",
            "transport": "stdio",
            "endpoint": "my-mcp-server"
        }"#;

        let server: DiscoveredServer = serde_json::from_str(json).expect("parse");
        assert_eq!(server.name, "standalone");
        assert_eq!(server.source, "manual");
    }
}
