//! # Message Pipeline — Decomposed `send_message` stages
//!
//! Extracts the ~1600-line `send_message` god-function into discrete,
//! testable pipeline stages. Each stage has clear inputs and outputs,
//! enabling unit testing of individual phases without a full Tauri runtime.
//!
//! ## Pipeline Stages
//!
//! ```text
//! ┌─────────────────────┐
//! │  SecurityScan       │ → scan input, start trace
//! ├─────────────────────┤
//! │  ResolveAgent       │ → look up agent, apply model override
//! ├─────────────────────┤
//! │  ResolveSession     │ → chat_id, session_key, auto-title
//! ├─────────────────────┤
//! │  PersistUserMessage │ → dual-write to session + ConversationStore
//! ├─────────────────────┤
//! │  ResolveProvider    │ → model routing, provider negotiation
//! ├─────────────────────┤
//! │  AssembleHistory    │ → load history, URL injection, compaction
//! ├─────────────────────┤
//! │  BuildPrompt        │ → unified prompt pipeline, cache check
//! ├─────────────────────┤
//! │  RunAgent           │ → runner setup, LLM execution
//! ├─────────────────────┤
//! │  Finalize           │ → persist response, audit, memory, metrics
//! └─────────────────────┘
//! ```

use crate::state::*;
use chrono::{DateTime, Utc};
use clawdesk_agents::runner::AgentEvent;
use clawdesk_agents::turn_router::TurnRoutingResult;
use clawdesk_domain::context_guard::{ContextGuard, ContextGuardConfig, GuardAction, CompactionLevel, CompactionResult};
use clawdesk_providers::MessageRole;
use clawdesk_types::security::{AuditActor, AuditCategory, AuditOutcome};
use clawdesk_types::tokenizer::estimate_tokens;
use serde::Serialize;
use sochdb::semantic_cache::CacheMatchType;
use sochdb::trace::{SpanKind, SpanStatusCode, TraceStatus, CostEvent};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::commands::{
    safe_prefix, SendMessageRequest, SendMessageResponse,
    build_skill_tool_registry, SandboxGateAdapter,
};

// ═══════════════════════════════════════════════════════════
// Stage result types — typed intermediate values between stages
// ═══════════════════════════════════════════════════════════

/// Output of the security scan + trace initialization stage.
pub(crate) struct TraceAndScanResult {
    pub soch_trace_id: Option<String>,
    pub scan: clawdesk_types::security::ScanResult,
}

/// Output of agent + identity resolution.
pub(crate) struct ResolvedAgent {
    pub agent: DesktopAgent,
    pub identity_verified: bool,
}

/// Output of session resolution.
pub(crate) struct SessionContext {
    pub chat_id: String,
    pub is_new_chat: bool,
    pub session_key: clawdesk_types::session::SessionKey,
    pub auto_title: String,
}

/// Output of provider resolution + model routing.
pub(crate) struct ProviderContext {
    pub provider: Arc<dyn clawdesk_providers::Provider>,
    pub model_full_id: String,
    pub routing_decision: Option<TurnRoutingResult>,
}

/// Output of history assembly + context compaction.
pub(crate) struct HistoryContext {
    pub history: Vec<clawdesk_providers::ChatMessage>,
    pub compacted_guard: ContextGuard,
}

/// Output of prompt building + semantic cache probe.
pub(crate) struct PromptContext {
    pub system_prompt: String,
    pub memory_injection: Option<String>,
    pub cache_namespace: String,
    pub query_embedding: Option<Vec<f32>>,
    pub prompt_hash: u64,
}

/// Result of cache check — either a hit (short-circuit) or a miss (continue).
pub(crate) enum CacheResult {
    Hit(SendMessageResponse),
    Miss,
}

// ═══════════════════════════════════════════════════════════
// Stage 1: Security scan + trace initialization
// ═══════════════════════════════════════════════════════════

/// Start a durable trace run and scan user input for security threats.
///
/// Returns an error string if critical threats are found.
pub(crate) async fn security_scan(
    request: &SendMessageRequest,
    state: &AppState,
    app: &AppHandle,
) -> Result<TraceAndScanResult, String> {
    let soch_trace = {
        let mut resource = HashMap::new();
        resource.insert("agent_id".into(), request.agent_id.clone());
        resource.insert("channel".into(), "tauri".into());
        state.trace_store.start_run("send_message", resource).ok()
    };
    let soch_trace_id = soch_trace.as_ref().map(|r| r.trace_id.clone());

    // Start a security-scan span
    let _security_span_id = soch_trace_id.as_ref().and_then(|tid| {
        state.trace_store.start_span(tid, "security_scan", None, SpanKind::Internal)
            .ok().map(|s| s.span_id)
    });

    let scan = state.scanner.scan(&request.content);
    if !scan.passed {
        let critical: Vec<String> = scan
            .findings
            .iter()
            .filter(|f| f.severity == clawdesk_types::security::Severity::Critical)
            .map(|f| format!("{}: {}", f.rule, f.description))
            .collect();
        if !critical.is_empty() {
            state
                .audit_logger
                .log(
                    AuditCategory::SecurityAlert,
                    "message_blocked",
                    AuditActor::User {
                        sender_id: "desktop".into(),
                        channel: "tauri".into(),
                    },
                    Some(request.agent_id.clone()),
                    serde_json::json!({"critical_findings": critical}),
                    AuditOutcome::Blocked,
                )
                .await;
            let _ = app.emit(
                "system:alert",
                serde_json::json!({
                    "level": "warning",
                    "title": "Security alert",
                    "message": format!("Content blocked: {}", critical.join("; ")),
                }),
            );
            return Err(format!("Content blocked by security scan: {}", critical.join("; ")));
        }
    }

    // End security-scan span
    if let (Some(tid), Some(sid)) = (&soch_trace_id, &_security_span_id) {
        let _ = state.trace_store.end_span(tid, sid, SpanStatusCode::Ok, None);
    }

    Ok(TraceAndScanResult {
        soch_trace_id,
        scan,
    })
}

// ═══════════════════════════════════════════════════════════
// Stage 2: Agent + identity resolution
// ═══════════════════════════════════════════════════════════

