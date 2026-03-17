//! Generic OpenAI-compatible provider adapter.
//!
//! Many LLM providers expose an OpenAI-compatible chat completions API.
//! This module provides a single configurable `OpenAiCompatibleProvider`
//! that works with any provider following the OpenAI API convention:
//!
//! - Groq, Mistral, Together, Fireworks, Perplexity, DeepSeek, xAI (Grok),
//!   NVIDIA NIM, Moonshot, Qwen, Cloudflare Workers AI, Venice, etc.
//!
//! ## Architecture
//!
//! Instead of creating a separate provider file for each OpenAI-compatible
//! service, this module uses a builder-pattern configuration struct that
//! captures the relevant differences:
//!
//! - Base URL
//! - Authentication style (Bearer token, X-API-Key header, custom header)
//! - Whether vision/streaming/native tools are supported
//! - Whether to merge system messages into user messages
//! - Custom HTTP headers
//!
//! Supports ~20+ providers through a single configurable type.

use async_trait::async_trait;
use clawdesk_types::error::{ProviderError, ProviderErrorKind};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{
    FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    StreamChunk, ToolCall, ToolDefinition, TokenUsage,
};

// ---------------------------------------------------------------------------
// Authentication style
// ---------------------------------------------------------------------------

/// How the provider expects credentials to be sent.
#[derive(Debug, Clone)]
pub enum AuthStyle {
    /// `Authorization: Bearer {token}` — most common.
    Bearer,
    /// `x-api-key: {key}` — Anthropic-style.
    XApiKey,
    /// Custom header name.
    Custom(String),
}

// ---------------------------------------------------------------------------
// Provider configuration
// ---------------------------------------------------------------------------

/// Configuration for an OpenAI-compatible provider instance.
#[derive(Debug, Clone)]
pub struct CompatibleConfig {
    /// Human-readable provider name (e.g., "groq", "mistral").
    pub name: String,
    /// Base URL (e.g., "https://api.groq.com/openai/v1").
    pub base_url: String,
    /// API credential.
    pub credential: String,
    /// How to send the credential.
    pub auth_style: AuthStyle,
    /// Whether the provider supports vision (multimodal).
    pub supports_vision: bool,
    /// Whether the provider supports SSE streaming.
    pub supports_streaming: bool,
    /// Whether the provider supports native function/tool calling.
    pub supports_native_tools: bool,
    /// Merge system messages into the first user message.
    /// Some providers (e.g., certain Qwen models) don't accept `role: system`.
    pub merge_system_into_user: bool,
    /// Custom user-agent string.
    pub user_agent: Option<String>,
    /// Additional static headers.
    pub extra_headers: Vec<(String, String)>,
    /// Default model for this provider.
    pub default_model: String,
    /// Available model list.
    pub models: Vec<String>,
}

impl CompatibleConfig {
    /// Create a minimal config with Bearer auth.
    pub fn new(name: impl Into<String>, base_url: impl Into<String>, credential: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            credential: credential.into(),
            auth_style: AuthStyle::Bearer,
            supports_vision: false,
            supports_streaming: true,
            supports_native_tools: true,
            merge_system_into_user: false,
            user_agent: None,
            extra_headers: Vec::new(),
            default_model: String::new(),
            models: Vec::new(),
        }
    }

    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    pub fn with_models(mut self, models: Vec<String>) -> Self {
        self.models = models;
        self
    }

    pub fn with_vision(mut self) -> Self {
        self.supports_vision = true;
        self
    }

    pub fn with_auth_style(mut self, style: AuthStyle) -> Self {
        self.auth_style = style;
        self
    }

    pub fn with_extra_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((key.into(), value.into()));
        self
    }

    pub fn with_merge_system(mut self) -> Self {
        self.merge_system_into_user = true;
        self
    }

    /// Disable native function/tool calling.
    ///
    /// Use this for local models (vLLM, llama.cpp, etc.) that don't reliably
    /// support the OpenAI tool-calling wire format. When disabled, tool
    /// definitions are omitted from the request body so the model responds
    /// with plain text instead of attempting function calls.
    pub fn without_native_tools(mut self) -> Self {
        self.supports_native_tools = false;
        self
    }
}

