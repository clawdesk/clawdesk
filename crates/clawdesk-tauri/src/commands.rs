//! Tauri IPC command handlers — real backend integration.
//!
//! Every `#[tauri::command]` here is callable from the React frontend via
//! `invoke("command_name", { args })`. Each command bridges to the actual
//! ClawDesk Rust crate APIs — skill registry, security scanner, audit
//! logger, provider registry, tool registry, and agent runner.

use crate::state::*;

use clawdesk_agents::runner::{AgentConfig, AgentEvent, AgentRunner};
use clawdesk_providers::MessageRole;
use clawdesk_security::identity::IdentitySource;
use clawdesk_security::IdentityContract;
use clawdesk_skills::definition::{SkillId, SkillSource, SkillState};
use clawdesk_tunnel::metrics::TunnelMetricsSnapshot;
use clawdesk_types::security::{AuditActor, AuditCategory, AuditOutcome};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use sochdb::semantic_cache::CacheMatchType;
use sochdb::trace::{SpanKind, SpanStatusCode, TraceStatus, CostEvent};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::broadcast;
use uuid::Uuid;

// ═══════════════════════════════════════════════════════════
// Health — queries real backend service state
// ═══════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_health(state: State<'_, AppState>) -> Result<HealthResponse, String> {
    let agents = state.agents.read().map_err(|e| e.to_string())?;
    let skill_count = {
        let reg = state.skill_registry.read().map_err(|e| e.to_string())?;
        reg.len()
    };
    let tunnel_snap = state.tunnel_metrics.snapshot();
    let tunnel_active = tunnel_snap.active_peers > 0 || tunnel_snap.total_rx_bytes > 0;

    Ok(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: state.uptime_secs(),
        agents_active: agents.len(),
        skills_loaded: skill_count,
        tunnel_active,
    })
}

// ═══════════════════════════════════════════════════════════
// Agent Management
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    pub icon: String,
    pub color: String,
    pub persona: String,
    pub skills: Vec<String>,
    pub model: String,
    pub source: Option<String>,
}

