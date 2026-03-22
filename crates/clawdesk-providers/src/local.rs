//! Local inference provider for ClawDesk.
//!
//! This provider manages llama.cpp (llama-server) processes directly,
//! exposing an OpenAI-compatible API for chat completions.
//! Replaces the need for Ollama, LM Studio, or other external tools.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::{
    ChatMessage, FinishReason, MessageRole, Provider, ProviderRequest,
    ProviderResponse, StreamChunk, ToolCall, ToolDefinition, TokenUsage,
};

/// Local inference provider that connects to locally-managed llama-server instances.
///
/// Unlike the Ollama provider which relies on a separate Ollama server,
/// this provider talks directly to llama-server processes managed by
/// `clawdesk-local-models::ServerManager`.
pub struct LocalProvider {
    client: Client,
    /// Map of model_name → base_url (e.g., "http://127.0.0.1:39090")
    model_endpoints: Arc<RwLock<Vec<LocalModelEndpoint>>>,
    default_model: String,
}

#[derive(Debug, Clone)]
pub struct LocalModelEndpoint {
    pub model_name: String,
    pub base_url: String,
    pub port: u16,
}

impl LocalProvider {
    pub fn new(default_model: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(300))
                .pool_idle_timeout(Duration::from_secs(90))
                .build()
                .expect("failed to build HTTP client"),
            model_endpoints: Arc::new(RwLock::new(Vec::new())),
            default_model: default_model.unwrap_or_default(),
        }
    }

    /// Register a running model endpoint.
    pub fn register_model(&self, name: String, port: u16) {
        let endpoint = LocalModelEndpoint {
            model_name: name.clone(),
            base_url: format!("http://127.0.0.1:{}", port),
            port,
        };
        if let Ok(mut endpoints) = self.model_endpoints.write() {
            // Remove old entry if exists
            endpoints.retain(|e| e.model_name != name);
            endpoints.push(endpoint);
        }
    }

    /// Unregister a model endpoint.
    pub fn unregister_model(&self, name: &str) {
        if let Ok(mut endpoints) = self.model_endpoints.write() {
            endpoints.retain(|e| e.model_name != name);
        }
    }

    /// Get the base URL for a model.
    fn get_endpoint(&self, model: &str) -> Option<String> {
        self.model_endpoints.read().ok().and_then(|endpoints| {
            endpoints
                .iter()
                .find(|e| e.model_name == model)
                .or_else(|| endpoints.first())
                .map(|e| e.base_url.clone())
        })
    }

    fn api_url(&self, base: &str, path: &str) -> String {
        format!("{}/v1{}", base, path)
    }
}

// --- OpenAI-compatible API types for llama-server ---

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ApiMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiTool>,
}

#[derive(Debug, Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ApiTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ApiFunction,
}

#[derive(Debug, Serialize)]
struct ApiFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    model: Option<String>,
    usage: Option<ApiUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Option<ResponseMessage>,
    delta: Option<ResponseMessage>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ApiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ApiToolCall {
    id: Option<String>,
    function: Option<ApiToolCallFunction>,
}

#[derive(Debug, Deserialize)]
struct ApiToolCallFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

fn convert_messages(request: &ProviderRequest) -> Vec<ApiMessage> {
    let mut msgs = Vec::new();

    if let Some(ref system) = request.system_prompt {
        msgs.push(ApiMessage {
            role: "system".into(),
            content: system.clone(),
        });
    }

    for m in &request.messages {
        msgs.push(ApiMessage {
            role: m.role.as_str().to_string(),
            content: m.content.to_string(),
        });
    }

    msgs
}

fn convert_tools(tools: &[ToolDefinition]) -> Vec<ApiTool> {
    tools
        .iter()
        .map(|t| ApiTool {
            tool_type: "function".to_string(),
            function: ApiFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        })
        .collect()
}

#[async_trait]
impl Provider for LocalProvider {
    fn name(&self) -> &str {
        "local"
    }

    fn models(&self) -> Vec<String> {
        self.model_endpoints
            .read()
            .map(|endpoints| endpoints.iter().map(|e| e.model_name.clone()).collect())
            .unwrap_or_default()
    }

    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let start = Instant::now();

        let model_name = if request.model.is_empty() || request.model == "default" || request.model == "auto" {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        let base_url = self.get_endpoint(&model_name).ok_or_else(|| {
            ProviderError::network_error(
                "local",
                format!("No local model '{}' is running. Start it from Local Models.", model_name),
            )
        })?;

        debug!(%model_name, %base_url, "calling local llama-server");

        let api_request = ChatRequest {
            model: model_name.clone(),
            messages: convert_messages(request),
            stream: false,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools: convert_tools(&request.tools),
        };

        let response = self
            .client
            .post(self.api_url(&base_url, "/chat/completions"))
            .json(&api_request)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    ProviderError::network_error(
                        "local",
                        format!("Cannot connect to local model at {} — is it running?", base_url),
                    )
                } else if e.is_timeout() {
                    ProviderError::timeout("local", model_name.clone(), start.elapsed())
                } else {
                    ProviderError::network_error("local", e.to_string())
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            return Err(ProviderError::server_error("local", status.as_u16()));
        }