// ---------------------------------------------------------------------------
// Request / response wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CompletionRequest {
    model: String,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct WireMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCallOut>>,
}

#[derive(Serialize)]
struct WireToolCallOut {
    id: String,
    r#type: String,
    function: WireFunctionOut,
}

#[derive(Serialize)]
struct WireFunctionOut {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct WireTool {
    r#type: String,
    function: WireFunction,
}

#[derive(Serialize)]
struct WireFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<UsageResponse>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Deserialize)]
struct ResponseToolCall {
    id: String,
    function: ResponseFunction,
}

#[derive(Deserialize)]
struct ResponseFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct UsageResponse {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

// SSE streaming types
#[derive(Deserialize)]
struct StreamDelta {
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<UsageResponse>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: DeltaContent,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct DeltaContent {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Deserialize)]
struct DeltaToolCall {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<DeltaFunction>,
}

#[derive(Deserialize)]
struct DeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// ---------------------------------------------------------------------------
// Provider implementation
// ---------------------------------------------------------------------------

/// Generic OpenAI-compatible provider.
///
/// Works with any service that implements the OpenAI chat completions API
/// format. Configure via `CompatibleConfig` to customize auth, headers,
/// capabilities, and models.
pub struct OpenAiCompatibleProvider {
    config: CompatibleConfig,
    client: Client,
}

impl OpenAiCompatibleProvider {
    /// Create a new compatible provider from configuration.
    pub fn new(config: CompatibleConfig) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .pool_max_idle_per_host(4)
            .build()
            .expect("failed to build HTTP client");

        Self { config, client }
    }