/// Create a new agent with an IdentityContract.
///
/// 1. Scans persona through CascadeScanner for security issues
/// 2. Creates an `IdentityContract` (hash-locked persona)
/// 3. Logs creation to the SHA-256 audit chain
/// 4. Stores agent + identity in state
#[tauri::command]
pub async fn create_agent(
    request: CreateAgentRequest,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<DesktopAgent, String> {
    // Scan persona for security issues
    let scan_result = state.scanner.scan(&request.persona);
    if !scan_result.passed {
        let findings: Vec<String> = scan_result
            .findings
            .iter()
            .map(|f| format!("{}: {}", f.rule, f.description))
            .collect();
        return Err(format!(
            "Persona failed security scan: {}",
            findings.join("; ")
        ));
    }

    let identity = IdentityContract::new(request.persona.clone(), IdentitySource::UserConfig);
    let persona_hash = identity.persona_hash_hex();

    let agent = DesktopAgent {
        id: Uuid::new_v4().to_string(),
        name: request.name,
        icon: request.icon,
        color: request.color,
        persona: request.persona,
        persona_hash: persona_hash.clone(),
        skills: request.skills,
        model: request.model,
        created: Utc::now().to_rfc3339(),
        msg_count: 0,
        status: "ready".to_string(),
        token_budget: 128_000,
        tokens_used: 0,
        source: request.source.unwrap_or_else(|| "clawdesk".to_string()),
    };

    {
        let mut identities = state.identities.write().map_err(|e| e.to_string())?;
        identities.insert(agent.id.clone(), identity);
    }

    // Audit log: agent creation
    state
        .audit_logger
        .log(
            AuditCategory::SessionLifecycle,
            "agent_created",
            AuditActor::System,
            Some(agent.id.clone()),
            serde_json::json!({
                "name": &agent.name,
                "model": &agent.model,
                "persona_hash": &agent.persona_hash,
            }),
            AuditOutcome::Success,
        )
        .await;

    let result = agent.clone();
    {
        let mut agents = state.agents.write().map_err(|e| e.to_string())?;
        agents.insert(agent.id.clone(), agent.clone());
    }

    // Write-through to SochDB
    state.persist_agent(&result.id, &result);
    emit_security_changed(&app, &state).await;

    Ok(result)
}

/// List all registered agents.
#[tauri::command]
pub async fn list_agents(state: State<'_, AppState>) -> Result<Vec<DesktopAgent>, String> {
    let agents = state.agents.read().map_err(|e| e.to_string())?;
    let mut result: Vec<DesktopAgent> = agents.values().cloned().collect();
    result.sort_by(|a, b| a.created.cmp(&b.created));
    Ok(result)
}

/// Delete an agent by ID.
#[tauri::command]
pub async fn delete_agent(agent_id: String, state: State<'_, AppState>, app: AppHandle) -> Result<bool, String> {
    let removed = {
        let mut agents = state.agents.write().map_err(|e| e.to_string())?;
        let removed = agents.remove(&agent_id).is_some();
        if removed {
            if let Ok(mut identities) = state.identities.write() {
                identities.remove(&agent_id);
            }
            if let Ok(mut sessions) = state.sessions.write() {
                sessions.remove(&agent_id);
            }
        }
        removed
    };

    if removed {
        // Delete from SochDB
        state.delete_agent_from_store(&agent_id);
        state
            .audit_logger
            .log(
                AuditCategory::SessionLifecycle,
                "agent_deleted",
                AuditActor::System,
                Some(agent_id),
                serde_json::json!({}),
                AuditOutcome::Success,
            )
            .await;
        emit_security_changed(&app, &state).await;
    }
    Ok(removed)
}

// ═══════════════════════════════════════════════════════════
// OpenClaw Import
// ═══════════════════════════════════════════════════════════

#[tauri::command]
pub async fn import_openclaw_config(
    config_json: String,
    state: State<'_, AppState>,
) -> Result<ImportResult, String> {
    // Scan raw config for secrets/PII
    let scan_result = state.scanner.scan(&config_json);
    let mut warnings = Vec::new();
    if !scan_result.passed {
        for finding in &scan_result.findings {
            warnings.push(format!(
                "Security scan: {} - {}",
                finding.rule, finding.description
            ));
        }
    }

    let config: serde_json::Value =
        serde_json::from_str(&config_json).map_err(|e| format!("Parse error: {}", e))?;

    let mut imported = Vec::new();

    if let Some(gw) = config.get("gateway") {
        if let Some(bind) = gw.get("bind").and_then(|v| v.as_str()) {
            if bind == "0.0.0.0" || bind == "lan" {
                warnings.push(
                    "OpenClaw was bound to 0.0.0.0 (exposed to network). \
                     ClawDesk defaults to 127.0.0.1 + WireGuard tunnel."
                        .to_string(),
                );
            }
        }
        if let Some(auth) = gw.get("auth") {
            if auth.get("mode").and_then(|v| v.as_str()) != Some("token") {
                warnings.push(
                    "No auth token configured in OpenClaw. \
                     ClawDesk uses scoped tokens with per-capability separation."
                        .to_string(),
                );
            }
        }
    }

    if let Some(exec) = config.get("exec") {
        if let Some(approvals) = exec.get("approvals") {
            if approvals.get("set").and_then(|v| v.as_str()) == Some("off") {
                warnings.push(
                    "Tool approvals were disabled in OpenClaw. \
                     ClawDesk enforces ToolPolicy with per-skill ACLs."
                        .to_string(),
                );
            }
        }
    }

    let primary_model = config
        .get("models")
        .and_then(|m| m.get("primary"))
        .and_then(|v| v.as_str())
        .or_else(|| config.get("model").and_then(|m| m.get("primary")).and_then(|v| v.as_str()))
        .unwrap_or("");

    if primary_model.to_lowercase().contains("opus")
        || primary_model.to_lowercase().contains("gpt-4")
    {
        warnings.push(
            "Expensive model in coordinator slot. \
             ClawDesk recommends Haiku for coordination ($0.25/M vs $15/M)."
                .to_string(),
        );
    }

    let custom_agents = config
        .get("customAgents")
        .or_else(|| config.get("agents"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if !custom_agents.is_empty() {
        for (i, agent_val) in custom_agents.iter().enumerate() {
            let name = agent_val
                .get("name")
                .or_else(|| agent_val.get("role"))
                .and_then(|v| v.as_str())
                .unwrap_or("Imported Agent")
                .to_string();

            let role = agent_val
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let icon = match role {
                "coordinator" => "target",
                "researcher" => "search",
                "communicator" => "message-square",
                "worker" => "zap",
                _ => "bot",
            }
            .to_string();

            let persona = agent_val
                .get("systemPrompt")
                .or_else(|| agent_val.get("system"))
                .or_else(|| agent_val.get("prompt"))
                .and_then(|v| v.as_str())
                .unwrap_or("Imported from OpenClaw")
                .to_string();

            let agent_model = agent_val
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or(primary_model);

            let identity = IdentityContract::new(persona.clone(), IdentitySource::UserConfig);
            let agent = DesktopAgent {
                id: format!("import-{}-{}", Uuid::new_v4(), i),
                name: format!("{} (imported)", name),
                icon,
                color: "#6366f1".to_string(),
                persona: persona.clone(),
                persona_hash: identity.persona_hash_hex(),
                skills: infer_skills(agent_val),
                model: map_model(agent_model),
                created: Utc::now().to_rfc3339(),
                msg_count: 0,
                status: "ready".to_string(),
                token_budget: 128_000,
                tokens_used: 0,
                source: "openclaw-import".to_string(),
            };

            if let Ok(mut identities) = state.identities.write() {
                identities.insert(agent.id.clone(), identity);
            }
            imported.push(agent);
        }
    }

    if imported.is_empty() {
        let persona = config
            .get("systemPrompt")
            .or_else(|| config.get("soul"))
            .and_then(|v| v.as_str())
            .unwrap_or("Imported from OpenClaw.")
            .to_string();

        let identity = IdentityContract::new(persona.clone(), IdentitySource::UserConfig);
        let agent = DesktopAgent {
            id: format!("import-{}", Uuid::new_v4()),
            name: "OpenClaw Default Agent".to_string(),
            icon: "bot".to_string(),
            color: "#6366f1".to_string(),
            persona: persona.clone(),
            persona_hash: identity.persona_hash_hex(),
            skills: vec!["web-search".into(), "code-exec".into(), "files".into()],
            model: map_model(primary_model),
            created: Utc::now().to_rfc3339(),
            msg_count: 0,
            status: "ready".to_string(),
            token_budget: 128_000,
            tokens_used: 0,
            source: "openclaw-import".to_string(),
        };
        if let Ok(mut identities) = state.identities.write() {
            identities.insert(agent.id.clone(), identity);
        }
        imported.push(agent);
    }

    {
        let mut agents = state.agents.write().map_err(|e| e.to_string())?;
        for agent in &imported {
            agents.insert(agent.id.clone(), agent.clone());
        }
    }

    state
        .audit_logger
        .log(
            AuditCategory::ConfigChange,
            "openclaw_import",
            AuditActor::System,
            None,
            serde_json::json!({
                "agents_imported": imported.len(),
                "warnings": warnings.len(),
            }),
            AuditOutcome::Success,
        )
        .await;

    state.persist();

    Ok(ImportResult {
        success: true,
        agents: imported,
        warnings,
        error: None,
    })
}

fn map_model(m: &str) -> String {
    let ml = m.to_lowercase();
    if ml.contains("haiku") || ml.contains("flash") || ml.contains("cheap") {
        "haiku".into()
    } else if ml.contains("opus") || ml.contains("gpt-4") || ml.contains("expensive") {
        "opus".into()
    } else if ml.contains("ollama") || ml.contains("local") || ml.contains("deepseek") {
        "local".into()
    } else {
        "sonnet".into()
    }
}

fn infer_skills(agent: &serde_json::Value) -> Vec<String> {
    let text = agent.to_string().to_lowercase();
    let mut skills = Vec::new();
    let patterns: &[(&str, &[&str])] = &[
        ("web-search", &["search", "web", "browse", "fetch"]),
        ("code-exec", &["code", "exec", "tool", "run", "python"]),
        ("files", &["file", "read", "write", "fs"]),
        ("cron", &["cron", "heartbeat", "schedule"]),
        ("email", &["email", "mail", "imap"]),
        ("git", &["git", "commit", "repo", "branch"]),
        ("alerts", &["alert", "notify", "telegram", "discord"]),
    ];
    for (skill, keywords) in patterns {
        if keywords.iter().any(|kw| text.contains(kw)) {
            skills.push(skill.to_string());
        }
    }
    if skills.is_empty() {
        skills.push("web-search".into());
        skills.push("files".into());
    }
    skills
}

// ═══════════════════════════════════════════════════════════
// Chat / Messaging — real scanner + audit integration
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub agent_id: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct SendMessageResponse {
    pub message: ChatMessage,
    pub trace: Vec<TraceEntry>,
}

/// Send a message to an agent and get a response via real AgentRunner + LLM.
///
/// 1. Scans content through CascadeScanner for security threats
/// 2. Verifies the agent's IdentityContract
/// 3. Logs message to the SHA-256 audit chain
/// 4. Resolves provider from registry based on agent model
/// 5. Builds conversation history and runs real AgentRunner pipeline
/// 6. Returns response with real token counts and tool usage
#[tauri::command]
pub async fn send_message(
    request: SendMessageRequest,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<SendMessageResponse, String> {
    let start = Instant::now();
    let now = Utc::now();

    // ── SochDB TraceStore: start a durable trace run for this message ──
    let soch_trace = {
        let mut resource = HashMap::new();
        resource.insert("agent_id".into(), request.agent_id.clone());
        resource.insert("channel".into(), "tauri".into());
        state.trace_store.start_run("send_message", resource).ok()
    };
    let soch_trace_id = soch_trace.as_ref().map(|r| r.trace_id.clone());

    // Start a security-scan span
    let security_span_id = soch_trace_id.as_ref().and_then(|tid| {
        state.trace_store.start_span(tid, "security_scan", None, SpanKind::Internal)
            .ok().map(|s| s.span_id)
    });

    // Security scan on user input
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
                    serde_json::json!({ "findings": critical }),
                    AuditOutcome::Blocked,
                )
                .await;
            let _ = app.emit(
                "system:alert",
                serde_json::json!({
                    "level": "error",
                    "title": "Message blocked",
                    "message": format!("Blocked by security scanner: {}", critical.join("; ")),
                }),
            );
            return Err(format!("Message blocked by security scanner: {}", critical.join("; ")));
        }
    }

    // ── End security-scan span ──
    if let (Some(tid), Some(sid)) = (&soch_trace_id, &security_span_id) {
        let _ = state.trace_store.end_span(tid, sid, SpanStatusCode::Ok, None);
    }

    let agent = {
        let agents = state.agents.read().map_err(|e| e.to_string())?;
        agents
            .get(&request.agent_id)
            .cloned()
            .ok_or_else(|| format!("Agent {} not found", request.agent_id))?
    };

    let identity_verified = {
        let identities = state.identities.read().map_err(|e| e.to_string())?;
        identities.get(&agent.id).map(|ic| ic.verify()).unwrap_or(false)
    };

    // Store user message in session (hot cache + SochDB write-through)
    let user_msg = ChatMessage {
        id: Uuid::new_v4().to_string(),
        role: "user".to_string(),
        content: request.content.clone(),
        timestamp: now.to_rfc3339(),
        metadata: None,
    };
    {
        let mut sessions = state.sessions.write().map_err(|e| e.to_string())?;
        let session_msgs = sessions.entry(agent.id.clone()).or_default();
        session_msgs.push(user_msg);
        // Write-through to SochDB
        state.persist_session(&agent.id, session_msgs);
    }

    // Audit log: user message
    state
        .audit_logger
        .log(
            AuditCategory::MessageSend,
            "user_message",
            AuditActor::User {
                sender_id: "desktop".into(),
                channel: "tauri".into(),
            },
            Some(agent.id.clone()),
            serde_json::json!({
                "content_length": request.content.len(),
                "scan_passed": scan.passed,
            }),
            AuditOutcome::Success,
        )
        .await;

    // Resolve the real LLM provider for this agent's model.
    // Task 26: Try ProviderNegotiator first for capability-aware routing,
    // fall back to hardcoded resolve_provider if negotiator has no match.
    let provider = {
        use clawdesk_providers::capability::ProviderCaps;
        let negotiator = state.negotiator.read().map_err(|e| e.to_string())?;
        let required = ProviderCaps::TEXT_COMPLETION.union(ProviderCaps::SYSTEM_PROMPT);
        match negotiator.resolve_model(&agent.model, required) {
            Some((p, _resolved_model)) => Arc::clone(p),
            None => {
                drop(negotiator);
                state.resolve_provider(&agent.model)?
            }
        }
    };

    // Build conversation history from session (convert Tauri ChatMessages to provider ChatMessages)
    let history = {
        let sessions = state.sessions.read().map_err(|e| e.to_string())?;
        let session_msgs = sessions.get(&agent.id).cloned().unwrap_or_default();
        session_msgs
            .iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "system" => MessageRole::System,
                    _ => MessageRole::User,
                };
                clawdesk_providers::ChatMessage::new(role, m.content.as_str())
            })
            .collect::<Vec<_>>()
    };

    // Task 27: Context Guard — check if history exceeds αC and trigger compaction
    {
        use clawdesk_domain::context_guard::{ContextGuard, ContextGuardConfig, GuardAction};
        use clawdesk_types::tokenizer::estimate_tokens;

        let total_history_tokens: usize = history.iter()
            .map(|m| estimate_tokens(&m.content))
            .sum();

        let mut guards = state.context_guards.write().map_err(|e| e.to_string())?;
        let guard = guards.entry(agent.id.clone()).or_insert_with(|| {
            ContextGuard::new(ContextGuardConfig {
                context_limit: agent.token_budget,
                trigger_threshold: 0.80,
                response_reserve: 8_192,
                circuit_breaker_threshold: 3,
                circuit_breaker_cooldown: Duration::from_secs(60),
            })
        });
        guard.set_token_count(total_history_tokens);

        match guard.check() {
            GuardAction::Ok => {}
            GuardAction::Compact(level) => {
                let _ = app.emit("agent-event", serde_json::json!({
                    "agent_id": &agent.id,
                    "event": { "type": "ContextGuardAction", "action": format!("compact_{:?}", level), "token_count": total_history_tokens, "threshold": 0.80 },
                }));
            }
            GuardAction::ForceTruncate { keep_last_n } => {
                let _ = app.emit("agent-event", serde_json::json!({
                    "agent_id": &agent.id,
                    "event": { "type": "ContextGuardAction", "action": format!("truncate_to_{}", keep_last_n), "token_count": total_history_tokens, "threshold": 0.80 },
                }));
            }
            GuardAction::CircuitBroken => {
                return Err("Context guard circuit breaker open — too many compaction failures. Try starting a new conversation.".to_string());
            }
        }
    }

    // Task 28: Use PromptBuilder for system prompt assembly
    let (system_prompt, prompt_manifest) = {
        use clawdesk_domain::prompt_builder::{PromptBuilder, PromptBudget, RuntimeContext, ScoredSkill};
        use clawdesk_types::tokenizer::estimate_tokens;

        let budget = PromptBudget {
            total: agent.token_budget,
            response_reserve: 8_192,
            identity_cap: 2_000,
            skills_cap: 4_096,
            memory_cap: 4_096,
            history_floor: 2_000,
            runtime_cap: 512,
            safety_cap: 1_024,
        };

        let runtime_ctx = RuntimeContext {
            datetime: Utc::now().to_rfc3339(),
            channel_description: Some("Tauri desktop".into()),
            model_name: Some(agent.model.clone()),
            metadata: vec![],
        };

        // Score active skills for knapsack packing
        let scored_skills: Vec<ScoredSkill> = {
            let reg = state.skill_registry.read().map_err(|e| e.to_string())?;
            reg.active_skills()
                .iter()
                .filter(|s| {
                    agent.skills.contains(&s.manifest.display_name)
                        || agent.skills.contains(&s.manifest.id.as_str().to_string())
                })
                .map(|s| ScoredSkill {
                    skill_id: s.manifest.id.as_str().to_string(),
                    display_name: s.manifest.display_name.clone(),
                    prompt_fragment: s.prompt_fragment.clone(),
                    token_cost: estimate_tokens(&s.prompt_fragment),
                    priority_weight: 1.0,
                    relevance: 1.0,
                })
                .collect()
        };

        match PromptBuilder::new(budget) {
            Ok(builder) => {
                let (assembled, manifest) = builder
                    .identity(agent.persona.clone())
                    .runtime(runtime_ctx)
                    .skills(scored_skills)
                    .build();

                // Store manifest for Task 28 inspector command
                if let Ok(mut manifests) = state.prompt_manifests.write() {
                    manifests.insert(agent.id.clone(), manifest.clone());
                }

                (assembled.text, Some(manifest))
            }
            Err(_) => {
                // Fallback to raw persona if PromptBuilder fails validation
                (agent.persona.clone(), None)
            }
        }
    };

    // ── SochDB SemanticCache: check for cached response before LLM call ──
    let cache_namespace = format!("agent:{}", agent.id);
    let cache_hit = state.semantic_cache.lookup(
        &request.content,
        &cache_namespace,
        0, // no allowed-set filtering
        None, // no pre-computed embedding
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

    // If we have a cache hit, short-circuit the LLM call entirely
    if let Some(cached_response) = cache_hit {
        // End the trace run as cache-hit
        if let Some(tid) = &soch_trace_id {
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
        // Store the cached response in session
        {
            let mut sessions = state.sessions.write().map_err(|e| e.to_string())?;
            let session_msgs = sessions.entry(agent.id.clone()).or_default();
            session_msgs.push(assistant_msg.clone());
            state.persist_session(&agent.id, session_msgs);
        }
        let _ = app.emit("incoming:message", serde_json::json!({
            "agent_id": agent.id,
            "preview": cached_response.chars().take(120).collect::<String>(),
            "timestamp": assistant_msg.timestamp,
            "cache_hit": true,
        }));
        return Ok(SendMessageResponse {
            message: assistant_msg,
            trace: vec![TraceEntry {
                timestamp: now.format("%H:%M:%S%.3f").to_string(),
                event: "CacheHit".to_string(),
                detail: format!("semantic_cache hit, skipped LLM call, elapsed={}ms", elapsed_ms),
            }],
        });
    }

    // ── SochDB KnowledgeGraph: ensure agent and session nodes + edge ──
    {
        let mut agent_props = HashMap::new();
        agent_props.insert("name".into(), serde_json::json!(agent.name));
        agent_props.insert("model".into(), serde_json::json!(agent.model));
        let _ = state.knowledge_graph.add_node(&agent.id, "agent", Some(agent_props));

        let session_id = format!("session:{}", agent.id);
        let mut session_props = HashMap::new();
        session_props.insert("started_at".into(), serde_json::json!(now.to_rfc3339()));
        let _ = state.knowledge_graph.add_node(&session_id, "session", Some(session_props));
        let _ = state.knowledge_graph.add_edge(&agent.id, "has_session", &session_id, None);
    }

    // ── Start LLM-call span ──
    let llm_span_id = soch_trace_id.as_ref().and_then(|tid| {
        state.trace_store.start_span(tid, "llm_call", None, SpanKind::Client)
            .ok().map(|s| s.span_id)
    });

    // Configure the agent runner
    let model_id = AppState::resolve_model_id(&agent.model);
    let config = AgentConfig {
        model: model_id,
        system_prompt: system_prompt.clone(),
        max_tool_rounds: 25,
        context_limit: agent.token_budget,
        response_reserve: 8_192,
        ..Default::default()
    };

    // Set up event channel for trace collection + live frontend streaming
    let (event_tx, mut event_rx) = broadcast::channel::<AgentEvent>(128);
    let event_log = Arc::new(tokio::sync::Mutex::new(Vec::<AgentEvent>::new()));
    let event_log_task = Arc::clone(&event_log);
    let app_for_events = app.clone();
    let agent_id_for_events = agent.id.clone();

    let runner = AgentRunner::new(
        provider,
        Arc::clone(&state.tool_registry),
        config,
        state.cancel.clone(),
    )
    .with_events(event_tx);

    let forward_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    {
                        let mut guard = event_log_task.lock().await;
                        guard.push(event.clone());
                    }
                    let _ = emit_agent_event(&app_for_events, &agent_id_for_events, &event);
                    if let AgentEvent::Response { content, .. } = &event {
                        emit_stream_chunks(&app_for_events, &agent_id_for_events, content);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });

    // Run the real agent pipeline
    let run_result = runner.run(history, system_prompt).await;

    // Give the forwarder a brief window to flush final events, then stop it.
    tokio::time::sleep(Duration::from_millis(20)).await;
    if !forward_task.is_finished() {
        forward_task.abort();
    }
    let _ = forward_task.await;

    let agent_response = run_result.map_err(|e| {
        let msg = format!("Agent execution failed: {}", e);
        let _ = app.emit(
            "system:alert",
            serde_json::json!({
                "level": "error",
                "title": "Agent execution failed",
                "message": msg.clone(),
            }),
        );
        msg
    })?;

    // ── End LLM-call span ──
    if let (Some(tid), Some(sid)) = (&soch_trace_id, &llm_span_id) {
        let _ = state.trace_store.end_span(tid, sid, SpanStatusCode::Ok, None);
    }

    // ── SochDB SemanticCache: store the LLM response for future cache hits ──
    {
        let _ = state.semantic_cache.store(
            &request.content,
            &cache_namespace,
            0,
            agent_response.content.as_bytes(),
            None,
            vec![],
            None,
        );
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;

    // Collect trace events
    let ts = now.format("%H:%M:%S%.3f").to_string();
    let mut trace = Vec::new();
    let mut tools_used = Vec::new();

    let collected_events = { event_log.lock().await.clone() };
    for event in collected_events {
        match event {
            AgentEvent::RoundStart { round } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "RoundStart".to_string(),
                    detail: format!("round={}", round),
                });
            }
            AgentEvent::PromptAssembled {
                total_tokens,
                skills_included,
                skills_excluded: _,
                memory_fragments,
                budget_utilization,
            } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "PromptAssembled".to_string(),
                    detail: format!(
                        "tokens={} skills=[{}] memory={} budget={:.1}%",
                        total_tokens,
                        skills_included.join(","),
                        memory_fragments,
                        budget_utilization * 100.0,
                    ),
                });
            }
            AgentEvent::IdentityVerified { hash_match, version } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "IdentityVerified".to_string(),
                    detail: format!("hash_match={} version={}", hash_match, version),
                });
            }
            AgentEvent::ToolStart { name, args: _ } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "ToolStart".to_string(),
                    detail: format!("name={}", name),
                });
            }
            AgentEvent::ToolEnd {
                name,
                success,
                duration_ms,
            } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "ToolEnd".to_string(),
                    detail: format!("name={} ok={} {}ms", name, success, duration_ms),
                });
                tools_used.push(ToolUsageSummary {
                    name,
                    success,
                    duration_ms,
                });
            }
            AgentEvent::Compaction {
                level,
                tokens_before,
                tokens_after,
            } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "Compaction".to_string(),
                    detail: format!("{:?} {} -> {} tokens", level, tokens_before, tokens_after),
                });
            }
            AgentEvent::ContextGuardAction {
                action,
                token_count,
                threshold,
            } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "ContextGuard".to_string(),
                    detail: format!(
                        "action={} tokens={} threshold={:.2}",
                        action, token_count, threshold
                    ),
                });
            }
            AgentEvent::FallbackTriggered {
                from_model,
                to_model,
                reason,
                ..
            } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "Fallback".to_string(),
                    detail: format!("{} -> {} reason={}", from_model, to_model, reason),
                });
            }
            AgentEvent::Response { finish_reason, .. } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "Response".to_string(),
                    detail: format!(
                        "finish={:?} tokens={}",
                        finish_reason, agent_response.output_tokens
                    ),
                });
            }
            AgentEvent::Done { total_rounds } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "Done".to_string(),
                    detail: format!("rounds={}", total_rounds),
                });
            }
            AgentEvent::Error { error } => {
                trace.push(TraceEntry {
                    timestamp: ts.clone(),
                    event: "Error".to_string(),
                    detail: error,
                });
            }
            _ => {}
        }
    }

    // Add identity verification trace if not already emitted by runner
    if !trace.iter().any(|t| t.event == "IdentityVerified") {
        trace.insert(0, TraceEntry {
            timestamp: ts.clone(),
            event: "IdentityVerified".to_string(),
            detail: format!("hash_match={} version=1", identity_verified),
        });
    }

    // Record real token usage and costs
    let input_tokens = agent_response.input_tokens;
    let output_tokens = agent_response.output_tokens;
    let cost_usd = {
        let (cpi, cpo) = model_cost_rates(&agent.model);
        (input_tokens as f64 * cpi / 1_000_000.0) + (output_tokens as f64 * cpo / 1_000_000.0)
    };
    state.record_usage(&agent.model, input_tokens, output_tokens);

    // ── SochDB TraceStore: record token metrics and finalize run ──
    if let Some(tid) = &soch_trace_id {
        let cost_millicents = (cost_usd * 100_000.0) as u64; // USD → millicents
        let _ = state.trace_store.update_run_metrics(
            tid,
            (input_tokens + output_tokens) as u64,
            cost_millicents,
        );
        let _ = state.trace_store.log_cost(tid, CostEvent {
            cost_type: "llm_call".into(),
            amount: (input_tokens + output_tokens) as u64,
            unit_price_millicents: cost_usd * 100_000.0 / (input_tokens + output_tokens).max(1) as f64,
            total_millicents: cost_millicents,
            model: Some(agent.model.clone()),
        });
        let _ = state.trace_store.end_run(tid, TraceStatus::Ok);
    }

    // ── SochDB KnowledgeGraph: record message node + edges ──
    {
        let msg_node_id = format!("msg:{}", Uuid::new_v4());
        let mut msg_props = HashMap::new();
        msg_props.insert("role".into(), serde_json::json!("user"));
        msg_props.insert("content_len".into(), serde_json::json!(request.content.len()));
        msg_props.insert("timestamp".into(), serde_json::json!(now.to_rfc3339()));
        let _ = state.knowledge_graph.add_node(&msg_node_id, "message", Some(msg_props));
        let session_id = format!("session:{}", agent.id);
        let _ = state.knowledge_graph.add_edge(&session_id, "contains", &msg_node_id, None);
    }

    // Content classification on response
    let _classification = state.scanner.classify_content(&agent_response.content);
    if !state.scanner.is_safe(&agent_response.content) {
        trace.push(TraceEntry {
            timestamp: ts.clone(),
            event: "ContentScan".to_string(),
            detail: "response_flagged".to_string(),
        });
    }

    trace.push(TraceEntry {
        timestamp: ts.clone(),
        event: "Done".to_string(),
        detail: format!(
            "rounds={} cost=${:.6} elapsed={}ms input_tokens={} output_tokens={}",
            agent_response.total_rounds, cost_usd, elapsed_ms, input_tokens, output_tokens
        ),
    });

    // Query activated skills for metadata
    let activated: Vec<String> = {
        let reg = state.skill_registry.read().map_err(|e| e.to_string())?;
        reg.active_skills()
            .iter()
            .filter(|s| agent.skills.contains(&s.manifest.display_name) || agent.skills.contains(&s.manifest.id.as_str().to_string()))
            .map(|s| s.manifest.display_name.clone())
            .collect()
    };

    let compaction_info = trace.iter().find(|t| t.event == "Compaction").map(|t| {
        CompactionInfo {
            level: t.detail.split_whitespace().next().unwrap_or("unknown").to_string(),
            tokens_before: 0,
            tokens_after: 0,
        }
    });

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

    // Audit log: assistant response
    state
        .audit_logger
        .log(
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
        )
        .await;

    {
        let mut sessions = state.sessions.write().map_err(|e| e.to_string())?;
        let session_msgs = sessions.entry(agent.id.clone()).or_default();
        session_msgs.push(assistant_msg.clone());
        // Write-through to SochDB
        state.persist_session(&agent.id, session_msgs);
    }
    {
        let mut traces = state.traces.write().map_err(|e| e.to_string())?;
        traces.insert(agent.id.clone(), trace.clone());
    }
    {
        let mut agents = state.agents.write().map_err(|e| e.to_string())?;
        if let Some(a) = agents.get_mut(&agent.id) {
            a.msg_count += 1;
            a.tokens_used += (input_tokens + output_tokens) as usize;
            // Write-through agent update to SochDB
            state.persist_agent(&agent.id, a);
        }
    }

    let _ = app.emit(
        "incoming:message",
        serde_json::json!({
            "agent_id": agent.id,
            "preview": assistant_msg.content.chars().take(120).collect::<String>(),
            "timestamp": assistant_msg.timestamp,
        }),
    );
    state.persist();
    emit_metrics_updated(&app, &state);
    emit_security_changed(&app, &state).await;

    Ok(SendMessageResponse { message: assistant_msg, trace })
}