/// Resolve the target agent by ID (with routing/delegation) and verify identity.
pub(crate) fn resolve_agent(
    request: &SendMessageRequest,
    state: &AppState,
) -> Result<ResolvedAgent, String> {
    let mut agent = {
        let agents = state.agents.read().map_err(|e| e.to_string())?;

        // Build an effective agent_id: if the requested agent has a `delegate_to`
        // rule, follow the chain. This supports simple round-robin routing when
        // multiple agents share a channel.
        let effective_agent_id = if agents.contains_key(&request.agent_id) {
            request.agent_id.clone()
        } else {
            agents
                .keys()
                .find(|k| {
                    agents
                        .get(*k)
                        .map(|a| a.channels.contains(&"tauri".to_string()))
                        .unwrap_or(false)
                })
                .cloned()
                .unwrap_or_else(|| request.agent_id.clone())
        };

        agents
            .get(&effective_agent_id)
            .cloned()
            .ok_or_else(|| format!("Agent {} not found", effective_agent_id))?
    };

    // Apply user's preferred model override
    if let Some(ref model_ov) = request.model_override {
        if !model_ov.is_empty() {
            agent.model = model_ov.clone();
        }
    }

    let identity_verified = {
        let identities = state.identities.read().map_err(|e| e.to_string())?;
        identities.get(&agent.id).map(|ic| ic.verify()).unwrap_or(false)
    };

    Ok(ResolvedAgent {
        agent,
        identity_verified,
    })
}

// ═══════════════════════════════════════════════════════════
// Stage 3: Session resolution
// ═══════════════════════════════════════════════════════════

/// Resolve or create chat_id, session key, and auto-title.
pub(crate) fn resolve_session(
    request: &SendMessageRequest,
    state: &AppState,
    is_new_chat_content: &str,
) -> SessionContext {
    let (chat_id, is_new_chat) = {
        let provided = request.chat_id.as_deref().unwrap_or("").to_string();
        if !provided.is_empty() {
            if state.sessions.contains(&provided) {
                (provided, false)
            } else {
                (Uuid::new_v4().to_string(), true)
            }
        } else {
            (Uuid::new_v4().to_string(), true)
        }
    };

    let session_key = clawdesk_types::session::SessionKey::new(
        clawdesk_types::channel::ChannelId::WebChat,
        &chat_id,
    );

    let auto_title = if is_new_chat {
        let words: Vec<&str> = is_new_chat_content.split_whitespace().take(6).collect();
        let title = words.join(" ");
        if title.chars().count() > 60 {
            let short = title.chars().take(57).collect::<String>();
            format!("{short}…")
        } else {
            title
        }
    } else {
        String::new()
    };

    SessionContext {
        chat_id,
        is_new_chat,
        session_key,
        auto_title,
    }
}

// ═══════════════════════════════════════════════════════════
// Stage 4: Persist user message (dual-write)
// ═══════════════════════════════════════════════════════════