    /// Build the chat completions URL.
    fn completions_url(&self) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_string()
        } else if base.ends_with("/v1") || base.ends_with("/v2") {
            format!("{base}/chat/completions")
        } else {
            format!("{base}/chat/completions")
        }
    }

    /// Apply authentication to a request builder.
    fn apply_auth(&self, mut builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.config.auth_style {
            AuthStyle::Bearer => {
                builder = builder.header("Authorization", format!("Bearer {}", self.config.credential));
            }
            AuthStyle::XApiKey => {
                builder = builder.header("x-api-key", &self.config.credential);
            }
            AuthStyle::Custom(header) => {
                builder = builder.header(header.as_str(), &self.config.credential);
            }
        }
        // Apply extra headers
        for (key, value) in &self.config.extra_headers {
            builder = builder.header(key.as_str(), value.as_str());
        }
        if let Some(ua) = &self.config.user_agent {
            builder = builder.header("User-Agent", ua.as_str());
        }
        builder
    }

    /// Convert internal messages to wire format.
    fn build_messages(&self, request: &ProviderRequest) -> Vec<WireMessage> {
        let mut msgs = Vec::new();

        // System prompt
        if let Some(ref sys) = request.system_prompt {
            if self.config.merge_system_into_user {
                // Will be merged into first user message below
            } else {
                msgs.push(WireMessage {
                    role: "system".into(),
                    content: sys.clone(),
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
        }

        for msg in &request.messages {
            let role_str = msg.role.as_str().to_string();
            let content = msg.content.to_string();

            // If merging system into first user message
            if self.config.merge_system_into_user
                && msg.role == MessageRole::User
                && msgs.is_empty()
            {
                let merged = if let Some(ref sys) = request.system_prompt {
                    format!("{sys}\n\n{content}")
                } else {
                    content
                };
                msgs.push(WireMessage {
                    role: role_str,
                    content: merged,
                    tool_call_id: None,
                    tool_calls: None,
                });
                continue;
            }

            // Try to reconstruct tool_calls from assistant message content
            if msg.role == MessageRole::Assistant {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(tcs) = parsed.get("tool_calls").and_then(|v| v.as_array()) {
                        let wire_tcs: Vec<WireToolCallOut> = tcs
                            .iter()
                            .filter_map(|tc| {
                                Some(WireToolCallOut {
                                    id: tc.get("id")?.as_str()?.to_string(),
                                    r#type: "function".to_string(),
                                    function: WireFunctionOut {
                                        name: tc.get("function")?.get("name")?.as_str()?.to_string(),
                                        arguments: tc
                                            .get("function")?
                                            .get("arguments")?
                                            .as_str()?
                                            .to_string(),
                                    },
                                })
                            })
                            .collect();

                        if !wire_tcs.is_empty() {
                            let text_content = parsed
                                .get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            msgs.push(WireMessage {
                                role: role_str,
                                content: text_content,
                                tool_call_id: None,
                                tool_calls: Some(wire_tcs),
                            });
                            continue;
                        }
                    }
                }
            }

            // Tool result message
            if msg.role == MessageRole::Tool {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                    let tool_call_id = parsed
                        .get("tool_call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let result_content = parsed
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&content)
                        .to_string();

                    msgs.push(WireMessage {
                        role: "tool".into(),
                        content: result_content,
                        tool_call_id: Some(tool_call_id),
                        tool_calls: None,
                    });
                    continue;
                }
            }

            msgs.push(WireMessage {
                role: role_str,
                content,
                tool_call_id: None,
                tool_calls: None,
            });
        }

        // Strip trailing assistant message (prefill) — some servers with
        // `enable_thinking` enabled reject requests that end with an
        // assistant message.  This is safe: a trailing assistant message
        // without tool_calls is always an optional prefill hint that we can
        // drop without changing semantics.
        if let Some(last) = msgs.last() {
            if last.role == "assistant" && last.tool_calls.is_none() {
                msgs.pop();
            }
        }

        msgs
    }

    /// Convert tool definitions to wire format.
    fn build_tools(tools: &[ToolDefinition]) -> Vec<WireTool> {
        tools
            .iter()
            .map(|t| WireTool {
                r#type: "function".to_string(),
                function: WireFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect()
    }

    /// Parse finish reason string.
    fn parse_finish_reason(reason: Option<&str>) -> FinishReason {
        match reason {
            Some("stop") | Some("end_turn") => FinishReason::Stop,
            Some("tool_calls") | Some("function_call") => FinishReason::ToolUse,
            Some("length") | Some("max_tokens") => FinishReason::MaxTokens,
            Some("content_filter") => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        }
    }

    /// Classify HTTP errors into structured ProviderError.
    fn classify_error(&self, status: u16, body: &str) -> ProviderError {
        let provider = self.config.name.clone();

        if status == 429 {
            return ProviderError::rate_limit(provider, None);
        }
        if status == 401 || status == 403 {
            return ProviderError::auth_failure(provider, String::new());
        }
        if status == 404 {
            return ProviderError::model_not_found(provider, String::new());
        }

        let lower = body.to_lowercase();
        if lower.contains("billing") || lower.contains("quota") || lower.contains("insufficient") {
            return ProviderError::billing(provider);
        }
        if lower.contains("context") && (lower.contains("length") || lower.contains("window")) {
            return ProviderError::context_length_exceeded(provider, String::new(), body.to_string());
        }
        // Detect thinking/prefill incompatibility — server has enable_thinking
        // active and rejects the assistant prefill we sent. Treat as a format
        // error with a clear message so failover can proceed.
        if lower.contains("prefill") && lower.contains("thinking") {
            return ProviderError::format_error(
                provider,
                format!("thinking/prefill conflict (server has enable_thinking active): {body}"),
            );
        }

        if status >= 500 {
            ProviderError::server_error(provider, status)
        } else {
            ProviderError::format_error(provider, format!("HTTP {status}: {body}"))
        }
    }

    /// Extract content, preferring `content` but falling back to `reasoning_content`.
    fn effective_content(msg: &ResponseMessage) -> String {
        if let Some(ref c) = msg.content {
            if !c.is_empty() {
                // Strip <think>...</think> blocks if present
                return strip_think_tags(c);
            }
        }
        if let Some(ref r) = msg.reasoning_content {
            if !r.is_empty() {
                return r.clone();
            }
        }
        String::new()
    }
}

/// Strip `<think>...</think>` tags from inline reasoning content.
fn strip_think_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find("<think>") {
        result.push_str(&remaining[..start]);
        if let Some(end) = remaining[start..].find("</think>") {
            remaining = &remaining[start + end + 8..];
        } else {
            // Unclosed <think> tag — drop everything after it
            return result;
        }
    }
    result.push_str(remaining);

    let trimmed = result.trim();
    if trimmed.is_empty() && !text.is_empty() {
        // All content was inside think tags — return empty
        String::new()
    } else {
        trimmed.to_string()
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn models(&self) -> Vec<String> {
        self.config.models.clone()
    }

    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let url = self.completions_url();
        let messages = self.build_messages(request);
        let model = if request.model.is_empty()
            || request.model == "default"
            || request.model == "auto"
        {
            &self.config.default_model
        } else {
            &request.model
        };

        let tools = if !request.tools.is_empty() && self.config.supports_native_tools {
            Some(Self::build_tools(&request.tools))
        } else {
            None
        };

        let body = CompletionRequest {
            model: model.to_string(),
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools,
            stream: None,
            stream_options: None,
        };

        let start = std::time::Instant::now();

        let builder = self.client.post(&url).json(&body);
        let builder = self.apply_auth(builder);
        let resp = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::timeout(self.config.name.clone(), model.to_string(), std::time::Duration::from_secs(120))
            } else {
                ProviderError::network_error(self.config.name.clone(), e.to_string())
            }
        })?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(self.classify_error(status, &body_text));
        }

        let resp_body: CompletionResponse = resp.json().await.map_err(|e| {
            ProviderError::format_error(self.config.name.clone(), format!("JSON parse error: {e}"))
        })?;

        let choice = resp_body
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::format_error(self.config.name.clone(), "no choices in response"))?;

        let content = Self::effective_content(&choice.message);
        let finish_reason = Self::parse_finish_reason(choice.finish_reason.as_deref());

        let tool_calls: Vec<ToolCall> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::String(tc.function.arguments)),
            })
            .collect();

        let usage = resp_body.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cache_read_tokens: None,
            cache_write_tokens: None,
        });

        Ok(ProviderResponse {
            content,
            model: resp_body.model.unwrap_or_else(|| model.to_string()),
            provider: self.config.name.clone(),
            usage: usage.unwrap_or_default(),
            tool_calls,
            finish_reason,
            latency: start.elapsed(),
        })
    }

    async fn stream(
        &self,
        request: &ProviderRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), ProviderError> {
        if !self.config.supports_streaming {
            // Fall back to non-streaming
            return Provider::stream(self, request, chunk_tx).await;
        }

        let url = self.completions_url();
        let messages = self.build_messages(request);
        let model = if request.model.is_empty()
            || request.model == "default"
            || request.model == "auto"
        {
            &self.config.default_model
        } else {
            &request.model
        };

        let tools = if !request.tools.is_empty() && self.config.supports_native_tools {
            Some(Self::build_tools(&request.tools))
        } else {
            None
        };

        let body = CompletionRequest {
            model: model.to_string(),
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools,
            stream: Some(true),
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
        };

        let builder = self.client.post(&url).json(&body);
        let builder = self.apply_auth(builder);
        let resp = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::timeout(self.config.name.clone(), model.to_string(), std::time::Duration::from_secs(120))
            } else {
                ProviderError::network_error(self.config.name.clone(), e.to_string())
            }
        })?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(self.classify_error(status, &body_text));
        }

        // Stream SSE events
        let mut stream = resp.bytes_stream();
        use futures::StreamExt;
        let mut buffer = String::new();
        let mut tool_accum: Vec<(String, String, String)> = Vec::new(); // (id, name, args)
        let mut last_usage: Option<TokenUsage> = None;
        let mut first_chunk = true;

        while let Some(chunk_result) = stream.next().await {
            let bytes = chunk_result.map_err(|e| ProviderError::network_error(self.config.name.clone(), e.to_string()))?;

            // Detect binary/compressed responses on the first chunk.
            // Gzip starts with 0x1f 0x8b; other non-UTF-8 payloads will
            // also fail this check.  Bail early instead of pushing lossy
            // replacement characters into the SSE parser.
            if first_chunk {
                first_chunk = false;
                if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
                    return Err(ProviderError::format_error(
                        self.config.name.clone(),
                        "server returned gzip-compressed body — expected text/event-stream SSE".to_string(),
                    ));
                }
                if std::str::from_utf8(&bytes).is_err() {
                    return Err(ProviderError::format_error(
                        self.config.name.clone(),
                        "server returned non-UTF-8 binary data — expected text/event-stream SSE".to_string(),
                    ));
                }
            }

            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Process complete SSE lines
            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    let data = data.trim();
                    if data == "[DONE]" {
                        // Final — send done chunk
                        let tool_calls: Vec<ToolCall> = tool_accum
                            .drain(..)
                            .map(|(id, name, args)| ToolCall {
                                id,
                                name,
                                arguments: serde_json::from_str(&args)
                                    .unwrap_or(serde_json::Value::String(args)),
                            })
                            .collect();

                        let finish = if tool_calls.is_empty() {
                            FinishReason::Stop
                        } else {
                            FinishReason::ToolUse
                        };

                        let _ = chunk_tx
                            .send(StreamChunk {
                                delta: String::new(),
                                reasoning_delta: String::new(),
                                done: true,
                                finish_reason: Some(finish),
                                usage: last_usage.take(),
                                tool_calls,
                            })
                            .await;
                        return Ok(());
                    }

                    if let Ok(delta) = serde_json::from_str::<StreamDelta>(data) {
                        // Track usage
                        if let Some(u) = delta.usage {
                            last_usage = Some(TokenUsage {
                                input_tokens: u.prompt_tokens,
                                output_tokens: u.completion_tokens,
                                cache_read_tokens: None,
                                cache_write_tokens: None,
                            });
                        }

                        if let Some(choice) = delta.choices.into_iter().next() {
                            // Accumulate tool call deltas
                            if let Some(tcs) = choice.delta.tool_calls {
                                for tc in tcs {
                                    let idx = tc.index.unwrap_or(tool_accum.len());
                                    while tool_accum.len() <= idx {
                                        tool_accum.push((String::new(), String::new(), String::new()));
                                    }
                                    if let Some(id) = tc.id {
                                        tool_accum[idx].0 = id;
                                    }
                                    if let Some(f) = tc.function {
                                        if let Some(name) = f.name {
                                            tool_accum[idx].1 = name;
                                        }
                                        if let Some(args) = f.arguments {
                                            tool_accum[idx].2.push_str(&args);
                                        }
                                    }
                                }
                            }

                            // Emit text delta (content only — reasoning_content streamed separately)
                            let text = choice.delta.content.unwrap_or_default();
                            let reasoning = choice.delta.reasoning_content.unwrap_or_default();

                            if !text.is_empty() || !reasoning.is_empty() {
                                let _ = chunk_tx
                                    .send(StreamChunk {
                                        delta: text,
                                        reasoning_delta: reasoning,
                                        done: false,
                                        finish_reason: None,
                                        usage: None,
                                        tool_calls: Vec::new(),
                                    })
                                    .await;
                            }
                        }
                    }
                }
            }
        }

        // Stream ended without [DONE] — send final chunk
        let tool_calls: Vec<ToolCall> = tool_accum
            .drain(..)
            .map(|(id, name, args)| ToolCall {
                id,
                name,
                arguments: serde_json::from_str(&args)
                    .unwrap_or(serde_json::Value::String(args)),
            })
            .collect();

        let _ = chunk_tx
            .send(StreamChunk {
                delta: String::new(),
                reasoning_delta: String::new(),
                done: true,
                finish_reason: Some(if tool_calls.is_empty() {
                    FinishReason::Stop
                } else {
                    FinishReason::ToolUse
                }),
                usage: last_usage.take(),
                tool_calls,
            })
            .await;

        Ok(())
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        // Try a minimal request to validate auth
        let url = {
            let base = self.config.base_url.trim_end_matches('/');
            format!("{base}/models")
        };

        let builder = self.client.get(&url);
        let builder = self.apply_auth(builder);
        let resp = builder.send().await.map_err(|e| ProviderError::network_error(self.config.name.clone(), e.to_string()))?;

        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            return Err(ProviderError::auth_failure(self.config.name.clone(), String::new()));
        }

        debug!(provider = %self.config.name, "health check passed");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Factory functions for well-known providers
