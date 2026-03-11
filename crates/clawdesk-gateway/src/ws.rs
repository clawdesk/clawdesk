//! WebSocket handler for streaming agent responses.
//!
//! Per-connection session cache: session metadata is loaded once on first use
//! and cached for the lifetime of the WebSocket connection. User messages use
//! best-effort persistence through `DurableMessageWriter`, while assistant
//! responses use confirmed writes to prevent data loss on crash.

use crate::state::GatewayState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use clawdesk_providers::StreamChunk;
use clawdesk_runtime::DurableMessageWriter;
use clawdesk_storage::conversation_store::ConversationStore;
use clawdesk_sochdb::SochStore;
use clawdesk_storage::session_store::SessionStore;
use clawdesk_types::channel::ChannelId;
use clawdesk_types::session::{AgentMessage, Role, Session, SessionKey};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Incoming WebSocket message from a client.
#[derive(Debug, Deserialize)]
struct WsRequest {
    message: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    /// Optional agent ID to select a specific agent from the registry.
    #[serde(default)]
    agent_id: Option<String>,
}

/// Outgoing WebSocket message to a client.
#[derive(Debug, Serialize)]
struct WsResponse {
    #[serde(rename = "type")]
    msg_type: &'static str,
    content: String,
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    done: Option<bool>,
}

/// GET /ws — WebSocket upgrade handler.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

/// LRU session cache entry — wraps `Arc<Session>` with a per-entry dirty flag.
struct CacheEntry {
    session: Arc<Session>,
    dirty: bool,
}

/// Per-connection LRU session cache — `Arc<Session>` eliminates clones on read.
///
/// Capped at `MAX_CACHED_SESSIONS` entries. When full, the least-recently-used
/// (front of `order` VecDeque) entry is evicted — flushed to storage first if
/// dirty. Per-entry dirty tracking means `flush()` only writes modified entries
/// instead of the entire cache.
struct SessionCache {
    entries: HashMap<String, CacheEntry>,
    /// LRU order: front = oldest, back = most recently used.
    order: std::collections::VecDeque<String>,
}

/// Maximum number of sessions held in the per-connection LRU cache.
const MAX_CACHED_SESSIONS: usize = 32;

impl SessionCache {
    fn new() -> Self {
        Self {
            entries: HashMap::with_capacity(MAX_CACHED_SESSIONS),
            order: std::collections::VecDeque::with_capacity(MAX_CACHED_SESSIONS),
        }
    }

    /// Promote `session_id` to most-recently-used (back of `order`).
    fn touch(&mut self, session_id: &str) {
        if let Some(pos) = self.order.iter().position(|s| s == session_id) {
            self.order.remove(pos);
        }
        self.order.push_back(session_id.to_string());
    }

    /// Evict the least-recently-used entry, flushing to storage if dirty.
    async fn evict_lru(&mut self, store: &SochStore) {
        if let Some(evicted_id) = self.order.pop_front() {
            if let Some(entry) = self.entries.remove(&evicted_id) {
                if entry.dirty {
                    let key = SessionKey::from(evicted_id.clone());
                    if let Err(e) = store.save_session(&key, &entry.session).await {
                        warn!(session_id = %evicted_id, "LRU eviction flush failed: {e}");
                    }
                }
                debug!(session_id = %evicted_id, "evicted session from LRU cache");
            }
        }
    }

