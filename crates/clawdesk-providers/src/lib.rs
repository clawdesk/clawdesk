//! # clawdesk-providers
//!
//! LLM provider adapters for ClawDesk.
//!
//! Each provider implements the `Provider` trait, providing a uniform interface
//! for sending messages to different LLM backends (Anthropic, OpenAI, Google, Ollama).
//!
//! Structured `ProviderError` variants replace regex-classified error strings.

pub mod anthropic;
pub mod bedrock;
pub mod capability;
pub mod gemini;
pub mod negotiator;
pub mod ollama;
pub mod openai;
pub mod registry;

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use clawdesk_types::estimate_tokens;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A request to send to an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub system_prompt: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub tools: Vec<ToolDefinition>,
    pub stream: bool,
}

/// Role in a chat message — closed enum eliminates heap-allocated role strings.
///
/// Serializes to lowercase (`"user"`, `"assistant"`, `"system"`, `"tool"`)
/// for compatibility with LLM provider APIs. Pattern-matching replaces all
/// `msg.role == "system"` string comparisons with zero-cost enum discriminant checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

impl MessageRole {
    /// Return the wire-format string for provider API requests.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A message in the chat format.
///
/// `cached_tokens` stores a pre-computed token estimate, enabling O(1) token
/// accounting when messages are appended or compacted. Without it, every
/// compaction pass must re-estimate all messages: O(n × |content|).
///
/// Uses `Arc<str>` for content to enable zero-copy sharing: when the same
/// message is referenced by history, compaction, and the provider request,
/// only one heap allocation exists. `Arc::clone` is a single atomic increment
/// vs `String::clone` which copies the entire buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    #[serde(serialize_with = "serialize_arc_str", deserialize_with = "deserialize_arc_str")]
    pub content: std::sync::Arc<str>,
    /// Pre-computed token estimate. Set by the agent runner on creation.
    /// Compaction reads this field instead of re-scanning `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<usize>,
}

fn serialize_arc_str<S: serde::Serializer>(arc: &std::sync::Arc<str>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(arc)
}

fn deserialize_arc_str<'de, D: serde::Deserializer<'de>>(d: D) -> Result<std::sync::Arc<str>, D::Error> {
    let s = String::deserialize(d)?;
    Ok(std::sync::Arc::from(s))
}

impl ChatMessage {
    /// Create a message with owned content.
    pub fn new(role: MessageRole, content: impl Into<std::sync::Arc<str>>) -> Self {
        Self {
            role,
            content: content.into(),
            cached_tokens: None,
        }
    }

    /// Return the cached token count, or estimate via the canonical
    /// LUT-accelerated character-class estimator from clawdesk-types.
    pub fn token_count(&self) -> usize {
        self.cached_tokens
            .unwrap_or_else(|| estimate_tokens(&self.content))
    }

    /// Invalidate the cached token count (e.g., after mutating `content`).
    pub fn invalidate_token_cache(&mut self) {
        self.cached_tokens = None;
    }
}

/// Tool definition for function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Response from an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub content: String,
    pub model: String,
    pub provider: String,
    pub usage: TokenUsage,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    #[serde(skip)]
    pub latency: Duration,
}

/// Token usage statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

/// A tool call requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolUse,
    MaxTokens,
    ContentFilter,
}

/// A chunk emitted during streaming completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    /// Incremental text fragment (may be empty on the final chunk).
    pub delta: String,
    /// True on the last chunk — callers should finalize after receiving this.
    pub done: bool,
    /// Populated only on the final chunk.
    pub finish_reason: Option<FinishReason>,
    /// Populated only on the final chunk.
    pub usage: Option<TokenUsage>,
}

/// Port: LLM provider interface.
#[async_trait]
pub trait Provider: Send + Sync + 'static {
    /// Provider name (e.g., "anthropic", "openai").
    fn name(&self) -> &str;

    /// List available models.
    fn models(&self) -> Vec<String>;

    /// Send a completion request.
    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError>;

    /// Stream a completion request.
    ///
    /// Sends `StreamChunk`s through `chunk_tx`. The final chunk has `done: true`.
    /// Default implementation falls back to `complete()` and emits a single chunk,
    /// so providers without native SSE support work out-of-the-box.
    async fn stream(
        &self,
        request: &ProviderRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), ProviderError> {
        let resp = self.complete(request).await?;
        let _ = chunk_tx
            .send(StreamChunk {
                delta: resp.content,
                done: true,
                finish_reason: Some(resp.finish_reason),
                usage: Some(resp.usage),
            })
            .await;
        Ok(())
    }

    /// Check if the provider is healthy / reachable.
    async fn health_check(&self) -> Result<(), ProviderError>;
}
