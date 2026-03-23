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
use tracing::{debug, warn};
use crate::thread_ownership::AcquireResult;

// ── Security gate adapters ─────────────────────────────────────────────────
// Bridge clawdesk-security (concrete) → clawdesk-agents (trait), respecting
// hexagonal dependency inversion. Equivalent to Tauri's adapters in commands.rs.

/// Sandbox policy gate — blocks tools whose required isolation level exceeds
/// what the platform provides.
struct GatewaySandboxGateAdapter {
    engine: Arc<clawdesk_security::sandbox_policy::SandboxPolicyEngine>,
}

#[async_trait::async_trait]
impl clawdesk_agents::runner::SandboxGate for GatewaySandboxGateAdapter {
    fn check_policy(&self, tool_name: &str) -> Result<(), String> {
        use clawdesk_security::sandbox_policy::SandboxDecision;
        match self.engine.decide(tool_name) {
            SandboxDecision::Allow { .. } => Ok(()),
            SandboxDecision::Block { required, available, tool_name } => {
                Err(format!(
                    "tool '{}' requires {} isolation but platform only supports {}",
                    tool_name, required, available
                ))
            }
        }
    }
}

/// Egress gate — per-endpoint network access control with SSRF prevention.
struct GatewayEgressGateAdapter {
    policy: Arc<std::sync::RwLock<clawdesk_security::NetworkEgressPolicy>>,
}