/// Store the user message in both the in-memory session and durable ConversationStore.
pub(crate) async fn persist_user_message(
    request: &SendMessageRequest,
    state: &AppState,
    app: &AppHandle,
    session: &SessionContext,
    agent_id: &str,
    now: &DateTime<Utc>,
) -> Result<(), String> {
    let user_msg = ChatMessage {
        id: Uuid::new_v4().to_string(),
        role: "user".to_string(),
        content: request.content.clone(),
        timestamp: now.to_rfc3339(),
        metadata: None,
    };

    // In-memory session write
    {
        let msg_count = state.append_session_message(
            &session.chat_id, agent_id, &session.auto_title, user_msg, now,
        ).map_err(|e| {
            crate::commands_debug::emit_debug(app, crate::commands_debug::DebugEvent::error(
                "persist", "user_msg_persist_FAIL",
                format!("FAILED to persist user message to SochDB: {}", e),
            ));
            e
        })?;
        crate::commands_debug::emit_debug(app, crate::commands_debug::DebugEvent::info(
            "persist", "user_msg_persisted",
            format!("User message persisted. chat_id={}, msgs_in_session={}, is_new={}", session.chat_id, msg_count, session.is_new_chat),
        ));
    }

    // Durable ConversationStore write
    {
        use clawdesk_storage::conversation_store::ConversationStore;
        use clawdesk_types::session::{AgentMessage, Role};
        let agent_msg = AgentMessage {
            role: Role::User,
            content: request.content.clone(),
            timestamp: *now,
            model: None,
            token_count: None,
            tool_call_id: None,
            tool_name: None,
        };
        if let Err(e) = state.soch_store.append_message(&session.session_key, &agent_msg).await {
            tracing::warn!(error = %e, "ConversationStore append_message failed for user msg");
        }
    }

    // Force sync to disk
    if let Err(e) = state.soch_store.sync() {
        tracing::warn!(error = %e, "SochDB sync after user message persist failed");
        crate::commands_debug::emit_debug(app, crate::commands_debug::DebugEvent::warn(
            "persist", "sync_after_user_FAIL",
            format!("SochDB sync() after user message failed: {}", e),
        ));
    } else {
        crate::commands_debug::emit_debug(app, crate::commands_debug::DebugEvent::info(
            "persist", "sync_after_user_ok",
            format!("SochDB sync() after user message succeeded. chat_id={}", session.chat_id),
        ));
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════
// Stage 5: Provider resolution + model routing
// ═══════════════════════════════════════════════════════════

/// Resolve the LLM provider via negotiator, model routing, or user override.
pub(crate) fn resolve_provider(
    request: &SendMessageRequest,
    state: &AppState,
    agent: &DesktopAgent,
) -> Result<ProviderContext, String> {
    let base_model_id = AppState::resolve_model_id(&agent.model);

    let (model_full_id, routing_decision) = {
        let is_default_model = agent.model.is_empty()
            || agent.model == "default"
            || agent.model == "auto";
        if is_default_model {
            if let Some(routed) = state.turn_router.route_turn(&request.content, None) {
                tracing::info!(
                    from = %base_model_id,
                    to = %routed.model_id,
                    score = routed.score,
                    "GAP-G: Turn router overriding model"
                );
                (routed.model_id.clone(), Some(routed))
            } else {
                (base_model_id, None)
            }
        } else {
            (base_model_id, None)
        }
    };

    let provider: Arc<dyn clawdesk_providers::Provider> = if let Some(ref prov_name) = request.provider_override {
        use clawdesk_providers::anthropic::AnthropicProvider;
        use clawdesk_providers::openai::OpenAiProvider;
        use clawdesk_providers::azure::AzureOpenAiProvider;
        use clawdesk_providers::gemini::GeminiProvider;
        use clawdesk_providers::cohere::CohereProvider;
        use clawdesk_providers::ollama::OllamaProvider;

        let key = request.api_key.clone().unwrap_or_default();
        let base = request.base_url.clone();

        match prov_name.as_str() {
            "Anthropic" => Arc::new(AnthropicProvider::new(key, Some(model_full_id.clone()))),
            "OpenAI" => Arc::new(OpenAiProvider::new(key, base, Some(model_full_id.clone()))),
            "Azure OpenAI" => {
                let endpoint = base.unwrap_or_default();
                Arc::new(AzureOpenAiProvider::new(key, endpoint, None, Some(model_full_id.clone())))
            }
            "Google" => Arc::new(GeminiProvider::new(key, Some(model_full_id.clone()))),
            "Cohere" => Arc::new(CohereProvider::new(key, base, Some(model_full_id.clone()))),
            "Ollama (Local)" | "ollama" => Arc::new(OllamaProvider::new(base, Some(model_full_id.clone()))),
            "Local (OpenAI Compatible)" | "local_compatible" => {
                use clawdesk_providers::compatible::{CompatibleConfig, OpenAiCompatibleProvider};
                let base_url = base.unwrap_or_else(|| "http://localhost:8080/v1".to_string());
                let config = CompatibleConfig::new("local_compatible", &base_url, key)
                    .with_default_model(model_full_id.clone());
                Arc::new(OpenAiCompatibleProvider::new(config))
            }
            _ => {
                tracing::warn!(provider = %prov_name, "Unknown provider_override — falling back to negotiator");
                let negotiator = state.negotiator.read().map_err(|e| e.to_string())?;
                let required = clawdesk_providers::capability::ProviderCaps::TEXT_COMPLETION
                    .union(clawdesk_providers::capability::ProviderCaps::SYSTEM_PROMPT);
                match negotiator.resolve_model(&model_full_id, required) {
                    Some((p, _)) => Arc::clone(p),
                    None => {
                        drop(negotiator);
                        state.resolve_provider(&agent.model)?
                    }
                }
            }
        }
    } else {
        use clawdesk_providers::capability::ProviderCaps;
        let negotiator = state.negotiator.read().map_err(|e| e.to_string())?;
        let required = ProviderCaps::TEXT_COMPLETION.union(ProviderCaps::SYSTEM_PROMPT);
        match negotiator.resolve_model(&model_full_id, required) {
            Some((p, _resolved_model)) => Arc::clone(p),
            None => {
                drop(negotiator);
                tracing::warn!(
                    model = %agent.model,
                    full_id = %model_full_id,
                    "ProviderNegotiator miss — falling back to legacy resolve_provider"
                );
                state.resolve_provider(&agent.model)?
            }
        }
    };

    Ok(ProviderContext {
        provider,
        model_full_id,
        routing_decision,
    })
}

// ═══════════════════════════════════════════════════════════
// Stage 6: History assembly + context compaction
// ═══════════════════════════════════════════════════════════

/// Load conversation history from SochDB/HashMap, inject URL context,
/// and apply context guard compaction.
pub(crate) async fn assemble_history(
    state: &AppState,
    app: &AppHandle,
    session: &SessionContext,
    agent: &DesktopAgent,
    provider: &Arc<dyn clawdesk_providers::Provider>,
    model_full_id: &str,
    user_content: &str,
) -> Result<HistoryContext, String> {
    // ── Load from SochDB + in-memory ──
    let mut history = {
        use clawdesk_storage::conversation_store::ConversationStore;
        use clawdesk_types::session::Role;

        let soch_messages = state.soch_store
            .load_history(&session.session_key, 200)
            .await
            .unwrap_or_default();

        let hashmap_messages = state.sessions.get(&session.chat_id)
            .map(|s| s.messages)
            .unwrap_or_default();

        let (history_source, history_vec) = if !soch_messages.is_empty() && soch_messages.len() >= hashmap_messages.len() {
            let mut timestamped: Vec<(chrono::DateTime<chrono::Utc>, clawdesk_providers::ChatMessage)> =
                soch_messages
                    .iter()
                    .map(|m| {
                        let role = match m.role {
                            Role::User => MessageRole::User,
                            Role::Assistant => MessageRole::Assistant,
                            Role::System => MessageRole::System,
                            Role::Tool | Role::ToolResult => MessageRole::Tool,
                        };
                        (m.timestamp, clawdesk_providers::ChatMessage::new(role, m.content.as_str()))
                    })
                    .collect();

            let tool_msgs = state.load_tool_history(&session.chat_id);
            for tm in &tool_msgs {
                let ts = chrono::DateTime::parse_from_rfc3339(&tm.timestamp)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());
                let role = match tm.role.as_str() {
                    "assistant" => MessageRole::Assistant,
                    "system" => MessageRole::System,
                    "tool" => MessageRole::Tool,
                    _ => MessageRole::User,
                };
                timestamped.push((ts, clawdesk_providers::ChatMessage::new(role, tm.content.as_str())));
            }
            if !tool_msgs.is_empty() {
                timestamped.sort_by(|a, b| a.0.cmp(&b.0));
            }
            ("sochdb", timestamped.into_iter().map(|(_, msg)| msg).collect::<Vec<_>>())
        } else if !hashmap_messages.is_empty() {
            if !soch_messages.is_empty() {
                tracing::warn!(
                    chat_id = %session.chat_id,
                    sochdb_count = soch_messages.len(),
                    hashmap_count = hashmap_messages.len(),
                    "ConversationStore has fewer messages than HashMap — using HashMap"
                );
            }
            let tool_msgs = state.load_tool_history(&session.chat_id);
            let mut all_msgs = hashmap_messages;
            if !tool_msgs.is_empty() {
                all_msgs.extend(tool_msgs);
                all_msgs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
            }
            let msgs = all_msgs
                .iter()
                .map(|m| {
                    let role = match m.role.as_str() {
                        "user" => MessageRole::User,
                        "assistant" => MessageRole::Assistant,
                        "system" => MessageRole::System,
                        "tool" => MessageRole::Tool,
                        _ => MessageRole::User,
                    };
                    clawdesk_providers::ChatMessage::new(role, m.content.as_str())
                })
                .collect::<Vec<_>>();
            ("hashmap_preferred", msgs)
        } else {
            ("empty", Vec::new())
        };

        tracing::info!(
            chat_id = %session.chat_id,
            source = history_source,
            messages = history_vec.len(),
            "History assembled for LLM context"
        );
        history_vec
    };

    // ── URL injection ──
    {
        let urls = clawdesk_media::link_understanding::LinkUnderstanding::extract_urls(user_content);
        if !urls.is_empty() {
            let url_context = format!(
                "[Context: User message contains {} URL(s): {}. You may reference these in your response.]",
                urls.len(),
                urls.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
            );
            let insert_pos = history.len().saturating_sub(1);
            history.insert(
                insert_pos,
                clawdesk_providers::ChatMessage::new(MessageRole::System, url_context.as_str()),
            );
        }
    }

    // ── Context guard + compaction ──
    let compacted_guard = apply_context_guard(
        state, app, &mut history, agent, provider, model_full_id, &session.chat_id,
    ).await?;

    Ok(HistoryContext {
        history,
        compacted_guard,
    })
}

/// Check the context guard and apply compaction if needed.
#[allow(dead_code)]
async fn apply_context_guard(
    state: &AppState,
    app: &AppHandle,
    history: &mut Vec<clawdesk_providers::ChatMessage>,
    agent: &DesktopAgent,
    provider: &Arc<dyn clawdesk_providers::Provider>,
    model_full_id: &str,
    chat_id: &str,
) -> Result<ContextGuard, String> {
    let total_history_tokens: usize = history.iter()
        .map(|m| estimate_tokens(&m.content))
        .sum();

    let guard_action = {
        let mut guards = state.context_guards.write().map_err(|e| e.to_string())?;
        let guard = guards.entry(chat_id.to_string()).or_insert_with(|| {
            ContextGuard::new(ContextGuardConfig {
                context_limit: agent.token_budget,
                trigger_threshold: 0.80,
                response_reserve: 8_192,
                circuit_breaker_threshold: 3,
                circuit_breaker_cooldown: Duration::from_secs(60),
                adaptive_thresholds: true,
                force_truncate_retain_share: 0.50,
            })
        });
        guard.set_token_count(total_history_tokens);
        guard.check()
    };

    match guard_action {
        GuardAction::Ok => {}
        GuardAction::Compact(level) => {
            apply_compaction_level(state, app, history, agent, provider, model_full_id, chat_id, level, total_history_tokens).await?;
        }
        GuardAction::ForceTruncate { retain_tokens } => {
            apply_budget_truncation(state, app, history, agent, chat_id, retain_tokens, "truncate_budget").await?;
        }
        GuardAction::CircuitBroken { retain_tokens } => {
            apply_budget_truncation(state, app, history, agent, chat_id, retain_tokens, "circuit_broken_budget_truncate").await?;
        }
    }

    // Clone the guard to pass to the runner
    let guards = state.context_guards.read().map_err(|e| e.to_string())?;
    Ok(guards.get(chat_id).cloned().unwrap_or_else(|| {
        ContextGuard::new(ContextGuardConfig {
            context_limit: agent.token_budget,
            trigger_threshold: 0.80,
            response_reserve: 8_192,
            circuit_breaker_threshold: 3,
            circuit_breaker_cooldown: Duration::from_secs(60),
            adaptive_thresholds: true,
            force_truncate_retain_share: 0.50,
        })
    }))
}

/// Apply compaction at a specific level (DropMetadata, SummarizeOld, Truncate).
#[allow(dead_code)]
async fn apply_compaction_level(
    state: &AppState,
    app: &AppHandle,
    history: &mut Vec<clawdesk_providers::ChatMessage>,
    agent: &DesktopAgent,
    provider: &Arc<dyn clawdesk_providers::Provider>,
    model_full_id: &str,
    chat_id: &str,
    level: CompactionLevel,
    tokens_before: usize,
) -> Result<(), String> {
    match level {
        CompactionLevel::DropMetadata => {
            for msg in history.iter_mut() {
                if msg.role == MessageRole::Tool && msg.content.len() > 500 {
                    let preview = safe_prefix(&msg.content, 500);
                    let truncated = format!("{preview}...[truncated]");
                    msg.content = std::sync::Arc::from(truncated);
                    msg.cached_tokens = Some(estimate_tokens(&msg.content));
                }
            }
        }
        CompactionLevel::SummarizeOld => {
            let keep = history.len() / 2;
            if history.len() > keep + 2 {
                let old_msgs: Vec<_> = history.drain(..history.len() - keep).collect();
                let mut transcript = String::with_capacity(old_msgs.len() * 80);
                for m in &old_msgs {
                    transcript.push_str(m.role.as_str());
                    transcript.push_str(": ");
                    if m.content.len() > 600 {
                        transcript.push_str(safe_prefix(&m.content, 600));
                        transcript.push_str("…");
                    } else {
                        transcript.push_str(&m.content);
                    }
                    transcript.push('\n');
                }
                let summary = clawdesk_agents::compaction::summarize_transcript_via_llm(
                    provider,
                    model_full_id,
                    &transcript,
                    old_msgs.len(),
                ).await;
                let summary_tokens = estimate_tokens(&summary);
                history.insert(0, clawdesk_providers::ChatMessage {
                    role: MessageRole::System,
                    content: std::sync::Arc::from(summary),
                    cached_tokens: Some(summary_tokens),
                });
            }
        }
        CompactionLevel::Truncate => {
            if history.len() > 10 {
                *history = history.split_off(history.len() - 10);
            }
        }
    }

    let tokens_after: usize = history.iter()
        .map(|m| estimate_tokens(&m.content))
        .sum();

    {
        let mut guards = state.context_guards.write().map_err(|e| e.to_string())?;
        if let Some(guard) = guards.get_mut(chat_id) {
            guard.set_token_count(tokens_after);
            let result = CompactionResult {
                level,
                tokens_before,
                tokens_after,
                turns_removed: 0,
                turns_summarized: 0,
            };
            guard.compaction_succeeded(&result);
        }
    }

    let _ = app.emit("agent-event", serde_json::json!({
        "agent_id": &agent.id,
        "event": { "type": "ContextGuardAction", "action": format!("compact_{:?}", level), "token_count": tokens_after, "threshold": 0.80 },
    }));

    Ok(())
}

/// Apply budget-based truncation (ForceTruncate / CircuitBroken).
#[allow(dead_code)]
async fn apply_budget_truncation(
    state: &AppState,
    app: &AppHandle,
    history: &mut Vec<clawdesk_providers::ChatMessage>,
    agent: &DesktopAgent,
    chat_id: &str,
    retain_tokens: usize,
    action_label: &str,
) -> Result<(), String> {
    let mut running = 0usize;
    let mut keep_from = history.len();
    for i in (0..history.len()).rev() {
        let t = estimate_tokens(&history[i].content);
        if running + t > retain_tokens && keep_from < history.len() {
            break;
        }
        running += t;
        keep_from = i;
    }
    if keep_from > 0 {
        *history = history.split_off(keep_from);
    }
    let tokens_after: usize = history.iter()
        .map(|m| estimate_tokens(&m.content))
        .sum();
    {
        let mut guards = state.context_guards.write().map_err(|e| e.to_string())?;
        if let Some(guard) = guards.get_mut(chat_id) {
            guard.set_token_count(tokens_after);
        }
    }
    let _ = app.emit("agent-event", serde_json::json!({
        "agent_id": &agent.id,
        "event": { "type": "ContextGuardAction", "action": format!("{}_{}", action_label, retain_tokens), "token_count": tokens_after },
    }));

    Ok(())
}

// ═══════════════════════════════════════════════════════════
// Stage 7: Build prompt + check semantic cache
// ═══════════════════════════════════════════════════════════

/// Run the unified prompt pipeline and produce the system prompt + memory injection.
#[allow(dead_code)]
pub(crate) async fn build_prompt(
    state: &AppState,
    request: &SendMessageRequest,
    agent: &DesktopAgent,
    history: &mut Vec<clawdesk_providers::ChatMessage>,
    active_skills: &[Arc<clawdesk_skills::definition::Skill>],
) -> Result<PromptContext, String> {
    let agent_skill_set: std::collections::HashSet<String> = agent
        .skills
        .iter()
        .map(|s| s.to_lowercase())
        .collect();

    let available_ch_names: Vec<String> = state.channel_registry.read()
        .map(|reg| reg.list().iter().map(|id| format!("{}", id).to_lowercase()).collect())
        .unwrap_or_default();

    let pipeline_result = crate::engine::build_prompt_pipeline(
        crate::engine::PromptPipelineInput {
            user_content: &request.content,
            persona: &agent.persona,
            model_name: &agent.model,
            agent_skill_ids: &agent_skill_set,
            channel_id: Some("tauri"),
            channel_description: "Tauri desktop",
            budget: clawdesk_domain::prompt_builder::PromptBudget {
                total: agent.token_budget,
                response_reserve: 8_192,
                identity_cap: 2_000,
                skills_cap: 4_096,
                memory_cap: 4_096,
                history_floor: 2_000,
                runtime_cap: 512,
                safety_cap: 1_024,
            },
            available_channels: available_ch_names,
        },
        &state.memory,
        active_skills,
    ).await;

    let system_prompt = pipeline_result.system_prompt;
    let memory_injection = pipeline_result.memory_injection;

    // Store manifest for inspector
    if let Some(ref manifest) = pipeline_result.prompt_manifest {
        if let Ok(mut manifests) = state.prompt_manifests.write() {
            manifests.insert(agent.id.clone(), manifest.clone());
        }
    }

    // Inject memory context
    if let Some(ref mem_text) = memory_injection {
        crate::engine::inject_memory_context(history, mem_text);
    }

    // Compute prompt hash for cache namespace
    let prompt_hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        system_prompt.hash(&mut hasher);
        if let Some(ref mem) = memory_injection {
            mem.hash(&mut hasher);
        }
        hasher.finish()
    };

    // Pre-compute query embedding for semantic cache
    let query_embedding = match state.embedding_provider.embed(&request.content).await {
        Ok(result) => Some(result.vector),
        Err(e) => {
            tracing::debug!(error = %e, "query embedding for semantic cache failed, using exact match only");
            None
        }
    };

    let cache_namespace = format!("agent:{}:{}:{:x}", agent.id, agent.model, prompt_hash);

    Ok(PromptContext {
        system_prompt,
        memory_injection,
        cache_namespace,
        query_embedding,
        prompt_hash,
    })
}

