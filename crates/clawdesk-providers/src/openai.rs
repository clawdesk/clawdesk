//! OpenAI provider adapter.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::Deserialize;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

use crate::{
    ChatMessage, FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    StreamChunk, TokenUsage, ToolCall,
};

const DEFAULT_OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";

/// Standard OpenAI provider.
///
/// For Azure OpenAI, use `AzureOpenAiProvider` from the `azure` module instead.
pub struct OpenAiProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_model: String,
}

impl OpenAiProvider {
    pub fn new(api_key: String, base_url: Option<String>, default_model: Option<String>) -> Self {
        let url = base_url.unwrap_or_else(|| DEFAULT_OPENAI_URL.to_string());
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
            api_key,
            base_url: url,
            default_model: default_model.unwrap_or_else(|| "gpt-4o".to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    model: String,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCall {
    id: String,
    function: OpenAiFunction,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn models(&self) -> Vec<String> {
        vec![
            "gpt-4o".to_string(),
            "gpt-4o-mini".to_string(),
            "o1".to_string(),
            "o1-mini".to_string(),
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

        debug!(%model, messages = request.messages.len(), "calling OpenAI API");

        // Use openai_compat to properly format tool call exchanges + images
        let messages = crate::openai_compat::build_openai_api_messages_with_images(
            request.system_prompt.as_deref(),
            &request.messages,
            &request.images,
        );

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
        });
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request.tools.iter().map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            }).collect();
            body["tools"] = serde_json::json!(tools);
        }

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::timeout("openai", model.clone(), start.elapsed())
                } else {
                    ProviderError::network_error("openai", e.to_string())
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            let status_code = status.as_u16();
            // Read error body for better diagnostics
            let err_body = response.text().await.unwrap_or_default();
            if !err_body.is_empty() {
                warn!(status = status_code, body = %err_body.chars().take(500).collect::<String>(), "OpenAI API error response");
            }
            return match status_code {
                429 => Err(ProviderError::rate_limit("openai", None)),
                401 | 403 => Err(ProviderError::auth_failure("openai", "default")),
                _ => Err(ProviderError::format_error("openai", format!("HTTP {} — {}", status_code, err_body.chars().take(300).collect::<String>()))),
            };
        }

        let api_response: OpenAiResponse =
            response.json().await.map_err(|e| ProviderError::format_error("openai", e.to_string()))?;

        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::format_error("openai", "no choices in response"))?;

        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::Null),
            })
            .collect();

        let usage = api_response.usage.unwrap_or(OpenAiUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
        });

        let finish_reason = match choice.finish_reason.as_deref() {
            Some("tool_calls") => FinishReason::ToolUse,
            Some("length") => FinishReason::MaxTokens,
            Some("content_filter") => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        };

        Ok(ProviderResponse {
            content: choice.message.content.unwrap_or_default(),
            model: api_response.model,
            provider: "openai".to_string(),
            usage: TokenUsage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            tool_calls,
            finish_reason,
            latency,
        })
    }

    /// Native SSE streaming for OpenAI-compatible APIs.
    ///
    /// Sends `POST /v1/chat/completions` with `"stream": true`.
    /// Parses the SSE event stream:
    ///   `data: {"choices":[{"delta":{"content":"..."}}]}` → StreamChunk
    ///   `data: [DONE]` → final signal
    ///
    /// Works with OpenAI, Azure, Ollama, and any API conforming to the
    /// OpenAI streaming protocol. O(n) total, O(1) per chunk.
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

        debug!(%model, "streaming OpenAI API");

        // Use openai_compat to properly format tool call exchanges + images
        let messages = crate::openai_compat::build_openai_api_messages_with_images(
            request.system_prompt.as_deref(),
            &request.messages,
            &request.images,
        );

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
        });

        body["stream_options"] = serde_json::json!({ "include_usage": true });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        // Include tool definitions for function calling
        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request.tools.iter().map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            }).collect();
            body["tools"] = serde_json::json!(tools);
        }

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::network_error("openai", e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            // Read the error body for diagnostics
            let err_body = response.text().await.unwrap_or_default();
            if !err_body.is_empty() {
                warn!(status, body = %err_body.chars().take(500).collect::<String>(), "OpenAI stream: HTTP error response");
            }
            return match status {
                429 => Err(ProviderError::rate_limit("openai", None)),
                401 | 403 => Err(ProviderError::auth_failure("openai", "default")),
                _ => Err(ProviderError::format_error("openai", format!("HTTP {} — {}", status, err_body.chars().take(300).collect::<String>()))),
            };
        }

        // Parse SSE event stream
        let mut buffer = String::new();
        let mut response = response;
        let mut received_any_content = false;

        // Accumulate tool call deltas during streaming.
        // OpenAI sends tool calls as incremental fragments:
        //   delta.tool_calls[i].id (first chunk for call i)
        //   delta.tool_calls[i].function.name (first chunk for call i)
        //   delta.tool_calls[i].function.arguments (subsequent chunks, partial JSON)
        let mut tool_call_accum: Vec<(String, String, String)> = Vec::new(); // (id, name, args_json)

        while let Some(chunk) = response.chunk().await.map_err(|e| {
            ProviderError::network_error("openai", e.to_string())
        })? {
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete SSE lines (each prefixed with `data: ` and ending with `\n`)
            while let Some(boundary) = buffer.find("\n\n") {
                let event_block = buffer[..boundary].to_string();
                buffer = buffer[boundary + 2..].to_string();

                for line in event_block.lines() {
                    let data = if let Some(d) = line.strip_prefix("data: ") {
                        d.trim()
                    } else {
                        continue;
                    };

                    // Terminal signal
                    if data == "[DONE]" {
                        if !received_any_content {
                            warn!("OpenAI stream completed with [DONE] but produced no content or tool calls");
                        }
                        return Ok(());
                    }

                    // Parse chunk JSON
                    let Ok(chunk_json) = serde_json::from_str::<serde_json::Value>(data) else {
                        warn!(raw_data = %data.chars().take(200).collect::<String>(), "OpenAI stream: unparseable SSE chunk");
                        continue;
                    };

                    // ── Check for error responses embedded in the stream ──
                    // Some proxies can return error JSON mid-stream
                    // (e.g., content filter violations, quota exceeded, etc.).
                    if let Some(error_obj) = chunk_json.get("error") {
                        let err_msg = error_obj.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error");
                        let err_code = error_obj.get("code").and_then(|c| c.as_str()).unwrap_or("unknown");
                        warn!(
                            error_code = %err_code,
                            error_message = %err_msg,
                            "OpenAI stream: error response received in SSE stream"
                        );
                        return Err(ProviderError::format_error(
                            "openai",
                            format!("Stream error [{}]: {}", err_code, err_msg),
                        ));
                    }

                    // Extract usage (may appear in final chunk or separate)
                    let usage = chunk_json.get("usage").and_then(|u| {
                        Some(TokenUsage {
                            input_tokens: u.get("prompt_tokens")?.as_u64()?,
                            output_tokens: u.get("completion_tokens")?.as_u64()?,
                            cache_read_tokens: None,
                            cache_write_tokens: None,
                        })
                    });

                    let choices = chunk_json
                        .get("choices")
                        .and_then(|c| c.as_array());

                    if let Some(choices) = choices {
                        if choices.is_empty() {
                            // Azure content filter can return empty choices array
                            warn!("OpenAI stream: empty choices array (possible content filter)");
                        }
                        for choice in choices {
                            let delta = choice.get("delta");
                            let content = delta
                                .and_then(|d| d.get("content"))
                                .and_then(|c| c.as_str())
                                .unwrap_or("");

                            if !content.is_empty() {
                                received_any_content = true;
                            }

                            // ── Accumulate tool call deltas ──
                            // OpenAI streams tool calls as incremental fragments
                            // across multiple chunks.
                            if let Some(tc_arr) = delta.and_then(|d| d.get("tool_calls")).and_then(|v| v.as_array()) {
                                for tc in tc_arr {
                                    let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                    // Ensure accumulator has enough slots
                                    while tool_call_accum.len() <= idx {
                                        tool_call_accum.push((String::new(), String::new(), String::new()));
                                    }
                                    // First chunk for this call carries id + function.name
                                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                        tool_call_accum[idx].0 = id.to_string();
                                    }
                                    if let Some(func) = tc.get("function") {
                                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                            tool_call_accum[idx].1 = name.to_string();
                                        }
                                        // Arguments come as incremental string fragments
                                        if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                            tool_call_accum[idx].2.push_str(args);
                                        }
                                    }
                                }
                            }

                            let finish = choice
                                .get("finish_reason")
                                .and_then(|f| f.as_str())
                                .map(|s| match s {
                                    "tool_calls" => FinishReason::ToolUse,
                                    "length" => FinishReason::MaxTokens,
                                    "content_filter" => FinishReason::ContentFilter,
                                    _ => FinishReason::Stop,
                                });

                            let done = finish.is_some();

                            // On final chunk, assemble accumulated tool calls
                            let final_tool_calls = if done && !tool_call_accum.is_empty() {
                                tool_call_accum.drain(..).filter_map(|(id, name, args)| {
                                    if name.is_empty() { return None; }
                                    let arguments = if args.is_empty() {
                                        serde_json::json!({})
                                    } else {
                                        serde_json::from_str(&args).unwrap_or(serde_json::json!({}))
                                    };
                                    Some(ToolCall { id, name, arguments })
                                }).collect()
                            } else {
                                Vec::new()
                            };

                            let _ = chunk_tx
                                .send(StreamChunk {
                                    delta: content.to_string(),
                                    reasoning_delta: String::new(),
                                    done,
                                    finish_reason: finish,
                                    usage: if done { usage.clone() } else { None },
                                    tool_calls: final_tool_calls,
                                })
                                .await;
                        }
                    } else if usage.is_some() {
                        // Usage-only chunk (stream_options.include_usage)
                        let _ = chunk_tx
                            .send(StreamChunk {
                                delta: String::new(),
                                reasoning_delta: String::new(),
                                done: true,
                                finish_reason: Some(FinishReason::Stop),
                                usage,
                                tool_calls: Vec::new(),
                            })
                            .await;
                    } else {
                        // Chunk has neither choices nor usage — log for debugging
                        debug!(
                            chunk_keys = %chunk_json.as_object()
                                .map(|o| o.keys().map(|k| k.as_str()).collect::<Vec<_>>().join(","))
                                .unwrap_or_default(),
                            "OpenAI stream: chunk without choices or usage"
                        );
                    }
                }
            }
        }

        // Stream ended without [DONE] signal
        if !received_any_content {
            warn!("OpenAI stream: connection closed without [DONE] and no content was received");
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
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
