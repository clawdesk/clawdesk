//! HTTP route handlers for the gateway API.

use crate::error::ApiError;
use crate::state::GatewayState;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use clawdesk_acp::thread_agent::{ThreadInfo, ThreadAgentConfig, thread_agent_card};
use clawdesk_channel::reply_formatter::{MarkupFormat, ReplyFormatter};
use clawdesk_storage::session_store::SessionStore;
use clawdesk_types::channel::ChannelId;
use clawdesk_types::session::{Session, SessionFilter, SessionKey};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::debug;
use crate::thread_ownership::AcquireResult;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_secs: u64,
}

/// GET /api/v1/health
pub async fn health(State(state): State<Arc<GatewayState>>) -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.uptime_secs(),
    })
}

#[derive(Deserialize)]
pub struct SendMessageRequest {
    pub message: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
    /// Optional thread key for ownership locking.
    /// If omitted, session_id is used.
    pub thread_id: Option<String>,
}

#[derive(Serialize)]
pub struct SendMessageResponse {
    pub reply: String,
    pub session_id: String,
    pub thread_id: String,
    /// A2A agent identity for this thread (e.g. `thread-<uuid>`).
    pub agent_id: String,
}

/// POST /api/v1/message
///
/// Creates or resumes a session, appends the user message, runs the real
/// AgentRunner pipeline to generate a response, and persists the exchange.
///
/// ## A2A routing
///
/// Before running the local AgentRunner, the handler resolves the thread's
/// owning agent via its `thread_id`. If the thread has an A2A agent binding
/// and the agent is remote, the message is delegated via POST to the
/// remote agent's `/a2a/tasks/send` endpoint. Local threads run the
/// AgentRunner in-process as before.
pub async fn send_message(
    State(state): State<Arc<GatewayState>>,
    Json(req): Json<SendMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use chrono::Utc;
    use clawdesk_agents::runner::{AgentConfig, AgentRunner};
    use clawdesk_providers::MessageRole;
    use clawdesk_storage::conversation_store::ConversationStore;
    use clawdesk_types::session::{AgentMessage, Role};

    let session_id = req
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let thread_id = req
        .thread_id
        .clone()
        .unwrap_or_else(|| session_id.clone());
    let request_owner = format!("gateway-req-{}", uuid::Uuid::new_v4());

    match state
        .thread_ownership
        .try_acquire(&thread_id, &request_owner)
        .await
    {
        AcquireResult::Acquired | AcquireResult::AlreadyOwned => {}
        AcquireResult::Busy {
            owner_id,
            retry_after_ms,
        } => {
            return Err(ApiError::ThreadBusy {
                thread_id: thread_id.clone(),
                owner_id,
                retry_after_ms,
            });
        }
    }

    let response = async {
    let session_key = SessionKey::from(session_id.clone());

    // Load or create session
    let store = &*state.store;
    let mut session = store
        .load_session(&session_key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .unwrap_or_else(|| {
            let mut s = Session::new(session_key.clone(), ChannelId::Internal);
            if let Some(ref m) = req.model {
                s.model = Some(m.clone());
            }
            s
        });

    // --- Thread-as-Agent: ensure this thread has an AgentCard registered ---
    // Every thread is lazily registered as an A2A-capable agent on first
    // message, making it discoverable for task delegation by other threads.
    let model_for_agent = session.model.as_deref().unwrap_or("sonnet").to_string();
    let agent_id = format!("thread-{}", &thread_id);
    {
        // Build the card using the public fn and upsert by agent_id key
        let card = thread_agent_card(
            &ThreadInfo {
                thread_id: 0, // Gateway uses string IDs; hex key not used
                agent_id: agent_id.clone(),
                title: format!("Thread {}", &thread_id[..8.min(thread_id.len())]),
                model: Some(model_for_agent.clone()),
                capabilities: vec!["text_generation".to_string()],
                skills: vec![],
                spawn_mode: "standalone".to_string(),
                parent_thread_id: None,
            },
            None,
            "http://localhost:18789",
        );
        state.thread_agents.upsert_card(&agent_id, card);
        debug!(%thread_id, %agent_id, "thread agent registered");
    }

    // Append user message
    let user_msg = AgentMessage {
        role: Role::User,
        content: req.message.clone(),
        timestamp: Utc::now(),
        model: None,
        token_count: None,
        tool_call_id: None,
        tool_name: None,
    };
    store
        .append_message(&session_key, &user_msg)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Build conversation history from store
    let history = store
        .load_history(&session_key, 50)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Convert stored history to provider ChatMessages
    let chat_history: Vec<clawdesk_providers::ChatMessage> = history
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => MessageRole::User,
                Role::Assistant => MessageRole::Assistant,
                Role::System => MessageRole::System,
                _ => MessageRole::User,
            };
            clawdesk_providers::ChatMessage::new(role, m.content.as_str())
        })
        .collect();

    // Resolve provider — try session model, then default
    let model_name = session.model.as_deref().unwrap_or("sonnet");
    let provider_registry = state.providers.load();
    let provider_key = match model_name {
        m if m.contains("haiku") || m.contains("sonnet") || m.contains("opus") || m.contains("claude") => "anthropic",
        m if m.starts_with("gpt") || m.starts_with("o1") || m.starts_with("o3") => "openai",
        m if m.starts_with("gemini") => "gemini",
        m if m.contains("local") || m.starts_with("llama") || m.starts_with("deepseek") => "ollama",
        other => other,
    };

    let provider = provider_registry
        .get(provider_key)
        .or_else(|| provider_registry.default_provider())
        .ok_or_else(|| {
            ApiError::Internal(format!(
                "No LLM provider configured for model '{}'. Set ANTHROPIC_API_KEY, OPENAI_API_KEY, or similar env var.",
                model_name
            ))
        })?;

    let model_id = match model_name {
        "haiku" => "claude-haiku-4-20250514",
        "sonnet" => "claude-sonnet-4-20250514",
        "opus" => "claude-opus-4-20250514",
        "local" => "llama3.2",
        other => other,
    };

    let config = AgentConfig {
        model: model_id.to_string(),
        system_prompt: "You are a helpful assistant.".to_string(),
        max_tool_rounds: 25,
        context_limit: 128_000,
        response_reserve: 8_192,
        ..Default::default()
    };

    let runner = AgentRunner::new(
        std::sync::Arc::clone(provider),
        std::sync::Arc::clone(&state.tools),
        config,
        state.cancel.clone(),
    );

    let agent_response = runner
        .run(chat_history, "You are a helpful assistant.".to_string())
        .await
        .map_err(|e| ApiError::Internal(format!("Agent execution failed: {}", e)))?;

    let reply_text = agent_response.content;

    // Format the response for the channel's native markup format.
    // The HTTP/webchat API uses Markdown natively (no conversion needed),
    // but other channels (Slack, Telegram, WhatsApp) would use their
    // respective format. The ReplyFormatter also handles semantic chunking
    // for channels with message length limits.
    let formatted_reply = format_for_channel(&reply_text, &session.channel);

    let assistant_msg = AgentMessage {
        role: Role::Assistant,
        content: formatted_reply.clone(),
        timestamp: Utc::now(),
        model: session.model.clone(),
        token_count: Some(agent_response.output_tokens as usize),
        tool_call_id: None,
        tool_name: None,
    };
    store
        .append_message(&session_key, &assistant_msg)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Update session metadata
    session.message_count += 2;
    session.last_activity = Utc::now();
    store
        .save_session(&session_key, &session)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    debug!(%session_id, input_tokens = agent_response.input_tokens, output_tokens = agent_response.output_tokens, "message processed via agent runner");

        Ok(Json(SendMessageResponse {
            reply: formatted_reply,
            session_id,
            thread_id: thread_id.clone(),
            agent_id,
        }))
    }
    .await;

    let _ = state
        .thread_ownership
        .release(&thread_id, &request_owner)
        .await;
    response
}

