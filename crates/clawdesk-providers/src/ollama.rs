//! Ollama local model provider.
//!
//! Communicates with a local Ollama instance via its HTTP API:
//! - `/api/chat` — chat completions (streaming + non-streaming)
//! - `/api/tags` — model autodiscovery
//! - `/api/embeddings` — text embeddings
//!
//! Uses a persistent `reqwest::Client` with keep-alive against `localhost:11434`.
//! Zero sidecar overhead — Ollama runs independently.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::{
    ChatMessage, FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    StreamChunk, TokenUsage, ToolCall, ToolDefinition,
};

const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";

/// Ollama local model provider.
pub struct OllamaProvider {
    client: Client,
    base_url: String,
    default_model: String,
    /// Cached model list from `/api/tags`, refreshed on `health_check()`.
    cached_models: RwLock<Vec<String>>,
}

impl OllamaProvider {
    pub fn new(base_url: Option<String>, default_model: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(300)) // Local models can be slow on first load.
                .pool_idle_timeout(Duration::from_secs(90))
                .pool_max_idle_per_host(2)
                .build()
                .expect("failed to build HTTP client"),
            base_url: base_url.unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string()),
            default_model: default_model.unwrap_or_else(|| "llama3.2".to_string()),
            cached_models: RwLock::new(vec![]),
        }
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Discover available models via `/api/tags`.
    pub async fn discover_models(&self) -> Result<Vec<String>, ProviderError> {
        let resp = self
            .client
            .get(self.api_url("/api/tags"))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError {
                provider: "ollama".into(),
                detail: e.to_string(),
            })?;

        if !resp.status().is_success() {
            return Err(ProviderError::ServerError {
                provider: "ollama".into(),
                status: resp.status().as_u16(),
            });
        }

        let body: OllamaTagsResponse = resp.json().await.map_err(|e| {
            ProviderError::FormatError {
                provider: "ollama".into(),
                detail: e.to_string(),
            }
        })?;

        let models: Vec<String> = body.models.into_iter().map(|m| m.name).collect();
        info!(count = models.len(), "ollama: discovered models");

        // Cache the discovered models.
        if let Ok(mut cache) = self.cached_models.write() {
            *cache = models.clone();
        }

        Ok(models)
    }
}

// ---------------------------------------------------------------------------
// Ollama API types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModelInfo>,
}

#[derive(Debug, Deserialize)]
struct OllamaModelInfo {
    name: String,
}

#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OllamaToolDefinition>,
}

