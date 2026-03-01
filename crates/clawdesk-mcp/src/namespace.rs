//! Transparent MCP Tool Dispatch — Namespace Flattening.
//!
//! Bridges MCP server tools into the agent's `ToolRegistry` with transparent
//! namespaced names (`mcp_{server}_{tool}`). The LLM sees these as regular
//! tools and calls them by name; this module routes the call through the MCP
//! client automatically.
//!
//! ## Architecture
//!
//! ```text
//! ToolRegistry
//!   ├── shell_exec       (builtin)
//!   ├── read_file        (builtin)
//!   ├── mcp_github_create_issue    ─┐
//!   └── mcp_github_list_repos      ─┤── McpBridgeTool → McpClient → MCP server
//!                                    └── auto-dispatched via prefix
//! ```
//!
//! `register_mcp_tools()` discovers tools from all connected MCP servers and
//! registers each as an `McpBridgeTool` in the `ToolRegistry`. The bridge tool
//! forwards `execute()` calls to the MCP client.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// A bridge tool that forwards execution to an MCP server.
///
/// Registered in the `ToolRegistry` under the namespaced name
/// `mcp_{server}_{tool}`. When the LLM calls it, `execute()` routes
/// the call through the MCP client.
pub struct McpBridgeTool {
    /// Namespaced name (e.g., `mcp_github_create_issue`).
    namespaced_name: String,
    /// Original MCP tool name (e.g., `create_issue`).
    original_name: String,
    /// MCP server name (e.g., `github`).
    server_name: String,
    /// Tool description from MCP discovery.
    description: String,
    /// Tool input schema from MCP discovery.
    input_schema: serde_json::Value,
    /// Shared MCP client for dispatching calls.
    #[cfg(feature = "mcp-bridge")]
    mcp_client: Arc<crate::client::McpClient>,
}

impl McpBridgeTool {
    /// Create a new bridge tool.
    pub fn new(
        namespaced_name: String,
        original_name: String,
        server_name: String,
        description: String,
        input_schema: serde_json::Value,
    ) -> Self {
        Self {
            namespaced_name,
            original_name,
            server_name,
            description,
            input_schema,
        }
    }
}

impl std::fmt::Debug for McpBridgeTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpBridgeTool")
            .field("namespaced", &self.namespaced_name)
            .field("original", &self.original_name)
            .field("server", &self.server_name)
            .finish()
    }
}

/// Parse a namespaced MCP tool name back into (server_name, tool_name).
///
/// Given `mcp_github_create_issue`, returns `Some(("github", "create_issue"))`.
/// Returns `None` if the name doesn't match the `mcp_` prefix pattern.
pub fn parse_namespaced_name(namespaced: &str) -> Option<(String, String)> {
    let stripped = namespaced.strip_prefix("mcp_")?;
    // Find the first underscore to split server from tool
    let underscore_pos = stripped.find('_')?;
    let server = &stripped[..underscore_pos];
    let tool = &stripped[underscore_pos + 1..];
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

/// Check if a tool name is an MCP namespaced tool.
pub fn is_mcp_tool(name: &str) -> bool {
    name.starts_with("mcp_") && parse_namespaced_name(name).is_some()
}

/// Produce a namespaced tool name from server + tool names.
pub fn make_namespaced_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "mcp_{}_{}",
        server_name.replace('-', "_").replace(' ', "_").to_lowercase(),
        tool_name.replace('-', "_").to_lowercase()
    )
}

/// Metadata about an MCP tool registered in the ToolRegistry.
#[derive(Debug, Clone)]
pub struct McpToolRegistration {
    /// Namespaced name used in ToolRegistry.
    pub namespaced_name: String,
    /// Original MCP tool name.
    pub original_name: String,
    /// MCP server name.
    pub server_name: String,
    /// Tool description.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_namespaced_name() {
        let result = parse_namespaced_name("mcp_github_create_issue");
        assert_eq!(result, Some(("github".to_string(), "create_issue".to_string())));
    }

    #[test]
    fn test_parse_namespaced_multi_underscore_tool() {
        // Server name is first segment, rest is tool name
        let result = parse_namespaced_name("mcp_server_my_cool_tool");
        assert_eq!(result, Some(("server".to_string(), "my_cool_tool".to_string())));
    }

    #[test]
    fn test_parse_not_mcp() {
        assert_eq!(parse_namespaced_name("shell_exec"), None);
        assert_eq!(parse_namespaced_name("read_file"), None);
    }

    #[test]
    fn test_parse_malformed() {
        assert_eq!(parse_namespaced_name("mcp_"), None);
        assert_eq!(parse_namespaced_name("mcp_server"), None); // no underscore after server
    }

    #[test]
    fn test_is_mcp_tool() {
        assert!(is_mcp_tool("mcp_github_create_issue"));
        assert!(!is_mcp_tool("shell_exec"));
        assert!(!is_mcp_tool("mcp_"));
    }

    #[test]
    fn test_make_namespaced_name() {
        assert_eq!(
            make_namespaced_name("github-mcp", "create_issue"),
            "mcp_github_mcp_create_issue"
        );
        assert_eq!(
            make_namespaced_name("My Server", "list-items"),
            "mcp_my_server_list_items"
        );
    }

    #[test]
    fn test_roundtrip() {
        let ns = make_namespaced_name("github", "create_issue");
        let (server, tool) = parse_namespaced_name(&ns).unwrap();
        assert_eq!(server, "github");
        assert_eq!(tool, "create_issue");
    }
}