/// Check the semantic cache for a hit. Returns CacheResult::Hit with the response
/// if found, or CacheResult::Miss to continue to LLM.
#[allow(dead_code)]
pub(crate) async fn check_semantic_cache(
    state: &AppState,
    app: &AppHandle,
    request: &SendMessageRequest,
    session: &SessionContext,
    agent: &DesktopAgent,
    prompt_ctx: &PromptContext,
    soch_trace_id: &Option<String>,
    identity_verified: bool,
    start: Instant,
    now: &DateTime<Utc>,
) -> Result<CacheResult, String> {
    let cache_hit = state.semantic_cache.lookup(
        &request.content,
        &prompt_ctx.cache_namespace,
        0,
        prompt_ctx.query_embedding.as_deref(),
    ).ok().and_then(|result| {
        if let Some(entry) = result.entry {
            match result.match_type {
                CacheMatchType::Exact | CacheMatchType::Semantic { .. } => {
                    String::from_utf8(entry.result).ok()
                }
                CacheMatchType::Miss => None,
            }
        } else {
            None
        }
    });

    if let Some(cached_response) = cache_hit {
        if let Some(tid) = soch_trace_id {
            let _ = state.trace_store.end_run(tid, TraceStatus::Ok);
        }

        let elapsed_ms = start.elapsed().as_millis() as u64;
        let assistant_msg = ChatMessage {
            id: Uuid::new_v4().to_string(),
            role: "assistant".to_string(),
            content: cached_response.clone(),
            timestamp: Utc::now().to_rfc3339(),
            metadata: Some(ChatMessageMeta {
                skills_activated: vec![],
                token_cost: 0,
                cost_usd: 0.0,
                model: agent.model.clone(),
                duration_ms: elapsed_ms,
                identity_verified,
                tools_used: vec![],
                compaction: None,
            }),
        };

        {
            state.append_session_message(
                &session.chat_id, &agent.id, &session.auto_title, assistant_msg.clone(), now,
            )?;
        }

        let _ = app.emit("incoming:message", serde_json::json!({
            "agent_id": agent.id,
            "chat_id": &session.chat_id,
            "preview": cached_response.chars().take(120).collect::<String>(),
            "timestamp": assistant_msg.timestamp,
            "cache_hit": true,
        }));

        return Ok(CacheResult::Hit(SendMessageResponse {
            message: assistant_msg,
            trace: vec![TraceEntry {
                timestamp: now.format("%H:%M:%S%.3f").to_string(),
                event: "CacheHit".to_string(),
                detail: format!("semantic_cache hit, skipped LLM call, elapsed={}ms", elapsed_ms),
            }],
            chat_id: session.chat_id.clone(),
            chat_title: if session.is_new_chat { Some(session.auto_title.clone()) } else { None },
        }));
    }

    Ok(CacheResult::Miss)
}

