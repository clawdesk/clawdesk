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

    /// Convert internal `ChatMessage` list + attached images to Anthropic API format.
    ///
    /// Images are injected as native `image` content blocks on the **last user
    /// message** (matching openclaw's pattern). All other messages pass through
    /// as plain text. System messages are skipped (sent via the top-level
    /// `system` field instead).
    fn build_messages(
        messages: &[ChatMessage],
        images: &[crate::ImageContent],
    ) -> Vec<AnthropicMessage> {
        // Find the index of the last user message — images attach there.
        let last_user_idx = messages
            .iter()
            .rposition(|m| m.role == MessageRole::User);

        messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role != MessageRole::System)
            .map(|(i, m)| {
                let is_image_target = Some(i) == last_user_idx && !images.is_empty();
                if is_image_target {
                    // Build multimodal content array: text + image blocks.
                    let mut blocks: Vec<serde_json::Value> = Vec::new();
                    let text = m.content.trim();
                    if !text.is_empty() {
                        blocks.push(serde_json::json!({"type": "text", "text": text}));
                    } else {
                        blocks.push(serde_json::json!({"type": "text", "text": "User sent image(s) with no text."}));
                    }
                    for img in images {
                        blocks.push(serde_json::json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": img.mime_type,
                                "data": img.data,
                            }
                        }));
                    }
                    AnthropicMessage {
                        role: m.role.as_str().to_string(),
                        content: serde_json::Value::Array(blocks),
                    }
                } else {
                    AnthropicMessage {
                        role: m.role.as_str().to_string(),
                        content: serde_json::Value::String(m.content.to_string()),
                    }
                }
            })
            .collect()
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicToolDef>,
}

/// Anthropic tool definition for function calling.
#[derive(Debug, Serialize)]
struct AnthropicToolDef {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: serde_json::Value,
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

        let tools: Vec<AnthropicToolDef> = request.tools.iter().map(|t| AnthropicToolDef {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.parameters.clone(),
        }).collect();

        let api_request = AnthropicRequest {
            model: model.clone(),
            max_tokens: request.max_tokens.unwrap_or(8192),
            messages: Self::build_messages(&request.messages, &request.images),
            system: request.system_prompt.clone(),
            temperature: request.temperature,
            tools,
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
                    ProviderError::timeout("anthropic", model.clone(), start.elapsed())
                } else {
                    ProviderError::network_error("anthropic", e.to_string())
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            let status_code = status.as_u16();
            let body = response.text().await.unwrap_or_default();

            return match status_code {
                429 => Err(ProviderError::rate_limit("anthropic", None)),
                401 | 403 => Err(ProviderError::auth_failure("anthropic", "default")),
                400 => {
                    if body.contains("billing") || body.contains("credit") {
                        Err(ProviderError::billing("anthropic"))
                    } else {
                        Err(ProviderError::format_error("anthropic", body))
                    }
                }
                _ => Err(ProviderError::server_error("anthropic", status_code)),
            };
        }

        let api_response: AnthropicResponse =
            response.json().await.map_err(|e| ProviderError::format_error("anthropic", e.to_string()))?;

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

        // Build messages with native multimodal image support
        let messages: Vec<serde_json::Value> = Self::build_messages(&request.messages, &request.images)
            .into_iter()
            .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
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

        // Include tool definitions so Claude can make structured tool calls
        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request.tools.iter().map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            }).collect();
            body["tools"] = serde_json::json!(tools);
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
            .map_err(|e| ProviderError::network_error("anthropic", e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            return match status {
                429 => Err(ProviderError::rate_limit("anthropic", None)),
                _ => Err(ProviderError::server_error("anthropic", status)),
            };
        }

        // Parse SSE event stream
        let mut buffer = String::new();
        let mut total_input_tokens: u64 = 0;
        let mut total_output_tokens: u64 = 0;

        // ── Accumulate tool calls during streaming ──
        // Track content blocks of type "tool_use" so we can return them
        // on the final chunk, eliminating the redundant complete() call.
        let mut current_tool_blocks: Vec<(String, String, String)> = Vec::new(); // (id, name, json_accum)
        let mut current_block_type: Option<String> = None;
        let mut current_block_index: Option<usize> = None;

        // reqwest chunk-based streaming — no extra feature flags needed
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            ProviderError::network_error("anthropic", e.to_string())
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
                    "content_block_start" => {
                        // Track the type of the new content block.
                        // For tool_use blocks, capture id + name for later assembly.
                        if let Some(cb) = event.get("content_block") {
                            let block_type = cb.get("type").and_then(|t| t.as_str()).unwrap_or("text");
                            current_block_type = Some(block_type.to_string());
                            if block_type == "tool_use" {
                                let id = cb.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let name = cb.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                current_tool_blocks.push((id, name, String::new()));
                                current_block_index = Some(current_tool_blocks.len() - 1);
                            } else {
                                current_block_index = None;
                            }
                        }
                    }
                    "content_block_delta" => {
                        if let Some(delta) = event.get("delta") {
                            let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            match delta_type {
                                "text_delta" => {
                                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                        let _ = chunk_tx
                                            .send(StreamChunk {
                                                delta: text.to_string(),
                                                reasoning_delta: String::new(),
                                                done: false,
                                                finish_reason: None,
                                                usage: None,
                                                tool_calls: Vec::new(),
                                            })
                                            .await;
                                    }
                                }
                                "input_json_delta" => {
                                    // Accumulate JSON fragments for the current tool_use block
                                    if let (Some(idx), Some(json_frag)) = (
                                        current_block_index,
                                        delta.get("partial_json").and_then(|j| j.as_str()),
                                    ) {
                                        if let Some(block) = current_tool_blocks.get_mut(idx) {
                                            block.2.push_str(json_frag);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "content_block_stop" => {
                        current_block_type = None;
                        current_block_index = None;
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

                        // Build tool calls from accumulated blocks
                        let tool_calls: Vec<ToolCall> = current_tool_blocks
                            .drain(..)
                            .filter_map(|(id, name, json_str)| {
                                let arguments = if json_str.is_empty() {
                                    serde_json::json!({})
                                } else {
                                    serde_json::from_str(&json_str).unwrap_or(serde_json::json!({}))
                                };
                                if name.is_empty() { None }
                                else { Some(ToolCall { id, name, arguments }) }
                            })
                            .collect();

                        let _ = chunk_tx
                            .send(StreamChunk {
                                delta: String::new(),
                                reasoning_delta: String::new(),
                                done: true,
                                finish_reason: Some(finish),
                                usage: Some(TokenUsage {
                                    input_tokens: total_input_tokens,
                                    output_tokens: total_output_tokens,
                                    cache_read_tokens: None,
                                    cache_write_tokens: None,
                                }),
                                tool_calls,
                            })
                            .await;
                    }
                    _ => {} // message_stop — skip
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
            images: vec![],
        };

        self.complete(&request).await?;
        Ok(())
    }
}