    /// Get or load a session. Returns `Arc<Session>` — zero-copy on cache hit.
    async fn get_or_load(
        &mut self,
        session_id: &str,
        session_key: &SessionKey,
        store: &SochStore,
        model: Option<&str>,
    ) -> Arc<Session> {
        if let Some(entry) = self.entries.get(session_id) {
            let arc = Arc::clone(&entry.session);
            self.touch(session_id);
            return arc;
        }

        // Evict LRU if at capacity before inserting.
        if self.entries.len() >= MAX_CACHED_SESSIONS {
            self.evict_lru(store).await;
        }

        let session = match store.load_session(session_key).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                let mut s = Session::new(session_key.clone(), ChannelId::Internal);
                if let Some(m) = model {
                    s.model = Some(m.to_string());
                }
                s
            }
            Err(e) => {
                warn!("failed to load session: {e}");
                Session::new(session_key.clone(), ChannelId::Internal)
            }
        };

        let arc = Arc::new(session);
        self.entries.insert(
            session_id.to_string(),
            CacheEntry {
                session: Arc::clone(&arc),
                dirty: false,
            },
        );
        self.touch(session_id);
        arc
    }

    /// Update cached session metadata via copy-on-write (`Arc::make_mut`).
    fn update(&mut self, session_id: &str, mutate: impl FnOnce(&mut Session)) {
        if let Some(entry) = self.entries.get_mut(session_id) {
            mutate(Arc::make_mut(&mut entry.session));
            entry.dirty = true;
            self.touch(session_id);
        }
    }

    /// Flush only dirty sessions to storage, then clear their dirty flags.
    async fn flush(&mut self, store: &SochStore) {
        for (id, entry) in self.entries.iter_mut() {
            if !entry.dirty {
                continue;
            }
            let key = SessionKey::from(id.clone());
            if let Err(e) = store.save_session(&key, &entry.session).await {
                warn!(session_id = %id, "session flush failed: {e}");
            } else {
                entry.dirty = false;
            }
        }
    }
}

