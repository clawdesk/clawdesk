//! # clawdesk-mcp
//!
//! Model Context Protocol (MCP) implementation for ClawDesk.
//!
//! Provides both **client** (consume external MCP servers) and **server**
//! (expose ClawDesk tools via MCP) functionality over JSON-RPC 2.0.
//!
//! ## Transports
//! - **Stdio**: Spawn subprocess, communicate via stdin/stdout lines
//! - **SSE**: HTTP POST for requests, Server-Sent Events for responses
//!
//! ## MCP Lifecycle
//! `initialize` → `notifications/initialized` → `tools/list` → `tools/call`

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub use client::McpClient;

#[cfg(feature = "server")]
pub mod server;

pub mod bundled;
pub mod auth;
pub mod discovery;
pub mod namespace;
pub mod protocol;
pub mod transport;

pub use protocol::*;
pub use auth::{McpAuthResolver, McpServerAuth, AuthScheme, AuthHeaders, McpAuthError};
pub use transport::{McpTransport, StreamableHttpTransport, StreamableHttpConfig};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// MCP Protocol Types (JSON-RPC 2.0)
// ---------------------------------------------------------------------------

/// JSON-RPC 2.0 request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::Value::Number(id.into())),
            method: method.into(),
            params,
        }
    }

    pub fn notification(method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: method.into(),
            params,
        }
    }
}

/// JSON-RPC 2.0 response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcError {
    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: msg.into(),
            data: None,
        }
    }
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: msg.into(),
            data: None,
        }
    }
    pub fn method_not_found(msg: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: msg.into(),
            data: None,
        }
    }
    pub fn internal_error(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
            data: None,
        }
    }
}

/// MCP errors
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("transport error: {0}")]
    Transport(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("connection closed")]
    ConnectionClosed,

    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),

    #[error("server error: code={code}, message={message}")]
    ServerError { code: i64, message: String },

    #[error("tool not found: {0}")]
    ToolNotFound(String),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
