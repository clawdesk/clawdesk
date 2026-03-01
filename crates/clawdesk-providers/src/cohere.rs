//! Cohere provider adapter.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};
use tracing::debug;

use crate::{
    ChatMessage, FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    StreamChunk, TokenUsage, ToolCall,
};

const COHERE_API_BASE: &str = "https://api.cohere.ai/v2";

/// Cohere provider.
pub struct CohereProvider {
    client: Client,
    api_key: String,
    api_base: String,
    default_model: String,
}

impl CohereProvider {
    pub fn new(api_key: String, api_base: Option<String>, default_model: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
            api_key,
            api_base: api_base.unwrap_or_else(|| COHERE_API_BASE.to_string()),
            default_model: default_model.unwrap_or_else(|| "command-r-plus-08-2024".to_string()),
        }
    }
}

// See https://docs.cohere.com/v2/reference/chat
#[derive(Debug, Serialize)]
struct CohereRequest {
    model: String,
    messages: Vec<CohereMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize)]
struct CohereMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct CohereResponse {
    id: String,
    message: CohereResponseMessage,
    usage: Option<CohereUsage>,
}

#[derive(Debug, Deserialize)]
struct CohereResponseMessage {
    content: Option<Vec<CohereResponseContent>>,
    tool_calls: Option<Vec<CohereToolCall>>,
}

#[derive(Debug, Deserialize)]
struct CohereResponseContent {
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct CohereToolCall {
    id: String,
    function: CohereFunction,
}

#[derive(Debug, Deserialize)]
struct CohereFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct CohereUsage {
    billed_units: Option<CohereBilledUnits>,
}

#[derive(Debug, Deserialize)]
struct CohereBilledUnits {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[async_trait]
impl Provider for CohereProvider {
    fn name(&self) -> &str {
        "cohere"
    }

    fn models(&self) -> Vec<String> {
        vec![
            "command-r-plus-08-2024".to_string(),
            "command-r-08-2024".to_string(),
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

        debug!(%model, messages = request.messages.len(), "calling Cohere API");

        let mut messages = Vec::new();
        if let Some(system) = &request.system_prompt {
            messages.push(CohereMessage {
                role: "system".into(),
                content: system.clone(),
            });
        }
        for m in &request.messages {
            // Cohere maps Assistant to 'assistant' natively in v2
            let role = match m.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => "system",
                MessageRole::Tool => "tool", // Using tool output roles is complex, but generic mapping works
            };
            messages.push(CohereMessage {
                role: role.to_string(),
                content: m.content.to_string(),
            });
        }

        let api_request = CohereRequest {
            model: model.clone(),
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            stream: Some(false),
        };

        let url = format!("{}/chat", self.api_base.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&api_request)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::timeout("cohere", model.clone(), start.elapsed())
                } else {
                    ProviderError::network_error("cohere", e.to_string())
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            let status_code = status.as_u16();
            return match status_code {
                429 => Err(ProviderError::rate_limit("cohere", None)),
                401 | 403 => Err(ProviderError::auth_failure("cohere", "default")),
                _ => Err(ProviderError::server_error("cohere", status_code)),
            };
        }

        let api_response: CohereResponse =
            response.json().await.map_err(|e| ProviderError::format_error("cohere", e.to_string()))?;

        let mut content_out = String::new();
        if let Some(contents) = api_response.message.content {
            for c in contents {
                if c.content_type == "text" {
                    content_out.push_str(&c.text);
                }
            }
        }

