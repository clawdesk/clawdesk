//! Anthropic Claude provider adapter.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

use crate::{
    ChatMessage, FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    StreamChunk, TokenUsage, ToolCall,
};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic Claude provider.
pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    default_model: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, default_model: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
            api_key,
            default_model: default_model
                .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string()),
        }
    }
}

/// Anthropic API request body.
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

/// Anthropic API response body.
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
    model: String,
    usage: AnthropicUsage,
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
}

/// Anthropic API error response.
#[derive(Debug, Deserialize)]
struct AnthropicError {
    #[serde(rename = "type")]
    error_type: String,
    error: AnthropicErrorDetail,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorDetail {
    #[serde(rename = "type")]
    detail_type: String,
    message: String,
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn models(&self) -> Vec<String> {
        vec![
            "claude-sonnet-4-20250514".to_string(),
            "claude-opus-4-20250514".to_string(),
            "claude-3-5-haiku-20241022".to_string(),
        ]
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

        debug!(%model, messages = request.messages.len(), "calling Anthropic API");

        let api_request = AnthropicRequest {
            model: model.clone(),
            max_tokens: request.max_tokens.unwrap_or(8192),
            messages: request
                .messages
                .iter()
                .map(|m| AnthropicMessage {
                    role: m.role.as_str().to_string(),
                    content: m.content.to_string(),
                })
                .collect(),
            system: request.system_prompt.clone(),
            temperature: request.temperature,
        };

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&api_request)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout {
                        provider: "anthropic".into(),
                        model: model.clone(),
                        after: start.elapsed(),
                    }
                } else {
                    ProviderError::NetworkError {
                        provider: "anthropic".into(),
                        detail: e.to_string(),
                    }
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            let status_code = status.as_u16();
            let body = response.text().await.unwrap_or_default();

            return match status_code {
                429 => Err(ProviderError::RateLimit {
                    provider: "anthropic".into(),
                    retry_after: None,
                }),
                401 | 403 => Err(ProviderError::AuthFailure {
                    provider: "anthropic".into(),
                    profile_id: "default".into(),
                }),
                400 => {
                    if body.contains("billing") || body.contains("credit") {
                        Err(ProviderError::Billing {
                            provider: "anthropic".into(),
                        })
                    } else {
                        Err(ProviderError::FormatError {
                            provider: "anthropic".into(),
                            detail: body,
                        })
                    }
                }
                _ => Err(ProviderError::ServerError {
                    provider: "anthropic".into(),
                    status: status_code,
                }),
            };
        }

        let api_response: AnthropicResponse =
            response.json().await.map_err(|e| ProviderError::FormatError {
                provider: "anthropic".into(),
                detail: e.to_string(),
            })?;

