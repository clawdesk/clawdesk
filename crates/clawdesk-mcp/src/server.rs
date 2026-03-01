//! MCP Server — expose ClawDesk tools via MCP protocol.
//!
//! Handles JSON-RPC requests: `initialize`, `tools/list`, `tools/call`, `ping`.
//! Can be wired into stdio transport (CLI) or Axum SSE endpoint (gateway).

use crate::protocol::*;
use crate::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpError};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info};

/// A registered tool that can be called via MCP.
pub struct McpServerTool {
    pub schema: McpTool,
    pub handler:
        Box<dyn Fn(HashMap<String, Value>) -> futures::future::BoxFuture<'static, Result<ToolCallResult, String>> + Send + Sync>,
}

impl std::fmt::Debug for McpServerTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerTool")
            .field("name", &self.schema.name)
            .finish()
    }
}

/// MCP Server handler — stateless JSON-RPC request processor.
#[derive(Debug)]
pub struct McpServer {
    /// Server identity
    pub name: String,
    pub version: String,
    /// Registered tools
    tools: HashMap<String, Arc<McpServerTool>>,
    /// Server instructions (optional)
    pub instructions: Option<String>,
}

impl McpServer {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tools: HashMap::new(),
            instructions: None,
        }
    }

    /// Register a tool.
    pub fn register_tool(&mut self, tool: McpServerTool) {
        self.tools.insert(tool.schema.name.clone(), Arc::new(tool));
    }

    /// Handle a JSON-RPC request and produce a response.
    pub async fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone();

        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(&request),
            "tools/list" => self.handle_tools_list(&request),
            "tools/call" => self.handle_tools_call(&request).await,
            "ping" => Ok(serde_json::json!({})),
            "notifications/initialized" => {
                // Notification — no response needed, but return empty for protocol
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(Value::Null),
                    error: None,
                };
            }
            _ => Err(JsonRpcError::method_not_found(format!(
                "unknown method: {}",
                request.method
            ))),
        };

        match result {
            Ok(value) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(value),
                error: None,
            },
            Err(err) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(err),
            },
        }
    }

    fn handle_initialize(&self, _request: &JsonRpcRequest) -> Result<Value, JsonRpcError> {
        let result = InitializeResult {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability { list_changed: false }),
                resources: None,
                prompts: None,
                logging: None,
            },
            server_info: Implementation {
                name: self.name.clone(),
                version: self.version.clone(),
            },
            instructions: self.instructions.clone(),
        };

        info!(name = %self.name, version = %self.version, "MCP server initialized");

        serde_json::to_value(&result)
            .map_err(|e| JsonRpcError::internal_error(format!("serialize: {}", e)))
    }

    fn handle_tools_list(&self, _request: &JsonRpcRequest) -> Result<Value, JsonRpcError> {
        let tools: Vec<&McpTool> = self.tools.values().map(|t| &t.schema).collect();

        let result = ToolsListResult {
            tools: tools.into_iter().cloned().collect(),
            next_cursor: None,
        };

        debug!(count = result.tools.len(), "tools/list response");

        serde_json::to_value(&result)
            .map_err(|e| JsonRpcError::internal_error(format!("serialize: {}", e)))
    }

    async fn handle_tools_call(&self, request: &JsonRpcRequest) -> Result<Value, JsonRpcError> {
        let params: ToolCallParams = match &request.params {
            Some(p) => serde_json::from_value(p.clone())
                .map_err(|e| JsonRpcError::invalid_request(format!("invalid params: {}", e)))?,
            None => return Err(JsonRpcError::invalid_request("missing params")),
        };

        let tool = self
            .tools
            .get(&params.name)
            .ok_or_else(|| JsonRpcError::method_not_found(format!("tool '{}' not found", params.name)))?;

        debug!(tool = %params.name, "executing tool call");

        let handler = &tool.handler;
        let result = handler(params.arguments).await;

        match result {
            Ok(call_result) => serde_json::to_value(&call_result)
                .map_err(|e| JsonRpcError::internal_error(format!("serialize result: {}", e))),
            Err(err) => {
                let error_result = ToolCallResult {
                    content: vec![McpContent::Text { text: err }],
                    is_error: true,
                };
                serde_json::to_value(&error_result)
                    .map_err(|e| JsonRpcError::internal_error(format!("serialize error: {}", e)))
            }
        }
    }

    /// Run the MCP server in stdio mode (read from stdin, write to stdout).
    /// Used by `clawdesk mcp serve`.
    pub async fn run_stdio(&self) -> Result<(), McpError> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        info!(name = %self.name, "MCP server listening on stdio");

        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let line: String = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<JsonRpcRequest>(&line) {
                        Ok(request) => {
                            let response = self.handle_request(request).await;
                            let response_line = serde_json::to_string(&response)
                                .unwrap_or_else(|_| r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"serialize failed"}}"#.to_string());
                            stdout.write_all(response_line.as_bytes()).await?;
                            stdout.write_all(b"\n").await?;
                            stdout.flush().await?;
                        }
                        Err(e) => {
                            let error_response = JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: None,
                                result: None,
                                error: Some(JsonRpcError::parse_error(e.to_string())),
                            };
                            let line = serde_json::to_string(&error_response).unwrap_or_default();
                            stdout.write_all(line.as_bytes()).await?;
                            stdout.write_all(b"\n").await?;
                            stdout.flush().await?;
                        }
                    }
                }
                Ok(None) => {
                    info!("MCP server stdin closed, shutting down");
                    break;
                }
                Err(e) => {
                    return Err(McpError::Io(e));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handle_ping() {
        let server = McpServer::new("test", "0.1.0");
        let request = JsonRpcRequest::new(1, "ping", None);
        let response = server.handle_request(request).await;
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn handle_initialize() {
        let server = McpServer::new("clawdesk", "0.1.0");
        let request = JsonRpcRequest::new(1, "initialize", None);
        let response = server.handle_request(request).await;
        assert!(response.error.is_none());
        let result = response.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], "clawdesk");
    }

    #[tokio::test]
    async fn handle_tools_list_empty() {
        let server = McpServer::new("test", "0.1.0");
        let request = JsonRpcRequest::new(1, "tools/list", None);
        let response = server.handle_request(request).await;
        assert!(response.error.is_none());
        let result = response.result.unwrap();
        assert_eq!(result["tools"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let server = McpServer::new("test", "0.1.0");
        let request = JsonRpcRequest::new(1, "nonexistent/method", None);
        let response = server.handle_request(request).await;
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32601);
    }
}
