//! Azure OpenAI provider adapter.

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

/// Azure OpenAI provider.
pub struct AzureOpenAiProvider {
    client: Client,
    api_key: String,
    api_base: String,
    api_version: String,
    default_model: String,
}

impl AzureOpenAiProvider {
    pub fn new(
        api_key: String,
        api_base: String,
        api_version: Option<String>,
        default_model: Option<String>,
    ) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
            api_key,
            api_base: api_base.trim_end_matches('/').to_string(),
            api_version: api_version.unwrap_or_else(|| "2024-12-01-preview".to_string()),
            default_model: default_model.unwrap_or_else(|| "gpt-4o".to_string()),
        }
    }

    fn build_url(&self, model: &str) -> String {
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            self.api_base, model, self.api_version
        )
    }
}

#[derive(Debug, Deserialize)]
struct AzureResponse {
    choices: Vec<AzureChoice>,
    model: String,
    usage: Option<AzureUsage>,
}

#[derive(Debug, Deserialize)]
struct AzureChoice {
    message: AzureResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AzureResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<AzureToolCall>>,
}

#[derive(Debug, Deserialize)]
struct AzureToolCall {
    id: String,
    function: AzureFunction,
}

#[derive(Debug, Deserialize)]
struct AzureFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct AzureUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[async_trait]
impl Provider for AzureOpenAiProvider {
    fn name(&self) -> &str {
        "azure_openai"
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

        debug!(%model, messages = request.messages.len(), "calling Azure OpenAI API");

        // Use openai_compat to properly format tool call exchanges + images
        let messages = crate::openai_compat::build_openai_api_messages_with_images(
            request.system_prompt.as_deref(),
            &request.messages,
            &request.images,
        );

        let url = self.build_url(&model);

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
            .post(&url)
            .header("api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::timeout("azure_openai", model.clone(), start.elapsed())
                } else {
                    ProviderError::network_error("azure_openai", e.to_string())
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            let status_code = status.as_u16();
            let body_bytes = response.bytes().await.unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes);
            
            if status_code == 429 {
                return Err(ProviderError::rate_limit("azure_openai", None));
            }

            // For Azure, 400, 401, 403, 404 usually contain a precise JSON error explaining what is wrong 
            // (e.g. invalid key, invalid deployment name, invalid location). Bubble this up to the user.
            return Err(ProviderError::format_error("azure_openai", format!("HTTP {}: {}", status_code, body_str)));
        }

        let api_response: AzureResponse =
            response.json().await.map_err(|e| ProviderError::format_error("azure_openai", e.to_string()))?;

        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::format_error("azure_openai", "no choices in response"))?;

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

        let usage = api_response.usage.unwrap_or(AzureUsage {
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
            provider: "azure_openai".to_string(),
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

    /// Native SSE streaming for Azure OpenAI API.
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

        debug!(%model, "streaming Azure OpenAI API");

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
            "stream_options": { "include_usage": true },
        });

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

        let url = self.build_url(&model);
        debug!(%url, tool_count = request.tools.len(), "Azure OpenAI stream: target URL");

        let response = self
            .client
            .post(&url)
            .header("api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::network_error("azure_openai", e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let err_body = response.text().await.unwrap_or_default();
            if !err_body.is_empty() {
                warn!(status, body = %err_body.chars().take(500).collect::<String>(), "Azure OpenAI stream: HTTP error response");
            }
            return match status {
                429 => Err(ProviderError::rate_limit("azure_openai", None)),
                401 | 403 => Err(ProviderError::auth_failure("azure_openai", "default")),
                _ => Err(ProviderError::format_error("azure_openai", format!("HTTP {} — {}", status, err_body.chars().take(300).collect::<String>()))),
            };
        }

        // Parse SSE event stream
        let mut buffer = String::new();
        let mut response = response;
        let mut received_any_content = false;

        // Accumulate tool call deltas during streaming.
        // Azure OpenAI sends tool calls as incremental fragments (same as OpenAI):
        //   delta.tool_calls[i].id (first chunk for call i)
        //   delta.tool_calls[i].function.name (first chunk for call i)
        //   delta.tool_calls[i].function.arguments (subsequent chunks, partial JSON)
        let mut tool_call_accum: Vec<(String, String, String)> = Vec::new(); // (id, name, args_json)

        while let Some(chunk) = response.chunk().await.map_err(|e| {
            ProviderError::network_error("azure_openai", e.to_string())
        })? {
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(boundary) = buffer.find("\n\n") {
                let event_block = buffer[..boundary].to_string();
                buffer = buffer[boundary + 2..].to_string();

                for line in event_block.lines() {
                    let data = if let Some(d) = line.strip_prefix("data: ") {
                        d.trim()
                    } else {
                        continue;
                    };

                    if data == "[DONE]" {
                        if !received_any_content && tool_call_accum.is_empty() {
                            warn!("Azure OpenAI stream completed with [DONE] but produced no content or tool calls");
                        }
                        return Ok(());
                    }

                    let Ok(chunk_json) = serde_json::from_str::<serde_json::Value>(data) else {
                        warn!(raw_data = %data.chars().take(200).collect::<String>(), "Azure OpenAI stream: unparseable SSE chunk");
                        continue;
                    };

                    // Check for error responses embedded in the stream
                    // Azure OpenAI can return error JSON mid-stream
                    // (e.g., content filter violations, quota exceeded).
                    if let Some(error_obj) = chunk_json.get("error") {
                        let err_msg = error_obj.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error");
                        let err_code = error_obj.get("code").and_then(|c| c.as_str()).unwrap_or("unknown");
                        warn!(
                            error_code = %err_code,
                            error_message = %err_msg,
                            "Azure OpenAI stream: error response received in SSE stream"
                        );
                        return Err(ProviderError::format_error(
                            "azure_openai",
                            format!("Stream error [{}]: {}", err_code, err_msg),
                        ));
                    }

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
                            warn!("Azure OpenAI stream: empty choices array (possible content filter)");
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
                            if let Some(tc_arr) = delta.and_then(|d| d.get("tool_calls")).and_then(|v| v.as_array()) {
                                for tc in tc_arr {
                                    let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                    while tool_call_accum.len() <= idx {
                                        tool_call_accum.push((String::new(), String::new(), String::new()));
                                    }
                                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                        tool_call_accum[idx].0 = id.to_string();
                                    }
                                    if let Some(func) = tc.get("function") {
                                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                            tool_call_accum[idx].1 = name.to_string();
                                        }
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
                        debug!(
                            chunk_keys = %chunk_json.as_object()
                                .map(|o| o.keys().map(|k| k.as_str()).collect::<Vec<_>>().join(","))
                                .unwrap_or_default(),
                            "Azure OpenAI stream: chunk without choices or usage"
                        );
                    }
                }
            }
        }

        // Stream ended without [DONE] signal
        if !received_any_content && tool_call_accum.is_empty() {
            warn!("Azure OpenAI stream: connection closed without [DONE] and no content was received");
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