/// Get message history for a session.
#[tauri::command]
pub async fn get_session_messages(agent_id: String, state: State<'_, AppState>) -> Result<Vec<ChatMessage>, String> {
    let sessions = state.sessions.read().map_err(|e| e.to_string())?;
    Ok(sessions.get(&agent_id).cloned().unwrap_or_default())
}

#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub agent_id: String,
    pub title: String,
    pub last_activity: String,
    pub message_count: usize,
    pub pending_approvals: usize,
    pub routine_generated: bool,
    pub has_proof_outputs: bool,
}

#[tauri::command]
pub async fn list_sessions(state: State<'_, AppState>) -> Result<Vec<SessionSummary>, String> {
    let sessions = state.sessions.read().map_err(|e| e.to_string())?;
    let agents = state.agents.read().map_err(|e| e.to_string())?;

    let mut summaries: Vec<SessionSummary> = sessions
        .iter()
        .map(|(agent_id, messages)| {
            let agent_name = agents
                .get(agent_id)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| "Conversation".to_string());
            let last_activity = messages
                .last()
                .map(|m| m.timestamp.clone())
                .unwrap_or_else(|| Utc::now().to_rfc3339());
            SessionSummary {
                agent_id: agent_id.clone(),
                title: agent_name,
                last_activity,
                message_count: messages.len(),
                pending_approvals: 0,
                routine_generated: false,
                has_proof_outputs: messages.iter().any(|m| m.role == "assistant"),
            }
        })
        .collect();

    summaries.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
    Ok(summaries)
}

