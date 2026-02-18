//! Gateway protocol types — canonical message format and protocol adapters.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Canonical internal message format — all protocols translate to/from this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalMessage {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub kind: CanonicalMessageKind,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// The discriminated union of all canonical message types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CanonicalMessageKind {
    /// Chat completion request.
    ChatRequest {
        messages: Vec<ChatMessagePayload>,
        model: Option<String>,
        tools: Vec<serde_json::Value>,
        stream: bool,
    },
    /// Chat completion response.
    ChatResponse {
        content: String,
        model: String,
        usage: TokenUsagePayload,
        finish_reason: String,
        tool_calls: Vec<ToolCallPayload>,
    },
    /// Streaming chunk.
    StreamChunk {
        content: String,
        done: bool,
    },
    /// RPC method call.
    RpcRequest {
        method: String,
        params: serde_json::Value,
        request_id: String,
    },
    /// RPC method response.
    RpcResponse {
        request_id: String,
        result: Option<serde_json::Value>,
        error: Option<RpcErrorPayload>,
    },
    /// Session event.
    SessionEvent {
        session_key: String,
        event: String,
        data: serde_json::Value,
    },
    /// Health/status.
    Health {
        status: String,
        version: String,
        uptime_secs: u64,
    },
    /// Ping/pong keepalive.
    Ping { nonce: u64 },
    Pong { nonce: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessagePayload {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsagePayload {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallPayload {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcErrorPayload {
    pub code: i32,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

/// Protocol identifier for adapter dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProtocolId {
    /// Native ClawDesk WebSocket protocol.
    Native,
    /// OpenAI Chat Completions compatible.
    OpenAiCompat,
    /// OpenResponses API compatible.
    OpenResponses,
    /// MCP (Model Context Protocol).
    Mcp,
}

/// RPC method metadata for the typed registry.
#[derive(Debug, Clone)]
pub struct RpcMethodMeta {
    pub name: String,
    pub description: String,
    /// Rate limit: max calls per minute.
    pub rate_limit: Option<u32>,
    /// Required auth level.
    pub auth_level: AuthLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AuthLevel {
    /// No auth required.
    None,
    /// Valid token required.
    Token,
    /// Admin token required.
    Admin,
}
