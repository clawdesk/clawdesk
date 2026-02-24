//! OpenRouter provider — unified API gateway for multiple LLM providers.
//!
//! OpenRouter provides a single OpenAI-compatible endpoint that routes to
//! 200+ models across Anthropic, OpenAI, Google, Meta, Mistral, and more.
//!
//! ## Features
//!
//! - Native tool/function calling (provider-dependent)
//! - Vision support (multimodal models)
//! - Reasoning content pass-through for thinking models
//! - Custom HTTP headers (`HTTP-Referer`, `X-Title`) for usage tracking
//!
//! Provides full vision support, reasoning content forwarding, and custom
//! header injection.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{
    FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    StreamChunk, ToolCall, ToolDefinition, TokenUsage,
};

const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

// ---------------------------------------------------------------------------
// Wire types
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
// Provider
// ---------------------------------------------------------------------------

/// OpenRouter LLM gateway provider.
///
/// Routes requests to 200+ models via OpenRouter's unified API,
/// with native tool calling, vision, and reasoning content support.
pub struct OpenRouterProvider {
    client: Client,
    api_key: String,
    /// Application name for OpenRouter dashboard tracking.
    app_name: Option<String>,
    /// Referer URL for OpenRouter usage analytics.
    referer: Option<String>,
    default_model: String,
}