// ═══════════════════════════════════════════════════════════
// Skills Registry — real SkillRegistry queries
// ═══════════════════════════════════════════════════════════

/// List all skills from the real SkillRegistry (loaded from bundled + disk).
#[tauri::command]
pub async fn list_skills(state: State<'_, AppState>) -> Result<Vec<SkillDescriptor>, String> {
    let reg = state.skill_registry.read().map_err(|e| e.to_string())?;
    let infos = reg.list();
    Ok(infos
        .into_iter()
        .map(|info| {
            let state_str = match info.state {
                SkillState::Active => "active",
                SkillState::Loaded => "loaded",
                SkillState::Resolved => "resolved",
                SkillState::Discovered => "discovered",
                SkillState::Disabled => "disabled",
                SkillState::Failed => "failed",
            };
            let icon = match info.id.namespace() {
                "core" => "code",
                "channel" => "send",
                "media" => "zap",
                "dev" => "code",
                "research" => "file",
                _ => "globe",
            };
            let source_label = match &info.source {
                SkillSource::Builtin => "builtin".to_string(),
                SkillSource::Local { path } => format!("local:{}", path),
                SkillSource::Remote { url, .. } => format!("remote:{}", url),
            };
            SkillDescriptor {
                id: info.id.as_str().to_string(),
                name: info.display_name,
                description: format!("v{} - {}", info.version, source_label),
                category: info.id.namespace().to_string(),
                estimated_tokens: info.estimated_tokens,
                state: state_str.to_string(),
                verified: info.source == SkillSource::Builtin,
                icon: icon.to_string(),
            }
        })
        .collect())
}