#[async_trait::async_trait]
impl clawdesk_agents::runner::EgressGate for GatewayEgressGateAdapter {
    fn check_egress(
        &self,
        tool_name: &str,
        host: &str,
        port: u16,
        method: Option<&str>,
        is_tls: bool,
    ) -> Result<bool, String> {
        let policy = self.policy.read().map_err(|e| format!("egress policy lock poisoned: {e}"))?;
        let http_method = method.and_then(clawdesk_security::HttpMethod::from_str_loose);
        match policy.evaluate(tool_name, host, port, http_method, is_tls) {
            clawdesk_security::EgressDecision::Allow { .. } => Ok(true),
            clawdesk_security::EgressDecision::RequireApproval { reason, .. } => {
                warn!(tool_name, host, port, %reason, "Egress requires approval (gateway: auto-deny)");
                Ok(false)
            }
            clawdesk_security::EgressDecision::Deny { reason } => {
                warn!(tool_name, host, port, %reason, "Egress denied");
                Err(reason)
            }
        }
    }
}

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
    /// Optional agent ID to select a specific agent from the registry.
    /// If omitted, falls back to the "default" agent or the first available agent.
    pub agent_id: Option<String>,
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

    // ── Security scan on user input (parity with Tauri CascadeScanner) ──
    let scan = state.scanner.scan(&req.message);
    if !scan.passed {
        let critical: Vec<String> = scan
            .findings
            .iter()
            .filter(|f| f.severity == clawdesk_types::security::Severity::Critical
                     || f.severity == clawdesk_types::security::Severity::High)
            .map(|f| format!("{}: {}", f.rule, f.description))
            .collect();
        if !critical.is_empty() {
            warn!(findings = ?critical, "Gateway message blocked by security scanner");
            return Err(ApiError::Forbidden(
                format!("Message blocked by security scanner: {}", critical.join("; "))
            ));
        }
    }

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
    let model_for_agent = session.model.as_deref().unwrap_or("default").to_string();
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

    // ── Task-Driven Agent Discovery ─────────────────────────────────
    // Instead of the old static agent_id lookup, we now do task-based
    // discovery: classify the user's message into domains, then find the
    // best matching agent from the registry using tags + capabilities.
    //
    // Priority chain (most specific → least specific):
    // 1. Explicit agent_id in request → direct lookup (user override)
    // 2. Task classification → match against agent tags/descriptions
    // 3. "default" agent → named fallback
    // 4. Hardcoded default → last resort
    let (agent_persona, agent_model_override) = {
        let registry = state.agent_registry.load();

        // Priority 1: Explicit agent_id (user knows what they want)
        let explicit = req.agent_id.as_deref()
            .and_then(|id| registry.get(id));

        let snapshot = if let Some(agent) = explicit {
            debug!(agent_id = %agent.id, selection = "explicit", "Agent selected by ID");
            Some(agent)
        } else if !registry.is_empty() {
            // Priority 2: Task-based discovery from the user's message
            let user_text = &req.message;

            let domains = clawdesk_agents::auto_compose::classify_task(user_text);
            debug!(?domains, "Task domains classified from message");

            // Score each agent by tag overlap with classified domains
            let domain_tags: Vec<&str> = domains.iter().map(|d| match d {
                clawdesk_agents::auto_compose::TaskDomain::Coding => "coding",
                clawdesk_agents::auto_compose::TaskDomain::Research => "research",
                clawdesk_agents::auto_compose::TaskDomain::Writing => "writing",
                clawdesk_agents::auto_compose::TaskDomain::DataAnalysis => "data",
                clawdesk_agents::auto_compose::TaskDomain::Design => "design",
                clawdesk_agents::auto_compose::TaskDomain::DevOps => "devops",
                clawdesk_agents::auto_compose::TaskDomain::Security => "security",
                clawdesk_agents::auto_compose::TaskDomain::Testing => "testing",
                clawdesk_agents::auto_compose::TaskDomain::General => "general",
            }).collect();

            // Find best matching agent: score = number of matching domain keywords
            // in the agent's system_prompt or id. Simple keyword overlap for now;
            // can be upgraded to embedding-based semantic matching later.
            let best = registry.values()
                .filter(|a| a.id != "default") // Don't match "default" by content
                .max_by_key(|agent| {
                    let id_lower = agent.id.to_lowercase();
                    let prompt_lower = agent.system_prompt.to_lowercase();
                    domain_tags.iter()
                        .filter(|&&tag| id_lower.contains(tag) || prompt_lower.contains(tag))
                        .count()
                })
                .filter(|agent| {
                    // Only use if at least one domain tag matches
                    let id_lower = agent.id.to_lowercase();
                    let prompt_lower = agent.system_prompt.to_lowercase();
                    domain_tags.iter().any(|&tag| id_lower.contains(tag) || prompt_lower.contains(tag))
                });

            if let Some(agent) = best {
                debug!(agent_id = %agent.id, selection = "task_match", ?domains, "Agent selected by task classification");
                Some(agent)
            } else {
                // Priority 3: Named "default" fallback
                let default = registry.get("default")
                    .or_else(|| registry.values().next());
                if let Some(agent) = default {
                    debug!(agent_id = %agent.id, selection = "fallback", "Agent selected as fallback");
                }
                default
            }
        } else {
            None
        };

        match snapshot {
            Some(agent) => {
                (agent.system_prompt.clone(), Some(agent.model.clone()))
            }
            None => {
                debug!("No agents in registry — using default system prompt");
                (clawdesk_types::session::DEFAULT_SYSTEM_PROMPT.to_string(), None)
            }
        }
    };

    // Determine effective model: request model → session model → agent model → default
    let effective_model_name = if let Some(ref m) = req.model {
        m.as_str()
    } else if let Some(ref m) = session.model {
        m.as_str()
    } else if let Some(ref m) = agent_model_override {
        if !m.is_empty() { m.as_str() } else { "default" }
    } else {
        "default"
    };

    // Resolve model alias to full model ID.
    // "default" is a sentinel: provider.complete() will use its configured default model.
    let effective_model_id = match effective_model_name {
        "haiku" => "claude-haiku-4-20250514".to_string(),
        "sonnet" => "claude-sonnet-4-20250514".to_string(),
        "opus" => "claude-opus-4-20250514".to_string(),
        "local" => "llama3.2".to_string(),
        other => other.to_string(),
    };

    // Resolve provider based on the effective model
    let provider_registry = state.providers.load();

    // First, try to find a provider that explicitly lists this model.
    // This handles Ollama models (qwen2.5:3b, etc.) and any provider
    // that reports its available models via Provider::models().
    let provider = provider_registry.list().iter()
        .filter_map(|name| provider_registry.get(name))
        .find(|p| p.models().iter().any(|m| m == effective_model_name))
        .or_else(|| {
            // Second, use pattern matching for well-known provider families.
            let provider_key = match effective_model_name {
                m if m.contains("haiku") || m.contains("sonnet") || m.contains("opus") || m.contains("claude") => "anthropic",
                m if m.starts_with("gpt") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") => "openai",
                m if m.starts_with("gemini") => "gemini",
                m if m.contains("qwen") || m.starts_with("llama") || m.starts_with("deepseek")
                     || m.starts_with("mistral") || m.starts_with("phi") || m.starts_with("gemma")
                     || m.contains(":") => "ollama",
                m if m.contains("grok") => "local_compatible",
                _ => "",
            };
            if !provider_key.is_empty() {
                provider_registry.get(provider_key)
            } else {
                None
            }
        })
        // Third, try local_compatible (OpenAI-compatible server), then default.
        .or_else(|| provider_registry.get("local_compatible"))
        .or_else(|| provider_registry.default_provider())
        .ok_or_else(|| {
            ApiError::Internal(format!(
                "No LLM provider configured for model '{}'. Set ANTHROPIC_API_KEY, OPENAI_API_KEY, or similar env var.",
                effective_model_name
            ))
        })?;

    // ── Unified Prompt Pipeline ──────────────────────────────────────
    // Build the system prompt using the same pipeline as the Tauri desktop:
    // 1. Score skills (trigger evaluation + memory signal boost)
    // 2. PromptBuilder knapsack (identity + safety + runtime + skills)
    // This ensures gateway agents get the same skill-enriched prompts as
    // desktop agents, not just the raw persona string.
    let system_prompt = {
        use clawdesk_domain::prompt_builder::{PromptBudget, PromptBuilder, RuntimeContext, ScoredSkill};
        use clawdesk_types::tokenizer::estimate_tokens;

        // Score active skills for this request
        let scored_skills: Vec<ScoredSkill> = {
            let registry = state.skills.load();
            let active = registry.active_skills();
            active.iter().map(|s| {
                ScoredSkill {
                    skill_id: s.manifest.id.as_str().to_string(),
                    display_name: s.manifest.display_name.clone(),
                    prompt_fragment: s.prompt_fragment.clone(),
                    token_cost: estimate_tokens(&s.prompt_fragment),
                    priority_weight: s.manifest.priority_weight,
                    relevance: 1.0, // Gateway doesn't have trigger context yet
                }
            }).collect()
        };

        let budget = PromptBudget {
            total: 128_000,
            response_reserve: 8_192,
            identity_cap: 2_000,
            skills_cap: 6_144, // Increased from 4K: 90+ skills need more budget for knapsack
            memory_cap: 4_096,
            history_floor: 2_000,
            runtime_cap: 512,
            safety_cap: 1_024,
        };

        let runtime_ctx = RuntimeContext {
            datetime: chrono::Utc::now().to_rfc3339(),
            channel_description: Some("HTTP API gateway".to_string()),
            model_name: Some(effective_model_id.clone()),
            metadata: vec![],
            available_channels: {
                let ch = state.channels.load();
                ch.list().iter().map(|id| format!("{:?}", id).to_lowercase()).collect()
            },
        };

        match PromptBuilder::new(budget) {
            Ok(builder) => {
                let (assembled, _manifest) = builder
                    .identity(agent_persona.clone())
                    .runtime(runtime_ctx)
                    .skills(scored_skills)
                    .build();
                assembled.text
            }
            Err(_) => agent_persona.clone(),
        }
    };

    let config = AgentConfig {
        model: effective_model_id,
        system_prompt: system_prompt.clone(),
        max_tool_rounds: 25,
        context_limit: 128_000,
        response_reserve: 8_192,
        ..Default::default()
    };

    // Load active skills from the hot-swappable registry and build a
    // SkillProvider so the AgentRunner can do per-turn skill selection
    // (trigger evaluation + token-budgeted knapsack). Without this, the
    // gateway agents have no skills — they can't use domain-specific prompts.
    let skill_provider: Option<std::sync::Arc<dyn clawdesk_agents::runner::SkillProvider>> = {
        let registry = state.skills.load();
        let active = registry.active_skills();
        if active.is_empty() {
            None
        } else {
            use clawdesk_skills::env_injection::EnvResolver;
            use clawdesk_skills::orchestrator::SkillOrchestrator;
            use clawdesk_skills::skill_provider::OrchestratorSkillProvider;

            let orchestrator = SkillOrchestrator::new(active, 8_000);
            let env_resolver = EnvResolver::default();
            Some(std::sync::Arc::new(OrchestratorSkillProvider::new(
                orchestrator,
                env_resolver,
            )))
        }
    };

    let mut builder = AgentRunner::builder(
        std::sync::Arc::clone(provider),
        std::sync::Arc::clone(&state.tools),
        config,
        state.cancel.clone(),
    )
    // Wire sandbox + egress gates (parity with Tauri desktop path).
    .with_sandbox_gate(Arc::new(GatewaySandboxGateAdapter {
        engine: Arc::clone(&state.sandbox_engine),
    }))
    .with_egress_gate(Arc::new(GatewayEgressGateAdapter {
        policy: Arc::clone(&state.egress_policy),
    }))
    .with_hook_manager(std::sync::Arc::clone(&state.hook_manager))
    .with_session_context(session_id.clone(), agent_id.clone())
    .with_event_bus(Arc::clone(&state.event_bus));

    if let Some(sp) = skill_provider {
        builder = builder.with_skill_provider(sp);
    }

    // Wire cross-session memory recall if memory subsystem is available.
    if let Some(ref mem) = state.memory {
        let mem = std::sync::Arc::clone(mem);
        let recall_fn: clawdesk_agents::MemoryRecallFn = std::sync::Arc::new(move |query: String| {
            let mem = std::sync::Arc::clone(&mem);
            Box::pin(async move {
                match mem.recall(&query, Some(10)).await {
                    Ok(results) => results
                        .into_iter()
                        .filter_map(|r| {
                            let content = r.content?;
                            Some(clawdesk_agents::MemoryRecallResult {
                                content,
                                relevance: r.score as f64,
                                source: Some("memory".to_string()),
                            })
                        })
                        .collect(),
                    Err(_) => Vec::new(),
                }
            })
        });
        builder = builder.with_memory_recall(recall_fn);
    }

    let runner = builder.build();

    let agent_response = runner
        .run(chat_history, system_prompt)
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

    // ── Auto-reply to originating channel ────────────────────
    // If the session belongs to a non-internal channel (Telegram, Slack, etc.),
    // deliver the reply directly via Channel::send(). HTTP/WebChat callers
    // consume the JSON response directly; external channels need active push.
    if session.channel != ChannelId::Internal && session.channel != ChannelId::WebChat {
        let channels = state.channels.load();
        if let Some(channel) = channels.get(&session.channel) {
            use clawdesk_types::message::{MessageOrigin, OutboundMessage};
            let origin = match session.channel {
                ChannelId::Telegram => {
                    // Extract chat_id from the session identifier
                    let chat_id = session.key.identifier().parse::<i64>().unwrap_or(0);
                    MessageOrigin::Telegram {
                        chat_id,
                        message_id: 0,
                        thread_id: None,
                    }
                }
                ChannelId::Slack => MessageOrigin::Slack {
                    team_id: String::new(),
                    channel_id: session.key.identifier().to_string(),
                    user_id: String::new(),
                    ts: String::new(),
                    thread_ts: None,
                },
                ChannelId::Discord => MessageOrigin::Discord {
                    guild_id: 0,
                    channel_id: session.key.identifier().parse::<u64>().unwrap_or(0),
                    message_id: 0,
                    is_dm: true,
                    thread_id: None,
                },
                _ => MessageOrigin::Internal {
                    source: "gateway".to_string(),
                },
            };

            let outbound = OutboundMessage {
                origin,
                body: formatted_reply.clone(),
                media: Vec::new(),
                reply_to: None,
                thread_id: None,
            };

            if let Err(e) = channel.send(outbound).await {
                warn!(
                    channel = %session.channel,
                    error = %e,
                    "Failed to auto-reply to originating channel"
                );
            } else {
                debug!(channel = %session.channel, "Auto-reply delivered to channel");
            }
        }
    }

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
                "capabilities": card.capabilities().iter().map(|c| format!("{:?}", c)).collect::<Vec<_>>(),
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
    use clawdesk_acp::task::{SpawnMode, CleanupPolicy};

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
        spawn_mode: SpawnMode::from(req.spawn_mode.as_str()),
        cleanup: CleanupPolicy::Keep,
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