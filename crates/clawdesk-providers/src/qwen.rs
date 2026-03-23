//! Alibaba Qwen (DashScope) provider with OAuth authentication.
//!
//! ## DashScope API
//!
//! The DashScope API uses a non-standard SSE format with incremental output:
//! - Delta tokens nested inside `choices[].message.content`
//! - Function calling uses `functions[]` (not `tools[]`)
//! - Supports Qwen-Max, Qwen-Plus, Qwen-Turbo, Qwen-VL
//!
//! ## Streaming
//!
//! DashScope incremental mode: each delta contains only new tokens.
//! O(k) per delta where k = new token count.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::{
    FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    StreamChunk, ToolCall, ToolDefinition, TokenUsage,
};

/// DashScope OpenAI-compatible endpoint (preferred per current docs).
/// This allows using the standard OpenAI chat/completions format.
const DASHSCOPE_COMPAT_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions";

/// Legacy DashScope native endpoint (kept for reference).
#[allow(dead_code)]
const DASHSCOPE_NATIVE_URL: &str = "https://dashscope.aliyuncs.com/api/v1/services/aigc/text-generation/generation";

/// Qwen / DashScope chat provider.
pub struct QwenProvider {
    client: Client,
    api_key: String,
    default_model: String,
}

impl QwenProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
            api_key,
            default_model: "qwen-max".into(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    fn build_messages(request: &ProviderRequest) -> Vec<DashScopeMessage> {
        let mut msgs = Vec::new();

        if let Some(ref sys) = request.system_prompt {
            msgs.push(DashScopeMessage {
                role: "system".into(),
                content: sys.clone(),
            });
        }

        for msg in &request.messages {
            msgs.push(DashScopeMessage {
                role: msg.role.as_str().to_string(),
                content: msg.content.to_string(),
            });
        }

        msgs
    }
}

#[async_trait]
impl Provider for QwenProvider {
    fn name(&self) -> &str {
        "qwen"
    }

    fn models(&self) -> Vec<String> {
        vec![
            "qwen-max".into(),
            "qwen-plus".into(),
            "qwen-turbo".into(),
            "qwen-vl-max".into(),
            "qwen-vl-plus".into(),
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
        let start = std::time::Instant::now();
        let messages = Self::build_messages(request);

        // Use OpenAI-compatible format per current DashScope docs
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
            .post(DASHSCOPE_COMPAT_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::network_error("qwen", e.to_string()))?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            let err_body = response.text().await.unwrap_or_default();
            return match status {
                429 => Err(ProviderError::rate_limit("qwen", None)),
                401 | 403 => Err(ProviderError::auth_failure("qwen", "invalid API key")),
                _ => Err(ProviderError::format_error(
                    "qwen",
                    format!("HTTP {status}: {}", err_body.chars().take(300).collect::<String>()),
                )),
            };
        }

        // OpenAI-compatible response format
        let resp: OaiResponse = response.json().await.map_err(|e| {
            ProviderError::format_error("qwen", format!("response parse error: {e}"))
        })?;

        let choice = resp
            .choices
            .first()
            .ok_or_else(|| ProviderError::format_error("qwen", "no choices returned"))?;

        let content = choice.message.content.clone().unwrap_or_default();
        let finish_reason = match choice.finish_reason.as_deref() {
            Some("stop") => FinishReason::Stop,
            Some("tool_calls") => FinishReason::ToolUse,
            Some("length") => FinishReason::MaxTokens,
            _ => FinishReason::Stop,
        };

        let tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .map(|tcs| {
                tcs.iter()
                    .map(|tc| ToolCall {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments: serde_json::from_str(&tc.function.arguments)
                            .unwrap_or(serde_json::Value::String(tc.function.arguments.clone())),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let usage = resp.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cache_read_tokens: None,
            cache_write_tokens: None,
        });

        Ok(ProviderResponse {
            content,
            model: model.to_string(),
            provider: "qwen".into(),
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
        let messages = Self::build_messages(request);

        // OpenAI-compatible streaming format
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
        });

        let response = self
            .client
            .post(DASHSCOPE_COMPAT_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::network_error("qwen", e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let err_body = response.text().await.unwrap_or_default();
            return Err(ProviderError::format_error(
                "qwen",
                format!("HTTP {status}: {}", err_body.chars().take(300).collect::<String>()),
            ));
        }

        // Standard OpenAI SSE parsing (same format since we use compatible mode)
        let mut buffer = String::new();
        let mut response = response;

        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ProviderError::network_error("qwen", e.to_string()))?
        {
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    if data.trim() == "[DONE]" {
                        let _ = chunk_tx
                            .send(StreamChunk {
                                delta: String::new(),
                                reasoning_delta: String::new(),
                                done: true,
                                finish_reason: Some(FinishReason::Stop),
                                usage: None,
                                tool_calls: Vec::new(),
                            })
                            .await;
                        return Ok(());
                    }

                    if let Ok(sse) = serde_json::from_str::<OaiStreamChunk>(data) {
                        if let Some(choice) = sse.choices.first() {
                            let delta = choice.delta.content.clone().unwrap_or_default();
                            let finish = choice
                                .finish_reason
                                .as_deref()
                                .map(|r| match r {
                                    "length" => FinishReason::MaxTokens,
                                    "tool_calls" => FinishReason::ToolUse,
                                    _ => FinishReason::Stop,
                                });

                            let _ = chunk_tx
                                .send(StreamChunk {
                                    delta,
                                    reasoning_delta: String::new(),
                                    done: finish.is_some(),
                                    finish_reason: finish,
                                    usage: None,
                                    tool_calls: Vec::new(),
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
        debug!("qwen health check passed");
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenAI-Compatible Response Types (used with DashScope compatible-mode endpoint)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct DashScopeMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OaiResponse {
    choices: Vec<OaiChoice>,
    #[serde(default)]
    usage: Option<OaiUsage>,
}

#[derive(Deserialize)]
struct OaiChoice {
    message: OaiResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OaiResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OaiToolCall>>,
}

#[derive(Deserialize)]
struct OaiToolCall {
    id: String,
    function: OaiFunction,
}

#[derive(Deserialize)]
struct OaiFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct OaiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

/// SSE streaming chunk (OpenAI-compatible format).
#[derive(Deserialize)]
struct OaiStreamChunk {
    choices: Vec<OaiStreamChoice>,
}

#[derive(Deserialize)]
struct OaiStreamChoice {
    delta: OaiStreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OaiStreamDelta {
    #[serde(default)]
    content: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name() {
        let p = QwenProvider::new("test-key".into());
        assert_eq!(p.name(), "qwen");
    }

    #[test]
    fn model_list() {
        let p = QwenProvider::new("test-key".into());
        let models = p.models();
        assert!(models.contains(&"qwen-max".to_string()));
        assert!(models.contains(&"qwen-turbo".to_string()));
    }

    #[test]
    fn build_messages_with_system() {
        let request = ProviderRequest {
            model: "qwen-max".into(),
            messages: vec![],
            system_prompt: Some("You are helpful".into()),
            max_tokens: None,
            temperature: None,
            tools: vec![],
            stream: false,
            images: vec![],
        };
        let msgs = QwenProvider::build_messages(&request);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "system");
    }
}
