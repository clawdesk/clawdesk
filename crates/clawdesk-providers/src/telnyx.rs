//! Telnyx LLM provider — simple OpenAI-compatible inference API.
//!
//! Telnyx offers an OpenAI-compatible chat completions endpoint at
//! `api.telnyx.com/v2/ai/chat/completions`. It supports basic text
//! completion without native tool calling or streaming.
//!
//! The simplest provider with no native tools, no streaming, and no vision.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{
    FinishReason, Provider, ProviderRequest, ProviderResponse,
    TokenUsage,
};

const TELNYX_BASE_URL: &str = "https://api.telnyx.com/v2/ai";

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
}

#[derive(Serialize)]
struct WireMessage {
    role: String,
    content: String,
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
}

#[derive(Deserialize)]
struct UsageResponse {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Telnyx LLM provider.
///
/// Simple provider with text completion only — no native tool calling,
/// no streaming, no vision. The trait's default `stream()` implementation
/// will be used (single-chunk fallback).
pub struct TelnyxProvider {
    client: Client,
    api_key: String,
    default_model: String,
}

impl TelnyxProvider {
    /// Create a new Telnyx provider.
    ///
    /// Resolves API key from parameter, `TELNYX_API_KEY` env var,
    /// or `API_KEY` env var.
    pub fn new(api_key: Option<&str>) -> Self {
        let key = api_key
            .map(String::from)
            .or_else(|| std::env::var("TELNYX_API_KEY").ok())
            .or_else(|| std::env::var("API_KEY").ok())
            .unwrap_or_default();

        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .pool_max_idle_per_host(2)
                .build()
                .expect("failed to build HTTP client"),
            api_key: key,
            default_model: "meta-llama/Meta-Llama-3.1-70B-Instruct".into(),
        }
    }

    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }
}

#[async_trait]
impl Provider for TelnyxProvider {
    fn name(&self) -> &str {
        "telnyx"
    }

    fn models(&self) -> Vec<String> {
        vec![
            "meta-llama/Meta-Llama-3.1-70B-Instruct".into(),
            "meta-llama/Meta-Llama-3.1-8B-Instruct".into(),
            "mistralai/Mistral-7B-Instruct-v0.2".into(),
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

        let mut messages = Vec::new();
        if let Some(ref sys) = request.system_prompt {
            messages.push(WireMessage {
                role: "system".into(),
                content: sys.clone(),
            });
        }
        for msg in &request.messages {
            messages.push(WireMessage {
                role: msg.role.as_str().to_string(),
                content: msg.content.to_string(),
            });
        }

        let body = CompletionRequest {
            model: model.to_string(),
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
        };

        let start = std::time::Instant::now();
        let url = format!("{TELNYX_BASE_URL}/chat/completions");

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout {
                        provider: "telnyx".into(),
                        model: model.to_string(),
                        after: std::time::Duration::from_secs(120),
                    }
                } else {
                    ProviderError::NetworkError {
                        provider: "telnyx".into(),
                        detail: e.to_string(),
                    }
                }
            })?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(match status {
                429 => ProviderError::RateLimit {
                    provider: "telnyx".into(),
                    retry_after: None,
                },
                401 | 403 => ProviderError::AuthFailure {
                    provider: "telnyx".into(),
                    profile_id: String::new(),
                },
                s if s >= 500 => ProviderError::ServerError {
                    provider: "telnyx".into(),
                    status: s,
                },
                _ => ProviderError::FormatError {
                    provider: "telnyx".into(),
                    detail: format!("HTTP {status}: {body_text}"),
                },
            });
        }

        let resp_body: CompletionResponse =
            resp.json().await.map_err(|e| ProviderError::FormatError {
                provider: "telnyx".into(),
                detail: format!("JSON parse error: {e}"),
            })?;

        let choice = resp_body
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::FormatError {
                provider: "telnyx".into(),
                detail: "no choices".into(),
            })?;

        let usage = resp_body.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cache_read_tokens: None,
            cache_write_tokens: None,
        });

        let finish_reason = match choice.finish_reason.as_deref() {
            Some("stop") => FinishReason::Stop,
            Some("length") => FinishReason::MaxTokens,
            _ => FinishReason::Stop,
        };

        Ok(ProviderResponse {
            content: choice.message.content.unwrap_or_default(),
            model: resp_body.model.unwrap_or_else(|| model.to_string()),
            provider: "telnyx".into(),
            usage: usage.unwrap_or_default(),
            tool_calls: Vec::new(),
            finish_reason,
            latency: start.elapsed(),
        })
    }

    // No native streaming — uses default single-chunk fallback.

    async fn health_check(&self) -> Result<(), ProviderError> {
        let resp = self
            .client
            .get(&format!("{TELNYX_BASE_URL}/models"))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError {
                provider: "telnyx".into(),
                detail: e.to_string(),
            })?;

        if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
            return Err(ProviderError::AuthFailure {
                provider: "telnyx".into(),
                profile_id: String::new(),
            });
        }

        debug!("telnyx health check passed");
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
    fn test_provider_name() {
        let p = TelnyxProvider::new(Some("test-key"));
        assert_eq!(p.name(), "telnyx");
    }

    #[test]
    fn test_models() {
        let p = TelnyxProvider::new(Some("test-key"));
        assert!(!p.models().is_empty());
    }

    #[test]
    fn test_default_model() {
        let p = TelnyxProvider::new(Some("test-key"))
            .with_default_model("custom-model");
        assert_eq!(p.default_model, "custom-model");
    }
}
