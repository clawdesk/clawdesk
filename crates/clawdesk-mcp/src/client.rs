//! MCP Client — consume external MCP servers.
//!
//! Implements the MCP client lifecycle:
//! `initialize` → `notifications/initialized` → `tools/list` → `tools/call`
//!
//! Discovered tools are namespaced as `mcp_{server_name}_{tool_name}`.

use crate::protocol::*;
use crate::transport::{McpTransport, SseTransport, StdioTransport};
use crate::{JsonRpcRequest, McpError};
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// An active MCP server connection.
pub struct McpConnection {
    /// Server name (used for tool namespacing)
    pub name: String,
    /// Transport layer
    transport: Box<dyn McpTransport>,
    /// Server info from initialize handshake
    pub server_info: Option<Implementation>,
    /// Server capabilities
    pub capabilities: Option<ServerCapabilities>,
    /// Discovered tools (MCP name → McpTool)
    pub tools: DashMap<String, McpTool>,
}

impl McpConnection {
    /// Create a connection from a server config.
    pub async fn from_config(config: &McpServerConfig) -> Result<Self, McpError> {
        let transport: Box<dyn McpTransport> = match &config.transport {
            McpTransportConfig::Stdio { command, args } => {
                Box::new(StdioTransport::spawn(command, args, &config.env).await?)
            }
            McpTransportConfig::Sse { url } => Box::new(SseTransport::new(url)),
        };

        Ok(Self {
            name: config.name.clone(),
            transport,
            server_info: None,
            capabilities: None,
            tools: DashMap::new(),
        })
    }

    /// Perform the MCP initialize handshake.
    pub async fn initialize(&mut self) -> Result<InitializeResult, McpError> {
        let params = InitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: "clawdesk".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let request = JsonRpcRequest::new(
            0,
            "initialize",
            Some(serde_json::to_value(&params).map_err(|e| McpError::Protocol(e.to_string()))?),
        );

        let response = self.transport.send_request(request).await?;

        if let Some(error) = response.error {
            return Err(McpError::ServerError {
                code: error.code,
                message: error.message,
            });
        }

        let result: InitializeResult = serde_json::from_value(
            response.result.ok_or_else(|| McpError::Protocol("no result in initialize response".into()))?,
        )
        .map_err(|e| McpError::Protocol(format!("parse init result: {}", e)))?;

        self.server_info = Some(result.server_info.clone());
        self.capabilities = Some(result.capabilities.clone());

        // Send initialized notification
        let notif = JsonRpcRequest::notification("notifications/initialized", None);
        self.transport.send_notification(notif).await?;

        info!(
            server = %result.server_info.name,
            version = %result.server_info.version,
            protocol = %result.protocol_version,
            "MCP server initialized"
        );

        Ok(result)
    }

    /// Discover available tools from the MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let request = JsonRpcRequest::new(0, "tools/list", None);
        let response = self.transport.send_request(request).await?;

        if let Some(error) = response.error {
            return Err(McpError::ServerError {
                code: error.code,
                message: error.message,
            });
        }

        let result: ToolsListResult = serde_json::from_value(
            response.result.ok_or_else(|| McpError::Protocol("no result in tools/list".into()))?,
        )
        .map_err(|e| McpError::Protocol(format!("parse tools list: {}", e)))?;

        // Cache tools
        for tool in &result.tools {
            self.tools.insert(tool.name.clone(), tool.clone());
        }