        // Extract text content and tool calls
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in &api_response.content {
            match block.block_type.as_str() {
                "text" => {
                    if let Some(text) = &block.text {
                        text_parts.push(text.clone());
                    }
                }
                "tool_use" => {
                    if let (Some(id), Some(name), Some(input)) =
                        (&block.id, &block.name, &block.input)
                    {
                        tool_calls.push(ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: input.clone(),
                        });
                    }
                }
                _ => {
                    warn!(block_type = %block.block_type, "unknown content block type");
                }
            }
        }

        let finish_reason = match api_response.stop_reason.as_deref() {
            Some("tool_use") => FinishReason::ToolUse,
            Some("max_tokens") => FinishReason::MaxTokens,
            _ => FinishReason::Stop,
        };

        Ok(ProviderResponse {
            content: text_parts.join(""),
            model: api_response.model,
            provider: "anthropic".to_string(),
            usage: TokenUsage {
                input_tokens: api_response.usage.input_tokens,
                output_tokens: api_response.usage.output_tokens,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            tool_calls,
            finish_reason,
            latency,
        })
    }

    /// Native SSE streaming for Anthropic.
    ///
    /// Sends `POST /v1/messages` with `"stream": true`.
    /// Parses the SSE event stream:
    ///   content_block_delta → StreamChunk { delta, done: false }
    ///   message_delta       → StreamChunk { done: true, usage, finish_reason }
    ///
    /// Complexity: O(n) over total token count, with O(1) per-chunk processing.
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

        debug!(%model, "streaming Anthropic API");

        // Build request JSON with stream: true
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role.as_str(),
                    "content": m.content.to_string(),
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": model,
            "max_tokens": request.max_tokens.unwrap_or(8192),
            "messages": messages,
            "stream": true,
        });

        if let Some(ref system) = request.system_prompt {
            body["system"] = serde_json::json!(system);
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError {
                provider: "anthropic".into(),
                detail: e.to_string(),
            })?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            return match status {
                429 => Err(ProviderError::RateLimit {
                    provider: "anthropic".into(),
                    retry_after: None,
                }),
                _ => Err(ProviderError::ServerError {
                    provider: "anthropic".into(),
                    status,
                }),
            };
        }

        // Parse SSE event stream
        let mut buffer = String::new();
        let mut total_input_tokens: u64 = 0;
        let mut total_output_tokens: u64 = 0;

        // reqwest chunk-based streaming — no extra feature flags needed
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            ProviderError::NetworkError {
                provider: "anthropic".into(),
                detail: e.to_string(),
            }
        })? {
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete SSE events (delimited by \n\n)
            while let Some(boundary) = buffer.find("\n\n") {
                let event_block = buffer[..boundary].to_string();
                buffer = buffer[boundary + 2..].to_string();

                // Extract `data:` line from SSE event
                let data_line = event_block
                    .lines()
                    .find(|l| l.starts_with("data: "))
                    .map(|l| &l[6..]);

                let Some(data) = data_line else { continue };

                // Parse JSON event
                let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };

                let event_type = event
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");

                match event_type {
                    "content_block_delta" => {
                        if let Some(delta_text) = event
                            .get("delta")
                            .and_then(|d| d.get("text"))
                            .and_then(|t| t.as_str())
                        {
                            let _ = chunk_tx
                                .send(StreamChunk {
                                    delta: delta_text.to_string(),
                                    done: false,
                                    finish_reason: None,
                                    usage: None,
                                })
                                .await;
                        }
                    }
                    "message_start" => {
                        // Extract input token count from message start
                        if let Some(usage) = event
                            .get("message")
                            .and_then(|m| m.get("usage"))
                        {
                            total_input_tokens = usage
                                .get("input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                        }
                    }
                    "message_delta" => {
                        // Final delta with stop_reason and output usage
                        let finish = event
                            .get("delta")
                            .and_then(|d| d.get("stop_reason"))
                            .and_then(|s| s.as_str())
                            .map(|s| match s {
                                "tool_use" => FinishReason::ToolUse,
                                "max_tokens" => FinishReason::MaxTokens,
                                _ => FinishReason::Stop,
                            })
                            .unwrap_or(FinishReason::Stop);

                        total_output_tokens = event
                            .get("usage")
                            .and_then(|u| u.get("output_tokens"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);

                        let _ = chunk_tx
                            .send(StreamChunk {
                                delta: String::new(),
                                done: true,
                                finish_reason: Some(finish),
                                usage: Some(TokenUsage {
                                    input_tokens: total_input_tokens,
                                    output_tokens: total_output_tokens,
                                    cache_read_tokens: None,
                                    cache_write_tokens: None,
                                }),
                            })
                            .await;
                    }
                    _ => {} // message_start, content_block_start/stop, message_stop — skip
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        // Simple check: verify the API key works with a minimal request
        let request = ProviderRequest {
            model: self.default_model.clone(),
            messages: vec![ChatMessage {
                role: MessageRole::User,
                content: "ping".into(),
                cached_tokens: None,
            }],
            system_prompt: None,
            max_tokens: Some(1),
            temperature: None,
            tools: vec![],
            stream: false,
        };

        self.complete(&request).await?;
        Ok(())
    }
}
