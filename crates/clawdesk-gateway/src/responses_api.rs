//! OpenAI Responses API compatibility layer.
//!
//! Implements the OpenAI Responses API (`POST /v1/responses`) on top of
//! ClawDesk's provider system, enabling drop-in compatibility with
//! OpenAI SDK clients that use the newer Responses API format.
//!
//! ## Endpoints
//! - `POST /v1/responses` — Create a response (streaming + non-streaming)
//! - `GET  /v1/responses/:id` — Retrieve a response
//!
//! ## Key differences from Chat Completions API
//! - Stateful: responses have persistent IDs
//! - Tool calls are first-class server-side objects
//! - Built-in tools (web_search, code_interpreter, file_search)
//! - Supports `previous_response_id` for conversation threading

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use chrono::Utc;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

use crate::state::GatewayState;

// ── Request types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateResponseRequest {
    /// The model to use (e.g., "gpt-4o", "claude-sonnet-4-20250514")
    pub model: String,
    /// Input messages/items for the response
    pub input: ResponseInput,
    /// Instructions (system prompt)
    #[serde(default)]
    pub instructions: Option<String>,
    /// Maximum output tokens
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// Temperature
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Available tools
    #[serde(default)]
    pub tools: Vec<ResponseTool>,
    /// Whether to stream the response
    #[serde(default)]
    pub stream: bool,
    /// Previous response ID for conversation threading
    #[serde(default)]
    pub previous_response_id: Option<String>,
    /// Metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ResponseInput {
    /// Simple text input
    Text(String),
    /// Array of input items
    Items(Vec<InputItem>),
}

#[derive(Debug, Deserialize)]
pub struct InputItem {
    /// Item type: "message", "item_reference"
    #[serde(rename = "type", default = "default_message_type")]
    pub item_type: String,
    /// Role for message items
    #[serde(default)]
    pub role: Option<String>,
    /// Content of the item
    #[serde(default)]
    pub content: Option<InputContent>,
    /// Item reference ID
    #[serde(default)]
    pub id: Option<String>,
}

fn default_message_type() -> String {
    "message".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum InputContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

// ── Response types ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ResponseObject {
    pub id: String,
    pub object: &'static str,
    pub created_at: i64,
    pub status: &'static str,
    pub model: String,
    pub output: Vec<OutputItem>,
    pub usage: ResponseUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputItem {
    #[serde(rename = "type")]
    pub item_type: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub content: Vec<OutputContent>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

// ── Response store ────────────────────────────────────────────

/// Maximum number of responses kept in the in-memory store.
/// Prevents unbounded growth on long-running instances.
const RESPONSE_STORE_MAX_ENTRIES: usize = 10_000;

/// Bounded in-memory response store with LRU eviction.
///
/// Responses are stored in a `HashMap` for O(1) lookup and a `VecDeque`
/// for insertion-order tracking. When capacity is reached, the oldest
/// response is evicted (FIFO approximation of LRU — responses are
/// write-once, so insertion order ≈ access order).
pub struct BoundedResponseStore {
    map: HashMap<String, ResponseObject>,
    order: std::collections::VecDeque<String>,
    max_entries: usize,
}

impl BoundedResponseStore {
    pub fn new(max_entries: usize) -> Self {
        Self {
            map: HashMap::with_capacity(max_entries.min(1024)),
            order: std::collections::VecDeque::with_capacity(max_entries.min(1024)),
            max_entries,
        }
    }

    /// Insert a response, evicting the oldest if at capacity.
    pub fn insert(&mut self, id: String, response: ResponseObject) {
        if self.map.contains_key(&id) {
            // Update existing — no order change needed.
            self.map.insert(id, response);
            return;
        }
        while self.map.len() >= self.max_entries {
            if let Some(evict_id) = self.order.pop_front() {
                self.map.remove(&evict_id);
            } else {
                break;
            }
        }
        self.order.push_back(id.clone());
        self.map.insert(id, response);
    }

    /// Get a response by ID.
    pub fn get(&self, id: &str) -> Option<&ResponseObject> {
        self.map.get(id)
    }
}

pub type ResponseStore = Arc<RwLock<BoundedResponseStore>>;

/// Create a new bounded response store.
pub fn new_response_store() -> ResponseStore {
    Arc::new(RwLock::new(BoundedResponseStore::new(RESPONSE_STORE_MAX_ENTRIES)))
}

// ── SSE event types ──────────────────────────────────────────

#[derive(Debug, Serialize)]
struct SseResponseEvent {
    #[serde(rename = "type")]
    event_type: String,
    response: Option<ResponseObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    item: Option<OutputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_index: Option<usize>,
}

// ── Handlers ─────────────────────────────────────────────────

/// POST /v1/responses — Create a response.
pub async fn create_response(
    State(state): State<Arc<GatewayState>>,
    Json(req): Json<CreateResponseRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    debug!(model = %req.model, stream = req.stream, "responses API: create");

    if req.stream {
        return create_response_streaming(state, req).await;
    }

    // Convert ResponseInput to ProviderRequest messages
    let messages = convert_input_to_messages(&req);
    let system_prompt = req.instructions.clone();

    // Convert tools
    let tools: Vec<clawdesk_providers::ToolDefinition> = req
        .tools
        .iter()
        .filter(|t| t.tool_type == "function")
        .filter_map(|t| {
            Some(clawdesk_providers::ToolDefinition {
                name: t.name.clone()?,
                description: t.description.clone().unwrap_or_default(),
                parameters: t.parameters.clone().unwrap_or(serde_json::json!({})),
            })
        })
        .collect();

    let provider_request = clawdesk_providers::ProviderRequest {
        model: req.model.clone(),
        messages,
        system_prompt,
        max_tokens: req.max_output_tokens,
        temperature: req.temperature,
        tools,
        stream: false,
        images: vec![],
    };

    // Try to find a provider
    let providers = state.providers.load();
    let provider = providers
        .default_provider()
        .ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": {
                    "message": "No providers configured",
                    "type": "server_error"
                }
            })),
        ))?;

    match provider.complete(&provider_request).await {
        Ok(resp) => {
            let response_id = format!("resp_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
            let output_id = format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));

            let response = ResponseObject {
                id: response_id.clone(),
                object: "response",
                created_at: Utc::now().timestamp(),
                status: "completed",
                model: resp.model,
                output: vec![OutputItem {
                    item_type: "message".to_string(),
                    id: output_id,
                    role: Some("assistant".to_string()),
                    content: vec![OutputContent {
                        content_type: "output_text".to_string(),
                        text: resp.content,
                    }],
                    status: "completed".to_string(),
                }],
                usage: ResponseUsage {
                    input_tokens: resp.usage.input_tokens,
                    output_tokens: resp.usage.output_tokens,
                    total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
                },
                metadata: req.metadata,
            };

            // Store for retrieval
            if let Some(store) = state.response_store.as_ref() {
                store.write().await.insert(response_id, response.clone());
            }

            Ok(Json(response).into_response())
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": {
                    "message": e.to_string(),
                    "type": "provider_error"
                }
            })),
        )),
    }
}