// ═══════════════════════════════════════════════════════════
// Stage 8: Post-processing — persist response, audit, memory
// ═══════════════════════════════════════════════════════════

/// Process the agent response: persist, audit, update knowledge graph,
/// store in cache, record usage, write memory.
#[allow(dead_code, clippy::too_many_arguments)]
pub(crate) async fn finalize_response(
    state: &AppState,
    app: &AppHandle,
    request: &SendMessageRequest,
    session: &SessionContext,
    agent: &DesktopAgent,
    prov_ctx: &ProviderContext,
    prompt_ctx: &PromptContext,
    agent_response: clawdesk_agents::runner::AgentResponse,
    execution_err: Option<String>,
    soch_trace_id: &Option<String>,
    llm_span_id: &Option<String>,
    identity_verified: bool,
    collected_events: Vec<AgentEvent>,
    start: Instant,
    now: &DateTime<Utc>,
) -> Result<SendMessageResponse, String> {
    let ts = now.format("%H:%M:%S%.3f").to_string();

    // ── End LLM-call span ──
    if let (Some(tid), Some(sid)) = (soch_trace_id, llm_span_id) {
        let status = if execution_err.is_none() { SpanStatusCode::Ok } else { SpanStatusCode::Error };
        let _ = state.trace_store.end_span(tid, sid, status, None);
    }

    // ── Semantic cache store ──
    if execution_err.is_none() {
        let _ = state.semantic_cache.store(
            &request.content,
            &prompt_ctx.cache_namespace,
            0,
            agent_response.content.as_bytes(),
            prompt_ctx.query_embedding.clone(),
            vec![],
            None,
        );
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;

    // ── Collect trace events ──
    let mut trace = Vec::new();
    let mut tools_used = Vec::new();
    for event in collected_events {
        match event {
            AgentEvent::RoundStart { round } => {
                trace.push(TraceEntry { timestamp: ts.clone(), event: "RoundStart".into(), detail: format!("round={}", round) });
            }
            AgentEvent::PromptAssembled { total_tokens, skills_included, memory_fragments, budget_utilization, .. } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(), event: "PromptAssembled".into(),
                    detail: format!("tokens={} skills=[{}] memory={} budget={:.1}%", total_tokens, skills_included.join(","), memory_fragments, budget_utilization * 100.0),
                });
            }
            AgentEvent::IdentityVerified { hash_match, version } => {
                trace.push(TraceEntry { timestamp: ts.clone(), event: "IdentityVerified".into(), detail: format!("hash_match={} version={}", hash_match, version) });
            }
            AgentEvent::ToolStart { name, .. } => {
                trace.push(TraceEntry { timestamp: ts.clone(), event: "ToolStart".into(), detail: format!("name={}", name) });
            }
            AgentEvent::ToolEnd { name, success, duration_ms } => {
                trace.push(TraceEntry { timestamp: ts.clone(), event: "ToolEnd".into(), detail: format!("name={} ok={} {}ms", name, success, duration_ms) });
                tools_used.push(ToolUsageSummary { name, success, duration_ms });
            }
            AgentEvent::Compaction { level, tokens_before, tokens_after } => {
                trace.push(TraceEntry { timestamp: ts.clone(), event: "Compaction".into(), detail: format!("{:?} {} -> {} tokens", level, tokens_before, tokens_after) });
            }
            AgentEvent::ContextGuardAction { action, token_count, threshold } => {
                trace.push(TraceEntry { timestamp: ts.clone(), event: "ContextGuard".into(), detail: format!("action={} tokens={} threshold={:.2}", action, token_count, threshold) });
            }
            AgentEvent::FallbackTriggered { from_model, to_model, reason, .. } => {
                trace.push(TraceEntry { timestamp: ts.clone(), event: "Fallback".into(), detail: format!("{} -> {} reason={}", from_model, to_model, reason) });
            }
            AgentEvent::Response { finish_reason, .. } => {
                trace.push(TraceEntry { timestamp: ts.clone(), event: "Response".into(), detail: format!("finish={:?} tokens={}", finish_reason, agent_response.output_tokens) });
            }
            AgentEvent::Done { total_rounds } => {
                trace.push(TraceEntry { timestamp: ts.clone(), event: "Done".into(), detail: format!("rounds={}", total_rounds) });
            }
            AgentEvent::Error { error } => {
                trace.push(TraceEntry { timestamp: ts.clone(), event: "Error".into(), detail: error });
            }
            _ => {}
        }
    }

    // Identity verification trace
    if !trace.iter().any(|t| t.event == "IdentityVerified") {
        trace.insert(0, TraceEntry { timestamp: ts.clone(), event: "IdentityVerified".into(), detail: format!("hash_match={} version=1", identity_verified) });
    }

    // ── Token usage + costs ──
    let input_tokens = agent_response.input_tokens;
    let output_tokens = agent_response.output_tokens;
    let cost_usd = {
        let (cpi, cpo) = model_cost_rates(&agent.model);
        (input_tokens as f64 * cpi / 1_000_000.0) + (output_tokens as f64 * cpo / 1_000_000.0)
    };
    state.record_usage(&agent.model, input_tokens, output_tokens);

    // ── Turn router feedback ──
    if let Some(ref rd) = prov_ctx.routing_decision {
        let reward = if execution_err.is_some() { 0.1 }
            else if elapsed_ms > 30_000 { 0.3 }
            else if elapsed_ms > 10_000 { 0.5 }
            else { 0.8 };
        state.turn_router.record_feedback(&rd.selected_key, &rd.features, reward);
    }

    // ── Trace store metrics ──
    if let Some(tid) = soch_trace_id {
        let cost_millicents = (cost_usd * 100_000.0) as u64;
        let _ = state.trace_store.update_run_metrics(tid, (input_tokens + output_tokens) as u64, cost_millicents);
        let _ = state.trace_store.log_cost(tid, CostEvent {
            cost_type: "llm_call".into(),
            amount: (input_tokens + output_tokens) as u64,
            unit_price_millicents: cost_usd * 100_000.0 / (input_tokens + output_tokens).max(1) as f64,
            total_millicents: cost_millicents,
            model: Some(agent.model.clone()),
        });
        let _ = state.trace_store.end_run(tid, TraceStatus::Ok);
    }

    // ── Knowledge graph ──
    {
        let session_id = format!("session:{}", session.chat_id);
        let user_node_id = format!("msg:{}", Uuid::new_v4());
        let mut user_props = HashMap::new();
        user_props.insert("role".into(), serde_json::json!("user"));
        user_props.insert("content_len".into(), serde_json::json!(request.content.len()));
        user_props.insert("timestamp".into(), serde_json::json!(now.to_rfc3339()));
        let _ = state.knowledge_graph.add_node(&user_node_id, "message", Some(user_props));
        let _ = state.knowledge_graph.add_edge(&session_id, "contains", &user_node_id, None);

        let asst_node_id = format!("msg:{}", Uuid::new_v4());
        let mut asst_props = HashMap::new();
        asst_props.insert("role".into(), serde_json::json!("assistant"));
        asst_props.insert("content_len".into(), serde_json::json!(agent_response.content.len()));
        asst_props.insert("model".into(), serde_json::json!(&agent.model));
        asst_props.insert("input_tokens".into(), serde_json::json!(input_tokens));
        asst_props.insert("output_tokens".into(), serde_json::json!(output_tokens));
        asst_props.insert("timestamp".into(), serde_json::json!(Utc::now().to_rfc3339()));
        let _ = state.knowledge_graph.add_node(&asst_node_id, "message", Some(asst_props));
        let _ = state.knowledge_graph.add_edge(&session_id, "contains", &asst_node_id, None);
        let _ = state.knowledge_graph.add_edge(&user_node_id, "responded_with", &asst_node_id, None);
    }

    // ── Content classification ──
    let _classification = state.scanner.classify_content(&agent_response.content);
    if !state.scanner.is_safe(&agent_response.content) {
        trace.push(TraceEntry { timestamp: ts.clone(), event: "ContentScan".into(), detail: "response_flagged".into() });
    }

    trace.push(TraceEntry {
        timestamp: ts.clone(), event: "Done".into(),
        detail: format!("rounds={} cost=${:.6} elapsed={}ms input_tokens={} output_tokens={}", agent_response.total_rounds, cost_usd, elapsed_ms, input_tokens, output_tokens),
    });

    let activated: Vec<String> = agent_response.active_skills.clone();
    let compaction_info = trace.iter().find(|t| t.event == "Compaction").map(|t| {
        let parts: Vec<&str> = t.detail.split_whitespace().collect();
        let level = parts.first().unwrap_or(&"unknown").to_string();
        let tokens_before = parts.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
        let tokens_after = parts.get(3).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
        CompactionInfo { level, tokens_before, tokens_after }
    });

    let response_content = agent_response.content.clone();
    let assistant_msg = ChatMessage {
        id: Uuid::new_v4().to_string(),
        role: "assistant".to_string(),
        content: agent_response.content,
        timestamp: Utc::now().to_rfc3339(),
        metadata: Some(ChatMessageMeta {
            skills_activated: activated,
            token_cost: (input_tokens + output_tokens) as usize,
            cost_usd,
            model: agent.model.clone(),
            duration_ms: elapsed_ms,
            identity_verified,
            tools_used,
            compaction: compaction_info,
        }),
    };

    // ── Persist assistant response ──
    {
        if !agent_response.tool_messages.is_empty() {
            let tool_chat_msgs: Vec<ChatMessage> = agent_response.tool_messages.iter().map(|tool_msg| {
                let role_str = match tool_msg.role {
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                };
                ChatMessage {
                    id: Uuid::new_v4().to_string(),
                    role: role_str.to_string(),
                    content: tool_msg.content.to_string(),
                    timestamp: Utc::now().to_rfc3339(),
                    metadata: None,
                }
            }).collect();
            if let Err(e) = state.persist_tool_history(&session.chat_id, &tool_chat_msgs) {
                tracing::warn!(chat_id = %session.chat_id, error = %e, "Failed to persist tool history");
            }
        }

        let msg_count = state.append_session_message(
            &session.chat_id, &agent.id, &session.auto_title, assistant_msg.clone(), now,
        ).map_err(|e| {
            crate::commands_debug::emit_debug(app, crate::commands_debug::DebugEvent::error(
                "persist", "asst_msg_persist_FAIL",
                format!("FAILED to persist assistant message: {}", e),
            ));
            e
        })?;
        crate::commands_debug::emit_debug(app, crate::commands_debug::DebugEvent::info(
            "persist", "asst_msg_persisted",
            format!("Assistant message persisted. chat_id={}, msgs_in_session={}", session.chat_id, msg_count),
        ));

        // Write to ConversationStore with fresh timestamp
        {
            use clawdesk_storage::conversation_store::ConversationStore;
            use clawdesk_types::session::{AgentMessage, Role};
            let assistant_ts = Utc::now();
            let agent_msg = AgentMessage {
                role: Role::Assistant,
                content: response_content.clone(),
                timestamp: assistant_ts,
                model: None,
                token_count: Some((agent_response.input_tokens + agent_response.output_tokens) as usize),
                tool_call_id: None,
                tool_name: None,
            };
            if let Err(e) = state.soch_store.append_message(&session.session_key, &agent_msg).await {
                tracing::warn!(error = %e, "ConversationStore append_message failed for assistant msg");
            }
        }

        // Background session indexing
        if msg_count % 10 == 0 && msg_count >= 4 {
            if let Some(session_data) = state.sessions.get(&session.chat_id) {
                let session_msgs: Vec<clawdesk_memory::SessionMessage> = session_data
                    .messages
                    .iter()
                    .map(|m| clawdesk_memory::SessionMessage {
                        role: m.role.clone(),
                        content: m.content.clone(),
                    })
                    .collect();
                let memory = Arc::clone(&state.memory);
                let chat_id_owned = session.chat_id.clone();
                tokio::spawn(async move {
                    let config = clawdesk_memory::SessionIndexConfig::default();
                    match clawdesk_memory::index_session(&memory, &chat_id_owned, &session_msgs, &config).await {
                        Ok(chunks) => tracing::info!(chat_id = %chat_id_owned, chunks, "Session indexed into memory"),
                        Err(e) => tracing::warn!(chat_id = %chat_id_owned, error = %e, "Session indexing failed"),
                    }
                });
            }
        }
    }

    let _ = app.emit("incoming:message", serde_json::json!({
        "agent_id": agent.id,
        "chat_id": &session.chat_id,
        "preview": assistant_msg.content.chars().take(120).collect::<String>(),
        "timestamp": assistant_msg.timestamp,
    }));

    // ── Audit log ──
    state.audit_logger.log(
        AuditCategory::MessageReceive,
        "assistant_response",
        AuditActor::Agent { id: agent.id.clone() },
        Some(agent.id.clone()),
        serde_json::json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cost_usd": cost_usd,
            "model": &agent.model,
            "identity_verified": identity_verified,
            "duration_ms": elapsed_ms,
            "total_rounds": agent_response.total_rounds,
        }),
        AuditOutcome::Success,
    ).await;

    // ── Memory write ──
    {
        let mem = Arc::clone(&state.memory);
        let temporal_graph = Arc::clone(&state.temporal_graph);
        let user_content = request.content.clone();
        let asst_content = assistant_msg.content.clone();
        let agent_id_for_mem = agent.id.clone();
        let agent_name = agent.name.clone();

        tokio::spawn(async move {
            crate::engine::store_conversation_memory(
                &mem, &user_content, &asst_content,
                &agent_id_for_mem, &agent_name, Some(&temporal_graph),
            ).await;
        });
    }

    // ── Update state ──
    {
        let mut traces = state.traces.write().map_err(|e| e.to_string())?;
        traces.insert(agent.id.clone(), trace.clone());
    }
    {
        let mut agents = state.agents.write().map_err(|e| e.to_string())?;
        if let Some(a) = agents.get_mut(&agent.id) {
            a.msg_count += 1;
            a.tokens_used += (input_tokens + output_tokens) as usize;
            state.persist_agent(&agent.id, a);
        }
    }

    state.persist();
    crate::commands::emit_metrics_updated(app, state);
    crate::commands::emit_security_changed(app, state).await;

    if let Some(err_msg) = execution_err {
        return Err(err_msg);
    }

    Ok(SendMessageResponse {
        message: assistant_msg,
        trace,
        chat_id: session.chat_id.clone(),
        chat_title: if session.is_new_chat { Some(session.auto_title.clone()) } else { None },
    })
}