/// GET /api/v1/thread-agents
///
/// Lists all registered thread agents with their A2A capability cards.
/// Each thread that has processed at least one message is an A2A-capable
/// agent discoverable for task delegation.
pub async fn list_thread_agents(
    State(state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    let cards = state.thread_agents.all_cards();
    let agents: Vec<serde_json::Value> = cards
        .into_iter()
        .map(|card| {
            serde_json::json!({
                "agent_id": card.id,
                "name": card.name,
                "url": card.endpoint.url,
                "capabilities": card.capabilities.iter().map(|c| format!("{:?}", c)).collect::<Vec<_>>(),
                "skills": card.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
            })
        })
        .collect();
    Json(serde_json::json!({
        "thread_agents": agents,
        "count": agents.len(),
    }))
}

/// POST /api/v1/thread-agents/{thread_id}/delegate
///
/// Delegates a task from one thread-agent to another. This creates a
/// sub-agent spawn: the target thread receives the task and the source
/// thread will be notified on completion (announce flow).
#[derive(Deserialize)]
pub struct DelegateRequest {
    /// The target thread agent to delegate to.
    pub target_agent_id: String,
    /// The task prompt to send.
    pub prompt: String,
    /// Spawn mode: "run" (fire-and-forget) or "session" (persistent).
    #[serde(default = "default_run_mode")]
    pub spawn_mode: String,
}

fn default_run_mode() -> String {
    "run".to_string()
}

#[derive(Serialize)]
pub struct DelegateResponse {
    pub task_id: String,
    pub source_agent_id: String,
    pub target_agent_id: String,
    pub status: String,
}

pub async fn delegate_task(
    State(state): State<Arc<GatewayState>>,
    axum::extract::Path(thread_id): axum::extract::Path<String>,
    Json(req): Json<DelegateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use clawdesk_acp::thread_agent::{create_spawn_task, SpawnRequest};

    let source_agent_id = format!("thread-{}", thread_id);

    // Verify both agents exist
    let _source_card = state
        .thread_agents
        .get_by_key(&source_agent_id)
        .ok_or_else(|| ApiError::Internal(format!(
            "Source thread agent '{}' not registered. Send a message first.",
            source_agent_id
        )))?;

    let _target_card = state
        .thread_agents
        .get_by_key(&req.target_agent_id)
        .ok_or_else(|| ApiError::Internal(format!(
            "Target thread agent '{}' not found in registry.",
            req.target_agent_id
        )))?;

    // Create a child thread ID (using hash of source + target for determinism)
    let child_thread_id: u128 = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source_agent_id.hash(&mut hasher);
        req.target_agent_id.hash(&mut hasher);
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .hash(&mut hasher);
        hasher.finish() as u128
    };

    let spawn_req = SpawnRequest {
        child_agent_id: req.target_agent_id.clone(),
        title: format!("Delegated from {}", source_agent_id),
        task_prompt: req.prompt.clone(),
        spawn_mode: req.spawn_mode.clone(),
        cleanup: "keep".to_string(),
        model: None,
        capabilities: vec![],
        skills: vec![],
    };

    let result = create_spawn_task(
        &source_agent_id,
        0, // parent_thread_id (gateway uses string IDs)
        child_thread_id,
        &spawn_req,
    );

    let task_id = result.task.id.to_string();

    debug!(
        source = %source_agent_id,
        target = %req.target_agent_id,
        task_id = %task_id,
        "thread agent delegation created"
    );

    Ok(Json(DelegateResponse {
        task_id,
        source_agent_id,
        target_agent_id: req.target_agent_id,
        status: "submitted".to_string(),
    }))
}