/// POST /v1/responses (streaming) — Create a response with SSE streaming.
async fn create_response_streaming(
    state: Arc<GatewayState>,
    req: CreateResponseRequest,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let messages = convert_input_to_messages(&req);
    let system_prompt = req.instructions.clone();

    let tools: Vec<clawdesk_providers::ToolDefinition> = req
        .tools
        .iter()
        .filter(|t| t.tool_type == "function")
        .filter_map(|t| {
            Some(clawdesk_providers::ToolDefinition {
                name: t.name.clone()?,
                description: t.description.clone().unwrap_or_default(),
                parameters: t.parameters.clone().unwrap_or(serde_json::json!({})),
            })
        })
        .collect();

    let provider_request = clawdesk_providers::ProviderRequest {
        model: req.model.clone(),
        messages,
        system_prompt,
        max_tokens: req.max_output_tokens,
        temperature: req.temperature,
        tools,
        stream: true,
        images: vec![],
    };

    let providers = state.providers.load();
    let provider = providers.default_provider().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": { "message": "No providers configured", "type": "server_error" }
        })),
    ))?;

    let response_id = format!("resp_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    let output_id = format!("msg_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    let model = req.model.clone();

    // Channel for streaming chunks from provider
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<clawdesk_providers::StreamChunk>(64);

    // Spawn provider streaming in background
    let provider = Arc::clone(provider);
    tokio::spawn(async move {
        if let Err(e) = provider.stream(&provider_request, chunk_tx).await {
            tracing::error!("streaming error: {}", e);
        }
    });

    // Build SSE stream
    let stream = async_stream::stream! {
        // 1. response.created event
        let created_event = serde_json::json!({
            "type": "response.created",
            "response": {
                "id": &response_id,
                "object": "response",
                "status": "in_progress",
                "model": &model,
            }
        });
        yield Ok::<_, Infallible>(SseEvent::default().data(created_event.to_string()));

        // 2. output_item.added
        let item_event = serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": &output_id,
                "role": "assistant",
                "status": "in_progress",
            }
        });
        yield Ok(SseEvent::default().data(item_event.to_string()));

        // 3. Stream text deltas
        let mut full_text = String::new();
        while let Some(chunk) = chunk_rx.recv().await {
            if !chunk.delta.is_empty() {
                full_text.push_str(&chunk.delta);
                let delta_event = serde_json::json!({
                    "type": "response.output_text.delta",
                    "output_index": 0,
                    "content_index": 0,
                    "delta": chunk.delta,
                });
                yield Ok(SseEvent::default().data(delta_event.to_string()));
            }

            if chunk.done {
                // 4. output_text.done
                let text_done = serde_json::json!({
                    "type": "response.output_text.done",
                    "output_index": 0,
                    "content_index": 0,
                    "text": &full_text,
                });
                yield Ok(SseEvent::default().data(text_done.to_string()));

                // 5. response.completed
                let usage = chunk.usage.unwrap_or_default();
                let completed = serde_json::json!({
                    "type": "response.completed",
                    "response": {
                        "id": &response_id,
                        "object": "response",
                        "status": "completed",
                        "model": &model,
                        "usage": {
                            "input_tokens": usage.input_tokens,
                            "output_tokens": usage.output_tokens,
                            "total_tokens": usage.input_tokens + usage.output_tokens,
                        }
                    }
                });
                yield Ok(SseEvent::default().data(completed.to_string()));
                break;
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()).into_response())
}