        info!(count = result.tools.len(), server = %self.name, "discovered MCP tools");
        Ok(result.tools)
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: HashMap<String, serde_json::Value>,
    ) -> Result<ToolCallResult, McpError> {
        let params = ToolCallParams {
            name: name.to_string(),
            arguments,
        };

        let request = JsonRpcRequest::new(
            0,
            "tools/call",
            Some(serde_json::to_value(&params).map_err(|e| McpError::Protocol(e.to_string()))?),
        );

        let response = self.transport.send_request(request).await?;

        if let Some(error) = response.error {
            return Err(McpError::ServerError {
                code: error.code,
                message: error.message,
            });
        }

        let result: ToolCallResult = serde_json::from_value(
            response.result.ok_or_else(|| McpError::Protocol("no result in tools/call".into()))?,
        )
        .map_err(|e| McpError::Protocol(format!("parse tool result: {}", e)))?;

        Ok(result)
    }

    /// Close the connection.
    pub async fn close(&self) -> Result<(), McpError> {
        self.transport.close().await
    }

    /// Get namespaced tool name: `mcp_{server}_{tool}`
    pub fn namespaced_tool_name(&self, tool_name: &str) -> String {
        format!(
            "mcp_{}_{}",
            self.name.replace('-', "_").replace(' ', "_").to_lowercase(),
            tool_name.replace('-', "_").to_lowercase()
        )
    }
}

impl std::fmt::Debug for McpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpConnection")
            .field("name", &self.name)
            .field("tool_count", &self.tools.len())
            .finish()
    }
}

/// MCP Client manager — manages multiple MCP server connections.
#[derive(Debug)]
pub struct McpClient {
    /// Active connections: server_name → McpConnection
    connections: DashMap<String, Arc<tokio::sync::RwLock<McpConnection>>>,
}

impl McpClient {
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
        }
    }

    /// Connect to an MCP server, perform handshake, and discover tools.
    pub async fn connect(&self, config: McpServerConfig) -> Result<Vec<McpTool>, McpError> {
        let name = config.name.clone();
        let mut conn = McpConnection::from_config(&config).await?;

        // Initialize handshake
        conn.initialize().await?;

        // Discover tools
        let tools = conn.list_tools().await?;

        // Store connection
        self.connections
            .insert(name, Arc::new(tokio::sync::RwLock::new(conn)));

        Ok(tools)
    }

    /// Call a tool on a specific MCP server.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: HashMap<String, serde_json::Value>,
    ) -> Result<ToolCallResult, McpError> {
        let conn = self
            .connections
            .get(server_name)
            .ok_or_else(|| McpError::ToolNotFound(format!("server '{}' not connected", server_name)))?;

        let conn = conn.read().await;
        conn.call_tool(tool_name, arguments).await
    }

    /// Disconnect from a specific MCP server.
    pub async fn disconnect(&self, server_name: &str) -> Result<(), McpError> {
        if let Some((_, conn)) = self.connections.remove(server_name) {
            let conn = conn.read().await;
            conn.close().await?;
        }
        Ok(())
    }

    /// List all connected servers.
    pub fn connected_servers(&self) -> Vec<String> {
        self.connections.iter().map(|e| e.key().clone()).collect()
    }

    /// Disconnect from all servers.
    pub async fn disconnect_all(&self) {
        let names: Vec<String> = self.connections.iter().map(|e| e.key().clone()).collect();
        for name in names {
            if let Err(e) = self.disconnect(&name).await {
                error!(server = %name, error = %e, "failed to disconnect MCP server");
            }
        }
    }

    /// List tools discovered from a specific server.
    pub async fn list_tools_for_server(&self, server_name: &str) -> Result<Vec<McpTool>, McpError> {
        let conn = self
            .connections
            .get(server_name)
            .ok_or_else(|| McpError::ToolNotFound(format!("server '{}' not connected", server_name)))?;
        let conn = conn.read().await;
        conn.list_tools().await
    }
}

impl Default for McpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_tool_name() {
        let conn = McpConnection {
            name: "github-mcp".to_string(),
            transport: Box::new(SseTransport::new("http://localhost")),
            server_info: None,
            capabilities: None,
            tools: DashMap::new(),
        };

        assert_eq!(
            conn.namespaced_tool_name("create_issue"),
            "mcp_github_mcp_create_issue"
        );
    }
}