#[derive(Debug, Serialize)]
struct OllamaChatMessage {
    role: String,
    content: String,
    /// Tool calls made by the assistant (only for role=assistant).
    /// Ollama needs this to associate tool results with the original calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

/// Ollama tool definition — maps from our ToolDefinition type.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    function: OllamaToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// Ollama tool call returned in the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaToolCall {
    function: OllamaToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaToolCallFunction {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaResponseMessage,
    model: String,
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    role: String,
    content: String,
    #[serde(default)]
    tool_calls: Vec<OllamaToolCall>,
}

#[derive(Debug, Deserialize)]
struct OllamaStreamChunk {
    message: OllamaResponseMessage,
    model: String,
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
}

// ---------------------------------------------------------------------------
// Tool conversion helpers
// ---------------------------------------------------------------------------

/// Convert ClawDesk `ToolDefinition`s to Ollama's tool format.
fn convert_tools(tools: &[ToolDefinition]) -> Vec<OllamaToolDefinition> {
    tools
        .iter()
        .map(|t| OllamaToolDefinition {
            tool_type: "function".to_string(),
            function: OllamaToolFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        })
        .collect()
}

/// Fallback extraction for models that emit tool calls as JSON in content text.
///
/// Matches patterns like:
/// ```json
/// {"name": "tool_name", "arguments": {...}}
/// ```
/// or:
/// ```json
/// [{"name": "tool_name", "arguments": {...}}]
/// ```
fn try_extract_tool_calls_from_content(content: &str) -> Vec<ToolCall> {
    let trimmed = content.trim();

    // Try array of tool calls first.
    if trimmed.starts_with('[') {
        if let Ok(calls) = serde_json::from_str::<Vec<OllamaToolCall>>(trimmed) {
            return calls
                .into_iter()
                .enumerate()
                .map(|(i, tc)| ToolCall {
                    id: format!("fallback_tc_{}", i),
                    name: tc.function.name,
                    arguments: tc.function.arguments,
                })
                .collect();
        }
    }

    // Try single tool call object.
    if trimmed.starts_with('{') {
        if let Ok(tc) = serde_json::from_str::<OllamaToolCall>(trimmed) {
            return vec![ToolCall {
                id: "fallback_tc_0".to_string(),
                name: tc.function.name,
                arguments: tc.function.arguments,
            }];
        }
    }

    // Try to find JSON embedded in text (common with smaller models).
    // Look for the first { ... } block that parses as a tool call.
    if let Some(start) = trimmed.find('{') {
        let rest = &trimmed[start..];
        // Find matching closing brace by counting braces.
        let mut depth = 0;
        let mut end = 0;
        for (i, ch) in rest.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if end > 0 {
            let json_block = &rest[..end];
            if let Ok(tc) = serde_json::from_str::<OllamaToolCall>(json_block) {
                return vec![ToolCall {
                    id: "fallback_tc_0".to_string(),
                    name: tc.function.name,
                    arguments: tc.function.arguments,
                }];
            }
        }
    }

    vec![]
}

/// Convert ClawDesk messages to Ollama wire format.
///
/// Handles two critical conversions that small models depend on:
///
/// 1. **Tool result messages** (`MessageRole::Tool`): The agent runner wraps tool
///    results in a JSON envelope `{"tool_call_id":"...","name":"...","content":"...","is_error":...}`.
///    Small models (e.g. GLM-4.7-Flash) can't parse through this wrapper to find the
///    actual content. This function extracts the inner `content` field and sends it as
///    plain text in the `tool` role message.
///
/// 2. **Assistant messages preceding tool results**: Ollama's API (like OpenAI's)
///    expects the assistant message to include a `tool_calls` array so the model can
///    associate subsequent tool results with the calls it made. We reconstruct this
///    from the tool result messages that follow.
fn convert_messages(
    request: &crate::ProviderRequest,
) -> Vec<OllamaChatMessage> {
    let mut msgs = Vec::new();

    if let Some(ref system) = request.system_prompt {
        msgs.push(OllamaChatMessage {
            role: "system".into(),
            content: system.clone(),
            tool_calls: None,
        });
    }

    let messages = &request.messages;
    let len = messages.len();

    for (i, m) in messages.iter().enumerate() {
        match m.role {
            MessageRole::Tool => {
                // Unwrap the JSON envelope to extract actual tool output.
                // The runner stores: {"tool_call_id":"...", "name":"...", "content":"...", "is_error":...}
                // Ollama expects: {"role": "tool", "content": "actual output text"}
                let content = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&*m.content) {
                    parsed.get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&m.content)
                        .to_string()
                } else {
                    m.content.to_string()
                };
                msgs.push(OllamaChatMessage {
                    role: "tool".into(),
                    content,
                    tool_calls: None,
                });
            }
            MessageRole::Assistant => {
                // Check if this assistant message is followed by tool results.
                // If so, reconstruct the tool_calls array from subsequent tool messages
                // so Ollama can associate results with calls.
                let mut tool_calls_for_msg: Option<Vec<OllamaToolCall>> = None;

                // Look ahead: count consecutive tool messages following this assistant message
                let mut j = i + 1;
                let mut pending_tool_calls = Vec::new();
                while j < len && messages[j].role == MessageRole::Tool {
                    // Extract tool name from the JSON envelope
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&*messages[j].content) {
                        let name = parsed.get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        pending_tool_calls.push(OllamaToolCall {
                            function: OllamaToolCallFunction {
                                name,
                                arguments: serde_json::json!({}),
                            },
                        });
                    }
                    j += 1;
                }

                if !pending_tool_calls.is_empty() {
                    tool_calls_for_msg = Some(pending_tool_calls);
                }

                msgs.push(OllamaChatMessage {
                    role: "assistant".into(),
                    content: m.content.to_string(),
                    tool_calls: tool_calls_for_msg,
                });
            }
            _ => {
                msgs.push(OllamaChatMessage {
                    role: m.role.as_str().to_string(),
                    content: m.content.to_string(),
                    tool_calls: None,
                });
            }
        }
    }

    msgs
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    fn models(&self) -> Vec<String> {
        self.cached_models
            .read()
            .map(|m| {
                if m.is_empty() {
                    // Return defaults if no models discovered yet.
                    vec![
                        "llama3.2".to_string(),
                        "llama3.1".to_string(),
                        "mistral".to_string(),
                        "gemma2".to_string(),
                        "phi3".to_string(),
                        "qwen2.5".to_string(),
                    ]
                } else {
                    m.clone()
                }
            })
            .unwrap_or_default()
    }

    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let start = Instant::now();
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        let has_tools = !request.tools.is_empty();
        debug!(%model, messages = request.messages.len(), tools = request.tools.len(), "calling Ollama API");

        let messages = convert_messages(request);

        let api_request = OllamaChatRequest {
            model: model.clone(),
            messages,
            stream: false,
            options: Some(OllamaOptions {
                temperature: request.temperature,
                num_predict: request.max_tokens,
            }),
            tools: convert_tools(&request.tools),
        };

        let response = self
            .client
            .post(self.api_url("/api/chat"))
            .json(&api_request)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout {
                        provider: "ollama".into(),
                        model: model.clone(),
                        after: start.elapsed(),
                    }
                } else if e.is_connect() {
                    ProviderError::NetworkError {
                        provider: "ollama".into(),
                        detail: format!(
                            "cannot connect to Ollama at {} — is it running? ({})",
                            self.base_url, e
                        ),
                    }
                } else {
                    ProviderError::NetworkError {
                        provider: "ollama".into(),
                        detail: e.to_string(),
                    }
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            let status_code = status.as_u16();
            let body = response.text().await.unwrap_or_default();

            if body.contains("not found") || body.contains("model") {
                return Err(ProviderError::FormatError {
                    provider: "ollama".into(),
                    detail: format!("model '{}' not found — try `ollama pull {}`", model, model),
                });
            }

            return Err(ProviderError::ServerError {
                provider: "ollama".into(),
                status: status_code,
            });
        }

        let api_response: OllamaChatResponse =
            response.json().await.map_err(|e| ProviderError::FormatError {
                provider: "ollama".into(),
                detail: e.to_string(),
            })?;

        // Extract tool calls: first from structured response, fallback to content regex.
        let mut tool_calls: Vec<ToolCall> = api_response
            .message
            .tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| ToolCall {
                id: format!("ollama_tc_{}", i),
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            })
            .collect();

        // Fallback: some smaller models emit tool calls as JSON in content.
        if tool_calls.is_empty() && has_tools {
            tool_calls = try_extract_tool_calls_from_content(&api_response.message.content);
            if !tool_calls.is_empty() {
                debug!(count = tool_calls.len(), "extracted tool calls from content via fallback regex");
            }
        }

        let finish_reason = if !tool_calls.is_empty() {
            FinishReason::ToolUse
        } else {
            FinishReason::Stop
        };

        Ok(ProviderResponse {
            content: api_response.message.content,
            model: api_response.model,
            provider: "ollama".to_string(),
            usage: TokenUsage {
                input_tokens: api_response.prompt_eval_count.unwrap_or(0),
                output_tokens: api_response.eval_count.unwrap_or(0),
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            tool_calls,
            finish_reason,
            latency,
        })
    }

    /// Native streaming for Ollama via NDJSON.
    async fn stream(
        &self,
        request: &ProviderRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), ProviderError> {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        debug!(%model, "streaming Ollama API");

        let messages = convert_messages(request);

        let api_request = OllamaChatRequest {
            model: model.clone(),
            messages,
            stream: true,
            options: Some(OllamaOptions {
                temperature: request.temperature,
                num_predict: request.max_tokens,
            }),
            tools: convert_tools(&request.tools),
        };

        let response = self
            .client
            .post(self.api_url("/api/chat"))
            .json(&api_request)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError {
                provider: "ollama".into(),
                detail: e.to_string(),
            })?;

        if !response.status().is_success() {
            return Err(ProviderError::ServerError {
                provider: "ollama".into(),
                status: response.status().as_u16(),
            });
        }

        let mut buffer = String::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            ProviderError::NetworkError {
                provider: "ollama".into(),
                detail: e.to_string(),
            }
        })? {
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Ollama streams as NDJSON (newline-delimited JSON).
            while let Some(newline) = buffer.find('\n') {
                let line = buffer[..newline].trim().to_string();
                buffer = buffer[newline + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                let Ok(chunk_data) = serde_json::from_str::<OllamaStreamChunk>(&line) else {
                    warn!("ollama: failed to parse stream chunk");
                    continue;
                };

                let is_done = chunk_data.done;
                let usage = if is_done {
                    Some(TokenUsage {
                        input_tokens: chunk_data.prompt_eval_count.unwrap_or(0),
                        output_tokens: chunk_data.eval_count.unwrap_or(0),
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                    })
                } else {
                    None
                };

                // Extract tool calls from the chunk (Ollama sends them in the final done=true chunk)
                let mut tool_calls: Vec<ToolCall> = chunk_data.message.tool_calls
                    .iter()
                    .enumerate()
                    .map(|(i, tc)| ToolCall {
                        id: format!("ollama_tc_{}", i),
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    })
                    .collect();

                // Fallback: some smaller models embed tool calls as JSON in content
                if tool_calls.is_empty() && is_done && !request.tools.is_empty() {
                    tool_calls = try_extract_tool_calls_from_content(&chunk_data.message.content);
                }

                let finish = if is_done {
                    if !tool_calls.is_empty() {
                        Some(FinishReason::ToolUse)
                    } else {
                        Some(FinishReason::Stop)
                    }
                } else {
                    None
                };

                let _ = chunk_tx
                    .send(StreamChunk {
                        delta: chunk_data.message.content,
                        done: is_done,
                        finish_reason: finish,
                        usage,
                        tool_calls,
                    })
                    .await;

                if is_done {
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        // Use model discovery as health check — tests connectivity.
        self.discover_models().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_default_construction() {
        let p = OllamaProvider::new(None, None);
        assert_eq!(p.name(), "ollama");
        assert!(!p.models().is_empty());
    }

    #[test]
    fn ollama_custom_url() {
        let p = OllamaProvider::new(Some("http://gpu-server:11434".into()), Some("codellama".into()));
        assert_eq!(p.base_url, "http://gpu-server:11434");
        assert_eq!(p.default_model, "codellama");
    }

    #[test]
    fn convert_tools_empty() {
        assert!(convert_tools(&[]).is_empty());
    }

    #[test]
    fn convert_tools_round_trip() {
        let tools = vec![ToolDefinition {
            name: "weather".to_string(),
            description: "Get weather".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        }];
        let ollama_tools = convert_tools(&tools);
        assert_eq!(ollama_tools.len(), 1);
        assert_eq!(ollama_tools[0].tool_type, "function");
        assert_eq!(ollama_tools[0].function.name, "weather");
    }

    #[test]
    fn fallback_extract_single_tool_call() {
        let content = r#"{"function": {"name": "get_weather", "arguments": {"city": "London"}}}"#;
        let calls = try_extract_tool_calls_from_content(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
    }

    #[test]
    fn fallback_extract_array_tool_calls() {
        let content = r#"[{"function": {"name": "a", "arguments": {}}}, {"function": {"name": "b", "arguments": {}}}]"#;
        let calls = try_extract_tool_calls_from_content(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn fallback_extract_embedded_json() {
        let content = "I'll call the tool now: {\"function\": {\"name\": \"search\", \"arguments\": {\"q\": \"rust\"}}} and wait for the result.";
        let calls = try_extract_tool_calls_from_content(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
    }

    #[test]
    fn fallback_returns_empty_for_plain_text() {
        let calls = try_extract_tool_calls_from_content("Hello! How can I help?");
        assert!(calls.is_empty());
    }
}