/// GET /v1/responses/:id — Retrieve a previously created response.
pub async fn get_response(
    State(state): State<Arc<GatewayState>>,
    Path(response_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if let Some(store) = state.response_store.as_ref() {
        let map = store.read().await;
        if let Some(response) = map.get(&response_id) {
            return Ok(Json(response.clone()));
        }
    }

    Err((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": {
                "message": format!("Response '{}' not found", response_id),
                "type": "invalid_request_error"
            }
        })),
    ))
}

/// Convert Responses API input to provider chat messages.
fn convert_input_to_messages(
    req: &CreateResponseRequest,
) -> Vec<clawdesk_providers::ChatMessage> {
    match &req.input {
        ResponseInput::Text(text) => {
            vec![clawdesk_providers::ChatMessage::new(
                clawdesk_providers::MessageRole::User,
                text.as_str(),
            )]
        }
        ResponseInput::Items(items) => {
            items
                .iter()
                .filter_map(|item| {
                    let role = match item.role.as_deref() {
                        Some("user") => clawdesk_providers::MessageRole::User,
                        Some("assistant") => clawdesk_providers::MessageRole::Assistant,
                        Some("system") => clawdesk_providers::MessageRole::System,
                        _ => clawdesk_providers::MessageRole::User,
                    };

                    let content = match &item.content {
                        Some(InputContent::Text(t)) => t.clone(),
                        Some(InputContent::Parts(parts)) => {
                            parts
                                .iter()
                                .filter_map(|p| p.text.clone())
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                        None => return None,
                    };

                    Some(clawdesk_providers::ChatMessage::new(role, content.as_str()))
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_text_input() {
        let req = CreateResponseRequest {
            model: "test".into(),
            input: ResponseInput::Text("hello".into()),
            instructions: None,
            max_output_tokens: None,
            temperature: None,
            tools: vec![],
            stream: false,
            previous_response_id: None,
            metadata: None,
        };
        let messages = convert_input_to_messages(&req);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_ref(), "hello");
    }

    #[test]
    fn convert_items_input() {
        let req = CreateResponseRequest {
            model: "test".into(),
            input: ResponseInput::Items(vec![
                InputItem {
                    item_type: "message".into(),
                    role: Some("user".into()),
                    content: Some(InputContent::Text("hello".into())),
                    id: None,
                },
                InputItem {
                    item_type: "message".into(),
                    role: Some("assistant".into()),
                    content: Some(InputContent::Text("hi!".into())),
                    id: None,
                },
            ]),
            instructions: None,
            max_output_tokens: None,
            temperature: None,
            tools: vec![],
            stream: false,
            previous_response_id: None,
            metadata: None,
        };
        let messages = convert_input_to_messages(&req);
        assert_eq!(messages.len(), 2);
    }

    #[tokio::test]
    async fn response_store_roundtrip() {
        let store = new_response_store();
        let resp = ResponseObject {
            id: "resp_test123".into(),
            object: "response",
            created_at: 1000,
            status: "completed",
            model: "gpt-4o".into(),
            output: vec![],
            usage: ResponseUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
            metadata: None,
        };
        store.write().await.insert("resp_test123".into(), resp);
        let read = store.read().await;
        let fetched = read.get("resp_test123");
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().model, "gpt-4o");
    }
}