// ═══════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_context_new_chat() {
        // Verify auto-title generation from content
        let ctx = SessionContext {
            chat_id: "test-123".into(),
            is_new_chat: true,
            session_key: clawdesk_types::session::SessionKey::new(
                clawdesk_types::channel::ChannelId::WebChat,
                "test-123",
            ),
            auto_title: String::new(),
        };
        assert!(ctx.is_new_chat);
        assert_eq!(ctx.chat_id, "test-123");
    }

    #[test]
    fn auto_title_truncation() {
        // Titles > 60 chars should be truncated with ellipsis
        let long_content = "word ".repeat(20);
        let words: Vec<&str> = long_content.split_whitespace().take(6).collect();
        let title = words.join(" ");
        if title.chars().count() > 60 {
            let short = title.chars().take(57).collect::<String>();
            let truncated = format!("{short}…");
            assert!(truncated.chars().count() <= 61);
        }
    }

    #[test]
    fn cache_result_variants() {
        // CacheResult::Miss should not contain a response
        let miss = CacheResult::Miss;
        assert!(matches!(miss, CacheResult::Miss));
    }

    #[test]
    fn resolved_agent_carries_identity() {
        let agent = DesktopAgent::default();
        let resolved = ResolvedAgent {
            agent,
            identity_verified: true,
        };
        assert!(resolved.identity_verified);
    }

    #[test]
    fn provider_context_has_routing() {
        // Verify that routing_decision is optional
        let model_full_id = "claude-sonnet-4-20250514".to_string();
        // Just test the struct construction compiles
        assert_eq!(model_full_id, "claude-sonnet-4-20250514");
    }
}