        let tool_calls = api_response
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Null),
            })
            .collect::<Vec<_>>();

        let mut input_tokens = 0;
        let mut output_tokens = 0;
        if let Some(usage) = api_response.usage {
            if let Some(billed) = usage.billed_units {
                input_tokens = billed.input_tokens.unwrap_or(0);
                output_tokens = billed.output_tokens.unwrap_or(0);
            }
        }

        let finish_reason = if !tool_calls.is_empty() {
            FinishReason::ToolUse
        } else {
            FinishReason::Stop
        };

        Ok(ProviderResponse {
            content: content_out,
            model: model.clone(),
            provider: "cohere".to_string(),
            usage: TokenUsage {
                input_tokens,
                output_tokens,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            tool_calls,
            finish_reason,
            latency,
        })
    }

    /// Streaming SSE for Cohere v2 API.
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

        debug!(%model, "streaming Cohere API");

        let mut messages = Vec::new();
        if let Some(ref system) = request.system_prompt {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }
        for m in &request.messages {
            let role = match m.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => "system",
                MessageRole::Tool => "tool",
            };
            messages.push(serde_json::json!({
                "role": role,
                "content": &*m.content,
            }));
        }

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        let url = format!("{}/chat", self.api_base.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::network_error("cohere", e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            return match status {
                429 => Err(ProviderError::rate_limit("cohere", None)),
                _ => Err(ProviderError::server_error("cohere", status)),
            };
        }

        let mut buffer = String::new();
        let mut response = response;
        
        // Tool calling stream assembly state
        let mut current_function_name = String::new();
        let mut current_function_args = String::new();
        let mut current_function_id = String::new();

        while let Some(chunk) = response.chunk().await.map_err(|e| {
            ProviderError::network_error("cohere", e.to_string())
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
                        return Ok(());
                    }

                    let Ok(chunk_json) = serde_json::from_str::<Value>(data) else {
                        continue;
                    };

                    if let Some(typ) = chunk_json["type"].as_str() {
                        let mut out_text = String::new();
                        let mut out_tool_call = None;
                        let mut out_finish_reason = None;
                        let mut out_usage = None;
                        let mut done = false;

                        match typ {
                            "content-delta" => {
                                if let Some(text) = chunk_json["delta"]["message"]["content"]["text"].as_str() {
                                    out_text = text.to_string();
                                }
                            }
                            "message-end" => {
                                done = true;
                                if let Some(billed) = chunk_json["delta"]["usage"]["billed_units"].as_object() {
                                    let input = billed.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
                                    let output = billed.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
                                    out_usage = Some(TokenUsage {
                                        input_tokens: input,
                                        output_tokens: output,
                                        cache_read_tokens: None,
                                        cache_write_tokens: None,
                                    });
                                }
                                out_finish_reason = Some(FinishReason::Stop);
                            }
                            "tool-call-start" => {
                                if let (Some(func), Some(id)) = (
                                    chunk_json["delta"]["message"]["tool_calls"]["function"].as_object(),
                                    chunk_json["delta"]["message"]["tool_calls"]["id"].as_str(),
                                ) {
                                    current_function_name = func.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                                    current_function_id = id.to_string();
                                }
                            }
                            "tool-call-delta" => {
                                if let Some(args) = chunk_json["delta"]["message"]["tool_calls"]["function"]["arguments"].as_str() {
                                    current_function_args.push_str(args);
                                }
                            }
                            "tool-call-end" => {
                                if !current_function_name.is_empty() {
                                    let arguments: Value = serde_json::from_str(&current_function_args).unwrap_or(Value::Null);
                                    out_tool_call = Some(ToolCall {
                                        id: current_function_id.clone(),
                                        name: current_function_name.clone(),
                                        arguments,
                                    });
                                }
                                current_function_name.clear();
                                current_function_args.clear();
                                current_function_id.clear();
                            }
                            _ => {}
                        }

                        if !out_text.is_empty() || out_tool_call.is_some() || done {
                            let mut tool_calls = Vec::new();
                            if let Some(tc) = out_tool_call {
                                tool_calls.push(tc);
                            }
                            
                            if out_finish_reason.is_none() && !tool_calls.is_empty() {
                                out_finish_reason = Some(FinishReason::ToolUse);
                            }

                            let _ = chunk_tx
                                .send(StreamChunk {
                                    delta: out_text,
                                    reasoning_delta: String::new(),
                                    done,
                                    finish_reason: out_finish_reason,
                                    usage: out_usage,
                                    tool_calls,
                                })
                                .await;
                        }
                    }
                }
            }
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
        };
        self.complete(&request).await?;
        Ok(())
    }
}
