//! OpenAI-compatible `/v1/chat/completions` endpoint.
//!
//! Enables ClawDesk to serve as a drop-in replacement for the OpenAI API,
//! allowing any OpenAI SDK client or tool to connect directly.
//!
//! Supports both non-streaming and streaming (SSE) modes:
//! - Non-streaming: returns full response in OpenAI format
//! - Streaming: returns `text/event-stream` with `data: {...}\n\n` frames
//!
//! ## Wire compatibility
//!
//! The response format exactly matches OpenAI's `ChatCompletion` object,
//! ensuring compatibility with libraries like `openai-python`, `langchain`,
//! `litellm`, and VS Code Continue extension.

use crate::state::GatewayState;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use clawdesk_providers::{
    ChatMessage, FinishReason, MessageRole, ProviderRequest, StreamChunk,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::error;

// ─── Request types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<OaiMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub stop: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct OaiMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

// ─── Response types ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Usage,
}

#[derive(Debug, Serialize)]
pub struct Choice {
    pub index: u32,
    pub message: ResponseMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub struct ResponseMessage {
    pub role: &'static str,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

// ─── Streaming response types ───────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Serialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// ─── Handler ────────────────────────────────────────────────────────

/// POST /v1/chat/completions
///
/// OpenAI-compatible chat completions endpoint. Routes to the configured
/// default provider, or to a specific provider based on model prefix
/// (e.g., "anthropic/claude-sonnet-4-20250514").
pub async fn chat_completions(
    State(state): State<Arc<GatewayState>>,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let stream = req.stream.unwrap_or(false);

    // Parse model: "provider/model" or just "model"
    let (provider_name, model_name) = if let Some(idx) = req.model.find('/') {
        (
            req.model[..idx].to_string(),
            req.model[idx + 1..].to_string(),
        )
    } else {
        (String::new(), req.model.clone())
    };

    // Resolve provider
    let registry = state.providers.load();
    let provider = if provider_name.is_empty() {
        registry
            .default_provider()
            .ok_or((StatusCode::SERVICE_UNAVAILABLE, "no provider configured".into()))?
    } else {
        registry
            .get(&provider_name)
            .ok_or((
                StatusCode::BAD_REQUEST,
                format!("unknown provider: {}", provider_name),
            ))?
    };

    // Convert messages
    let mut system_prompt = None;
    let mut messages = Vec::new();

    for msg in &req.messages {
        let content = msg.content.clone().unwrap_or_default();
        match msg.role.as_str() {
            "system" => {
                system_prompt = Some(content);
            }
            "user" => {
                messages.push(ChatMessage {
                    role: MessageRole::User,
                    content: std::sync::Arc::from(content),
                    cached_tokens: None,
                });
            }
            "assistant" => {
                messages.push(ChatMessage {
                    role: MessageRole::Assistant,
                    content: std::sync::Arc::from(content),
                    cached_tokens: None,
                });
            }
            "tool" => {
                messages.push(ChatMessage {
                    role: MessageRole::Tool,
                    content: std::sync::Arc::from(content),
                    cached_tokens: None,
                });
            }
            _ => {
                messages.push(ChatMessage {
                    role: MessageRole::User,
                    content: std::sync::Arc::from(content),
                    cached_tokens: None,
                });
            }
        }
    }

    let provider_request = ProviderRequest {
        model: model_name.clone(),
        messages,
        system_prompt,
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        tools: vec![],
        stream,
    };

    if stream {
        // Streaming mode: return SSE
        let (tx, mut rx) = mpsc::channel::<StreamChunk>(64);
        let provider = provider.clone();
        let completion_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
        let created = chrono::Utc::now().timestamp();
        let model = model_name.clone();

        // Spawn provider streaming with CancellationToken propagation.
        // When the gateway shuts down (cancel fired) or the client disconnects
        // (tx is dropped → provider sees SendError), this task aborts promptly
        // instead of hemorrhaging tokens against the upstream LLM.
        let cancel = state.cancel.clone();
        let _stream_id = completion_id.clone();
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    tracing::debug!("streaming cancelled by shutdown");
                }
                result = provider.stream(&provider_request, tx) => {
                    if let Err(e) = result {
                        error!(error = %e, "provider stream error");
                    }
                }
            }
        });

        // Build SSE response using axum's streaming body
        let body = async_stream::stream! {
            // First chunk with role
            let initial = ChatCompletionChunk {
                id: completion_id.clone(),
                object: "chat.completion.chunk",
                created,
                model: model.clone(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: Delta {
                        role: Some("assistant"),
                        content: None,
                    },
                    finish_reason: None,
                }],
                usage: None,
            };
            yield Ok::<_, std::convert::Infallible>(
                format!("data: {}\n\n", serde_json::to_string(&initial).unwrap())
            );

            while let Some(chunk) = rx.recv().await {
                let finish_reason = chunk.finish_reason.map(|f| match f {
                    FinishReason::Stop => "stop".to_string(),
                    FinishReason::ToolUse => "tool_calls".to_string(),
                    FinishReason::MaxTokens => "length".to_string(),
                    FinishReason::ContentFilter => "content_filter".to_string(),
                });

                let sse_chunk = ChatCompletionChunk {
                    id: completion_id.clone(),
                    object: "chat.completion.chunk",
                    created,
                    model: model.clone(),
                    choices: vec![ChunkChoice {
                        index: 0,
                        delta: Delta {
                            role: None,
                            content: if chunk.delta.is_empty() { None } else { Some(chunk.delta) },
                        },
                        finish_reason,
                    }],
                    usage: chunk.usage.map(|u| Usage {
                        prompt_tokens: u.input_tokens,
                        completion_tokens: u.output_tokens,
                        total_tokens: u.input_tokens + u.output_tokens,
                    }),
                };
                yield Ok(format!("data: {}\n\n", serde_json::to_string(&sse_chunk).unwrap()));
            }

            yield Ok("data: [DONE]\n\n".to_string());
        };

        let stream_body = axum::body::Body::from_stream(body);

        Ok(axum::response::Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .body(stream_body)
            .unwrap()
            .into_response())
    } else {
        // Non-streaming mode
        let resp = provider
            .complete(&provider_request)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let finish_reason = match resp.finish_reason {
            FinishReason::Stop => "stop",
            FinishReason::ToolUse => "tool_calls",
            FinishReason::MaxTokens => "length",
            FinishReason::ContentFilter => "content_filter",
        };

        let response = ChatCompletionResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            object: "chat.completion",
            created: chrono::Utc::now().timestamp(),
            model: resp.model,
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: "assistant",
                    content: resp.content,
                },
                finish_reason: finish_reason.to_string(),
            }],
            usage: Usage {
                prompt_tokens: resp.usage.input_tokens,
                completion_tokens: resp.usage.output_tokens,
                total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
            },
        };

        Ok(Json(response).into_response())
    }
}

/// GET /v1/models
///
/// List available models in OpenAI-compatible format.
pub async fn list_models(
    State(state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    let registry = state.providers.load();
    let mut models = Vec::new();

    for (provider_name, provider) in registry.iter() {
        for model in provider.models() {
            models.push(serde_json::json!({
                "id": format!("{}/{}", provider_name, model),
                "object": "model",
                "created": 0,
                "owned_by": provider_name,
            }));
        }
    }

    Json(serde_json::json!({
        "object": "list",
        "data": models,
    }))
}