// ---------------------------------------------------------------------------

/// Create a Groq provider.
pub fn groq(api_key: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        CompatibleConfig::new("groq", "https://api.groq.com/openai/v1", api_key)
            .with_default_model("llama-3.3-70b-versatile")
            .with_models(vec![
                "llama-3.3-70b-versatile".into(),
                "llama-3.1-8b-instant".into(),
                "mixtral-8x7b-32768".into(),
                "gemma2-9b-it".into(),
            ]),
    )
}

/// Create a Mistral provider.
pub fn mistral(api_key: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        CompatibleConfig::new("mistral", "https://api.mistral.ai/v1", api_key)
            .with_default_model("mistral-large-latest")
            .with_models(vec![
                "mistral-large-latest".into(),
                "mistral-medium-latest".into(),
                "mistral-small-latest".into(),
                "open-mistral-nemo".into(),
                "codestral-latest".into(),
            ]),
    )
}

/// Create a Together AI provider.
pub fn together(api_key: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        CompatibleConfig::new("together", "https://api.together.xyz/v1", api_key)
            .with_default_model("meta-llama/Llama-3.3-70B-Instruct-Turbo")
            .with_vision()
            .with_models(vec![
                "meta-llama/Llama-3.3-70B-Instruct-Turbo".into(),
                "mistralai/Mixtral-8x22B-Instruct-v0.1".into(),
                "Qwen/Qwen2.5-72B-Instruct-Turbo".into(),
            ]),
    )
}