        let api_response: ChatResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::format_error("local", e.to_string()))?;

        let choice = api_response.choices.first().ok_or_else(|| {
            ProviderError::format_error("local", "no choices in response".to_string())
        })?;

        let msg = choice.message.as_ref().ok_or_else(|| {
            ProviderError::format_error("local", "no message in choice".to_string())
        })?;

        let content = msg.content.clone().unwrap_or_default();
        let tool_calls = extract_tool_calls(msg);

        let finish_reason = if !tool_calls.is_empty() {
            FinishReason::ToolUse
        } else {
            match choice.finish_reason.as_deref() {
                Some("length") => FinishReason::MaxTokens,
                _ => FinishReason::Stop,
            }
        };

        let usage = api_response.usage.as_ref();

        Ok(ProviderResponse {
            content,
            model: api_response.model.unwrap_or(model_name),
            provider: "local".to_string(),
            usage: TokenUsage {
                input_tokens: usage.and_then(|u| u.prompt_tokens).unwrap_or(0),
                output_tokens: usage.and_then(|u| u.completion_tokens).unwrap_or(0),
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            tool_calls,
            finish_reason,
            latency,
        })
    }

    async fn stream(
        &self,
        request: &ProviderRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), ProviderError> {
        let model_name = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        let base_url = self.get_endpoint(&model_name).ok_or_else(|| {
            ProviderError::network_error(
                "local",
                format!("No local model '{}' is running", model_name),
            )
        })?;

        debug!(%model_name, "streaming from local llama-server");

        let api_request = ChatRequest {
            model: model_name.clone(),
            messages: convert_messages(request),
            stream: true,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools: convert_tools(&request.tools),
        };

        let response = self
            .client
            .post(self.api_url(&base_url, "/chat/completions"))
            .json(&api_request)
            .send()
            .await
            .map_err(|e| ProviderError::network_error("local", e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::server_error("local", response.status().as_u16()));
        }

        let mut buffer = String::new();
        let mut byte_buf: Vec<u8> = Vec::new();
        let mut total_input = 0u64;
        let mut total_output = 0u64;
        let mut response = response;

        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ProviderError::network_error("local", e.to_string()))?
        {
            byte_buf.extend_from_slice(&chunk);
            let valid_len = match std::str::from_utf8(&byte_buf) {
                Ok(s) => s.len(),
                Err(e) => e.valid_up_to(),
            };
            if valid_len == 0 { continue; }
            let text = std::str::from_utf8(&byte_buf[..valid_len]).expect("valid UTF-8");
            buffer.push_str(text);
            byte_buf.drain(..valid_len);

            while let Some(newline) = buffer.find('\n') {
                let line = buffer[..newline].trim().to_string();
                buffer = buffer[newline + 1..].to_string();

                if line.is_empty() || line == "data: [DONE]" {
                    if line == "data: [DONE]" {
                        let _ = chunk_tx
                            .send(StreamChunk {
                                delta: String::new(),
                                reasoning_delta: String::new(),
                                done: true,
                                finish_reason: Some(FinishReason::Stop),
                                usage: Some(TokenUsage {
                                    input_tokens: total_input,
                                    output_tokens: total_output,
                                    cache_read_tokens: None,
                                    cache_write_tokens: None,
                                }),
                                tool_calls: Vec::new(),
                            })
                            .await;
                        return Ok(());
                    }
                    continue;
                }

                let json_str = line.strip_prefix("data: ").unwrap_or(&line);

                let Ok(chunk_data) = serde_json::from_str::<ChatResponse>(json_str) else {
                    continue;
                };

                if let Some(choice) = chunk_data.choices.first() {
                    let delta = choice
                        .delta
                        .as_ref()
                        .and_then(|d| d.content.clone())
                        .unwrap_or_default();

                    let tool_calls = choice
                        .delta
                        .as_ref()
                        .map(|d| extract_tool_calls(d))
                        .unwrap_or_default();

                    if let Some(u) = &chunk_data.usage {
                        total_input = u.prompt_tokens.unwrap_or(total_input);
                        total_output = u.completion_tokens.unwrap_or(total_output);
                    }

                    let is_stop = choice.finish_reason.is_some();
                    let finish = if is_stop {
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
                            delta,
                            reasoning_delta: String::new(),
                            done: is_stop,
                            finish_reason: finish,
                            usage: if is_stop {
                                Some(TokenUsage {
                                    input_tokens: total_input,
                                    output_tokens: total_output,
                                    cache_read_tokens: None,
                                    cache_write_tokens: None,
                                })
                            } else {
                                None
                            },
                            tool_calls,
                        })
                        .await;

                    if is_stop {
                        return Ok(());
                    }
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        let health_url = {
            let endpoints = self
                .model_endpoints
                .read()
                .map_err(|_| ProviderError::network_error("local", "lock poisoned"))?;

            if endpoints.is_empty() {
                return Err(ProviderError::network_error(
                    "local",
                    "no local models running",
                ));
            }

            endpoints.first().map(|ep| format!("{}/health", ep.base_url))
        };

        if let Some(url) = health_url {
            self.client
                .get(&url)
                .send()
                .await
                .map_err(|e| ProviderError::network_error("local", e.to_string()))?;
        }

        Ok(())
    }
}

fn extract_tool_calls(msg: &ResponseMessage) -> Vec<ToolCall> {
    msg.tool_calls
        .as_ref()
        .map(|calls| {
            calls
                .iter()
                .enumerate()
                .filter_map(|(i, tc)| {
                    let func = tc.function.as_ref()?;
                    Some(ToolCall {
                        id: tc.id.clone().unwrap_or_else(|| format!("local_tc_{}", i)),
                        name: func.name.clone().unwrap_or_default(),
                        arguments: func
                            .arguments
                            .as_ref()
                            .and_then(|a| serde_json::from_str(a).ok())
                            .unwrap_or(serde_json::Value::Object(Default::default())),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