/// Activate a skill in the real SkillRegistry.
#[tauri::command]
pub async fn activate_skill(skill_id: String, state: State<'_, AppState>, app: AppHandle) -> Result<bool, String> {
    {
        let mut reg = state.skill_registry.write().map_err(|e| e.to_string())?;
        let id = SkillId::from(skill_id.as_str());
        reg.activate(&id).map_err(|e| e.to_string())?;
    }

    state
        .audit_logger
        .log(
            AuditCategory::ConfigChange,
            "skill_activated",
            AuditActor::System,
            Some(skill_id),
            serde_json::json!({}),
            AuditOutcome::Success,
        )
        .await;
    emit_security_changed(&app, &state).await;

    Ok(true)
}

/// Deactivate a skill in the real SkillRegistry.
#[tauri::command]
pub async fn deactivate_skill(skill_id: String, state: State<'_, AppState>, app: AppHandle) -> Result<bool, String> {
    {
        let mut reg = state.skill_registry.write().map_err(|e| e.to_string())?;
        let id = SkillId::from(skill_id.as_str());
        reg.deactivate(&id).map_err(|e| e.to_string())?;
    }

    state
        .audit_logger
        .log(
            AuditCategory::ConfigChange,
            "skill_deactivated",
            AuditActor::System,
            Some(skill_id),
            serde_json::json!({}),
            AuditOutcome::Success,
        )
        .await;
    emit_security_changed(&app, &state).await;

    Ok(true)
}

// ═══════════════════════════════════════════════════════════
// Pipelines
// ═══════════════════════════════════════════════════════════

#[tauri::command]
pub async fn list_pipelines(state: State<'_, AppState>) -> Result<Vec<PipelineDescriptor>, String> {
    let pipelines = state.pipelines.read().map_err(|e| e.to_string())?;
    Ok(pipelines.clone())
}

