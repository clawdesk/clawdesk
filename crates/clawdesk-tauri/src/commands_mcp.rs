//! MCP (Model Context Protocol) commands — connect to MCP servers,
//! discover tools, call remote tools, and manage bundled templates.
//!
//! Wraps clawdesk-mcp's `McpClient` (multi-server connection manager)
//! for the Tauri IPC surface.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

// ── Response types ────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct McpServerInfo {
    pub name: String,
    pub transport: String,
    pub connected: bool,
    pub tool_count: usize,
}

#[derive(Debug, Serialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub server: String,
}

#[derive(Debug, Serialize)]
pub struct McpToolCallResult {
    pub content: Vec<McpContentItem>,
    pub is_error: bool,
}

#[derive(Debug, Serialize)]
pub struct McpContentItem {
    pub content_type: String,
    pub text: Option<String>,
    pub data: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct McpBundledTemplate {
    pub name: String,
    pub category: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct McpConnectRequest {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub url: Option<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
}

// ── Commands ──────────────────────────────────────────────────

/// List all connected MCP servers and their tool counts.
#[tauri::command]
pub async fn list_mcp_servers(
    state: State<'_, AppState>,
) -> Result<Vec<McpServerInfo>, String> {
    let client = state.mcp_client.read().await;
    let servers = client.connected_servers();
    let mut infos = Vec::new();
    for name in &servers {
        let tool_count = match client.list_tools_for_server(name).await {
            Ok(tools) => tools.len(),
            Err(_) => 0,
        };
        infos.push(McpServerInfo {
            name: name.clone(),
            transport: "connected".into(),
            connected: true,
            tool_count,
        });
    }
    Ok(infos)
}

/// Connect to an MCP server using stdio or SSE transport.
#[tauri::command]
pub async fn connect_mcp_server(
    request: McpConnectRequest,
    state: State<'_, AppState>,
) -> Result<McpServerInfo, String> {
    let config = match request.transport.as_str() {
        "stdio" => {
            let cmd = request.command.ok_or("stdio transport requires 'command'")?;
            let args = request.args.unwrap_or_default();
            clawdesk_mcp::McpServerConfig {
                name: request.name.clone(),
                transport: clawdesk_mcp::McpTransportConfig::Stdio { command: cmd, args },
                env: request.env.unwrap_or_default(),
                description: String::new(),
            }
        }
        "sse" => {
            let url = request.url.ok_or("SSE transport requires 'url'")?;
            clawdesk_mcp::McpServerConfig {
                name: request.name.clone(),
                transport: clawdesk_mcp::McpTransportConfig::Sse { url },
                env: request.env.unwrap_or_default(),
                description: String::new(),
            }
        }
        other => return Err(format!("Unknown transport: {}", other)),
    };

    let mut client = state.mcp_client.write().await;
    client
        .connect(config)
        .await
        .map_err(|e| format!("{:?}", e))?;

    Ok(McpServerInfo {
        name: request.name,
        transport: request.transport,
        connected: true,
        tool_count: 0,
    })
}

/// Disconnect from an MCP server.
#[tauri::command]
pub async fn disconnect_mcp_server(
    server_name: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let mut client = state.mcp_client.write().await;
    client
        .disconnect(&server_name)
        .await
        .map_err(|e| format!("{:?}", e))?;
    Ok(true)
}

/// List all tools available from a specific MCP server (or all servers).
#[tauri::command]
pub async fn list_mcp_tools(
    server_name: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<McpToolInfo>, String> {
    let client = state.mcp_client.read().await;
    let servers = match &server_name {
        Some(name) => vec![name.clone()],
        None => client.connected_servers(),
    };

    let mut tools = Vec::new();
    for name in &servers {
        match client.list_tools_for_server(name).await {
            Ok(server_tools) => {
                for tool in server_tools {
                    tools.push(McpToolInfo {
                        name: tool.name.clone(),
                        description: tool.description.clone().unwrap_or_default(),
                        input_schema: tool.input_schema.clone(),
                        server: name.clone(),
                    });
                }
            }
            Err(e) => {
                tracing::warn!(server = %name, error = ?e, "Failed to list tools from MCP server");
            }
        }
    }
    Ok(tools)
}

/// Call a tool on an MCP server.
#[tauri::command]
pub async fn call_mcp_tool(
    server_name: String,
    tool_name: String,
    arguments: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<McpToolCallResult, String> {
    let client = state.mcp_client.read().await;
    let args: std::collections::HashMap<String, serde_json::Value> = match arguments {
        serde_json::Value::Object(map) => map.into_iter().collect(),
        _ => std::collections::HashMap::new(),
    };
    let result = client
        .call_tool(&server_name, &tool_name, args)
        .await
        .map_err(|e| format!("{:?}", e))?;

    let content_items: Vec<McpContentItem> = result
        .content
        .iter()
        .map(|c| match c {
            clawdesk_mcp::McpContent::Text { text } => McpContentItem {
                content_type: "text".into(),
                text: Some(text.clone()),
                data: None,
                mime_type: None,
            },
            clawdesk_mcp::McpContent::Image { data, mime_type } => McpContentItem {
                content_type: "image".into(),
                text: None,
                data: Some(data.clone()),
                mime_type: Some(mime_type.clone()),
            },
            clawdesk_mcp::McpContent::Resource { uri, text, .. } => McpContentItem {
                content_type: "resource".into(),
                text: text.clone(),
                data: Some(uri.clone()),
                mime_type: None,
            },
        })
        .collect();

    Ok(McpToolCallResult {
        content: content_items,
        is_error: result.is_error,
    })
}

/// Get the status of a specific MCP server connection.
#[tauri::command]
pub async fn get_mcp_server_status(
    server_name: String,
    state: State<'_, AppState>,
) -> Result<McpServerInfo, String> {
    let client = state.mcp_client.read().await;
    let connected = client.connected_servers().contains(&server_name);
    Ok(McpServerInfo {
        name: server_name,
        transport: if connected { "active" } else { "disconnected" }.into(),
        connected,
        tool_count: 0,
    })
}

/// List all bundled MCP server templates (pre-configured integrations).
#[tauri::command]
pub async fn list_mcp_templates() -> Result<Vec<McpBundledTemplate>, String> {
    let templates = clawdesk_mcp::bundled::list_templates();
    Ok(templates
        .iter()
        .map(|t| McpBundledTemplate {
            name: t.name.to_string(),
            category: t.category.to_string(),
            description: t.description.to_string(),
        })
        .collect())
}

/// Get MCP template categories.
#[tauri::command]
pub async fn list_mcp_categories() -> Result<Vec<String>, String> {
    let cats = clawdesk_mcp::bundled::categories();
    Ok(cats.iter().map(|c| c.to_string()).collect())
}

/// Install/connect using a bundled MCP template by name.
#[tauri::command]
pub async fn install_mcp_template(
    template_name: String,
    env_overrides: Option<std::collections::HashMap<String, String>>,
    state: State<'_, AppState>,
) -> Result<McpServerInfo, String> {
    let template = clawdesk_mcp::bundled::get_template(&template_name)
        .ok_or_else(|| format!("Template '{}' not found", template_name))?;

    let config = clawdesk_mcp::bundled::parse_template(template)
        .map_err(|e| format!("Failed to parse template: {:?}", e))?;

    // Apply env overrides if provided
    let mut final_config = config;
    if let Some(overrides) = env_overrides {
        final_config.env.extend(overrides);
    }

    let mut client = state.mcp_client.write().await;
    client
        .connect(final_config)
        .await
        .map_err(|e| format!("{:?}", e))?;

    Ok(McpServerInfo {
        name: template_name,
        transport: "stdio".into(),
        connected: true,
        tool_count: 0,
    })
}

/// Disconnect all MCP servers.
#[tauri::command]
pub async fn disconnect_all_mcp(
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let client = state.mcp_client.read().await;
    client.disconnect_all().await;
    Ok(true)
}