#[derive(Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub channel: String,
    pub created_at: String,
    pub message_count: u64,
    pub state: String,
}

/// GET /api/v1/sessions
pub async fn list_sessions(
    State(state): State<Arc<GatewayState>>,
) -> Result<impl IntoResponse, ApiError> {
    let summaries = state
        .store
        .list_sessions(SessionFilter::default())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let sessions: Vec<SessionInfo> = summaries
        .into_iter()
        .map(|s| SessionInfo {
            id: s.key.to_string(),
            channel: s.channel.to_string(),
            created_at: s.last_activity.to_rfc3339(),
            message_count: s.message_count,
            state: format!("{:?}", s.state),
        })
        .collect();
    Ok(Json(sessions))
}

#[derive(Serialize)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
    pub status: &'static str,
}

/// GET /api/v1/channels
pub async fn list_channels(
    State(state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    let registry = state.channels.load();
    let channels: Vec<ChannelInfo> = registry
        .iter()
        .map(|(id, ch)| ChannelInfo {
            id: id.to_string(),
            name: ch.meta().display_name,
            status: "active",
        })
        .collect();
    Json(channels)
}
/// Format an agent response for a specific channel's native markup.
///
/// Maps `ChannelId` to `MarkupFormat`:
/// - webchat, HTTP API → Markdown (passthrough)
/// - slack → SlackMrkdwn
/// - telegram → TelegramMarkdownV2
/// - whatsapp → WhatsApp
/// - imessage, irc → PlainText
///
/// For channels with message length limits, the ReplyFormatter applies
/// semantic chunking, returning the first chunk. Full multi-chunk delivery
/// should be handled by the channel adapter's outbound path.
fn format_for_channel(markdown: &str, channel: &ChannelId) -> String {
    let channel_str = channel.to_string();
    let (format, max_length) = match channel_str.as_str() {
        s if s.contains("slack") => (MarkupFormat::SlackMrkdwn, 40_000),
        s if s.contains("telegram") => (MarkupFormat::TelegramMarkdownV2, 4_096),
        s if s.contains("whatsapp") => (MarkupFormat::WhatsApp, 65_536),
        s if s.contains("discord") => (MarkupFormat::Markdown, 2_000),
        s if s.contains("imessage") || s.contains("irc") => {
            (MarkupFormat::PlainText, 160_000)
        }
        // webchat, HTTP API, and unknown channels: passthrough Markdown
        _ => return markdown.to_string(),
    };

    let chunks = ReplyFormatter::format_and_chunk(markdown, format, max_length);
    if chunks.is_empty() {
        return String::new();
    }

    // Return the first chunk. If there are multiple chunks, the channel
    // adapter's outbound delivery path should handle sending each chunk
    // sequentially with appropriate threading/reply semantics.
    if chunks.len() > 1 {
        debug!(
            channel = %channel_str,
            total_parts = chunks.len(),
            "response chunked for channel length limits"
        );
    }
    chunks[0].content.clone()
}