/// Create a Fireworks AI provider.
pub fn fireworks(api_key: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        CompatibleConfig::new("fireworks", "https://api.fireworks.ai/inference/v1", api_key)
            .with_default_model("accounts/fireworks/models/llama-v3p3-70b-instruct")
            .with_models(vec![
                "accounts/fireworks/models/llama-v3p3-70b-instruct".into(),
                "accounts/fireworks/models/mixtral-8x22b-instruct".into(),
            ]),
    )
}

/// Create a Perplexity provider.
pub fn perplexity(api_key: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        CompatibleConfig::new("perplexity", "https://api.perplexity.ai", api_key)
            .with_default_model("sonar-pro")
            .with_models(vec![
                "sonar-pro".into(),
                "sonar".into(),
                "sonar-reasoning-pro".into(),
            ]),
    )
}

/// Create a DeepSeek provider.
pub fn deepseek(api_key: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        CompatibleConfig::new("deepseek", "https://api.deepseek.com/v1", api_key)
            .with_default_model("deepseek-chat")
            .with_models(vec![
                "deepseek-chat".into(),
                "deepseek-reasoner".into(),
                "deepseek-coder".into(),
            ]),
    )
}

/// Create an xAI (Grok) provider.
pub fn xai(api_key: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        CompatibleConfig::new("xai", "https://api.x.ai/v1", api_key)
            .with_default_model("grok-2")
            .with_vision()
            .with_models(vec![
                "grok-2".into(),
                "grok-2-mini".into(),
                "grok-3".into(),
            ]),
    )
}