async fn handle_ws(mut socket: WebSocket, state: Arc<GatewayState>) {
    info!("WebSocket client connected");

    let mut cache = SessionCache::new();
    let mut flush_interval = interval(Duration::from_secs(5));
    // Skip the first immediate tick.
    flush_interval.tick().await;

    // CancellationToken — cancelled when the WS loop exits so any
    // orphaned provider-stream tasks are cleaned up promptly.
    let cancel = CancellationToken::new();

    // Durable message writer — best-effort for user msgs, confirmed for assistant msgs.
    let writer = DurableMessageWriter::new(
        Arc::clone(&state.store) as Arc<dyn ConversationStore>,
        256,
    );

    loop {
        tokio::select! {
            // Periodic session metadata flush.
            _ = flush_interval.tick() => {
                cache.flush(&*state.store).await;
            }
            // Incoming WebSocket message.
            maybe_msg = socket.recv() => {
                let Some(msg) = maybe_msg else { break };
                match msg {
            Ok(Message::Text(text)) => {
                debug!(len = text.len(), "received WS message");

                let req = match serde_json::from_str::<WsRequest>(&text) {
                    Ok(r) => r,
                    Err(_) => WsRequest {
                        message: text.to_string(),
                        session_id: None,
                        model: None,
                        agent_id: None,
                    },
                };

                let session_id = req
                    .session_id
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let session_key = SessionKey::from(session_id.clone());

                // Load from cache (hits store only on first access per session_id).
                let _session = cache
                    .get_or_load(
                        &session_id,
                        &session_key,
                        &*state.store,
                        req.model.as_deref(),
                    )
                    .await;

                // Best-effort persist user message through durable writer.
                let user_msg = AgentMessage {
                    role: Role::User,
                    content: req.message.clone(),
                    timestamp: chrono::Utc::now(),
                    model: None,
                    token_count: None,
                    tool_call_id: None,
                    tool_name: None,
                };
                if let Err(e) = writer.append_best_effort(&session_key, &user_msg) {
                    warn!(%e, "user message best-effort write failed");
                }

                // Stream a "thinking" token.
                let thinking = WsResponse {
                    msg_type: "token",
                    content: "...".to_string(),
                    session_id: session_id.clone(),
                    done: Some(false),
                };
                if let Ok(json) = serde_json::to_string(&thinking) {
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }

                // Stream reply via the default provider.
                //
                // Loads the provider from state, creates a ProviderRequest from
                // session history, and streams chunks through the WebSocket.
                // Resolves the system prompt from the agent registry so the LLM
                // has proper identity and capabilities. Falls back to a
                // placeholder when no provider is configured.
                let reply_text = {
                    let history = state
                        .store
                        .load_history(&session_key, 50)
                        .await
                        .unwrap_or_default();

                    // Resolve system prompt from agent registry.
                    let system_prompt = {
                        let registry = state.agent_registry.load();
                        let snapshot = req.agent_id.as_deref()
                            .and_then(|id| registry.get(id))
                            .or_else(|| registry.get("default"))
                            .or_else(|| registry.values().next());
                        match snapshot {
                            Some(agent) => Some(agent.system_prompt.clone()),
                            None => Some(
                                clawdesk_types::session::DEFAULT_SYSTEM_PROMPT.to_string()
                            ),
                        }
                    };

                    let providers = state.providers.load();
                    if let Some(provider) = providers.default_provider() {
                        use clawdesk_providers::{
                            ChatMessage, MessageRole, ProviderRequest,
                        };

                        let messages: Vec<ChatMessage> = history
                            .iter()
                            .map(|m| ChatMessage {
                                role: match m.role {
                                    Role::User => MessageRole::User,
                                    Role::Assistant => MessageRole::Assistant,
                                    Role::System => MessageRole::System,
                                    _ => MessageRole::User,
                                },
                                content: std::sync::Arc::from(m.content.as_str()),
                                cached_tokens: None,
                            })
                            .collect();

                        let pr = ProviderRequest {
                            model: cache
                                .entries
                                .get(&session_id)
                                .and_then(|e| e.session.model.clone())
                                .unwrap_or_else(|| "default".to_string()),
                            messages,
                            system_prompt,
                            max_tokens: None,
                            temperature: None,
                            tools: vec![],
                            stream: true,
                        };

                        let (chunk_tx, mut chunk_rx) =
                            tokio::sync::mpsc::channel::<StreamChunk>(32);

                        // Spawn the provider stream with cancellation support.
                        // If the WS disconnects, the CancellationToken fires and
                        // the spawned task exits cleanly via `tokio::select!`.
                        let prov = provider.clone();
                        let stream_cancel = cancel.child_token();
                        tokio::spawn(async move {
                            tokio::select! {
                                _ = stream_cancel.cancelled() => {
                                    debug!("provider stream cancelled (WS disconnect)");
                                }
                                result = prov.stream(&pr, chunk_tx) => {
                                    if let Err(e) = result {
                                        warn!("stream error: {e}");
                                    }
                                }
                            }
                        });

                        // Forward chunks to the WebSocket as they arrive.
                        let mut full_content = String::new();
                        while let Some(chunk) = chunk_rx.recv().await {
                            full_content.push_str(&chunk.delta);
                            let resp = WsResponse {
                                msg_type: if chunk.done { "message" } else { "token" },
                                content: chunk.delta,
                                session_id: session_id.clone(),
                                done: Some(chunk.done),
                            };
                            if let Ok(json) = serde_json::to_string(&resp) {
                                if socket.send(Message::Text(json.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        full_content
                    } else {
                        // No provider configured — echo placeholder.
                        let reply = format!(
                            "Received: \"{}\". Session has {} message(s).",
                            req.message,
                            history.len()
                        );
                        let resp = WsResponse {
                            msg_type: "message",
                            content: reply.clone(),
                            session_id: session_id.clone(),
                            done: Some(true),
                        };
                        if let Ok(json) = serde_json::to_string(&resp) {
                            if socket.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                        reply
                    }
                };

                // Confirmed persist assistant message — blocks until committed.
                let model_clone = cache
                    .entries
                    .get(&session_id)
                    .and_then(|e| e.session.model.clone());
                let assistant_msg = AgentMessage {
                    role: Role::Assistant,
                    content: reply_text.clone(),
                    timestamp: chrono::Utc::now(),
                    model: model_clone,
                    token_count: None,
                    tool_call_id: None,
                    tool_name: None,
                };
                if let Err(e) = writer.append_confirmed(&session_key, &assistant_msg).await {
                    error!(%e, "assistant message confirmed write failed");
                }

                // Update cached session metadata (flushed periodically).
                cache.update(&session_id, |s| {
                    s.message_count += 2;
                    s.last_activity = chrono::Utc::now();
                });
            }
            Ok(Message::Ping(data)) => {
                if socket.send(Message::Pong(data)).await.is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) => {
                info!("WebSocket client disconnected");
                // Final flush before disconnect.
                cache.flush(&*state.store).await;
                return;
            }
            Ok(_) => {}
            Err(e) => {
                error!("WebSocket error: {e}");
                break;
            }
                }
            }
        }
    }

    // Cancel any in-flight provider streams, then flush remaining dirty sessions.
    cancel.cancel();
    cache.flush(&*state.store).await;
}