#[derive(Debug, Deserialize)]
pub struct CreatePipelineRequest {
    pub name: String,
    pub description: String,
    pub steps: Vec<PipelineNodeDescriptor>,
    pub edges: Vec<(usize, usize)>,
}

#[tauri::command]
pub async fn create_pipeline(request: CreatePipelineRequest, state: State<'_, AppState>) -> Result<PipelineDescriptor, String> {
    let pipeline = PipelineDescriptor {
        id: Uuid::new_v4().to_string(),
        name: request.name,
        description: request.description,
        steps: request.steps,
        edges: request.edges,
        created: Utc::now().to_rfc3339(),
    };
    let result = pipeline.clone();
    {
        let mut pipelines = state.pipelines.write().map_err(|e| e.to_string())?;
        pipelines.push(pipeline);
    }

    // Write-through to SochDB
    state.persist_pipeline(&result);

    state
        .audit_logger
        .log(
            AuditCategory::ConfigChange,
            "pipeline_created",
            AuditActor::System,
            Some(result.id.clone()),
            serde_json::json!({ "name": &result.name, "steps": result.steps.len() }),
            AuditOutcome::Success,
        )
        .await;

    Ok(result)
}

/// Run a pipeline by executing each agent step via real AgentRunner.
///
/// Steps are executed in topological order following the DAG edges.
/// Non-agent steps (input, gate, output) pass data through.
/// Agent steps invoke the real LLM provider and accumulate results.
#[tauri::command]
pub async fn run_pipeline(
    pipeline_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<serde_json::Value, String> {
    let pipeline = {
        let pipelines = state.pipelines.read().map_err(|e| e.to_string())?;
        pipelines.iter().find(|p| p.id == pipeline_id)
            .cloned()
            .ok_or_else(|| format!("Pipeline {} not found", pipeline_id))?
    };

    let start = Instant::now();
    let mut step_results: Vec<serde_json::Value> = Vec::new();
    let mut previous_output = String::from("Pipeline started.");

    for (i, step) in pipeline.steps.iter().enumerate() {
        let step_start = Instant::now();

        match step.node_type.as_str() {
            "input" => {
                step_results.push(serde_json::json!({
                    "step_index": i,
                    "label": step.label,
                    "node_type": "input",
                    "success": true,
                    "duration_ms": step_start.elapsed().as_millis() as u64,
                    "output": "Pipeline input received.",
                }));
            }
            "agent" => {
                // Resolve the agent and provider for this step
                let agent_result = if let Some(ref agent_id) = step.agent_id {
                    let agents = state.agents.read().map_err(|e| e.to_string())?;
                    agents.get(agent_id).cloned()
                } else {
                    None
                };

                let model = step.model.as_deref().unwrap_or("sonnet");
                let model_lower = model.to_lowercase();

                match state.resolve_provider(&model_lower) {
                    Ok(provider) => {
                        let model_id = AppState::resolve_model_id(&model_lower);
                        let system_prompt = agent_result
                            .as_ref()
                            .map(|a| a.persona.clone())
                            .unwrap_or_else(|| format!("You are a {} agent.", step.label));

                        let config = AgentConfig {
                            model: model_id,
                            system_prompt: system_prompt.clone(),
                            max_tool_rounds: 10,
                            context_limit: 128_000,
                            response_reserve: 4_096,
                            ..Default::default()
                        };

                        let runner = AgentRunner::new(
                            provider,
                            Arc::clone(&state.tool_registry),
                            config,
                            state.cancel.clone(),
                        );

                        let history = vec![
                            clawdesk_providers::ChatMessage::new(
                                MessageRole::User,
                                format!("Previous step output:\n{}\n\nProcess this as the {} step.", previous_output, step.label),
                            ),
                        ];

                        match runner.run(history, system_prompt).await {
                            Ok(response) => {
                                let step_ms = step_start.elapsed().as_millis() as u64;
                                state.record_usage(&model_lower, response.input_tokens, response.output_tokens);
                                previous_output = response.content.clone();
                                step_results.push(serde_json::json!({
                                    "step_index": i,
                                    "label": step.label,
                                    "node_type": "agent",
                                    "success": true,
                                    "duration_ms": step_ms,
                                    "input_tokens": response.input_tokens,
                                    "output_tokens": response.output_tokens,
                                    "total_rounds": response.total_rounds,
                                    "output_preview": &response.content[..response.content.len().min(200)],
                                }));
                            }
                            Err(e) => {
                                let step_ms = step_start.elapsed().as_millis() as u64;
                                step_results.push(serde_json::json!({
                                    "step_index": i,
                                    "label": step.label,
                                    "node_type": "agent",
                                    "success": false,
                                    "duration_ms": step_ms,
                                    "error": e.to_string(),
                                }));
                            }
                        }
                    }
                    Err(e) => {
                        step_results.push(serde_json::json!({
                            "step_index": i,
                            "label": step.label,
                            "node_type": "agent",
                            "success": false,
                            "duration_ms": step_start.elapsed().as_millis() as u64,
                            "error": format!("Provider resolution failed: {}", e),
                        }));
                    }
                }
            }
            "gate" => {
                // Gates pass data through (approval gates would need UI interaction)
                step_results.push(serde_json::json!({
                    "step_index": i,
                    "label": step.label,
                    "node_type": "gate",
                    "success": true,
                    "duration_ms": step_start.elapsed().as_millis() as u64,
                    "output": "Gate passed.",
                }));
            }
            "output" => {
                step_results.push(serde_json::json!({
                    "step_index": i,
                    "label": step.label,
                    "node_type": "output",
                    "success": true,
                    "duration_ms": step_start.elapsed().as_millis() as u64,
                    "output": previous_output,
                }));
            }
            other => {
                step_results.push(serde_json::json!({
                    "step_index": i,
                    "label": step.label,
                    "node_type": other,
                    "success": true,
                    "duration_ms": step_start.elapsed().as_millis() as u64,
                }));
            }
        }
    }

    let total_ms = start.elapsed().as_millis() as u64;
    let all_success = step_results.iter().all(|s| s.get("success").and_then(|v| v.as_bool()).unwrap_or(false));

    state
        .audit_logger
        .log(
            AuditCategory::ToolExecution,
            "pipeline_executed",
            AuditActor::System,
            Some(pipeline_id.clone()),
            serde_json::json!({
                "pipeline": &pipeline.name,
                "steps": pipeline.steps.len(),
                "success": all_success,
                "total_duration_ms": total_ms,
            }),
            if all_success { AuditOutcome::Success } else { AuditOutcome::Failed },
        )
        .await;

    let pipeline_name = pipeline.name.clone();
    emit_metrics_updated(&app, &state);
    let _ = app.emit(
        "routine:executed",
        serde_json::json!({
            "pipeline_id": pipeline_id,
            "pipeline_name": pipeline_name,
            "success": all_success,
            "total_duration_ms": total_ms,
        }),
    );

    Ok(serde_json::json!({
        "pipeline_id": pipeline_id, "pipeline_name": pipeline.name,
        "success": all_success, "steps": step_results, "total_duration_ms": total_ms,
    }))
}

// ═══════════════════════════════════════════════════════════
// Monitoring — real metrics from backend services
// ═══════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_metrics(state: State<'_, AppState>) -> Result<CostMetrics, String> {
    let model_costs = state.model_costs.read().map_err(|e| e.to_string())?;
    let model_breakdown: Vec<ModelCostEntry> = model_costs
        .iter()
        .map(|(model, (input, output, cost_micro))| ModelCostEntry {
            model: model_display_name(model),
            input_tokens: *input, output_tokens: *output,
            cost: *cost_micro as f64 / 1_000_000.0,
        })
        .collect();
    Ok(CostMetrics {
        today_cost: state.cost_today_usd(),
        today_input_tokens: state.total_input_tokens.load(std::sync::atomic::Ordering::Relaxed),
        today_output_tokens: state.total_output_tokens.load(std::sync::atomic::Ordering::Relaxed),
        model_breakdown,
    })
}