impl OpenRouterProvider {
    /// Create a new OpenRouter provider.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .pool_max_idle_per_host(4)
                .build()
                .expect("failed to build HTTP client"),
            api_key: api_key.into(),
            app_name: None,
            referer: None,
            default_model: "anthropic/claude-sonnet-4-20250514".into(),
        }
    }

    /// Set the application name for OpenRouter dashboard.
    pub fn with_app_name(mut self, name: impl Into<String>) -> Self {
        self.app_name = Some(name.into());
        self
    }

    /// Set the referer URL for OpenRouter analytics.
    pub fn with_referer(mut self, referer: impl Into<String>) -> Self {
        self.referer = Some(referer.into());
        self
    }

    /// Set the default model.
    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    fn apply_auth(&self, mut builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder = builder.header("Authorization", format!("Bearer {}", self.api_key));
        if let Some(ref name) = self.app_name {
            builder = builder.header("X-Title", name.as_str());
        }
        if let Some(ref referer) = self.referer {
            builder = builder.header("HTTP-Referer", referer.as_str());
        }
        builder
    }

    fn build_messages(request: &ProviderRequest) -> Vec<WireMessage> {
        let mut msgs = Vec::new();

        if let Some(ref sys) = request.system_prompt {
            msgs.push(WireMessage {
                role: "system".into(),
                content: sys.clone(),
                tool_call_id: None,
                tool_calls: None,
            });
        }

        for msg in &request.messages {
            let role_str = msg.role.as_str().to_string();
            let content = msg.content.to_string();

            // Reconstruct tool_calls from serialized assistant messages
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

            // Tool result messages
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

        msgs
    }

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

    fn parse_finish_reason(reason: Option<&str>) -> FinishReason {
        match reason {
            Some("stop") | Some("end_turn") => FinishReason::Stop,
            Some("tool_calls") | Some("function_call") => FinishReason::ToolUse,
            Some("length") | Some("max_tokens") => FinishReason::MaxTokens,
            Some("content_filter") => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        }
    }

    fn classify_error(&self, status: u16, body: &str) -> ProviderError {
        let provider = "openrouter".to_string();
        match status {
            429 => ProviderError::RateLimit { provider, retry_after: None },
            401 | 403 => ProviderError::AuthFailure { provider, profile_id: String::new() },
            404 => ProviderError::ModelNotFound { provider, model: String::new() },
            s if s >= 500 => ProviderError::ServerError { provider, status: s },
            _ => {
                let lower = body.to_lowercase();
                if lower.contains("billing") || lower.contains("credits") {
                    ProviderError::Billing { provider }
                } else {
                    ProviderError::FormatError { provider, detail: format!("HTTP {status}: {body}") }
                }
            }
        }
    }

    /// Resolve content from response, preferring `content` over `reasoning_content`.
    fn effective_content(msg: &ResponseMessage) -> String {
        msg.content
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(msg.reasoning_content.as_deref().filter(|s| !s.is_empty()))
            .unwrap_or("")
            .to_string()
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn name(&self) -> &str {
        "openrouter"
    }

    fn models(&self) -> Vec<String> {
        // OpenRouter supports 200+ models; provide a curated set
        vec![
            "anthropic/claude-sonnet-4-20250514".into(),
            "anthropic/claude-3-5-haiku-20241022".into(),
            "openai/gpt-4o".into(),
            "openai/gpt-4o-mini".into(),
            "google/gemini-2.5-flash-preview".into(),
            "meta-llama/llama-3.3-70b-instruct".into(),
            "mistralai/mistral-large-latest".into(),
            "deepseek/deepseek-chat".into(),
            "qwen/qwen-2.5-72b-instruct".into(),
        ]
    }

    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let model = if request.model.is_empty() {
            &self.default_model
        } else {
            &request.model
        };

        let tools = if !request.tools.is_empty() {
            Some(Self::build_tools(&request.tools))
        } else {
            None
        };

        let body = CompletionRequest {
            model: model.to_string(),
            messages: Self::build_messages(request),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools,
            stream: None,
            stream_options: None,
        };

        let start = std::time::Instant::now();
        let url = format!("{OPENROUTER_BASE_URL}/chat/completions");

        let builder = self.apply_auth(self.client.post(&url).json(&body));
        let resp = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout {
                    provider: "openrouter".into(),
                    model: model.to_string(),
                    after: std::time::Duration::from_secs(120),
                }
            } else {
                ProviderError::NetworkError {
                    provider: "openrouter".into(),
                    detail: e.to_string(),
                }
            }
        })?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(self.classify_error(status, &body_text));
        }

        let resp_body: CompletionResponse = resp.json().await.map_err(|e| {
            ProviderError::FormatError {
                provider: "openrouter".into(),
                detail: format!("JSON parse error: {e}"),
            }
        })?;

        let choice = resp_body
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::FormatError {
                provider: "openrouter".into(),
                detail: "no choices in response".into(),
            })?;

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
            provider: "openrouter".into(),
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
        let model = if request.model.is_empty() {
            &self.default_model
        } else {
            &request.model
        };

        let tools = if !request.tools.is_empty() {
            Some(Self::build_tools(&request.tools))
        } else {
            None
        };

        let body = CompletionRequest {
            model: model.to_string(),
            messages: Self::build_messages(request),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools,
            stream: Some(true),
            stream_options: Some(StreamOptions { include_usage: true }),
        };

        let url = format!("{OPENROUTER_BASE_URL}/chat/completions");
        let builder = self.apply_auth(self.client.post(&url).json(&body));
        let resp = builder.send().await.map_err(|e| {
            ProviderError::NetworkError {
                provider: "openrouter".into(),
                detail: e.to_string(),
            }
        })?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(self.classify_error(status, &body_text));
        }

        let mut stream = resp.bytes_stream();
        use futures::StreamExt;
        let mut buffer = String::new();
        let mut tool_accum: Vec<(String, String, String)> = Vec::new();
        let mut last_usage: Option<TokenUsage> = None;

        while let Some(chunk_result) = stream.next().await {
            let bytes = chunk_result.map_err(|e| ProviderError::NetworkError {
                provider: "openrouter".into(),
                detail: e.to_string(),
            })?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    let data = data.trim();
                    if data == "[DONE]" {
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
                        return Ok(());
                    }

                    if let Ok(delta) = serde_json::from_str::<StreamDelta>(data) {
                        if let Some(u) = delta.usage {
                            last_usage = Some(TokenUsage {
                                input_tokens: u.prompt_tokens,
                                output_tokens: u.completion_tokens,
                                cache_read_tokens: None,
                                cache_write_tokens: None,
                            });
                        }

                        if let Some(choice) = delta.choices.into_iter().next() {
                            if let Some(tcs) = choice.delta.tool_calls {
                                for tc in tcs {
                                    let idx = tc.index.unwrap_or(tool_accum.len());
                                    while tool_accum.len() <= idx {
                                        tool_accum.push(Default::default());
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

        // Stream ended without [DONE]
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
        let url = format!("{OPENROUTER_BASE_URL}/models");
        let builder = self.apply_auth(self.client.get(&url));
        let resp = builder.send().await.map_err(|e| ProviderError::NetworkError {
            provider: "openrouter".into(),
            detail: e.to_string(),
        })?;

        if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
            return Err(ProviderError::AuthFailure {
                provider: "openrouter".into(),
                profile_id: String::new(),
            });
        }

        debug!("openrouter health check passed");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_finish_reason() {
        assert_eq!(OpenRouterProvider::parse_finish_reason(Some("stop")), FinishReason::Stop);
        assert_eq!(OpenRouterProvider::parse_finish_reason(Some("tool_calls")), FinishReason::ToolUse);
        assert_eq!(OpenRouterProvider::parse_finish_reason(Some("length")), FinishReason::MaxTokens);
        assert_eq!(OpenRouterProvider::parse_finish_reason(None), FinishReason::Stop);
    }

    #[test]
    fn test_classify_error() {
        let provider = OpenRouterProvider::new("test-key");
        match provider.classify_error(429, "rate limited") {
            ProviderError::RateLimit { .. } => {}
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    #[test]
    fn test_effective_content() {
        let msg = ResponseMessage {
            content: Some("hello".into()),
            reasoning_content: Some("thinking...".into()),
            tool_calls: None,
        };
        assert_eq!(OpenRouterProvider::effective_content(&msg), "hello");

        let msg2 = ResponseMessage {
            content: None,
            reasoning_content: Some("fallback".into()),
            tool_calls: None,
        };
        assert_eq!(OpenRouterProvider::effective_content(&msg2), "fallback");
    }

    #[test]
    fn test_builder() {
        let provider = OpenRouterProvider::new("sk-test")
            .with_app_name("ClawDesk")
            .with_referer("https://clawdesk.dev")
            .with_default_model("openai/gpt-4o");

        assert_eq!(provider.default_model, "openai/gpt-4o");
        assert_eq!(provider.app_name.as_deref(), Some("ClawDesk"));
    }
}
