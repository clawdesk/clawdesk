//! Azure OpenAI provider adapter.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::debug;

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

// Reuse the same JSON structs as OpenAiProvider since the body format is identical.
#[derive(Debug, Serialize)]
struct AzureRequest {
    model: String,
    messages: Vec<AzureMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
struct AzureMessage {
    role: String,
    content: String,
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

        let mut messages = Vec::new();
        if let Some(system) = &request.system_prompt {
            messages.push(AzureMessage {
                role: "system".into(),
                content: system.clone(),
            });
        }
        for m in &request.messages {
            messages.push(AzureMessage {
                role: m.role.as_str().to_string(),
                content: m.content.to_string(),
            });
        }

        let api_request = AzureRequest {
            model: model.clone(), // Model string may be ignored by Azure depending on deployment, but we send it
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
        };

        let url = self.build_url(&model);

        let response = self
            .client
            .post(&url)
            .header("api-key", &self.api_key)
            .json(&api_request)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout {
                        provider: "azure_openai".into(),
                        model: model.clone(),
                        after: start.elapsed(),
                    }
                } else {
                    ProviderError::NetworkError {
                        provider: "azure_openai".into(),
                        detail: e.to_string(),
                    }
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            let status_code = status.as_u16();
            let body_bytes = response.bytes().await.unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes);
            
            if status_code == 429 {
                return Err(ProviderError::RateLimit {
                    provider: "azure_openai".into(),
                    retry_after: None,
                });
            }

            // For Azure, 400, 401, 403, 404 usually contain a precise JSON error explaining what is wrong 
            // (e.g. invalid key, invalid deployment name, invalid location). Bubble this up to the user.
            return Err(ProviderError::FormatError {
                provider: "azure_openai".into(),
                detail: format!("HTTP {}: {}", status_code, body_str),
            });
        }

        let api_response: AzureResponse =
            response.json().await.map_err(|e| ProviderError::FormatError {
                provider: "azure_openai".into(),
                detail: e.to_string(),
            })?;

        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::FormatError {
                provider: "azure_openai".into(),
                detail: "no choices in response".into(),
            })?;

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

        let mut messages = Vec::new();
        if let Some(ref system) = request.system_prompt {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }
        for m in &request.messages {
            messages.push(serde_json::json!({
                "role": m.role.as_str(),
                "content": &*m.content,
            }));
        }

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

        let url = self.build_url(&model);

        let response = self
            .client
            .post(&url)
            .header("api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError {
                provider: "azure_openai".into(),
                detail: e.to_string(),
            })?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_bytes = response.bytes().await.unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes);

            if status == 429 {
                return Err(ProviderError::RateLimit {
                    provider: "azure_openai".into(),
                    retry_after: None,
                });
            }

            return Err(ProviderError::FormatError {
                provider: "azure_openai".into(),
                detail: format!("HTTP {}: {}", status, body_str),
            });
        }

        // Parse SSE event stream (Identical to OpenAI streaming logic)
        let mut buffer = String::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            ProviderError::NetworkError {
                provider: "azure_openai".into(),
                detail: e.to_string(),
            }
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

                    let Ok(chunk_json) = serde_json::from_str::<serde_json::Value>(data) else {
                        continue;
                    };

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
                        for choice in choices {
                            let delta = choice.get("delta");
                            let content = delta
                                .and_then(|d| d.get("content"))
                                .and_then(|c| c.as_str())
                                .unwrap_or("");

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

                            let _ = chunk_tx
                                .send(StreamChunk {
                                    delta: content.to_string(),
                                    done,
                                    finish_reason: finish,
                                    usage: if done { usage.clone() } else { None },
                                    tool_calls: Vec::new(),
                                })
                                .await;
                        }
                    } else if usage.is_some() {
                        let _ = chunk_tx
                            .send(StreamChunk {
                                delta: String::new(),
                                done: true,
                                finish_reason: Some(FinishReason::Stop),
                                usage,
                                tool_calls: Vec::new(),
                            })
                            .await;
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