/// Security status — queries real CascadeScanner and AuditLogger.
#[tauri::command]
pub async fn get_security_status(state: State<'_, AppState>) -> Result<SecurityStatus, String> {
    let identity_contracts = {
        let identities = state.identities.read().map_err(|e| e.to_string())?;
        identities.len()
    };
    let recent_entries = state.audit_logger.recent(10000).await;
    let audit_entries = recent_entries.len();
    let chain = state.audit_logger.verify_chain().await;
    let tunnel_snap = state.tunnel_metrics.snapshot();
    let tunnel_active = tunnel_snap.active_peers > 0 || tunnel_snap.total_rx_bytes > 0;

    Ok(SecurityStatus {
        gateway_bind: "127.0.0.1:18789 (loopback only)".into(),
        tunnel_active,
        tunnel_endpoint: format!(
            "WireGuard tunnel - {} peers, {} bytes rx",
            tunnel_snap.active_peers, tunnel_snap.total_rx_bytes
        ),
        auth_mode: "Scoped tokens (chat|admin|tools separate)".into(),
        scoped_tokens: true,
        identity_contracts,
        skill_scanning: format!(
            "CascadeScanner (Aho-Corasick + Regex) - chain valid: {}, entries: {}",
            chain.valid, chain.entries_checked
        ),
        rate_limiter: "ShardedRateLimiter - 256KB fixed, lock-free".into(),
        mdns_disabled: true,
        scanner_patterns: state.scanner_pattern_count,
        audit_entries,
    })
}

#[tauri::command]
pub async fn get_agent_trace(agent_id: Option<String>, state: State<'_, AppState>) -> Result<Vec<TraceEntry>, String> {
    let traces = state.traces.read().map_err(|e| e.to_string())?;
    if let Some(id) = agent_id {
        Ok(traces.get(&id).cloned().unwrap_or_default())
    } else {
        Ok(traces.values().last().cloned().unwrap_or_default())
    }
}

// ═══════════════════════════════════════════════════════════
// Tunnel
// ═══════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_tunnel_status(state: State<'_, AppState>) -> Result<TunnelMetricsSnapshot, String> {
    Ok(state.tunnel_metrics.snapshot())
}

#[derive(Debug, Serialize)]
pub struct InviteResponse {
    pub invite_code: String,
    pub qr_text: String,
    pub expires_at: u64,
    pub label: String,
}

#[tauri::command]
pub async fn create_invite(label: String, endpoint: String, ttl_hours: Option<u64>, state: State<'_, AppState>) -> Result<InviteResponse, String> {
    let gateway_pubkey = [0u8; 32];
    let ttl = Duration::from_secs(ttl_hours.unwrap_or(24) * 3600);
    let (invite_code, qr_text, expires_at, label_val) = {
        let mut invites = state.invites.write().map_err(|e| e.to_string())?;
        let invite = invites.create_invite_with_ttl(gateway_pubkey, endpoint, label, ttl);
        (
            invite.to_invite_code(),
            invite.to_qr_text(),
            invite.expires_at,
            invite.label.clone(),
        )
    };

    state
        .audit_logger
        .log(
            AuditCategory::AdminAction,
            "invite_created",
            AuditActor::System,
            None,
            serde_json::json!({ "label": &label_val, "expires_at": expires_at }),
            AuditOutcome::Success,
        )
        .await;

    Ok(InviteResponse { invite_code, qr_text, expires_at, label: label_val })
}

// ═══════════════════════════════════════════════════════════
// Config & Models
// ═══════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let skill_count = {
        let reg = state.skill_registry.read().map_err(|e| e.to_string())?;
        reg.len()
    };
    let provider_list = {
        let preg = state.provider_registry.read().map_err(|e| e.to_string())?;
        preg.list()
    };
    let tool_count = state.tool_registry.total_count();

    Ok(serde_json::json!({
        "gateway": { "host": "127.0.0.1", "port": 18789, "auth_mode": "scoped_token" },
        "tunnel": { "listen_addr": "0.0.0.0:51820", "max_peers": 50, "keepalive_secs": 25 },
        "security": { "binding": "loopback", "scoped_tokens": true, "skill_scanning": "cascade", "rate_limiter": "sharded", "scanner_patterns": state.scanner_pattern_count },
        "skills": { "loaded": skill_count },
        "providers": provider_list,
        "tools": { "registered": tool_count },
    }))
}

#[derive(Debug, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub cost_per_m_input: String,
    pub speed: String,
    pub use_case: String,
    pub context_window: usize,
}

#[tauri::command]
pub async fn list_models(state: State<'_, AppState>) -> Result<Vec<ModelInfo>, String> {
    let mut models = vec![
        ModelInfo { id: "haiku".into(), name: "Claude Haiku 4.5".into(), cost_per_m_input: "$0.25".into(), speed: "fastest".into(), use_case: "Coordinator, heartbeats, routing".into(), context_window: 200_000 },
        ModelInfo { id: "sonnet".into(), name: "Claude Sonnet 4.5".into(), cost_per_m_input: "$3".into(), speed: "fast".into(), use_case: "Code, research, complex tasks".into(), context_window: 200_000 },
        ModelInfo { id: "opus".into(), name: "Claude Opus 4.6".into(), cost_per_m_input: "$15".into(), speed: "moderate".into(), use_case: "Creative, architecture, deep analysis".into(), context_window: 200_000 },
        ModelInfo { id: "local".into(), name: "Local (Ollama)".into(), cost_per_m_input: "Free".into(), speed: "varies".into(), use_case: "Experimentation, privacy-first tasks".into(), context_window: 32_000 },
    ];

    let preg = state.provider_registry.read().map_err(|e| e.to_string())?;
    for name in preg.list() {
        if !models.iter().any(|m| m.id == name) {
            models.push(ModelInfo {
                id: name.clone(), name: format!("{} (provider)", name),
                cost_per_m_input: "varies".into(), speed: "varies".into(),
                use_case: "Custom provider".into(), context_window: 128_000,
            });
        }
    }

    Ok(models)
}

#[derive(Debug, Serialize)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
    pub status: String,
    pub channel_type: String,
}

/// List available channel adapters.
///
/// Returns all supported channel types with their configuration status.
/// Channels with env vars set (e.g., TELEGRAM_BOT_TOKEN) show as "configured",
/// others show as "available" (can be configured).
#[tauri::command]
pub async fn list_channels() -> Result<Vec<ChannelInfo>, String> {
    let channels = vec![
        ("webchat", "Web Chat", "WebChat", true),
        ("internal", "Internal", "Internal", true),
        ("telegram", "Telegram", "Telegram", std::env::var("TELEGRAM_BOT_TOKEN").is_ok()),
        ("discord", "Discord", "Discord", std::env::var("DISCORD_TOKEN").is_ok()),
        ("slack", "Slack", "Slack", std::env::var("SLACK_BOT_TOKEN").is_ok()),
        ("whatsapp", "WhatsApp", "WhatsApp", std::env::var("WHATSAPP_TOKEN").is_ok()),
        ("signal", "Signal", "Signal", std::env::var("SIGNAL_CLI_PATH").is_ok()),
        ("matrix", "Matrix", "Matrix", std::env::var("MATRIX_HOMESERVER").is_ok()),
        ("email", "Email", "Email", std::env::var("IMAP_HOST").is_ok()),
        ("msteams", "MS Teams", "MsTeams", std::env::var("MSTEAMS_APP_ID").is_ok()),
        ("googlechat", "Google Chat", "GoogleChat", std::env::var("GOOGLE_CHAT_CREDENTIALS").is_ok()),
        ("nostr", "Nostr", "Nostr", std::env::var("NOSTR_PRIVATE_KEY").is_ok()),
        ("irc", "IRC", "Irc", std::env::var("IRC_SERVER").is_ok()),
        ("mattermost", "Mattermost", "Mattermost", std::env::var("MATTERMOST_URL").is_ok()),
        ("line", "LINE", "Line", std::env::var("LINE_CHANNEL_TOKEN").is_ok()),
        ("feishu", "Feishu/Lark", "Feishu", std::env::var("FEISHU_APP_ID").is_ok()),
        ("twitch", "Twitch", "Twitch", std::env::var("TWITCH_OAUTH_TOKEN").is_ok()),
        ("imessage", "iMessage", "IMessage", cfg!(target_os = "macos")),
    ];

    Ok(channels
        .into_iter()
        .map(|(id, name, channel_type, configured)| ChannelInfo {
            id: id.into(),
            name: name.into(),
            status: if configured { "active".into() } else { "available".into() },
            channel_type: channel_type.into(),
        })
        .collect()
    )
}