/// Create an NVIDIA NIM provider.
pub fn nvidia(api_key: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        CompatibleConfig::new("nvidia", "https://integrate.api.nvidia.com/v1", api_key)
            .with_default_model("nvidia/llama-3.1-nemotron-70b-instruct")
            .with_models(vec![
                "nvidia/llama-3.1-nemotron-70b-instruct".into(),
                "nvidia/nemotron-4-340b-instruct".into(),
            ]),
    )
}

/// Create a Cloudflare Workers AI provider.
pub fn cloudflare(account_id: &str, api_key: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        CompatibleConfig::new(
            "cloudflare",
            &format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1"),
            api_key,
        )
        .with_default_model("@cf/meta/llama-3.3-70b-instruct-fp8-fast")
        .with_models(vec![
            "@cf/meta/llama-3.3-70b-instruct-fp8-fast".into(),
            "@cf/mistral/mistral-7b-instruct-v0.2-lora".into(),
        ]),
    )
}

/// Create a Venice AI provider.
pub fn venice(api_key: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(
        CompatibleConfig::new("venice", "https://api.venice.ai/api/v1", api_key)
            .with_default_model("llama-3.3-70b")
            .with_models(vec![
                "llama-3.3-70b".into(),
                "deepseek-r1-671b".into(),
            ]),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatMessage;

    #[test]
    fn test_strip_think_tags() {
        assert_eq!(strip_think_tags("hello"), "hello");
        assert_eq!(strip_think_tags("<think>reasoning</think>answer"), "answer");
        assert_eq!(
            strip_think_tags("before<think>middle</think>after"),
            "beforeafter"
        );
        assert_eq!(strip_think_tags("<think>all thinking</think>"), "");
    }

    #[test]
    fn test_completions_url() {
        let provider = OpenAiCompatibleProvider::new(
            CompatibleConfig::new("test", "https://api.example.com/v1", "key"),
        );
        assert_eq!(
            provider.completions_url(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_completions_url_already_complete() {
        let provider = OpenAiCompatibleProvider::new(
            CompatibleConfig::new("test", "https://api.example.com/v1/chat/completions", "key"),
        );
        assert_eq!(
            provider.completions_url(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_parse_finish_reason() {
        assert_eq!(
            OpenAiCompatibleProvider::parse_finish_reason(Some("stop")),
            FinishReason::Stop
        );
        assert_eq!(
            OpenAiCompatibleProvider::parse_finish_reason(Some("tool_calls")),
            FinishReason::ToolUse
        );
        assert_eq!(
            OpenAiCompatibleProvider::parse_finish_reason(Some("length")),
            FinishReason::MaxTokens
        );
        assert_eq!(
            OpenAiCompatibleProvider::parse_finish_reason(None),
            FinishReason::Stop
        );
    }

    #[test]
    fn test_config_builder() {
        let config = CompatibleConfig::new("test", "https://api.test.com", "sk-test")
            .with_default_model("model-v1")
            .with_vision()
            .with_merge_system()
            .with_extra_header("X-Custom", "value");

        assert_eq!(config.name, "test");
        assert!(config.supports_vision);
        assert!(config.merge_system_into_user);
        assert_eq!(config.extra_headers.len(), 1);
    }

    #[test]
    fn test_build_messages_system_merge() {
        let config = CompatibleConfig::new("test", "https://api.test.com", "key")
            .with_merge_system();
        let provider = OpenAiCompatibleProvider::new(config);

        let request = ProviderRequest {
            model: "test-model".into(),
            messages: vec![ChatMessage::new(MessageRole::User, "Hello")],
            system_prompt: Some("You are helpful".into()),
            max_tokens: None,
            temperature: None,
            tools: Vec::new(),
            stream: false,
            images: vec![],
        };

        let msgs = provider.build_messages(&request);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        assert!(msgs[0].content.contains("You are helpful"));
        assert!(msgs[0].content.contains("Hello"));
    }

    #[test]
    fn test_classify_error() {
        let provider = OpenAiCompatibleProvider::new(
            CompatibleConfig::new("test", "https://api.test.com", "key"),
        );

        match provider.classify_error(429, "rate limited").kind {
            ProviderErrorKind::RateLimit { .. } => {}
            ref other => panic!("expected RateLimit, got {other:?}"),
        }

        match provider.classify_error(401, "unauthorized").kind {
            ProviderErrorKind::AuthFailure { .. } => {}
            ref other => panic!("expected AuthFailure, got {other:?}"),
        }

        match provider.classify_error(500, "internal error").kind {
            ProviderErrorKind::ServerError { status: 500 } => {}
            ref other => panic!("expected ServerError(500), got {other:?}"),
        }
    }
}