// ═══════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
struct FrontendAgentEvent {
    agent_id: String,
    event: TauriAgentEvent,
}

fn emit_agent_event(app: &AppHandle, agent_id: &str, event: &AgentEvent) -> Result<(), tauri::Error> {
    let mapped = match event {
        AgentEvent::RoundStart { round } => TauriAgentEvent::RoundStart { round: *round },
        AgentEvent::Response {
            content,
            finish_reason,
        } => TauriAgentEvent::Response {
            content: content.clone(),
            finish_reason: format!("{:?}", finish_reason),
        },
        AgentEvent::ToolStart { name, args } => TauriAgentEvent::ToolStart {
            name: name.clone(),
            args: args.clone(),
        },
        AgentEvent::ToolEnd {
            name,
            success,
            duration_ms,
        } => TauriAgentEvent::ToolEnd {
            name: name.clone(),
            success: *success,
            duration_ms: *duration_ms,
        },
        AgentEvent::Compaction {
            level,
            tokens_before,
            tokens_after,
        } => TauriAgentEvent::Compaction {
            level: format!("{:?}", level),
            tokens_before: *tokens_before,
            tokens_after: *tokens_after,
        },
        AgentEvent::StreamChunk { text, done } => TauriAgentEvent::StreamChunk {
            text: text.clone(),
            done: *done,
        },
        AgentEvent::Done { total_rounds } => TauriAgentEvent::Done {
            total_rounds: *total_rounds,
        },
        AgentEvent::Error { error } => TauriAgentEvent::Error {
            error: error.clone(),
        },
        AgentEvent::PromptAssembled {
            total_tokens,
            skills_included,
            skills_excluded,
            memory_fragments,
            budget_utilization,
        } => TauriAgentEvent::PromptAssembled {
            total_tokens: *total_tokens,
            skills_included: skills_included.clone(),
            skills_excluded: skills_excluded.clone(),
            memory_fragments: *memory_fragments,
            budget_utilization: *budget_utilization,
        },
        AgentEvent::IdentityVerified {
            hash_match,
            version,
        } => TauriAgentEvent::IdentityVerified {
            hash_match: *hash_match,
            version: *version,
        },
        AgentEvent::ContextGuardAction {
            action,
            token_count,
            threshold,
        } => TauriAgentEvent::ContextGuardAction {
            action: action.clone(),
            token_count: *token_count,
            threshold: *threshold,
        },
        AgentEvent::FallbackTriggered {
            from_model,
            to_model,
            reason,
            ..
        } => TauriAgentEvent::FallbackTriggered {
            from_model: from_model.clone(),
            to_model: to_model.clone(),
            reason: reason.clone(),
        },
        AgentEvent::SkillDecision { .. } => return Ok(()),
    };

    app.emit(
        "agent-event",
        FrontendAgentEvent {
            agent_id: agent_id.to_string(),
            event: mapped,
        },
    )
}

fn emit_stream_chunks(app: &AppHandle, agent_id: &str, content: &str) {
    if content.is_empty() {
        return;
    }
    let mut chunk = String::new();
    let mut chunk_len = 0usize;
    for ch in content.chars() {
        chunk.push(ch);
        chunk_len += 1;
        if chunk_len >= 64 {
            let _ = app.emit(
                "agent-event",
                FrontendAgentEvent {
                    agent_id: agent_id.to_string(),
                    event: TauriAgentEvent::StreamChunk {
                        text: chunk.clone(),
                        done: false,
                    },
                },
            );
            chunk.clear();
            chunk_len = 0;
        }
    }
    if !chunk.is_empty() {
        let _ = app.emit(
            "agent-event",
            FrontendAgentEvent {
                agent_id: agent_id.to_string(),
                event: TauriAgentEvent::StreamChunk {
                    text: chunk,
                    done: false,
                },
            },
        );
    }
    let _ = app.emit(
        "agent-event",
        FrontendAgentEvent {
            agent_id: agent_id.to_string(),
            event: TauriAgentEvent::StreamChunk {
                text: String::new(),
                done: true,
            },
        },
    );
}

fn emit_metrics_updated(app: &AppHandle, state: &AppState) {
    let model_costs = match state.model_costs.read() {
        Ok(v) => v,
        Err(_) => return,
    };
    let model_breakdown: Vec<ModelCostEntry> = model_costs
        .iter()
        .map(|(model, (input, output, cost_micro))| ModelCostEntry {
            model: model_display_name(model),
            input_tokens: *input,
            output_tokens: *output,
            cost: *cost_micro as f64 / 1_000_000.0,
        })
        .collect();
    let payload = CostMetrics {
        today_cost: state.cost_today_usd(),
        today_input_tokens: state.total_input_tokens.load(std::sync::atomic::Ordering::Relaxed),
        today_output_tokens: state.total_output_tokens.load(std::sync::atomic::Ordering::Relaxed),
        model_breakdown,
    };
    let _ = app.emit("metrics:updated", payload);
}

async fn emit_security_changed(app: &AppHandle, state: &AppState) {
    let identity_contracts = match state.identities.read() {
        Ok(v) => v.len(),
        Err(_) => return,
    };
    let recent_entries = state.audit_logger.recent(2000).await;
    let audit_entries = recent_entries.len();
    let chain = state.audit_logger.verify_chain().await;
    let tunnel_snap = state.tunnel_metrics.snapshot();
    let tunnel_active = tunnel_snap.active_peers > 0 || tunnel_snap.total_rx_bytes > 0;
    let payload = SecurityStatus {
        gateway_bind: "127.0.0.1:18789 (loopback only)".into(),
        tunnel_active,
        tunnel_endpoint: format!(
            "WireGuard tunnel - {} peers, {} bytes rx",
            tunnel_snap.active_peers, tunnel_snap.total_rx_bytes
        ),
        auth_mode: "Scoped tokens (chat|admin|tools separate)".into(),
        scoped_tokens: true,
        identity_contracts,
        skill_scanning: format!(
            "CascadeScanner (Aho-Corasick + Regex) - chain valid: {}, entries: {}",
            chain.valid, chain.entries_checked
        ),
        rate_limiter: "ShardedRateLimiter - 256KB fixed, lock-free".into(),
        mdns_disabled: true,
        scanner_patterns: state.scanner_pattern_count,
        audit_entries,
    };
    let _ = app.emit("security:changed", payload);
}

fn model_display_name(model: &str) -> String {
    match model {
        "haiku" => "Claude Haiku 4.5".into(),
        "sonnet" => "Claude Sonnet 4.5".into(),
        "opus" => "Claude Opus 4.6".into(),
        "local" => "Local (Ollama)".into(),
        _ => model.into(),
    }
}
