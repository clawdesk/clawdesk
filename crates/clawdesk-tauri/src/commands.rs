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
// ClawDeskError/AgentError no longer needed — failover is handled inside runner

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

pub(crate) fn safe_prefix(input: &str, max_bytes: usize) -> &str {
    if input.len() <= max_bytes {
        return input;
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    &input[..end]
}

// ═══════════════════════════════════════════════════════════
// SandboxGate adapter — bridges SandboxPolicyEngine → SandboxGate trait
// ═══════════════════════════════════════════════════════════

/// Adapter implementing the runner's `SandboxGate` trait by delegating to
/// `SandboxPolicyEngine::decide()`. Lives in the Tauri layer because it
/// bridges `clawdesk-security` (concrete) → `clawdesk-agents` (trait),
/// respecting the dependency inversion between crates.
pub(crate) struct SandboxGateAdapter {
    pub(crate) engine: Arc<clawdesk_security::sandbox_policy::SandboxPolicyEngine>,
}

#[async_trait::async_trait]
impl clawdesk_agents::runner::SandboxGate for SandboxGateAdapter {
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
        storage_healthy: !state.soch_store.is_ephemeral(),
        storage_path: state.soch_store.store_path().display().to_string(),
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
    /// Optional template ID — if set, persona/skills/model derive from template defaults.
    #[serde(default)]
    pub template_id: Option<String>,
    /// Channels this agent should handle (e.g. ["telegram", "discord"]).
    /// Empty means the agent is a wildcard and handles any channel.
    #[serde(default)]
    pub channels: Vec<String>,
    /// If this agent belongs to a team, the shared team identifier.
    #[serde(default)]
    pub team_id: Option<String>,
    /// Role within the team (e.g. "router", "researcher", "developer").
    #[serde(default)]
    pub team_role: Option<String>,
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

    // If a template_id is provided, merge template defaults into the agent.
    // User-supplied values take priority; template fills in blanks.
    let (effective_persona, effective_skills, effective_model, template_id) =
        if let Some(ref tmpl_id) = request.template_id {
            if let Some(tmpl) = clawdesk_skills::templates::get_template(tmpl_id) {
                let persona = if request.persona.is_empty() {
                    format!("{}\n\n{}", tmpl.soul, tmpl.guidelines)
                } else {
                    request.persona.clone()
                };
                let skills = if request.skills.is_empty() {
                    tmpl.default_skills.iter().map(|s| s.to_string()).collect()
                } else {
                    request.skills.clone()
                };
                let model = if request.model.is_empty() {
                    tmpl.default_model.to_string()
                } else {
                    request.model.clone()
                };
                (persona, skills, model, Some(tmpl_id.clone()))
            } else {
                (request.persona.clone(), request.skills.clone(), request.model.clone(), None)
            }
        } else {
            (request.persona.clone(), request.skills.clone(), request.model.clone(), None)
        };

    let agent = DesktopAgent {
        id: Uuid::new_v4().to_string(),
        name: request.name,
        icon: request.icon,
        color: request.color,
        persona: effective_persona,
        persona_hash: persona_hash.clone(),
        skills: effective_skills,
        model: effective_model,
        created: Utc::now().to_rfc3339(),
        msg_count: 0,
        status: "ready".to_string(),
        token_budget: 128_000,
        tokens_used: 0,
        source: request.source.unwrap_or_else(|| "clawdesk".to_string()),
        template_id,
        channels: request.channels,
        team_id: request.team_id,
        team_role: request.team_role,
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

    // Register in A2A directory so other agents can discover this one.
    if let Ok(mut dir) = state.agent_directory.write() {
        let mut card = clawdesk_acp::AgentCard::new(
            result.id.clone(),
            result.name.clone(),
            "local://desktop",
        );
        card.description = result.persona.chars().take(200).collect();
        card = card.with_capability(clawdesk_acp::CapabilityId::TextGeneration);
        if !result.channels.is_empty() {
            card = card.with_capability(clawdesk_acp::CapabilityId::Messaging);
        }
        dir.register(card);
    }

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

/// Request to update an existing agent's properties.
#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    pub name: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub persona: Option<String>,
    pub skills: Option<Vec<String>>,
    pub model: Option<String>,
    pub channels: Option<Vec<String>>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub team_role: Option<String>,
}

/// Update an existing agent's properties.
///
/// Re-scans persona through CascadeScanner, re-hashes the identity contract,
/// and persists the updated agent to SochDB.
#[tauri::command]
pub async fn update_agent(
    agent_id: String,
    request: UpdateAgentRequest,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<DesktopAgent, String> {
    // Pull the existing agent
    let mut agent = {
        let agents = state.agents.read().map_err(|e| e.to_string())?;
        agents
            .get(&agent_id)
            .cloned()
            .ok_or_else(|| format!("Agent {} not found", agent_id))?
    };

    // Apply updates
    if let Some(name) = request.name { agent.name = name; }
    if let Some(icon) = request.icon { agent.icon = icon; }
    if let Some(color) = request.color { agent.color = color; }
    if let Some(model) = request.model { agent.model = model; }
    if let Some(skills) = request.skills { agent.skills = skills; }
    if let Some(channels) = request.channels { agent.channels = channels; }
    if let Some(team_id) = request.team_id { agent.team_id = Some(team_id); }
    if let Some(team_role) = request.team_role { agent.team_role = Some(team_role); }

    // If persona changed, re-scan and re-hash
    if let Some(persona) = request.persona {
        let scan_result = state.scanner.scan(&persona);
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
        agent.persona = persona.clone();
        let identity = IdentityContract::new(persona, IdentitySource::UserConfig);
        agent.persona_hash = identity.persona_hash_hex();
        // Update identity store
        if let Ok(mut identities) = state.identities.write() {
            identities.insert(agent_id.clone(), identity);
        }
    }

    let result = agent.clone();
    {
        let mut agents = state.agents.write().map_err(|e| e.to_string())?;
        agents.insert(agent_id.clone(), agent);
    }

    // Persist & audit
    state.persist_agent(&agent_id, &result);
    state
        .audit_logger
        .log(
            AuditCategory::SessionLifecycle,
            "agent_updated",
            AuditActor::System,
            Some(agent_id),
            serde_json::json!({
                "name": &result.name,
                "model": &result.model,
                "persona_hash": &result.persona_hash,
            }),
            AuditOutcome::Success,
        )
        .await;
    emit_security_changed(&app, &state).await;

    // Update A2A directory
    if let Ok(mut dir) = state.agent_directory.write() {
        let mut card = clawdesk_acp::AgentCard::new(
            result.id.clone(),
            result.name.clone(),
            "local://desktop",
        );
        card.description = result.persona.chars().take(200).collect();
        card = card.with_capability(clawdesk_acp::CapabilityId::TextGeneration);
        if !result.channels.is_empty() {
            card = card.with_capability(clawdesk_acp::CapabilityId::Messaging);
        }
        dir.register(card);
    }

    Ok(result)
}

/// Delete an agent by ID.
#[tauri::command]
pub async fn delete_agent(agent_id: String, state: State<'_, AppState>, app: AppHandle) -> Result<bool, String> {
    tracing::info!(agent_id = %agent_id, "delete_agent: request received");

    let removed = {
        let mut agents = state.agents.write().map_err(|e| e.to_string())?;
        let agent_exists = agents.contains_key(&agent_id);
        tracing::info!(
            agent_id = %agent_id,
            exists_in_hashmap = agent_exists,
            total_agents_before = agents.len(),
            "delete_agent: checking in-memory state"
        );
        let removed = agents.remove(&agent_id).is_some();
        if removed {
            if let Ok(mut identities) = state.identities.write() {
                identities.remove(&agent_id);
            }
            state.sessions.remove(&agent_id);
        }
        tracing::info!(
            agent_id = %agent_id,
            removed = removed,
            total_agents_after = agents.len(),
            "delete_agent: in-memory removal done"
        );
        removed
    };

    if removed {
        // Delete from SochDB (durable — commit + fsync)
        let sochdb_key = format!("agents/{}", agent_id);
        tracing::info!(agent_id = %agent_id, key = %sochdb_key, "delete_agent: deleting from SochDB");
        state.delete_agent_from_store(&agent_id);

        // Verify the deletion actually stuck in SochDB
        match state.soch_store.get(&sochdb_key) {
            Ok(None) => {
                tracing::info!(agent_id = %agent_id, key = %sochdb_key, "delete_agent: ✓ verified — key is gone from SochDB");
            }
            Ok(Some(bytes)) => {
                tracing::error!(
                    agent_id = %agent_id,
                    key = %sochdb_key,
                    bytes_len = bytes.len(),
                    "delete_agent: ✗ CRITICAL — key STILL EXISTS in SochDB after delete_durable! Agent will reappear on restart."
                );
                // Nuclear option: try one more time with a direct delete + commit
                if let Err(e) = state.soch_store.delete_durable(&sochdb_key) {
                    tracing::error!(agent_id = %agent_id, error = %e, "delete_agent: retry delete_durable also failed");
                } else {
                    tracing::info!(agent_id = %agent_id, "delete_agent: retry delete_durable succeeded");
                }
            }
            Err(e) => {
                tracing::warn!(agent_id = %agent_id, error = %e, "delete_agent: could not verify deletion (get failed)");
            }
        }

        // Remove from A2A directory
        if let Ok(mut dir) = state.agent_directory.write() {
            dir.deregister(&agent_id);
        }
        state
            .audit_logger
            .log(
                AuditCategory::SessionLifecycle,
                "agent_deleted",
                AuditActor::System,
                Some(agent_id.clone()),
                serde_json::json!({}),
                AuditOutcome::Success,
            )
            .await;
        emit_security_changed(&app, &state).await;
        tracing::info!(agent_id = %agent_id, "delete_agent: completed successfully");
    } else {
        tracing::warn!(agent_id = %agent_id, "delete_agent: agent not found in in-memory state");
    }
    Ok(removed)
}

// ═══════════════════════════════════════════════════════════
// Legacy Import
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
                template_id: None,
                channels: Vec::new(),
                team_id: None,
                team_role: None,
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
            template_id: None,
            channels: Vec::new(),
            team_id: None,
            team_role: None,
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
    /// Optional model override from user preferences (takes priority over agent.model)
    pub model_override: Option<String>,
    /// Optional chat_id. If empty/missing, a new chat is created.
    #[serde(default)]
    pub chat_id: Option<String>,
    /// Optional provider override from user preferences (e.g. "Azure OpenAI", "OpenAI").
    /// When set, the backend creates a one-shot provider from these credentials
    /// instead of relying on env-var auto-registration.
    #[serde(default)]
    pub provider_override: Option<String>,
    /// API key for the overridden provider.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Base URL / endpoint for the overridden provider.
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SendMessageResponse {
    pub message: ChatMessage,
    pub trace: Vec<TraceEntry>,
    /// The chat_id for the conversation (useful when a new chat was auto-created).
    pub chat_id: String,
    /// Auto-generated title (set on first message).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_title: Option<String>,
}

// ── Skills = Prompts, Not Tools ──────────────────────────────────────────────
//
// Skills inject *instructions* into the system prompt that reference existing
// builtin tools (shell_exec, file_read, file_write, http_fetch). Skills NEVER
// define their own tool implementations — the LLM reads the skill prompt and
// decides which builtin tool to call.
//
// This eliminates the SkillBridgeTool stub architecture and aligns with
// the working model where skills are SKILL.md markdown files.

/// Clone the base tool registry for per-request use.
///
/// Previously this created `SkillBridgeTool` stubs for each skill's
/// `provided_tools` — dead handlers that returned placeholder strings.
/// Now skills teach the LLM *how to use* existing builtin tools via their
/// prompt_fragment. No skill-defined tools are registered.
pub(crate) fn build_skill_tool_registry(
    _skills: &[clawdesk_skills::definition::Skill],
    base_registry: &clawdesk_agents::tools::ToolRegistry,
) -> clawdesk_agents::tools::ToolRegistry {
    use clawdesk_agents::tools::ToolRegistry;

    let mut registry = ToolRegistry::new();

    // Copy all builtin tools from the base registry.
    // Skills teach the LLM which tools to call via prompt fragments —
    // no additional tool registrations needed.
    for schema in base_registry.schemas() {
        if let Some(tool) = base_registry.get(&schema.name) {
            registry.register(tool);
        }
    }

    registry
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

    let mut agent = {
        let agents = state.agents.read().map_err(|e| e.to_string())?;

        // Check channel bindings before falling back to request.agent_id.
        // If bindings are configured, a more-specific match (channel + account +
        // group + thread) may override the requested agent_id. This enables
        // multi-channel deployments where different agents handle different channels.
        let effective_agent_id = {
            let bindings = state.channel_bindings.read().map_err(|e| e.to_string())?;
            if bindings.is_empty() {
                request.agent_id.clone()
            } else {
                clawdesk_domain::routing::resolve_binding(
                    &bindings,
                    clawdesk_types::channel::ChannelId::WebChat,
                    &request.agent_id,
                ).unwrap_or_else(|| request.agent_id.clone())
            }
        };

        agents
            .get(&effective_agent_id)
            .cloned()
            .ok_or_else(|| format!("Agent {} not found", effective_agent_id))?
    };

    // Apply user's preferred model override (from Preferences / Onboarding)
    if let Some(ref model_ov) = request.model_override {
        if !model_ov.is_empty() {
            agent.model = model_ov.clone();
        }
    }

    let identity_verified = {
        let identities = state.identities.read().map_err(|e| e.to_string())?;
        identities.get(&agent.id).map(|ic| ic.verify()).unwrap_or(false)
    };

    // ── Resolve or create chat_id ──
    // If the frontend provides a chat_id we use it; otherwise we create a new chat.
    let (chat_id, is_new_chat) = {
        let provided = request.chat_id.as_deref().unwrap_or("").to_string();
        if !provided.is_empty() {
            // Validate it exists
            if state.sessions.contains(&provided) {
                (provided, false)
            } else {
                // Frontend sent a stale chat_id — create a new one
                (Uuid::new_v4().to_string(), true)
            }
        } else {
            (Uuid::new_v4().to_string(), true)
        }
    };

    // Construct a proper SessionKey for SochDB ConversationStore
    // operations. Tauri desktop sessions use "webchat" channel with the UUID
    // as identifier. This key is used for durable `ConversationStore::append_message`
    // calls that write to the SochDB conversation log alongside the in-memory HashMap.
    let session_key = clawdesk_types::session::SessionKey::new(
        clawdesk_types::channel::ChannelId::WebChat,
        &chat_id,
    );

    // Auto-generate title from first user message (first 6 words)
    let auto_title = if is_new_chat {
        let words: Vec<&str> = request.content.split_whitespace().take(6).collect();
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

    // Store user message in session
    let user_msg = ChatMessage {
        id: Uuid::new_v4().to_string(),
        role: "user".to_string(),
        content: request.content.clone(),
        timestamp: now.to_rfc3339(),
        metadata: None,
    };
    {
        let msg_count = state.append_session_message(
            &chat_id, &agent.id, &auto_title, user_msg, &now,
        ).map_err(|e| {
            crate::commands_debug::emit_debug(&app, crate::commands_debug::DebugEvent::error(
                "persist", "user_msg_persist_FAIL",
                format!("FAILED to persist user message to SochDB: {}", e),
            ));
            e
        })?;
        crate::commands_debug::emit_debug(&app, crate::commands_debug::DebugEvent::info(
            "persist", "user_msg_persisted",
            format!("User message persisted. chat_id={}, msgs_in_session={}, is_new={}", chat_id, msg_count, is_new_chat),
        ));
    }

    // Also write the user message to the ConversationStore under the
    // structured SessionKey. This dual-write ensures the SochDB conversation log
    // stays in sync with the in-memory HashMap while we migrate to SessionKey-primary.
    {
        use clawdesk_storage::conversation_store::ConversationStore;
        use clawdesk_types::session::{AgentMessage, Role};
        let agent_msg = AgentMessage {
            role: Role::User,
            content: request.content.clone(),
            timestamp: now,
            model: None,
            token_count: None,
            tool_call_id: None,
            tool_name: None,
        };
        if let Err(e) = state.soch_store.append_message(&session_key, &agent_msg).await {
            tracing::warn!(error = %e, "ConversationStore append_message failed for user msg");
        }
    }

    // Ensure user message is durable before the potentially long LLM call.
    // Without this, a timeout or crash during the LLM call could lose the
    // user message that was only in the WAL (not yet checkpointed).
    if let Err(e) = state.soch_store.sync() {
        tracing::warn!(error = %e, "SochDB sync after user message persist failed");
        crate::commands_debug::emit_debug(&app, crate::commands_debug::DebugEvent::warn(
            "persist", "sync_after_user_FAIL",
            format!("SochDB sync() after user message failed: {}", e),
        ));
    } else {
        crate::commands_debug::emit_debug(&app, crate::commands_debug::DebugEvent::info(
            "persist", "sync_after_user_ok",
            format!("SochDB sync() after user message succeeded. chat_id={}", chat_id),
        ));
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
    //
    // Priority:
    // 1. provider_override from user settings (creates a one-shot provider)
    // 2. ProviderNegotiator (registered from env vars at startup)
    // 3. Legacy resolve_provider fallback
    //
    // GAP-G: Per-turn dynamic model routing — when the agent uses a default
    // model, the TurnRouter (LinUCB bandit + ModelCatalog) may suggest a
    // better model based on the user message features. The routed model
    // overrides the default but respects explicitly pinned models.
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
        // Create a one-shot provider from the user's saved settings
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
            "Local (Built-in)" | "local" => {
                // Built-in llama.cpp inference managed by clawdesk
                Arc::new(clawdesk_providers::local::LocalProvider::new(Some(model_full_id.clone())))
            }
            "Local (OpenAI Compatible)" | "local_compatible" => {
                // llama.cpp, vLLM, text-generation-webui, LM Studio, etc.
                // These serve an OpenAI-compatible /v1/chat/completions endpoint.
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

    // Build conversation history from SochDB ConversationStore (primary) with
    // in-memory HashMap fallback for legacy sessions that pre-date dual-write.
    //
    // SochDB is the durable source of truth. The per-message records under
    // `sessions/{key}/messages/{ts}` are written by dual-write for every
    // user and assistant message. `load_history()` does a reverse prefix scan
    // (O(log N + k)) and returns messages in chronological order.
    //
    // BUG FIX: Previously we preferred ConversationStore when non-empty, but a
    // timestamp collision bug caused user messages to be overwritten by assistant
    // messages (same millisecond key). This left ConversationStore with fewer
    // messages than the in-memory HashMap (hydrated from `chats/{chat_id}`).
    // Now we compare both sources and use whichever has more messages, ensuring
    // the LLM always sees the complete conversation history.
    let mut history = {
        use clawdesk_storage::conversation_store::ConversationStore;
        use clawdesk_types::session::Role;

        let soch_messages = state.soch_store
            .load_history(&session_key, 200)
            .await
            .unwrap_or_default();

        // Also load from in-memory LRU cache (hydrated from chats/{chat_id} blob)
        let hashmap_messages = state.sessions.get(&chat_id)
            .map(|s| s.messages)
            .unwrap_or_default();

        let (history_source, history_vec) = if !soch_messages.is_empty() && soch_messages.len() >= hashmap_messages.len() {
            // ── Primary path: SochDB ConversationStore (more complete) ──
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

            // Merge tool history (stored separately in tool_history/{chat_id})
            let tool_msgs = state.load_tool_history(&chat_id);
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
            // ── HashMap has more messages (ConversationStore may be incomplete
            // due to timestamp collision bug) — use HashMap as source ──
            if !soch_messages.is_empty() {
                tracing::warn!(
                    chat_id = %chat_id,
                    sochdb_count = soch_messages.len(),
                    hashmap_count = hashmap_messages.len(),
                    "ConversationStore has fewer messages than HashMap — using HashMap (likely timestamp collision data loss)"
                );
            }

            let tool_msgs = state.load_tool_history(&chat_id);
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
            // Both sources empty — fresh session
            ("empty", Vec::new())
        };

        tracing::info!(
            chat_id = %chat_id,
            source = history_source,
            messages = history_vec.len(),
            "History assembled for LLM context"
        );
        history_vec
    };

    // Media pipeline — enrich context with URL metadata.
    // Uses LinkUnderstanding::extract_urls() to detect URLs in the user message.
    // If URLs are found, prepend a context note so the LLM is aware of linked content.
    // Full content fetching requires an HttpFetcher implementation (future work).
    {
        let urls = clawdesk_media::link_understanding::LinkUnderstanding::extract_urls(&request.content);
        if !urls.is_empty() {
            let url_context = format!(
                "[Context: User message contains {} URL(s): {}. You may reference these in your response.]",
                urls.len(),
                urls.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
            );
            // Insert as a system message just before the last user message
            let insert_pos = history.len().saturating_sub(1);
            history.insert(
                insert_pos,
                clawdesk_providers::ChatMessage::new(MessageRole::System, url_context.as_str()),
            );
        }
    }

    // Context Guard — check if history exceeds αC and APPLY compaction
    // After compaction, the guard clone is passed to the runner to prevent
    // duplicate compaction (runner uses shared state instead of a fresh guard).
    let compacted_guard = {
        use clawdesk_domain::context_guard::{ContextGuard, ContextGuardConfig, GuardAction, CompactionLevel, CompactionResult};
        use clawdesk_types::tokenizer::estimate_tokens;

        let total_history_tokens: usize = history.iter()
            .map(|m| estimate_tokens(&m.content))
            .sum();

        // Phase 1: Determine action under lock, then release the guard
        // before any async work (RwLockWriteGuard is !Send).
        let guard_action = {
            let mut guards = state.context_guards.write().map_err(|e| e.to_string())?;
            let guard = guards.entry(chat_id.clone()).or_insert_with(|| {
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
        }; // <-- guard dropped here, safe to await

        // Phase 2: Apply compaction (may involve async LLM call)
        match guard_action {
            GuardAction::Ok => {}
            GuardAction::Compact(level) => {
                let tokens_before = total_history_tokens;
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
                            // LLM-based summarization (async, guard released above)
                            let summary = clawdesk_agents::compaction::summarize_transcript_via_llm(
                                &provider,
                                &model_full_id,
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
                            history = history.split_off(history.len() - 10);
                        }
                    }
                }
                let tokens_after: usize = history.iter()
                    .map(|m| estimate_tokens(&m.content))
                    .sum();

                // Phase 3: Re-acquire guard to update state
                {
                    let mut guards = state.context_guards.write().map_err(|e| e.to_string())?;
                    if let Some(guard) = guards.get_mut(&chat_id) {
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
            }
            GuardAction::ForceTruncate { retain_tokens } => {
                // Budget-based truncation — keep newest messages within budget
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
                    history = history.split_off(keep_from);
                }
                let tokens_after: usize = history.iter()
                    .map(|m| estimate_tokens(&m.content))
                    .sum();
                {
                    let mut guards = state.context_guards.write().map_err(|e| e.to_string())?;
                    if let Some(guard) = guards.get_mut(&chat_id) {
                        guard.set_token_count(tokens_after);
                    }
                }
                let _ = app.emit("agent-event", serde_json::json!({
                    "agent_id": &agent.id,
                    "event": { "type": "ContextGuardAction", "action": format!("truncate_budget_{}", retain_tokens), "token_count": tokens_after, "threshold": 0.80 },
                }));
            }
            GuardAction::CircuitBroken { retain_tokens } => {
                // Circuit breaker open — budget-based fallback truncation
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
                    history = history.split_off(keep_from);
                }
                let tokens_after: usize = history.iter()
                    .map(|m| estimate_tokens(&m.content))
                    .sum();
                {
                    let mut guards = state.context_guards.write().map_err(|e| e.to_string())?;
                    if let Some(guard) = guards.get_mut(&chat_id) {
                        guard.set_token_count(tokens_after);
                    }
                }
                let _ = app.emit("agent-event", serde_json::json!({
                    "agent_id": &agent.id,
                    "event": { "type": "ContextGuardAction", "action": "circuit_broken_budget_truncate", "token_count": tokens_after },
                }));
            }
        }

        // Clone the guard to pass to the runner — preserves token count
        // and circuit breaker state, preventing duplicate compaction.
        let guards = state.context_guards.read().map_err(|e| e.to_string())?;
        guards.get(&chat_id).cloned().unwrap_or_else(|| {
            ContextGuard::new(ContextGuardConfig {
                context_limit: agent.token_budget,
                trigger_threshold: 0.80,
                response_reserve: 8_192,
                circuit_breaker_threshold: 3,
                circuit_breaker_cooldown: Duration::from_secs(60),
                adaptive_thresholds: true,
                force_truncate_retain_share: 0.50,
            })
        })
    };

    // ── Unified prompt pipeline (shared with Discord/channel path) ──
    // Uses the engine module for memory recall, skill scoring, PromptBuilder
    // assembly, and memory injection. Single codepath = no drift.
    let agent_skill_set: std::collections::HashSet<String> = agent
        .skills
        .iter()
        .map(|s| s.to_lowercase())
        .collect();
    let active_skills = crate::engine::load_active_skills(&state.skill_registry);

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
        &active_skills,
    ).await;

    let mut system_prompt = pipeline_result.system_prompt;
    let memory_injection = pipeline_result.memory_injection;

    // ── Team roster injection ──────────────────────────────────────────
    // If this agent is a team router, inject a structured roster of all
    // team members into the system prompt so the LLM knows their IDs,
    // names, and roles and can delegate via `spawn_subagent`.
    if agent.team_role.as_deref() == Some("router") {
        if let Some(ref team_id) = agent.team_id {
            let agents_guard = state.agents.read().map_err(|e| e.to_string())?;
            let teammates: Vec<_> = agents_guard
                .values()
                .filter(|a| a.team_id.as_deref() == Some(team_id) && a.id != agent.id)
                .collect();
            if !teammates.is_empty() {
                let mut roster = String::from(
                    "\n\n## Your Team — Agentic Delegation\n\n\
**MANDATORY**: You MUST use the `spawn_subagent` tool to delegate to specialists below. \
NEVER write specialist content yourself. For EVERY user request, your workflow is:\n\n\
1. **Analyze** the request and identify which specialist(s) are needed.\n\
2. **Delegate** by calling `spawn_subagent` for each specialist with a SPECIFIC, DETAILED task.\n\
   - Include expected deliverables, format, depth, and scope in each task.\n\
   - You can call `spawn_subagent` multiple times in a SINGLE response to run specialists in parallel.\n\
3. **Review** the results — if a result is incomplete, delegate again with more specifics.\n\
4. **Synthesize** a polished, unified response combining all specialist outputs.\n\n\
**CRITICAL RULES**:\n\
- You MUST call `spawn_subagent` at least once per user request\n\
- You are FORBIDDEN from writing business plans, marketing strategies, code, or other specialist content directly\n\
- If you find yourself writing content that a specialist could produce, STOP and delegate instead\n\n\
### Team Members\n\n"
                );
                for mate in &teammates {
                    let role = mate.team_role.as_deref().unwrap_or("specialist");
                    roster.push_str(&format!(
                        "- **{}** (role: {}) — agent_id: `{}`\n",
                        mate.name, role, mate.id,
                    ));
                    // Include persona as capability hint (up to 300 chars)
                    let hint: String = mate.persona.chars().take(300).collect();
                    if !hint.is_empty() {
                        roster.push_str(&format!("  Expertise: {}\n", hint.trim()));
                    }
                }
                roster.push('\n');
                system_prompt.push_str(&roster);
            }
        }
    }

    // Store manifest for inspector command
    if let Some(ref manifest) = pipeline_result.prompt_manifest {
        if let Ok(mut manifests) = state.prompt_manifests.write() {
            manifests.insert(agent.id.clone(), manifest.clone());
        }
    }

    // ── Pre-user-message memory injection (unified engine) ──
    if let Some(ref mem_text) = memory_injection {
        crate::engine::inject_memory_context(&mut history, mem_text);
    }

    // ── SochDB SemanticCache: check for cached response before LLM call ──
    // Include a hash of the assembled system prompt (which incorporates memory
    // fragments and skills) in the cache namespace. This prevents stale cache
    // hits when memory or active skills change for the same user query.
    let prompt_hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        system_prompt.hash(&mut hasher);
        // Memory is now separate from system_prompt — hash it too
        // so cache invalidation still works correctly.
        if let Some(ref mem) = memory_injection {
            mem.hash(&mut hasher);
        }
        hasher.finish()
    };

    // Pre-compute query embedding for semantic cache lookup.
    // This embedding is reused for both cache lookup and cache store,
    // eliminating duplicate embedding API calls and enabling ANN-based
    // semantic cache matching instead of exact-only matching.
    let query_embedding = match state.embedding_provider.embed(&request.content).await {
        Ok(result) => Some(result.vector),
        Err(e) => {
            tracing::debug!(error = %e, "query embedding for semantic cache failed, using exact match only");
            None
        }
    };

    // Include model name in cache namespace — prevents cross-model cache hits
    // (same query + same persona but different model would otherwise collide).
    let cache_namespace = format!("agent:{}:{}:{:x}", agent.id, agent.model, prompt_hash);
    let cache_hit = state.semantic_cache.lookup(
        &request.content,
        &cache_namespace,
        0, // no allowed-set filtering
        query_embedding.as_deref(), // pass pre-computed embedding for ANN lookup
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
            state.append_session_message(
                &chat_id, &agent.id, &auto_title, assistant_msg.clone(), &now,
            )?;
        }
        let _ = app.emit("incoming:message", serde_json::json!({
            "agent_id": agent.id,
            "chat_id": &chat_id,
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
            chat_id: chat_id.clone(),
            chat_title: if is_new_chat { Some(auto_title) } else { None },
        });
    }

    // ── SochDB KnowledgeGraph: ensure agent and session nodes + edge ──
    {
        let mut agent_props = HashMap::new();
        agent_props.insert("name".into(), serde_json::json!(agent.name));
        agent_props.insert("model".into(), serde_json::json!(agent.model));
        let _ = state.knowledge_graph.add_node(&agent.id, "agent", Some(agent_props));

        let session_id = format!("session:{}", chat_id);
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
    let parent_model_id_for_spawn = model_id.clone();
    let config = AgentConfig {
        model: model_id,
        system_prompt: system_prompt.clone(),
        max_tool_rounds: 25,
        context_limit: agent.token_budget,
        response_reserve: 8_192,
        failover: Some(clawdesk_types::failover::FailoverConfig::default()),
        // Wire workspace path so bootstrap context, tool scoping,
        // and skill file discovery all activate. Without this, ShellTool
        // runs unconfined and bootstrap files (CLAUDE.md, README.md) are
        // never discovered.
        workspace_path: Some(state.workspace_root.to_string_lossy().into_owned()),
        ..Default::default()
    };

    // Set up event channel for trace collection + live frontend streaming
    let (event_tx, mut event_rx) = broadcast::channel::<AgentEvent>(128);
    let event_log = Arc::new(tokio::sync::Mutex::new(Vec::<AgentEvent>::new()));
    let event_log_task = Arc::clone(&event_log);
    let app_for_events = app.clone();
    let agent_id_for_events = agent.id.clone();

    // Acquire per-session serialization lane before running the agent.
    // This ensures only one agent run per session at a time. If another run is
    // active for this agent, this await blocks until it completes. The guard is
    // held for the duration of the agent run and released on drop.
    let session_lane_key = format!("session:{}", chat_id);
    let _session_guard = state.session_lanes.acquire(&session_lane_key).await
        .map_err(|e| format!("Session lane error: {}", e))?;

    // Per-run cancellation token for this chat request.
    let run_cancel = state.cancel.child_token();

    // Build per-request ToolRegistry with skill-provided tools
    let deref_skills: Vec<clawdesk_skills::definition::Skill> = active_skills
        .iter()
        .map(|s| (**s).clone())
        .collect();
    let mut request_tool_registry = build_skill_tool_registry(
        &deref_skills,
        &state.tool_registry,
    );

    // Register the sub-agent spawn tool per-request so the callback
    // has access to AppState (agents, negotiator, cancel token). The LLM can call
    // `spawn_subagent` to delegate a task to another agent and get the result.
    {
        let agents_ref = {
            let agents = state.agents.read().map_err(|e| e.to_string())?;
            agents.clone()
        };
        let negotiator_ref = Arc::clone(&state.negotiator);
        let cancel_ref = run_cancel.clone();
        let base_tools = Arc::clone(&state.tool_registry);
        let sandbox_engine_ref = Arc::clone(&state.sandbox_engine);
        let memory_ref = Arc::clone(&state.memory);
        // Capture the parent's provider so sub-agents can inherit it when
        // the negotiator has no matching provider (e.g. channel_provider override
        // from the UI that only the parent sees).
        let parent_provider = Arc::clone(&provider);
        // Capture the parent's resolved model so sub-agents with model="default"
        // can inherit the real model ID instead of sending literal "default" to the API.
        let parent_model_id = parent_model_id_for_spawn.clone();

        let spawn_fn: clawdesk_agents::port::AsyncPort<clawdesk_agents::port::SpawnSubAgentRequest, Result<String, String>> = Arc::new(move |req: clawdesk_agents::port::SpawnSubAgentRequest| {
            let agent_id = req.agent_id;
            let task = req.task;
            let timeout_secs = req.timeout_secs;
            let agents = agents_ref.clone();
            let negotiator = Arc::clone(&negotiator_ref);
            let cancel = cancel_ref.clone();
            let tools = Arc::clone(&base_tools);
            let sandbox_eng = Arc::clone(&sandbox_engine_ref);
            let memory = Arc::clone(&memory_ref);
            let fallback_provider = Arc::clone(&parent_provider);
            let parent_mdl = parent_model_id.clone();
            Box::pin(async move {
                let agent = agents.get(&agent_id)
                    .ok_or_else(|| format!("Sub-agent '{}' not found", agent_id))?;
                // If the sub-agent model is "default"/"auto"/empty, inherit
                // the parent's resolved model so it doesn't send literal
                // "default" to the LLM API.
                let model_id = {
                    let raw = &agent.model;
                    if raw.is_empty() || raw == "default" || raw == "auto" {
                        parent_mdl.clone()
                    } else {
                        crate::state::AppState::resolve_model_id(raw)
                    }
                };
                let required = clawdesk_providers::capability::ProviderCaps::TEXT_COMPLETION
                    .union(clawdesk_providers::capability::ProviderCaps::SYSTEM_PROMPT);
                // Try the negotiator first; fall back to the parent's provider
                // (which may be a channel_provider override from the UI).
                let provider = {
                    let neg = negotiator.read().map_err(|e| format!("negotiator lock: {e}"))?;
                    match neg.resolve_model(&model_id, required) {
                        Some((p, _)) => Arc::clone(p),
                        None => {
                            tracing::info!(
                                agent_id = %agent_id,
                                model = %agent.model,
                                "Sub-agent: negotiator miss — inheriting parent provider"
                            );
                            fallback_provider
                        }
                    }
                };
                let config = clawdesk_agents::AgentConfig {
                    model: model_id,
                    system_prompt: String::new(),
                    max_tool_rounds: 15,
                    ..Default::default()
                };
                let runner = clawdesk_agents::AgentRunner::new(provider, tools, config, cancel)
                    // Wire sandbox gate into sub-agent runner
                    .with_sandbox_gate(Arc::new(crate::commands::SandboxGateAdapter {
                        engine: sandbox_eng,
                    }))
                    // GAP-B: Wire memory recall so sub-agents get memory context
                    .with_memory_recall({
                        let mem = memory;
                        Arc::new(move |query: String| {
                            let mem = Arc::clone(&mem);
                            Box::pin(async move {
                                match mem.recall(&query, Some(5)).await {
                                    Ok(results) => results.into_iter().filter_map(|r| {
                                        let text = r.content?;
                                        if text.is_empty() { return None; }
                                        Some(clawdesk_agents::MemoryRecallResult {
                                            relevance: r.score as f64,
                                            source: r.metadata.get("source")
                                                .and_then(|v| v.as_str())
                                                .map(String::from),
                                            content: text,
                                        })
                                    }).collect(),
                                    Err(_) => vec![],
                                }
                            })
                        })
                    });
                let history = vec![
                    clawdesk_providers::ChatMessage::new(clawdesk_providers::MessageRole::User, task.as_str()),
                ];
                let timeout = tokio::time::Duration::from_secs(timeout_secs);
                match tokio::time::timeout(timeout, runner.run(history, agent.persona.clone())).await {
                    Ok(Ok(response)) => Ok(response.content),
                    Ok(Err(e)) => Err(format!("Sub-agent error: {e}")),
                    Err(_) => Err(format!("Sub-agent timed out after {}s", timeout_secs)),
                }
            })
        });
        clawdesk_agents::builtin_tools::register_subagent_tool(&mut request_tool_registry, spawn_fn);
    }

    // Register the dynamic agent spawn tool per-request.
    // The LLM can call `dynamic_spawn` to create ephemeral specialist agents
    // without needing a pre-registered agent ID. The factory function
    // `build_dynamic_spawn_fn` returns a closure with the correct depth baked
    // in, enabling recursive multi-level spawning with depth tracking.
    {
        let dynamic_fn = build_dynamic_spawn_fn(
            Arc::clone(&state.negotiator),
            run_cancel.clone(),
            Arc::clone(&state.tool_registry),
            Arc::clone(&state.sandbox_engine),
            Arc::clone(&state.sub_mgr),
            agent.id.clone(),
            agent.model.clone(),
            0, // root depth
            Arc::clone(&provider), // parent provider fallback
        );
        clawdesk_agents::builtin_tools::register_dynamic_spawn_tool(
            &mut request_tool_registry,
            dynamic_fn,
        );
    }

    // Register the ask_human tool — allows the agent to pause and ask the
    // user for a decision instead of refusing or restating capabilities.
    // Also fans out the question to all connected external channels.
    {
        let pending = Arc::clone(&state.ask_human_pending);
        let app_for_ask = app.clone();
        let channels_for_ask = Arc::clone(&state.channel_registry);
        let origins_for_ask = Arc::clone(&state.last_channel_origins);
        let ask_fn: clawdesk_agents::port::AsyncPort<
            clawdesk_agents::port::AskHumanRequest,
            Result<String, String>,
        > = Arc::new(move |req: clawdesk_agents::port::AskHumanRequest| {
            let pending = Arc::clone(&pending);
            let app = app_for_ask.clone();
            let channels = Arc::clone(&channels_for_ask);
            let origins = Arc::clone(&origins_for_ask);
            Box::pin(async move {
                let request_id = uuid::Uuid::new_v4();
                let notify = Arc::new(tokio::sync::Notify::new());

                // Format the question for channel delivery
                let options_text = if req.options.is_empty() {
                    String::new()
                } else {
                    format!("\nOptions: {}", req.options.join(" | "))
                };
                let channel_body = format!(
                    "🤔 **Decision needed{}**\n\n{}{}\n\n_Reply to this message with your answer._",
                    if req.urgent { " (URGENT)" } else { "" },
                    &req.question,
                    options_text,
                );

                // Fan out to connected external channels
                let mut sent_channels = Vec::new();
                {
                    use clawdesk_types::channel::ChannelId;
                    let external_channels: Vec<(ChannelId, std::sync::Arc<dyn clawdesk_channel::Channel>)> =
                        if let Ok(reg) = channels.read() {
                            reg.iter()
                                .filter(|(id, _)| !matches!(id, ChannelId::WebChat | ChannelId::Internal))
                                .map(|(id, ch)| (*id, std::sync::Arc::clone(ch)))
                                .collect()
                        } else {
                            vec![]
                        };

                    let origins_snapshot: std::collections::HashMap<ChannelId, clawdesk_types::message::MessageOrigin> =
                        origins.read().map(|g| g.clone()).unwrap_or_default();

                    for (ch_id, ch) in external_channels {
                        let origin = match origins_snapshot.get(&ch_id).cloned() {
                            Some(o) => o,
                            None => match ch.default_origin() {
                                Some(o) => o,
                                None => continue, // no target — skip
                            },
                        };
                        let outbound = clawdesk_types::message::OutboundMessage {
                            origin,
                            body: channel_body.clone(),
                            media: vec![],
                            reply_to: None,
                            thread_id: None,
                        };
                        match ch.send(outbound).await {
                            Ok(_receipt) => {
                                tracing::info!(channel = %ch_id, "ask_human: question sent to channel");
                                sent_channels.push(format!("{}", ch_id));
                            }
                            Err(e) => {
                                tracing::warn!(channel = %ch_id, error = %e, "ask_human: failed to send to channel");
                            }
                        }
                    }
                }

                let entry = crate::state::AskHumanEntry {
                    notify: Arc::clone(&notify),
                    response: std::sync::Mutex::new(None),
                    sent_to_channels: std::sync::Mutex::new(sent_channels.clone()),
                    question: req.question.clone(),
                };
                pending.insert(request_id, entry);

                // Emit event to frontend so it can render the question
                let _ = app.emit("ask-human:pending", serde_json::json!({
                    "id": request_id.to_string(),
                    "question": &req.question,
                    "options": &req.options,
                    "urgent": req.urgent,
                    "sent_to_channels": &sent_channels,
                }));

                // Block until the frontend OR a channel inbound responds
                notify.notified().await;

                // Retrieve the response
                let response = pending
                    .remove(&request_id)
                    .and_then(|(_, entry)| entry.response.lock().ok()?.take())
                    .unwrap_or_else(|| "No response received".to_string());

                Ok(response)
            })
        });
        clawdesk_agents::builtin_tools::register_ask_human_tool(
            &mut request_tool_registry,
            ask_fn,
        );
    }

    let request_tool_registry = Arc::new(request_tool_registry);

    // /1 FIX: Look up channel metadata from the dock and build a
    // ChannelContext for the runner. For Tauri desktop, this is always WebChat.
    // Include cross-channel messaging hint so the LLM knows it can send
    // messages to Telegram/Discord/etc. via message_send tool.
    let channel_context = {
        use clawdesk_types::channel::ChannelId;

        // Build cross-channel hint listing all connected external channels
        let other_channels: Vec<String> = state.channel_registry.read()
            .map(|reg| {
                reg.list()
                    .iter()
                    .filter(|id| !matches!(id,
                        ChannelId::WebChat | ChannelId::Internal
                    ))
                    .map(|id| format!("{:?}", id).to_lowercase())
                    .collect()
            })
            .unwrap_or_default();

        let cross_channel_hint = if other_channels.is_empty() {
            None
        } else {
            Some(format!(
                "You are connected to these other channels: [{}]. \
                 When the user asks you to send a message to one of those channels \
                 (e.g. \"say hi to telegram\", \"tell discord hello\"), \
                 IMMEDIATELY call the message_send tool with channel=<name> and content=<message>. \
                 Do NOT ask for IDs or configuration. Do NOT refuse. Just call message_send.",
                other_channels.join(", ")
            ))
        };

        state.channel_dock.to_runner_context(ChannelId::WebChat).map(|rcc| {
            clawdesk_agents::runner::ChannelContext {
                channel_name: rcc.channel_name,
                supports_threading: rcc.supports_threading,
                supports_streaming: rcc.supports_streaming,
                supports_reactions: rcc.supports_reactions,
                supports_media: rcc.supports_media,
                max_message_length: rcc.max_message_length,
                markup_format: rcc.markup_format,
                extra_instructions: cross_channel_hint,
                history_limit: Some(200),
            }
        })
    };

    // Build a ToolPolicy with adequate timeout for spawn-capable agents.
    // spawn_subagent / dynamic_spawn run complete sub-agent loops that easily
    // exceed the default per-tool timeout, so use 300s for the outer runner.
    let tool_policy = {
        let mut p = clawdesk_agents::tools::ToolPolicy::default();
        p.tool_timeout_secs = 300;
        Arc::new(p)
    };

    let mut runner = AgentRunner::new(
        provider,
        request_tool_registry,
        config,
        run_cancel.clone(),
    )
    .with_tool_policy(tool_policy)
    .with_events(event_tx.clone())
    .with_approval_gate(Arc::new(crate::state::TauriApprovalGate::new(
        Arc::clone(&state.approval_manager),
        app.clone(),
    )))
    .with_context_guard(compacted_guard)
    .with_profile_rotator(Arc::new(
        clawdesk_providers::profile_rotation::ProfileRotator::new(
            agent.model.as_str(),
            clawdesk_providers::profile_rotation::RotationConfig::default(),
        ),
    ))
    // /5 FIX: Inject channel context so runner can adapt prompts and
    // chunk responses for the target channel. Gap 5 (multi-payload segments)
    // auto-activates once channel_context is Some.
    .with_hook_manager(Arc::clone(&state.hook_manager))      //    .with_session_context(chat_id.clone(), agent.id.clone()) // session+agent for hook context
    // Wire sandbox policy gate — tools whose required isolation level
    // exceeds platform capability are blocked before execution.
    .with_sandbox_gate(Arc::new(crate::commands::SandboxGateAdapter {
        engine: Arc::clone(&state.sandbox_engine),
    }));

    if let Some(ch_ctx) = channel_context {
        runner = runner.with_channel_context(ch_ctx);
    }

    // Wire the runner-level SkillProvider for per-turn dynamic
    // skill selection during multi-round tool-use conversations (unified engine).
    if let Some(skill_provider) = crate::engine::build_skill_provider(active_skills) {
        runner = runner.with_skill_provider(skill_provider);
    }

    // Keep a handle to drop the sender after run completes, signaling the forwarder to stop.
    let _event_tx_keepalive = event_tx;

    let forward_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    {
                        let mut guard = event_log_task.lock().await;
                        guard.push(event.clone());
                    }
                    let _ = emit_agent_event(&app_for_events, &agent_id_for_events, &event);
                    // StreamChunk events are now emitted by AgentRunner in real-time
                    // via provider.stream() — no post-hoc chunking needed.
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });

    // ── LLM-Native Agentic Loop ──────────────────────────────────────
    //
    // Architecture (informed by Claude Code, Pi-mono, Pi-side-agents, OpenClaw):
    //
    // Instead of rigid heuristic phases (decompose → dispatch → synthesize),
    // we let the LLM BE the orchestrator — just like the human brain:
    //
    //   1. The router LLM sees the user's request + team roster in system prompt.
    //   2. It decides which specialist(s) to delegate to via `spawn_subagent` tool.
    //   3. spawn_subagent calls run IN PARALLEL via JoinSet (already built-in).
    //   4. Results flow back as tool results in context.
    //   5. The LLM naturally synthesizes and responds.
    //   6. If a result is incomplete, the LLM can re-delegate.
    //   7. The LLM knows when to stop — finishReason: "stop".
    //
    // Zero heuristic prediction. Zero JSON parsing. The LLM decides everything.
    // For team routers, the system prompt already contains the team roster and
    // delegation instructions. The `spawn_subagent` tool is already registered.
    // Tools execute in parallel via JoinSet when the LLM emits multiple calls.

    // Register this run in the cancellation registry BEFORE any LLM call.
    {
        let mut active = state.active_chat_runs.write().await;
        active.insert(chat_id.clone(), run_cancel.clone());
    }

    // ── Single unified execution path for ALL agents (solo + team routers) ──
    //
    // `run_with_failover()` uses the FailoverController DFA:
    //   Level 1: Retry same model with decorrelated-jitter backoff
    //   Level 2: Rotate to next auth profile via ProfileRotator
    //   Level 3: Fallback to next model in the failover chain
    //   Level 4: Thinking-level downgrade on context overflow
    let _llm_permit = state.llm_concurrency.acquire().await
        .map_err(|_| "LLM concurrency semaphore closed".to_string())?;

    let run_result = runner
        .run_with_failover(history.clone(), system_prompt.clone())
        .await
        .map_err(|e| e.to_string());
    drop(_llm_permit);

    {
        let mut active = state.active_chat_runs.write().await;
        active.remove(&chat_id);
    }

    // Drop ALL broadcast senders so the forwarder sees `Closed` and exits.
    drop(runner);
    drop(_event_tx_keepalive);
    let _ = forward_task.await;

    let (agent_response, execution_err): (clawdesk_agents::runner::AgentResponse, Option<String>) =
        match run_result {
            Ok(resp) => (resp, None),
            Err(e) => {
                let msg = format!("Agent execution failed: {}", e);
                let _ = app.emit(
                    "system:alert",
                    serde_json::json!({
                        "level": "error",
                        "title": "Agent execution failed",
                        "message": msg.clone(),
                    }),
                );
                let err_resp = clawdesk_agents::runner::AgentResponse {
                    content: msg.clone(),
                    total_rounds: 1,
                    tool_messages: vec![],
                    finish_reason: clawdesk_providers::FinishReason::Stop,
                    input_tokens: 0,
                    output_tokens: 0,
                    segments: vec![],
                    active_skills: vec![],
                    messaging_sends: vec![],
                };
                (err_resp, Some(msg))
            }
        };
    // ── End LLM-call span ──
    if let (Some(tid), Some(sid)) = (&soch_trace_id, &llm_span_id) {
        let status = if execution_err.is_none() { SpanStatusCode::Ok } else { SpanStatusCode::Error };
        let _ = state.trace_store.end_span(tid, sid, status, None);
    }

    // ── SochDB SemanticCache: store the LLM response for future cache hits ──
    // Pass the pre-computed query embedding so SemanticCache stores it
    // alongside the response. Future lookups with similar embeddings will hit
    // ANN-based semantic matching, not just exact string matching.
    if execution_err.is_none() {
        let _ = state.semantic_cache.store(
            &request.content,
            &cache_namespace,
            0,
            agent_response.content.as_bytes(),
            query_embedding.clone(), // store the query embedding
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

    // GAP-G: Record reward feedback for the turn router's online learning.
    // Reward signal: 1.0 for successful + fast, degraded for errors/slow.
    if let Some(ref rd) = routing_decision {
        let reward = if execution_err.is_some() {
            0.1 // Execution failed
        } else if elapsed_ms > 30_000 {
            0.3 // Very slow
        } else if elapsed_ms > 10_000 {
            0.5 // Slow
        } else {
            0.8 // Good
        };
        state.turn_router.record_feedback(&rd.selected_key, &rd.features, reward);
    }

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

    // ── SochDB KnowledgeGraph: record user + assistant message nodes + edges ──
    {
        let session_id = format!("session:{}", chat_id);

        // User message node
        let user_node_id = format!("msg:{}", Uuid::new_v4());
        let mut user_props = HashMap::new();
        user_props.insert("role".into(), serde_json::json!("user"));
        user_props.insert("content_len".into(), serde_json::json!(request.content.len()));
        user_props.insert("timestamp".into(), serde_json::json!(now.to_rfc3339()));
        let _ = state.knowledge_graph.add_node(&user_node_id, "message", Some(user_props));
        let _ = state.knowledge_graph.add_edge(&session_id, "contains", &user_node_id, None);

        // Assistant message node
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
        // Chain: user -> responded_with -> assistant
        let _ = state.knowledge_graph.add_edge(&user_node_id, "responded_with", &asst_node_id, None);
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

    // Only report skills that were actually used during this response,
    // not every loaded skill in the registry.
    let activated: Vec<String> = agent_response.active_skills.clone();

    let compaction_info = trace.iter().find(|t| t.event == "Compaction").map(|t| {
        // Parse tokens from trace detail: "{Level} {before} -> {after} tokens"
        let parts: Vec<&str> = t.detail.split_whitespace().collect();
        let level = parts.first().unwrap_or(&"unknown").to_string();
        let tokens_before = parts.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
        let tokens_after = parts.get(3).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
        CompactionInfo {
            level,
            tokens_before,
            tokens_after,
        }
    });

    // Save content before moving into ChatMessage so block can reference it.
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

    // ── CRITICAL: Persist assistant response to session IMMEDIATELY ──
    // This MUST happen before audit logging, memory writes, and other
    // post-processing. The frontend streaming `Finished` event fires before
    // this function returns, so if the user switches threads before we
    // persist here, get_session_messages would return stale data.
    {
        // Store tool messages in separate tool_history key instead of
        // inflating the main session. This keeps session serialization O(V) where
        // V = visible messages (user + final assistant) instead of O(V + T) where
        // T = tool messages (can be 50+ per turn for tool-heavy conversations).
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
            // Persist tool history asynchronously (best-effort)
            if let Err(e) = state.persist_tool_history(&chat_id, &tool_chat_msgs) {
                tracing::warn!(chat_id = %chat_id, error = %e, "Failed to persist tool history");
            }
        }

        // Unified write via append_session_message
        let msg_count = state.append_session_message(
            &chat_id, &agent.id, &auto_title, assistant_msg.clone(), &now,
        ).map_err(|e| {
            crate::commands_debug::emit_debug(&app, crate::commands_debug::DebugEvent::error(
                "persist", "asst_msg_persist_FAIL",
                format!("FAILED to persist assistant message: {}", e),
            ));
            e
        })?;
        crate::commands_debug::emit_debug(&app, crate::commands_debug::DebugEvent::info(
            "persist", "asst_msg_persisted",
            format!("Assistant message persisted. chat_id={}, msgs_in_session={}", chat_id, msg_count),
        ));

        // Write assistant response to ConversationStore under SessionKey.
        // BUG FIX: Use Utc::now() instead of `now` (which is the request-start
        // timestamp). Using the same `now` for both user and assistant messages
        // causes them to share the same millisecond key in ConversationStore
        // (`sessions/{key}/messages/{ts}`), which means the assistant put()
        // OVERWRITES the user message. On restart, load_history() then returns
        // history with all user messages missing — causing the LLM to lose
        // context of what the user actually asked.
        {
            use clawdesk_storage::conversation_store::ConversationStore;
            use clawdesk_types::session::{AgentMessage, Role};
            let assistant_ts = Utc::now();
            let agent_msg = AgentMessage {
                role: Role::Assistant,
                content: response_content.clone(),
                timestamp: assistant_ts,
                model: None, // AgentResponse does not carry model name; set by caller if needed
                token_count: Some((agent_response.input_tokens + agent_response.output_tokens) as usize),
                tool_call_id: None,
                tool_name: None,
            };
            if let Err(e) = state.soch_store.append_message(&session_key, &agent_msg).await {
                tracing::warn!(error = %e, "ConversationStore append_message failed for assistant msg");
            }
        }

        // Periodically index the session into semantic memory.
        // Every 10 turns, chunk the conversation and store in MemoryManager
        // so it can be recalled in future conversations.
        if msg_count % 10 == 0 && msg_count >= 4 {
            if let Some(session) = state.sessions.get(&chat_id) {
                let session_msgs: Vec<clawdesk_memory::SessionMessage> = session
                    .messages
                    .iter()
                    .map(|m| clawdesk_memory::SessionMessage {
                        role: m.role.clone(),
                        content: m.content.clone(),
                    })
                    .collect();
                let memory = Arc::clone(&state.memory);
                let chat_id_owned = chat_id.clone();
                // Spawn indexing as a background task — don't block the response
                tokio::spawn(async move {
                    let config = clawdesk_memory::SessionIndexConfig::default();
                    match clawdesk_memory::index_session(&memory, &chat_id_owned, &session_msgs, &config).await {
                        Ok(chunks) => {
                            tracing::info!(
                                chat_id = %chat_id_owned,
                                chunks,
                                "Session indexed into memory"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                chat_id = %chat_id_owned,
                                error = %e,
                                "Session indexing failed"
                            );
                        }
                    }
                });
            }
        }
    }
    let _ = app.emit(
        "incoming:message",
        serde_json::json!({
            "agent_id": agent.id,
            "chat_id": &chat_id,
            "preview": assistant_msg.content.chars().take(120).collect::<String>(),
            "timestamp": assistant_msg.timestamp,
        }),
    );

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

    // ── Memory Write Path (unified engine) ──
    // Durable write with UTF-8 safe truncation, content-hash dedup, and
    // batch embedding. Same logic for desktop and Discord paths.
    {
        let mem = Arc::clone(&state.memory);
        let temporal_graph = Arc::clone(&state.temporal_graph);
        let user_content = request.content.clone();
        let asst_content = assistant_msg.content.clone();
        let agent_id_for_mem = agent.id.clone();
        let agent_name = agent.name.clone();

        tokio::spawn(async move {
            crate::engine::store_conversation_memory(
                &mem,
                &user_content,
                &asst_content,
                &agent_id_for_mem,
                &agent_name,
                Some(&temporal_graph),
            )
            .await;
        });
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

    state.persist();
    emit_metrics_updated(&app, &state);
    emit_security_changed(&app, &state).await;

    // ── Emit to reactive event bus ──
    // Publishes "agent.message.sent" so bus subscribers (pipelines,
    // cron, telemetry, orchestrator bridge) can react.
    {
        let bus = &state.event_bus;
        let content = &assistant_msg.content;
        let preview = if content.len() > 200 {
            &content[..200]
        } else {
            content
        };
        bus.emit(
            "agent.message.sent",
            clawdesk_bus::event::EventKind::MessageSent,
            clawdesk_bus::event::Priority::Standard,
            serde_json::json!({
                "agent_id": agent.id,
                "chat_id": chat_id,
                "preview": preview,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }),
            format!("agent:{}", agent.id),
        )
        .await;
    }

    if let Some(err_msg) = execution_err {
        return Err(err_msg);
    }

    Ok(SendMessageResponse {
        message: assistant_msg,
        trace,
        chat_id: chat_id.clone(),
        chat_title: if is_new_chat { Some(auto_title) } else { None },
    })
}

/// Get message history for a chat by chat_id.
#[tauri::command]
pub async fn get_session_messages(agent_id: String, state: State<'_, AppState>) -> Result<Vec<ChatMessage>, String> {
    // Support both chat_id and agent_id lookups for backward compat
    if let Some(session) = state.sessions.get(&agent_id) {
        return Ok(session.messages);
    }
    // Fallback: search by agent_id (return first matching chat)
    for session in state.sessions.values() {
        if session.agent_id == agent_id {
            return Ok(session.messages);
        }
    }
    Ok(vec![])
}

/// Cancel active chat runs.
///
/// If `chat_id` is provided, cancels only that run.
/// If `chat_id` is `None`, cancels all active chat runs.
#[tauri::command]
pub async fn cancel_active_run(
    chat_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let mut active = state.active_chat_runs.write().await;

    if let Some(chat_id) = chat_id {
        if let Some(token) = active.remove(&chat_id) {
            token.cancel();
            return Ok(true);
        }
        return Ok(false);
    }

    let had_any = !active.is_empty();
    for (_, token) in active.drain() {
        token.cancel();
    }
    Ok(had_any)
}

/// Get message history for a specific chat by chat_id.
/// Filters out intermediate tool messages — only returns user, system,
/// and final assistant messages (those with metadata).
///
/// Uses `read_through_session()` so that sessions evicted from the LRU
/// hot cache are transparently recovered from SochDB (source of truth).
#[tauri::command]
pub async fn get_chat_messages(chat_id: String, state: State<'_, AppState>) -> Result<Vec<ChatMessage>, String> {
    Ok(state.read_through_session(&chat_id)
        .map(|s| {
            s.messages.iter()
                .filter(|m| {
                    if m.role == "user" { return true; }
                    if m.role == "assistant" && m.metadata.is_some() { return true; }
                    if m.role == "system" { return true; }
                    false
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default())
}

#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub chat_id: String,
    pub agent_id: String,
    pub title: String,
    pub created_at: String,
    pub last_activity: String,
    pub message_count: usize,
    pub pending_approvals: usize,
    pub routine_generated: bool,
    pub has_proof_outputs: bool,
    /// Preview of the first user message (for identification in the sidebar).
    pub first_message_preview: Option<String>,
}

#[tauri::command]
pub async fn list_sessions(state: State<'_, AppState>) -> Result<Vec<SessionSummary>, String> {
    // Read directly from SochDB (source of truth) instead of the LRU hot cache.
    // The LRU cache only holds the 200 most-recently-accessed sessions, so
    // `state.sessions.values()` can silently drop older conversations.
    // SochDB scan returns ALL persisted sessions regardless of cache state.
    let all_sessions: Vec<ChatSession> = {
        let entries = state.soch_store.scan("chats/")
            .map_err(|e| format!("Failed to scan sessions from SochDB: {}", e))?;
        entries
            .into_iter()
            .filter_map(|(_key, bytes)| serde_json::from_slice::<ChatSession>(&bytes).ok())
            .collect()
    };

    let mut summaries: Vec<SessionSummary> = all_sessions
        .iter()
        .map(|session| {
            // Count only user messages and final assistant messages (with metadata).
            // Skip intermediate tool messages and tool_use messages without metadata.
            let visible_count = session.messages.iter().filter(|m| {
                if m.role == "user" { return true; }
                if m.role == "assistant" && m.metadata.is_some() { return true; }
                if m.role == "system" { return true; }
                false
            }).count();
            // First user message preview (truncated to 80 chars) for identification
            let first_preview = session.messages.iter()
                .find(|m| m.role == "user")
                .map(|m| {
                    let s: String = m.content.chars().take(80).collect();
                    if m.content.chars().count() > 80 { format!("{s}…") } else { s }
                });
            SessionSummary {
                chat_id: session.id.clone(),
                agent_id: session.agent_id.clone(),
                title: session.title.clone(),
                created_at: session.created_at.clone(),
                last_activity: session.updated_at.clone(),
                message_count: visible_count,
                pending_approvals: 0,
                routine_generated: false,
                has_proof_outputs: session.messages.iter().any(|m| m.role == "assistant" && m.metadata.is_some()),
                first_message_preview: first_preview,
            }
        })
        .collect();

    // Deterministic sort: primary by last_activity DESC, secondary by created_at DESC,
    // tertiary by chat_id (absolute stability across restarts).
    summaries.sort_by(|a, b| {
        b.last_activity.cmp(&a.last_activity)
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.chat_id.cmp(&b.chat_id))
    });

    tracing::info!(
        total = summaries.len(),
        "list_sessions: scanned SochDB directly — returning all persisted sessions"
    );
    if !summaries.is_empty() {
        tracing::debug!(
            first_chat = %summaries[0].chat_id,
            first_title = %summaries[0].title,
            first_activity = %summaries[0].last_activity,
            last_chat = %summaries.last().unwrap().chat_id,
            last_activity = %summaries.last().unwrap().last_activity,
            "list_sessions: ordering envelope"
        );
    }
    Ok(summaries)
}

/// Create a new empty chat session for an agent.
#[tauri::command]
pub async fn create_chat(agent_id: String, state: State<'_, AppState>) -> Result<SessionSummary, String> {
    let agents = state.agents.read().map_err(|e| e.to_string())?;
    let agent = agents.get(&agent_id)
        .ok_or_else(|| format!("Agent '{}' not found", agent_id))?;

    let chat_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let session = ChatSession {
        id: chat_id.clone(),
        agent_id: agent_id.clone(),
        title: format!("New chat with {}", agent.name),
        messages: Vec::new(),
        created_at: now.clone(),
        updated_at: now.clone(),
    };

    state.sessions.insert(chat_id.clone(), session.clone());
    state.persist_session(&chat_id, &session)?;

    Ok(SessionSummary {
        chat_id,
        agent_id,
        title: session.title,
        created_at: session.created_at,
        last_activity: session.updated_at,
        message_count: 0,
        pending_approvals: 0,
        routine_generated: false,
        has_proof_outputs: false,
        first_message_preview: None,
    })
}

/// Diagnostic: dump all session metadata from SochDB for debugging.
/// Returns a JSON-serializable list of session metadata WITHOUT full message content.
#[tauri::command]
pub async fn debug_session_storage(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    let entries = state.soch_store.scan("chats/")
        .map_err(|e| format!("SochDB scan failed: {}", e))?;

    let mut results = Vec::new();
    for (key, bytes) in &entries {
        match serde_json::from_slice::<ChatSession>(bytes) {
            Ok(session) => {
                results.push(serde_json::json!({
                    "sochdb_key": key,
                    "chat_id": session.id,
                    "agent_id": session.agent_id,
                    "title": session.title,
                    "created_at": session.created_at,
                    "updated_at": session.updated_at,
                    "message_count": session.messages.len(),
                    "first_msg_time": session.messages.first().map(|m| m.timestamp.clone()),
                    "last_msg_time": session.messages.last().map(|m| m.timestamp.clone()),
                    "first_msg_role": session.messages.first().map(|m| m.role.clone()),
                    "last_msg_role": session.messages.last().map(|m| m.role.clone()),
                    "bytes": bytes.len(),
                    "in_lru_cache": state.sessions.contains(&session.id),
                }));
            }
            Err(e) => {
                results.push(serde_json::json!({
                    "sochdb_key": key,
                    "error": e.to_string(),
                    "bytes": bytes.len(),
                    "preview": String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).to_string(),
                }));
            }
        }
    }

    // Sort by updated_at desc for readability
    results.sort_by(|a, b| {
        let a_time = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let b_time = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        b_time.cmp(a_time)
    });

    tracing::info!(
        total_entries = entries.len(),
        deserialized = results.iter().filter(|r| r.get("chat_id").is_some()).count(),
        errors = results.iter().filter(|r| r.get("error").is_some()).count(),
        lru_cache_size = state.sessions.len(),
        "debug_session_storage: full SochDB session diagnostic"
    );

    Ok(results)
}

/// Delete a chat session with full cascade (session state + messages + summaries +
/// indexes + tool history + graph + traces + checkpoints + memory namespace).
///
/// Uses the LifecycleManager for unified cleanup across all storage layers.
#[tauri::command]
pub async fn delete_chat(chat_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    // Grab agent_id before removing from cache (needed for cascade cleanup)
    let agent_id = state.sessions.get(&chat_id).map(|s| s.agent_id.clone());
    state.sessions.remove(&chat_id);

    // Full cascade delete via LifecycleManager
    let report = state.lifecycle_manager.delete_session_full(
        &chat_id,
        agent_id.as_deref(),
    )?;

    tracing::info!(
        chat_id = %chat_id,
        total_deleted = report.total_deleted,
        warnings = report.warnings.len(),
        duration_us = report.duration_us,
        "delete_chat: cascade complete"
    );

    if !report.warnings.is_empty() {
        for w in &report.warnings {
            tracing::warn!(chat_id = %chat_id, warning = %w, "delete_chat: cascade warning");
        }
    }

    Ok(true)
}

/// Clear all chat history — delete every session from SochDB and the in-memory cache.
/// Used by the "Full Reset" UI action.
///
/// Uses `SochStore::clear_all_chat_data()` which truncates the WAL after
/// deleting to work around SochDB's WAL recovery not preserving tombstones.
/// Without this, deleted sessions re-appear after restart because the WAL
/// replay turns tombstones into empty-value writes.
#[tauri::command]
pub async fn clear_all_chats(state: State<'_, AppState>) -> Result<u32, String> {
    // 1. Clear the in-memory session cache
    let count = state.sessions.len() as u32;
    state.sessions.clear();

    // 2. Atomically clear all chat data from SochDB (delete + WAL truncate + re-persist non-chat data)
    match state.soch_store.clear_all_chat_data() {
        Ok(deleted) => {
            tracing::info!(
                cached = count,
                sochdb = deleted,
                "All chat history cleared (WAL truncated, non-chat data preserved)"
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "clear_all_chats: clear_all_chat_data failed");
            return Err(format!("Failed to clear chats: {e}"));
        }
    }

    Ok(count)
}

/// Rename a chat session.
#[tauri::command]
pub async fn update_chat_title(chat_id: String, title: String, state: State<'_, AppState>) -> Result<bool, String> {
    let found = state.sessions.mutate(&chat_id, |session| {
        session.title = title;
    });
    if found {
        let session = state.sessions.get(&chat_id).ok_or_else(|| format!("Chat '{}' disappeared", chat_id))?;
        state.persist_session(&chat_id, &session)?;
        Ok(true)
    } else {
        Err(format!("Chat '{}' not found", chat_id))
    }
}

/// Export a session as Markdown.
#[tauri::command]
pub async fn export_session_markdown(
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // Find by chat_id first, then by agent_id
    let session = state.sessions.get(&agent_id)
        .or_else(|| state.sessions.values().into_iter().find(|s| s.agent_id == agent_id))
        .ok_or("Session not found")?;

    let agent_name = state
        .agents
        .read()
        .ok()
        .and_then(|a| a.get(&session.agent_id).map(|ag| ag.name.clone()))
        .unwrap_or_else(|| "Agent".to_string());

    let mut md = format!("# Conversation with {}\n\n", agent_name);
    md.push_str(&format!("*Exported: {}*\n\n---\n\n", Utc::now().to_rfc3339()));

    for msg in &session.messages {
        let role_label = match msg.role.as_str() {
            "user" => "**You**",
            "assistant" => &format!("**{}**", agent_name),
            "tool" => "**Tool**",
            "system" => "**System**",
            _ => "**Unknown**",
        };
        md.push_str(&format!("{}\n\n{}\n\n---\n\n", role_label, msg.content));
    }

    Ok(md)
}

/// Export a session as JSON.
#[tauri::command]
pub async fn export_session_json(
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    // Find by chat_id first, then by agent_id
    let messages = state.sessions.get(&agent_id)
        .or_else(|| state.sessions.values().into_iter().find(|s| s.agent_id == agent_id))
        .map(|s| s.messages)
        .unwrap_or_default();
    serde_json::to_value(&messages).map_err(|e| e.to_string())
}

/// Clone an agent (deep copy with new ID).
#[tauri::command]
pub async fn clone_agent(
    agent_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<DesktopAgent, String> {
    let source = {
        let agents = state.agents.read().map_err(|e| e.to_string())?;
        agents
            .get(&agent_id)
            .cloned()
            .ok_or_else(|| format!("Agent '{}' not found", agent_id))?
    };

    let new_id = Uuid::new_v4().to_string();
    let identity = IdentityContract::new(source.persona.clone(), IdentitySource::UserConfig);

    let cloned = DesktopAgent {
        id: new_id,
        name: format!("{} (Copy)", source.name),
        persona_hash: identity.persona_hash_hex(),
        created: Utc::now().to_rfc3339(),
        msg_count: 0,
        tokens_used: 0,
        status: "ready".to_string(),
        ..source
    };

    {
        let mut identities = state.identities.write().map_err(|e| e.to_string())?;
        identities.insert(cloned.id.clone(), identity);
    }
    {
        let mut agents = state.agents.write().map_err(|e| e.to_string())?;
        agents.insert(cloned.id.clone(), cloned.clone());
    }

    state.persist_agent(&cloned.id, &cloned);

    state
        .audit_logger
        .log(
            AuditCategory::SessionLifecycle,
            "agent_cloned",
            AuditActor::System,
            Some(cloned.id.clone()),
            serde_json::json!({
                "source_agent_id": agent_id,
                "new_agent_id": &cloned.id,
                "name": &cloned.name,
            }),
            AuditOutcome::Success,
        )
        .await;

    emit_security_changed(&app, &state).await;
    Ok(cloned)
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
            // Resolve a real description from the skill manifest via the registry
            let real_description = {
                let skill_id = &info.id;
                reg.get(skill_id)
                    .map(|entry| entry.skill.manifest.description.clone())
                    .unwrap_or_default()
            };
            let description = if real_description.is_empty() {
                let source_label = match &info.source {
                    SkillSource::Builtin => "builtin".to_string(),
                    SkillSource::Local { path } => format!("local:{}", path),
                    SkillSource::Remote { url, .. } => format!("remote:{}", url),
                };
                format!("v{} - {}", info.version, source_label)
            } else {
                real_description
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
                description,
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

/// Delete a skill from the SkillRegistry entirely.
#[tauri::command]
pub async fn delete_skill(skill_id: String, state: State<'_, AppState>, app: AppHandle) -> Result<bool, String> {
    {
        let mut reg = state.skill_registry.write().map_err(|e| e.to_string())?;
        let id = SkillId::from(skill_id.as_str());
        reg.remove(&id);
    }

    // Also remove from SochDB if persisted
    let key = format!("skills/{}", skill_id);
    let _ = state.soch_store.delete(&key);

    state
        .audit_logger
        .log(
            AuditCategory::ConfigChange,
            "skill_deleted",
            AuditActor::System,
            Some(skill_id),
            serde_json::json!({}),
            AuditOutcome::Success,
        )
        .await;
    emit_security_changed(&app, &state).await;

    Ok(true)
}

/// Get full detail for a skill — including prompt fragment (instructions).
#[derive(Debug, Serialize)]
pub struct SkillDetail {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub category: String,
    pub instructions: String,
    pub tags: Vec<String>,
    pub required_tools: Vec<String>,
    pub estimated_tokens: usize,
    pub state: String,
    pub source: String,
    pub author: Option<String>,
}

#[tauri::command]
pub async fn get_skill_detail(skill_id: String, state: State<'_, AppState>) -> Result<SkillDetail, String> {
    let reg = state.skill_registry.read().map_err(|e| e.to_string())?;
    let id = SkillId::from(skill_id.as_str());
    let entry = reg.get(&id).ok_or_else(|| format!("Skill {} not found", skill_id))?;
    let skill = &entry.skill;
    let state_str = match entry.state {
        SkillState::Active => "active",
        SkillState::Loaded => "loaded",
        SkillState::Resolved => "resolved",
        SkillState::Discovered => "discovered",
        SkillState::Disabled => "disabled",
        SkillState::Failed => "failed",
    };
    let source_label = match &entry.source {
        SkillSource::Builtin => "builtin".to_string(),
        SkillSource::Local { path } => format!("local:{}", path),
        SkillSource::Remote { url, .. } => format!("remote:{}", url),
    };
    Ok(SkillDetail {
        id: skill.manifest.id.as_str().to_string(),
        name: skill.manifest.display_name.clone(),
        description: skill.manifest.description.clone(),
        version: skill.manifest.version.clone(),
        category: skill.manifest.id.namespace().to_string(),
        instructions: skill.prompt_fragment.clone(),
        tags: skill.manifest.tags.clone(),
        required_tools: skill.manifest.required_tools.clone(),
        estimated_tokens: skill.token_cost(),
        state: state_str.to_string(),
        source: source_label,
        author: skill.manifest.author.clone(),
    })
}

/// Register or update a skill from the SkillDesigner.
#[derive(Debug, Deserialize)]
pub struct RegisterSkillRequest {
    pub name: String,
    pub description: String,
    pub version: String,
    pub category: String,
    pub instructions: String,
    pub tags: Vec<String>,
    pub allowed_tools: Vec<String>,
    /// When editing an existing skill, pass its original ID here to update
    /// rather than creating a new skill with a generated ID.
    #[serde(default)]
    pub existing_id: Option<String>,
}

#[tauri::command]
pub async fn register_skill(
    request: RegisterSkillRequest,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<SkillDescriptor, String> {
    use clawdesk_skills::definition::{Skill, SkillManifest};

    // Use existing_id when editing, otherwise build from category + name
    let skill_id_str = if let Some(ref eid) = request.existing_id {
        eid.clone()
    } else {
        format!("{}/{}", request.category, request.name)
    };
    let skill_id = SkillId::from(skill_id_str.as_str());

    // Scan instructions for security issues
    let scan = state.scanner.scan(&request.instructions);
    if !scan.passed {
        let findings: Vec<String> = scan.findings.iter()
            .filter(|f| f.severity == clawdesk_types::security::Severity::Critical)
            .map(|f| format!("{}: {}", f.rule, f.description))
            .collect();
        if !findings.is_empty() {
            return Err(format!("Skill instructions blocked by security scanner: {}", findings.join("; ")));
        }
    }

    let manifest = SkillManifest {
        id: skill_id.clone(),
        display_name: request.name.clone(),
        description: request.description.clone(),
        version: request.version.clone(),
        author: Some("user".to_string()),
        dependencies: vec![],
        required_tools: request.allowed_tools.clone(),
        parameters: vec![],
        triggers: vec![clawdesk_skills::definition::SkillTrigger::Always],
        estimated_tokens: clawdesk_types::tokenizer::estimate_tokens(&request.instructions),
        priority_weight: 1.0,
        tags: request.tags.clone(),
        signature: None,
        publisher_key: None,
        content_hash: None,
        schema_version: 1,
    };

    let skill = Skill {
        manifest,
        prompt_fragment: request.instructions.clone(),
        provided_tools: vec![],
        parameter_values: serde_json::json!({}),
        source_path: None,
    };

    // Register in the skill registry (overwrites if same ID)
    {
        let source = if request.existing_id.is_some() {
            SkillSource::Local { path: "user-edited".to_string() }
        } else {
            SkillSource::Local { path: "user-designed".to_string() }
        };
        let mut reg = state.skill_registry.write().map_err(|e| e.to_string())?;
        reg.register(skill, source);
        reg.activate(&skill_id).map_err(|e| e.to_string())?;
    }

    state.audit_logger.log(
        AuditCategory::ConfigChange,
        "skill_registered",
        AuditActor::User { sender_id: "desktop".into(), channel: "tauri".into() },
        Some(skill_id_str.clone()),
        serde_json::json!({ "name": request.name, "category": request.category }),
        AuditOutcome::Success,
    ).await;
    emit_security_changed(&app, &state).await;

    Ok(SkillDescriptor {
        id: skill_id_str,
        name: request.name,
        description: request.description,
        category: request.category,
        estimated_tokens: clawdesk_types::tokenizer::estimate_tokens(&request.instructions),
        state: "active".to_string(),
        verified: false,
        icon: "⚡".to_string(),
    })
}

/// Backend validation of SKILL.md content — runs through parse_skill_md + adapt_skill.
#[derive(Debug, Serialize)]
pub struct SkillValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub estimated_tokens: usize,
    pub parsed_name: Option<String>,
    pub parsed_description: Option<String>,
}

#[tauri::command]
pub async fn validate_skill_md(
    skill_md_content: String,
    state: State<'_, AppState>,
) -> Result<SkillValidationResult, String> {
    use clawdesk_skills::openclaw_adapter::{parse_skill_md, adapt_skill, AdapterConfig};

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut parsed_name = None;
    let mut parsed_description = None;
    let mut estimated_tokens = 0;

    // Step 1: Parse SKILL.md frontmatter
    match parse_skill_md(&skill_md_content) {
        Ok((frontmatter, body)) => {
            parsed_name = frontmatter.name.clone();
            parsed_description = frontmatter.description.clone();

            if parsed_name.is_none() {
                errors.push("Missing 'name' in SKILL.md frontmatter.".to_string());
            }
            if parsed_description.is_none() {
                warnings.push("Missing 'description' in SKILL.md frontmatter.".to_string());
            }

            // Step 2: Try full adapter pipeline
            let config = AdapterConfig::default();
            match adapt_skill(&frontmatter, &body, &config) {
                Ok(adapted) => {
                    estimated_tokens = adapted.skill.token_cost();
                    if adapted.skill.prompt_fragment.trim().is_empty() {
                        warnings.push("Skill instructions body is empty.".to_string());
                    }
                    if estimated_tokens > 8000 {
                        warnings.push(format!(
                            "Large skill: {} tokens. Consider trimming to reduce context cost.",
                            estimated_tokens
                        ));
                    }
                }
                Err(e) => {
                    errors.push(format!("Adapter pipeline failed: {}", e));
                }
            }
        }
        Err(e) => {
            errors.push(format!("Failed to parse SKILL.md: {}", e));
        }
    }

    // Step 3: Security scan
    let scan = state.scanner.scan(&skill_md_content);
    if !scan.passed {
        for finding in &scan.findings {
            if finding.severity == clawdesk_types::security::Severity::Critical {
                errors.push(format!("Security: {} - {}", finding.rule, finding.description));
            } else {
                warnings.push(format!("Security: {} - {}", finding.rule, finding.description));
            }
        }
    }

    let valid = errors.is_empty();
    Ok(SkillValidationResult {
        valid,
        errors,
        warnings,
        estimated_tokens,
        parsed_name,
        parsed_description,
    })
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
    /// Optional cron expression for scheduled execution (e.g. "0 9 * * 1-5").
    #[serde(default)]
    pub schedule: Option<String>,
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
        schedule: request.schedule,
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

    // Sync cron schedule — register/remove CronTask for this pipeline
    sync_pipeline_cron_schedule(&state, &result).await;

    Ok(result)
}

/// Delete a pipeline by ID — removes from memory, SochDB, and cron schedule.
#[tauri::command]
pub async fn delete_pipeline(
    pipeline_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    {
        let mut pipelines = state.pipelines.write().map_err(|e| e.to_string())?;
        let before = pipelines.len();
        pipelines.retain(|p| p.id != pipeline_id);
        if pipelines.len() == before {
            return Err(format!("Pipeline {} not found", pipeline_id));
        }
    }
    // Remove from SochDB
    state.delete_pipeline_from_store(&pipeline_id);
    // Remove cron task
    let task_id = format!("pipeline:{}", pipeline_id);
    let _ = state.cron_manager.remove_task(&task_id).await;

    state
        .audit_logger
        .log(
            AuditCategory::ConfigChange,
            "pipeline_deleted",
            AuditActor::System,
            Some(pipeline_id),
            serde_json::json!({}),
            AuditOutcome::Success,
        )
        .await;

    Ok(true)
}

/// Update an existing pipeline — replaces steps, edges, schedule, name, description.
#[tauri::command]
pub async fn update_pipeline(
    request: CreatePipelineRequest,
    pipeline_id: String,
    state: State<'_, AppState>,
) -> Result<PipelineDescriptor, String> {
    let updated = {
        let mut pipelines = state.pipelines.write().map_err(|e| e.to_string())?;
        let pipeline = pipelines.iter_mut().find(|p| p.id == pipeline_id)
            .ok_or_else(|| format!("Pipeline {} not found", pipeline_id))?;
        pipeline.name = request.name;
        pipeline.description = request.description;
        pipeline.steps = request.steps;
        pipeline.edges = request.edges;
        pipeline.schedule = request.schedule;
        pipeline.clone()
    };

    // Write-through to SochDB
    state.persist_pipeline(&updated);

    state
        .audit_logger
        .log(
            AuditCategory::ConfigChange,
            "pipeline_updated",
            AuditActor::System,
            Some(updated.id.clone()),
            serde_json::json!({ "name": &updated.name, "steps": updated.steps.len() }),
            AuditOutcome::Success,
        )
        .await;

    // Sync cron schedule
    sync_pipeline_cron_schedule(&state, &updated).await;

    Ok(updated)
}

/// Get historical pipeline run results from SochDB.
#[tauri::command]
pub async fn get_pipeline_runs(
    pipeline_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let prefix = format!("pipeline_runs/{}/", pipeline_id);
    match state.soch_store.scan(&prefix) {
        Ok(entries) => {
            let mut runs: Vec<serde_json::Value> = entries
                .into_iter()
                .filter_map(|(_key, value)| serde_json::from_slice(&value).ok())
                .collect();
            // Sort by completed_at descending (newest first)
            runs.sort_by(|a, b| {
                let ta = a.get("completed_at").and_then(|v| v.as_str()).unwrap_or("");
                let tb = b.get("completed_at").and_then(|v| v.as_str()).unwrap_or("");
                tb.cmp(ta)
            });
            Ok(runs)
        }
        Err(e) => Err(format!("Failed to read pipeline runs: {}", e)),
    }
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
    // Session lane serialization — prevent concurrent pipeline runs
    // for the same pipeline from racing. Use "pipeline:{id}" as the session key
    // to keep pipeline lanes separate from chat session lanes.
    let lane_key = format!("pipeline:{}", pipeline_id);
    let _lane_guard = state.session_lanes.acquire(&lane_key).await
        .map_err(|e| format!("Failed to acquire pipeline lane: {}", e))?;

    let pipeline = {
        let pipelines = state.pipelines.read().map_err(|e| e.to_string())?;
        pipelines.iter().find(|p| p.id == pipeline_id)
            .cloned()
            .ok_or_else(|| format!("Pipeline {} not found", pipeline_id))?
    };

    let start = Instant::now();
    let mut step_results: Vec<serde_json::Value> = Vec::new();
    let mut step_outputs: std::collections::HashMap<usize, String> = std::collections::HashMap::new();

    // ── Build cross-channel tool hint ──────────────────────────────
    // The `message_send` tool is in state.tool_registry but the LLM
    // won't use it unless the system prompt tells it to. Build the
    // same channel hint that the chat path injects, so pipeline agents
    // can send messages to Telegram, Discord, etc.
    let channel_tool_hint: String = {
        use clawdesk_types::channel::ChannelId;
        let names: Vec<String> = match state.channel_registry.read() {
            Ok(reg) => {
                let list = reg.list();
                // Filter out WebChat and Internal — same as the chat path.
                // Only external channels (Telegram, Discord, etc.) are relevant.
                let external: Vec<String> = list.iter()
                    .filter(|id| !matches!(id, ChannelId::WebChat | ChannelId::Internal))
                    .map(|id| format!("{}", id).to_lowercase())
                    .collect();
                tracing::info!(
                    total_channels = list.len(),
                    external_channels = external.len(),
                    channels = ?external,
                    "run_pipeline: channel registry channels available"
                );
                external
            }
            Err(e) => {
                tracing::error!(error = %e, "run_pipeline: channel_registry lock POISONED — message_send will not work");
                Vec::new()
            }
        };
        if names.is_empty() {
            tracing::warn!("run_pipeline: no external channels available — message_send tool hint will be empty");
            String::new()
        } else {
            format!(
                "\n\n## Available Messaging Channels\n\
                 You have access to these messaging channels: [{}]. \
                 When your instructions mention sending a message to one of these channels, \
                 you MUST use the `message_send` tool to deliver it. \
                 Call `message_send` with channel=\"<channel_name>\" and content=\"<your message>\". \
                 IMPORTANT: You MUST actually call the tool — do NOT just describe or narrate what you would do. \
                 Do NOT ask for IDs, configuration, or confirmation. Just call the tool immediately.",
                names.join(", ")
            )
        }
    };

    // NOTE: Pipeline ↔ Runner Bridge
    //
    // A `RunnerBackend` + `PipelineExecutor` abstraction exists in
    // `clawdesk_agents::agent_backend_bridge` and `pipeline_executor`.
    // The inline loop below pre-dates that abstraction and implements
    // its own DAG scheduling, model resolution, custom prompt injection,
    // gate approval, and parallel level grouping.
    //
    // Migration path:
    //   1. Extend `PipelineAgentConfig` to support step-level custom prompts,
    //      model negotiation, and per-step tool registries
    //   2. Add `PipelineDescriptor → AgentPipeline` conversion
    //   3. Replace this inline loop with:
    //      ```
    //      let backend = RunnerBackend::new(provider, tools, cancel);
    //      for (id, agent) in agents { backend.register_agent(id, config); }
    //      let executor = PipelineExecutor::new(Arc::new(backend));
    //      executor.execute(&pipeline.into(), input).await
    //      ```
    //   4. Delete the 300+ lines below

    // ── Build DAG structures: adjacency, in-degree, topo order ──
    let num_steps = pipeline.steps.len();
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); num_steps];
    let mut in_degree = vec![0usize; num_steps];
    for &(from, to) in &pipeline.edges {
        if from < num_steps && to < num_steps {
            adjacency[from].push(to);
            in_degree[to] += 1;
        }
    }

    // Kahn's algorithm for topological order (used to compute levels)
    let execution_order: Vec<usize> = if pipeline.edges.is_empty() {
        (0..num_steps).collect()
    } else {
        let mut deg = in_degree.clone();
        let mut queue: std::collections::VecDeque<usize> = deg.iter()
            .enumerate()
            .filter(|(_, &d)| d == 0)
            .map(|(i, _)| i)
            .collect();
        let mut order = Vec::with_capacity(num_steps);
        while let Some(node) = queue.pop_front() {
            order.push(node);
            for &next in &adjacency[node] {
                deg[next] -= 1;
                if deg[next] == 0 {
                    queue.push_back(next);
                }
            }
        }
        // If cycle detected, append remaining nodes
        if order.len() < num_steps {
            for i in 0..num_steps {
                if !order.contains(&i) {
                    order.push(i);
                }
            }
        }
        order
    };

    // Predecessor map for input aggregation
    let mut predecessors: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for &(from, to) in &pipeline.edges {
        predecessors.entry(to).or_default().push(from);
    }

    // ── Compute execution levels for parallel scheduling ──
    // Level = max(level of all predecessors) + 1; source nodes = level 0.
    // Steps at the same level have no data dependencies and can run concurrently.
    let mut levels = vec![0usize; num_steps];
    for &node in &execution_order {
        if let Some(preds) = predecessors.get(&node) {
            levels[node] = preds.iter().map(|&p| levels[p]).max().unwrap_or(0) + 1;
        }
    }
    let max_level = levels.iter().copied().max().unwrap_or(0);
    let mut level_groups: Vec<Vec<usize>> = vec![Vec::new(); max_level + 1];
    for &node in &execution_order {
        level_groups[levels[node]].push(node);
    }

    // Track gate-blocked downstream nodes
    let mut blocked: std::collections::HashSet<usize> = std::collections::HashSet::new();

    // ── Level-grouped parallel execution ──
    for level_idx in 0..=max_level {
        let group: Vec<usize> = level_groups[level_idx]
            .iter()
            .copied()
            .filter(|i| !blocked.contains(i))
            .collect();
        if group.is_empty() {
            continue;
        }

        // Pre-compute predecessor inputs for each step in this level.
        // All predecessors are at lower levels and guaranteed complete.
        let step_inputs: Vec<(usize, String)> = group
            .iter()
            .map(|&i| {
                let previous_output = if let Some(preds) = predecessors.get(&i) {
                    preds
                        .iter()
                        .filter_map(|p| step_outputs.get(p))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n---\n")
                } else if i > 0 {
                    step_outputs
                        .values()
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "Pipeline started.".to_string())
                } else {
                    "Pipeline started.".to_string()
                };
                (i, previous_output)
            })
            .collect();

        // Pre-resolve agent data outside async block to avoid holding RwLock across await.
        // Returns: (step_index, previous_output, Option<(agent, provider, model_lower)>)
        let mut prepared: Vec<(
            usize,
            String,
            Option<(
                Option<DesktopAgent>,
                Arc<dyn clawdesk_providers::Provider>,
                String,
                Arc<clawdesk_agents::tools::ToolRegistry>,
            )>,
        )> = Vec::with_capacity(step_inputs.len());

        for (i, prev_out) in step_inputs {
            let step = &pipeline.steps[i];
            if step.node_type == "agent" {
                // Resolve agent
                let agent_result = if let Some(ref agent_id) = step.agent_id {
                    let agents = state.agents.read().map_err(|e| e.to_string())?;
                    agents.get(agent_id).cloned()
                } else {
                    None
                };

                // Model resolution priority for pipeline steps:
                // 1. Explicit step.model (user chose a specific model)
                // 2. channel_provider override (user's configured default from UI)
                // 3. Fallback to "default" (let negotiator pick)
                let model_lower = if let Some(ref m) = step.model {
                    m.to_lowercase()
                } else {
                    // Check channel_provider override — this is the user's
                    // configured provider from the UI Preferences/Onboarding.
                    let cp = state.channel_provider.read().ok().and_then(|g| g.clone());
                    if let Some(ref cp) = cp {
                        if !cp.model.is_empty() {
                            cp.model.to_lowercase()
                        } else {
                            "default".to_string()
                        }
                    } else {
                        "default".to_string()
                    }
                };
                let model_full = AppState::resolve_model_id(&model_lower);

                // Provider resolution via negotiator, with channel_provider override fallback.
                // This ensures pipelines use the same provider the user configured in the UI.
                let provider_result = {
                    use clawdesk_providers::capability::ProviderCaps;

                    // First try: channel_provider override (user's UI-configured provider)
                    let cp = state.channel_provider.read().ok().and_then(|g| g.clone());
                    if let Some(ref cp) = cp {
                        use clawdesk_providers::anthropic::AnthropicProvider;
                        use clawdesk_providers::openai::OpenAiProvider;
                        use clawdesk_providers::azure::AzureOpenAiProvider;
                        use clawdesk_providers::gemini::GeminiProvider;
                        use clawdesk_providers::ollama::OllamaProvider;
                        use clawdesk_providers::cohere::CohereProvider;

                        let prov_result: Result<Arc<dyn clawdesk_providers::Provider>, String> = match cp.provider.as_str() {
                            "Anthropic" => Ok(Arc::new(AnthropicProvider::new(cp.api_key.clone(), Some(model_full.clone())))),
                            "OpenAI" => Ok(Arc::new(OpenAiProvider::new(cp.api_key.clone(), if cp.base_url.is_empty() { None } else { Some(cp.base_url.clone()) }, Some(model_full.clone())))),
                            "Azure OpenAI" => {
                                let endpoint = if cp.base_url.is_empty() { "https://example.openai.azure.com".to_string() } else { cp.base_url.clone() };
                                Ok(Arc::new(AzureOpenAiProvider::new(cp.api_key.clone(), endpoint, None, Some(model_full.clone()))))
                            }
                            "Google" => Ok(Arc::new(GeminiProvider::new(cp.api_key.clone(), Some(model_full.clone())))),
                            "Cohere" => Ok(Arc::new(CohereProvider::new(cp.api_key.clone(), if cp.base_url.is_empty() { None } else { Some(cp.base_url.clone()) }, Some(model_full.clone())))),
                            "Ollama (Local)" | "ollama" => Ok(Arc::new(OllamaProvider::new(if cp.base_url.is_empty() { None } else { Some(cp.base_url.clone()) }, Some(model_full.clone())))),
                            "Local (OpenAI Compatible)" | "local_compatible" => {
                                use clawdesk_providers::compatible::{CompatibleConfig, OpenAiCompatibleProvider};
                                let url = if cp.base_url.is_empty() { "http://localhost:8080/v1".to_string() } else { cp.base_url.clone() };
                                let config = CompatibleConfig::new("local_compatible", &url, &cp.api_key).with_default_model(model_full.clone());
                                Ok(Arc::new(OpenAiCompatibleProvider::new(config)))
                            }
                            _ => {
                                // Unknown provider name — fall through to negotiator
                                let negotiator = state.negotiator.read().map_err(|e| e.to_string())?;
                                let required = ProviderCaps::TEXT_COMPLETION.union(ProviderCaps::SYSTEM_PROMPT);
                                match negotiator.resolve_model(&model_full, required) {
                                    Some((p, _)) => Ok(Arc::clone(p)),
                                    None => { drop(negotiator); state.resolve_provider(&model_lower) }
                                }
                            }
                        };
                        prov_result
                    } else {
                        // No channel_provider override — use negotiator → resolve_provider fallback.
                        let negotiator = state.negotiator.read().map_err(|e| e.to_string())?;
                        let required = ProviderCaps::TEXT_COMPLETION.union(ProviderCaps::SYSTEM_PROMPT);
                        match negotiator.resolve_model(&model_full, required) {
                            Some((p, _resolved)) => Ok(Arc::clone(p)),
                            None => {
                                drop(negotiator);
                                state.resolve_provider(&model_lower)
                            }
                        }
                    }
                };

                // Build skill-aware tool registry for agent steps.
                // All active skills are available to every agent.
                let tool_reg = {
                    let all_active: Vec<clawdesk_skills::definition::Skill> = {
                        let reg = state
                            .skill_registry
                            .read()
                            .map_err(|e| e.to_string())?;
                        reg.active_skills()
                            .iter()
                            .map(|s| (**s).clone())
                            .collect()
                    };
                    if all_active.is_empty() {
                        Arc::clone(&state.tool_registry)
                    } else {
                        Arc::new(build_skill_tool_registry(
                            &all_active,
                            &state.tool_registry,
                        ))
                    }
                };

                match provider_result {
                    Ok(provider) => {
                        prepared.push((
                            i,
                            prev_out,
                            Some((agent_result, provider, model_lower, tool_reg)),
                        ));
                    }
                    Err(e) => {
                        // Provider resolution failed — record immediately
                        step_results.push(serde_json::json!({
                            "step_index": i,
                            "label": step.label,
                            "node_type": "agent",
                            "success": false,
                            "duration_ms": 0,
                            "error": format!("Provider resolution failed: {}", e),
                        }));
                    }
                }
            } else {
                prepared.push((i, prev_out, None));
            }
        }

        // Execute all steps in this level concurrently via join_all.
        // Agent steps benefit from I/O concurrency (parallel LLM API calls).
        let state_ref = &state;
        let pipeline_ref = &pipeline;
        let hint_ref = &channel_tool_hint;

        let futures: Vec<_> = prepared
            .into_iter()
            .map(|(i, previous_output, agent_prep)| {
                let channel_hint = hint_ref.clone();
                async move {
                let step = &pipeline_ref.steps[i];
                let step_start = Instant::now();

                match step.node_type.as_str() {
                    "input" => (
                        i,
                        Some("Pipeline input received.".to_string()),
                        serde_json::json!({
                            "step_index": i,
                            "label": step.label,
                            "node_type": "input",
                            "success": true,
                            "duration_ms": step_start.elapsed().as_millis() as u64,
                            "output": "Pipeline input received.",
                        }),
                        false,
                    ),
                    "agent" => {
                        if let Some((agent_result, provider, model_lower, tool_reg)) = agent_prep {
                            let model_id = AppState::resolve_model_id(&model_lower);

                            // Read step config for custom prompts and settings.
                            // User-configured prompt text and max_rounds from the
                            // pipeline step UI config override defaults.
                            let custom_prompt = step.config.get("prompt").cloned().filter(|s| !s.trim().is_empty());
                            let max_rounds: usize = step.config.get("max_rounds")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(10);

                            let base_prompt = agent_result
                                .as_ref()
                                .map(|a| a.persona.clone())
                                .unwrap_or_else(|| format!("You are a {} agent.", step.label));

                            // If the step has a custom prompt, append it to the
                            // agent's base persona. This allows pipeline designers to
                            // specialize agent behavior per-step.
                            // The channel_tool_hint ensures the LLM knows about
                            // message_send for Telegram/Discord/Slack delivery.
                            let system_prompt = if let Some(ref custom) = custom_prompt {
                                format!("{}\n\n## Pipeline Step Instructions\n{}{}", base_prompt, custom, channel_hint)
                            } else {
                                format!("{}{}", base_prompt, channel_hint)
                            };

                            // Diagnostic logging for pipeline step execution.
                            let tool_count = tool_reg.schemas().len();
                            let has_message_send = tool_reg.schemas().iter().any(|s| s.name == "message_send");
                            tracing::info!(
                                step_index = i,
                                step_label = %step.label,
                                model = %model_id,
                                tool_count = tool_count,
                                has_message_send = has_message_send,
                                channel_hint_len = channel_hint.len(),
                                has_custom_prompt = custom_prompt.is_some(),
                                custom_prompt_preview = ?custom_prompt.as_deref().map(|s| &s[..s.len().min(100)]),
                                system_prompt_len = system_prompt.len(),
                                "run_pipeline: executing agent step"
                            );

                            let config = AgentConfig {
                                model: model_id,
                                system_prompt: system_prompt.clone(),
                                max_tool_rounds: max_rounds,
                                context_limit: 128_000,
                                response_reserve: 4_096,
                                ..Default::default()
                            };

                            let runner = AgentRunner::new(
                                provider,
                                tool_reg,
                                config,
                                state_ref.cancel.clone(),
                            )
                            // Wire sandbox gate into pipeline runner too
                            .with_sandbox_gate(Arc::new(crate::commands::SandboxGateAdapter {
                                engine: Arc::clone(&state_ref.sandbox_engine),
                            }))
                            // GAP-B: Wire memory recall for pipeline step agents
                            .with_memory_recall({
                                let mem = Arc::clone(&state_ref.memory);
                                Arc::new(move |query: String| {
                                    let mem = Arc::clone(&mem);
                                    Box::pin(async move {
                                        match mem.recall(&query, Some(5)).await {
                                            Ok(results) => results.into_iter().filter_map(|r| {
                                                let text = r.content?;
                                                if text.is_empty() { return None; }
                                                Some(clawdesk_agents::MemoryRecallResult {
                                                    relevance: r.score as f64,
                                                    source: r.metadata.get("source")
                                                        .and_then(|v| v.as_str())
                                                        .map(String::from),
                                                    content: text,
                                                })
                                            }).collect(),
                                            Err(_) => vec![],
                                        }
                                    })
                                })
                            });

                            // Use step config prompt as instruction context
                            // rather than generic "Process this as the X step" text.
                            // If the channel hint is non-empty and the custom prompt
                            // mentions a channel name, append a tool-usage reminder
                            // to the user message — LLMs are more reliable about calling
                            // tools when the user message explicitly references them.
                            let needs_messaging_reminder = !channel_hint.is_empty() && custom_prompt.as_ref().map_or(false, |p| {
                                let lower = p.to_lowercase();
                                ["telegram", "discord", "slack", "whatsapp", "email", "signal", "teams", "matrix", "irc", "imessage"]
                                    .iter()
                                    .any(|ch| lower.contains(ch))
                                    || lower.contains("send") || lower.contains("message")
                            });
                            let messaging_reminder = if needs_messaging_reminder {
                                "\n\nREMINDER: You MUST use the `message_send` tool to deliver the message. Call it now."
                            } else {
                                ""
                            };
                            let user_message = if let Some(ref custom) = custom_prompt {
                                format!(
                                    "Previous step output:\n{}\n\nInstructions:\n{}{}",
                                    previous_output, custom, messaging_reminder
                                )
                            } else {
                                format!(
                                    "Previous step output:\n{}\n\nProcess this as the {} step.",
                                    previous_output, step.label
                                )
                            };

                            let history = vec![clawdesk_providers::ChatMessage::new(
                                MessageRole::User,
                                user_message,
                            )];

                            match runner.run(history, system_prompt).await {
                                Ok(response) => {
                                    let step_ms = step_start.elapsed().as_millis() as u64;
                                    let msg_sends = response.messaging_sends.len();
                                    tracing::info!(
                                        step_index = i,
                                        step_label = %step.label,
                                        total_rounds = response.total_rounds,
                                        input_tokens = response.input_tokens,
                                        output_tokens = response.output_tokens,
                                        messaging_sends = msg_sends,
                                        content_preview = %safe_prefix(&response.content, response.content.len().min(300)),
                                        duration_ms = step_ms,
                                        "run_pipeline: agent step completed"
                                    );
                                    if msg_sends == 0 && channel_hint.len() > 0 {
                                        tracing::warn!(
                                            step_label = %step.label,
                                            "run_pipeline: step had channel hint but LLM did NOT call message_send! \
                                             The LLM may have responded with text instead of a tool call."
                                        );
                                    }
                                    state_ref.record_usage(
                                        &model_lower,
                                        response.input_tokens,
                                        response.output_tokens,
                                    );
                                    let preview_len = response.content.len().min(200);
                                    let preview = safe_prefix(&response.content, preview_len);
                                    (
                                        i,
                                        Some(response.content.clone()),
                                        serde_json::json!({
                                            "step_index": i,
                                            "label": step.label,
                                            "node_type": "agent",
                                            "success": true,
                                            "duration_ms": step_ms,
                                            "input_tokens": response.input_tokens,
                                            "output_tokens": response.output_tokens,
                                            "total_rounds": response.total_rounds,
                                            "output_preview": preview,
                                        }),
                                        false,
                                    )
                                }
                                Err(e) => {
                                    let step_ms = step_start.elapsed().as_millis() as u64;
                                    (
                                        i,
                                        None,
                                        serde_json::json!({
                                            "step_index": i,
                                            "label": step.label,
                                            "node_type": "agent",
                                            "success": false,
                                            "duration_ms": step_ms,
                                            "error": e.to_string(),
                                        }),
                                        false,
                                    )
                                }
                            }
                        } else {
                            // Provider was already rejected in prep phase
                            (i, None, serde_json::json!(null), false)
                        }
                    }
                    "gate" => {
                        let gate_passed = if let Some(ref condition) = step.condition {
                            previous_output
                                .to_lowercase()
                                .contains(&condition.to_lowercase())
                        } else {
                            true
                        };
                        let output = if gate_passed {
                            Some(previous_output.clone())
                        } else {
                            None
                        };
                        (
                            i,
                            output,
                            serde_json::json!({
                                "step_index": i,
                                "label": step.label,
                                "node_type": "gate",
                                "success": true, // Gate evaluating correctly is always a success
                                "gate_passed": gate_passed,
                                "duration_ms": step_start.elapsed().as_millis() as u64,
                                "output": if gate_passed { "Gate passed." } else { "Gate blocked — downstream steps skipped." },
                            }),
                            !gate_passed,
                        )
                    }
                    "output" => (
                        i,
                        Some(previous_output.clone()),
                        serde_json::json!({
                            "step_index": i,
                            "label": step.label,
                            "node_type": "output",
                            "success": true,
                            "duration_ms": step_start.elapsed().as_millis() as u64,
                            "output": &previous_output,
                        }),
                        false,
                    ),
                    other => (
                        i,
                        None,
                        serde_json::json!({
                            "step_index": i,
                            "label": step.label,
                            "node_type": other,
                            "success": true,
                            "duration_ms": step_start.elapsed().as_millis() as u64,
                        }),
                        false,
                    ),
                }
            }})
            .collect();

        let level_results = futures::future::join_all(futures).await;

        // Merge results and propagate gate-blocking to downstream nodes
        for (idx, output, result, gate_blocked) in level_results {
            // Skip null results from pre-rejected provider failures
            if result.is_null() {
                continue;
            }
            if let Some(out) = output {
                step_outputs.insert(idx, out);
            }
            step_results.push(result);

            if gate_blocked {
                // BFS to find all transitive downstream nodes and block them
                let mut bfs_queue = std::collections::VecDeque::new();
                for &next in &adjacency[idx] {
                    bfs_queue.push_back(next);
                }
                while let Some(node) = bfs_queue.pop_front() {
                    if blocked.insert(node) {
                        for &next in &adjacency[node] {
                            bfs_queue.push_back(next);
                        }
                    }
                }
            }
        }
    }

    // Emit "skipped" results for blocked downstream nodes so the frontend
    // can display their status instead of leaving them stuck on "started".
    for &node_idx in &blocked {
        if node_idx < pipeline.steps.len() {
            let step = &pipeline.steps[node_idx];
            step_results.push(serde_json::json!({
                "step_index": node_idx,
                "label": step.label,
                "node_type": step.node_type,
                "success": true,
                "skipped": true,
                "duration_ms": 0,
                "output": "Skipped — upstream gate blocked this branch.",
            }));
        }
    }

    let total_ms = start.elapsed().as_millis() as u64;
    // A pipeline succeeds if no step had a real error. Gate-blocked and
    // skipped steps are NOT failures — they are valid branching outcomes.
    let all_success = step_results.iter().all(|s| {
        let success = s.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
        let skipped = s.get("skipped").and_then(|v| v.as_bool()).unwrap_or(false);
        success || skipped
    });

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

    // ── Emit pipeline completion to reactive event bus ──
    {
        let bus = &state.event_bus;
        crate::bus_integration::emit_pipeline_completed(
            bus,
            &pipeline_id,
            &pipeline_name,
            all_success,
            pipeline.steps.len(),
        )
        .await;
    }

    let result = serde_json::json!({
        "pipeline_id": pipeline_id, "pipeline_name": pipeline.name,
        "success": all_success, "steps": step_results, "total_duration_ms": total_ms,
        "completed_at": Utc::now().to_rfc3339(),
    });

    // ── Persist pipeline run result to SochDB (durable) ──
    {
        let run_id = Uuid::new_v4().to_string();
        let key = format!("pipeline_runs/{}/{}", pipeline_id, run_id);
        if let Ok(bytes) = serde_json::to_vec(&result) {
            if let Err(e) = state.soch_store.put_durable(&key, &bytes) {
                tracing::error!(key = %key, error = %e, "Failed to persist pipeline run result");
            }
        }
    }

    Ok(result)
}

// ═══════════════════════════════════════════════════════════
// Cron Scheduling
// ═══════════════════════════════════════════════════════════

/// Sync a pipeline's schedule field with the CronManager.
/// If `schedule` is Some, upserts a CronTask; if None, removes any existing task.
async fn sync_pipeline_cron_schedule(state: &AppState, pipeline: &PipelineDescriptor) {
    use clawdesk_types::cron::CronTask;

    let task_id = format!("pipeline:{}", pipeline.id);

    if let Some(ref schedule) = pipeline.schedule {
        // Build a detailed prompt from the pipeline steps so the cron agent
        // knows exactly what to do (e.g., "send hippo to telegram").
        let step_instructions: Vec<String> = pipeline.steps.iter()
            .enumerate()
            .filter(|(_, s)| s.node_type == "agent")
            .map(|(i, step)| {
                let custom_prompt = step.config.get("prompt").cloned().unwrap_or_default();
                if !custom_prompt.is_empty() {
                    format!("Step {}: {} — {}", i + 1, step.label, custom_prompt)
                } else {
                    format!("Step {}: {}", i + 1, step.label)
                }
            })
            .collect();

        let prompt = if step_instructions.is_empty() {
            format!(
                "Execute the scheduled pipeline '{}'. {}",
                pipeline.name, pipeline.description
            )
        } else {
            format!(
                "Execute the scheduled pipeline '{}'. {}\n\nSteps to perform:\n{}",
                pipeline.name,
                pipeline.description,
                step_instructions.join("\n")
            )
        };

        let task = CronTask {
            id: task_id,
            name: format!("Pipeline: {}", pipeline.name),
            schedule: schedule.clone(),
            prompt,
            agent_id: Some(format!("pipeline:{}", pipeline.id)),
            delivery_targets: Vec::new(),
            skip_if_running: true,
            timeout_secs: 600,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            depends_on: Vec::new(),
            chain_mode: Default::default(),
            max_retained_logs: 0,
        };
        if let Err(e) = state.cron_manager.upsert_task(task).await {
            tracing::warn!(error = %e, pipeline_id = %pipeline.id, "Failed to register cron schedule for pipeline");
        }
    } else {
        // No schedule — remove any existing cron task
        let _ = state.cron_manager.remove_task(&task_id).await;
    }
}

/// List all registered cron tasks.
#[tauri::command]
pub async fn list_cron_tasks(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    let tasks = state.cron_manager.list_tasks().await;
    let result: Vec<serde_json::Value> = tasks
        .into_iter()
        .map(|t| serde_json::json!({
            "id": t.id,
            "name": t.name,
            "schedule": t.schedule,
            "enabled": t.enabled,
            "agent_id": t.agent_id,
            "created_at": t.created_at.to_rfc3339(),
        }))
        .collect();
    Ok(result)
}

/// Manually trigger a cron task by ID.
#[tauri::command]
pub async fn trigger_cron_task(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let start = std::time::Instant::now();
    let log = state
        .cron_manager
        .trigger(&task_id)
        .await
        .map_err(|e| e.to_string())?;

    // Emit cron execution event to the reactive bus
    let success = log.error.is_none();
    let duration_ms = start.elapsed().as_millis() as u64;
    crate::bus_integration::emit_cron_executed(
        &state.event_bus,
        &log.task_id,
        &log.task_id, // name fallback
        success,
        duration_ms,
    )
    .await;

    Ok(serde_json::json!({
        "task_id": log.task_id,
        "run_id": log.run_id,
        "status": format!("{:?}", log.status),
        "result_preview": log.result_preview,
        "error": log.error,
    }))
}

/// Get recent cron run logs.
#[tauri::command]
pub async fn get_cron_logs(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let logs = state.cron_manager.recent_logs(limit.unwrap_or(50)).await;
    let result: Vec<serde_json::Value> = logs
        .into_iter()
        .map(|l| serde_json::json!({
            "task_id": l.task_id,
            "run_id": l.run_id,
            "started_at": l.started_at.to_rfc3339(),
            "finished_at": l.finished_at.map(|d| d.to_rfc3339()),
            "status": format!("{:?}", l.status),
            "result_preview": l.result_preview,
            "error": l.error,
        }))
        .collect();
    Ok(result)
}

// ═══════════════════════════════════════════════════════════
// Template Commands — persona + Life OS pipeline templates
// ═══════════════════════════════════════════════════════════

/// List all bundled persona templates for agent creation.
#[tauri::command]
pub async fn list_persona_templates() -> Result<Vec<serde_json::Value>, String> {
    let templates = clawdesk_skills::templates::bundled_templates();
    let result: Vec<serde_json::Value> = templates
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "display_name": t.display_name,
                "category": t.category,
                "soul": t.soul,
                "guidelines": t.guidelines,
                "default_allow_tools": t.default_allow_tools,
                "default_deny_tools": t.default_deny_tools,
                "default_skills": t.default_skills,
                "default_model": t.default_model,
            })
        })
        .collect();
    Ok(result)
}

/// List all Life OS pipeline templates.
#[tauri::command]
pub async fn list_life_os_templates(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let templates = state.template_registry.list();
    let result: Vec<serde_json::Value> = templates
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "name": t.name,
                "description": t.description,
                "category": format!("{:?}", t.category),
                "steps": t.steps.len(),
                "required_skills": t.required_skills,
                "default_schedule": t.default_schedule,
                "requires_approval": t.requires_approval,
                "variables": t.variables.iter().map(|v| serde_json::json!({
                    "name": v.name,
                    "description": v.description,
                    "default": v.default,
                    "required": v.required,
                })).collect::<Vec<_>>(),
                "version": t.version,
            })
        })
        .collect();
    Ok(result)
}

/// Instantiate a Life OS pipeline template with variable substitutions,
/// creating a concrete pipeline descriptor.
#[tauri::command]
pub async fn instantiate_life_os_template(
    template_id: String,
    variables: HashMap<String, String>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let instance = state
        .template_registry
        .instantiate(&template_id, &variables)
        .ok_or_else(|| format!("Template '{}' not found", template_id))?;

    serde_json::to_value(&instance).map_err(|e| e.to_string())
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
    pub capabilities: Vec<String>,
    pub configured: bool,
    #[serde(default)]
    pub config: std::collections::HashMap<String, String>,
}

/// List available channel adapters.
///
/// Merges channels that are actually registered in the `ChannelRegistry`
/// (status = "active") with the full catalog of known adapters whose
/// status is derived from environment-variable probing ("available").
#[tauri::command]
pub async fn list_channels(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ChannelInfo>, String> {
    // ── Phase 1: Registered (live) channels from the registry ──
    let mut result: Vec<ChannelInfo> = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();

    {
        let reg = state.channel_registry.read().map_err(|e| e.to_string())?;
        for (cid, ch) in reg.iter() {
            let meta = ch.meta();
            let id_str = cid.to_string();
            let mut caps = Vec::new();
            if meta.supports_threading { caps.push("threads".into()); }
            if meta.supports_streaming { caps.push("streaming".into()); }
            if meta.supports_reactions { caps.push("reactions".into()); }
            if meta.supports_media { caps.push("media".into()); }
            if meta.supports_groups { caps.push("group".into()); }

            seen.insert(id_str.clone());
            result.push(ChannelInfo {
                id: id_str,
                name: meta.display_name.clone(),
                status: "active".into(),
                channel_type: format!("{:?}", cid),
                capabilities: caps,
                configured: true,
                config: std::collections::HashMap::new(),
            });
        }
    }

    // Read saved channel configs so we can merge them into the catalog entries.
    let saved_configs = state.channel_configs.read().map_err(|e| e.to_string())?;

    // ── Phase 2: Catalog — known adapters not yet registered ──
    let catalog: Vec<(&str, &str, &str, bool)> = vec![
        ("webchat", "Web Chat", "WebChat", true),
        ("internal", "Internal", "Internal", true),
        ("telegram", "Telegram", "Telegram", std::env::var("TELEGRAM_BOT_TOKEN").is_ok()),
        ("discord", "Discord", "Discord", std::env::var("DISCORD_TOKEN").is_ok()),
        ("slack", "Slack", "Slack", std::env::var("SLACK_BOT_TOKEN").is_ok()),
        ("whatsapp", "WhatsApp", "WhatsApp", std::env::var("WHATSAPP_TOKEN").is_ok()),
        ("email", "Email", "Email", std::env::var("IMAP_HOST").is_ok()),
        ("irc", "IRC", "Irc", std::env::var("IRC_SERVER").is_ok()),
        ("imessage", "iMessage", "IMessage", cfg!(target_os = "macos")),
    ];

    for (id, name, channel_type, env_ok) in catalog {
        if seen.contains(id) {
            continue; // already emitted from registry
        }
        let has_saved = saved_configs.contains_key(id);
        let cfg = saved_configs.get(id).cloned().unwrap_or_default();
        // "active" = running in registry; "configured" = saved config but not
        // yet registered (e.g. failed hot-start, will try on next restart);
        // "available" = no config saved.
        let status = if has_saved { "configured" } else { "available" };
        result.push(ChannelInfo {
            id: id.into(),
            name: name.into(),
            status: status.into(),
            channel_type: channel_type.into(),
            capabilities: vec![],
            configured: env_ok || has_saved,
            config: cfg,
        });
    }

    Ok(result)
}

/// Save configuration for a channel adapter and hot-start it.
///
/// Stores the key-value config in `AppState::channel_configs`, persists
/// to `~/.clawdesk/channels.json`, sets env vars, and creates + starts
/// the channel adapter so the user doesn't need to restart the app.
#[tauri::command]
pub async fn update_channel(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
    channel_id: String,
    config: std::collections::HashMap<String, String>,
) -> Result<bool, String> {
    // Map well-known config keys → env vars so channel adapters can bootstrap.
    let env_mappings: &[(&str, &str, &str)] = &[
        ("telegram", "bot_token", "TELEGRAM_BOT_TOKEN"),
        ("discord", "bot_token", "DISCORD_TOKEN"),
        ("discord", "application_id", "DISCORD_APP_ID"),
        ("discord", "guild_id", "DISCORD_GUILD_ID"),
        ("slack", "bot_token", "SLACK_BOT_TOKEN"),
        ("slack", "app_token", "SLACK_APP_TOKEN"),
        ("whatsapp", "access_token", "WHATSAPP_TOKEN"),
        ("whatsapp", "phone_number_id", "WHATSAPP_PHONE_NUMBER_ID"),
        ("whatsapp", "app_secret", "WHATSAPP_APP_SECRET"),
        ("email", "imap_host", "IMAP_HOST"),
        ("email", "smtp_host", "SMTP_HOST"),
        ("email", "email_user", "EMAIL_USER"),
        ("email", "email_password", "EMAIL_PASSWORD"),
        ("irc", "server", "IRC_SERVER"),
        ("irc", "nickname", "IRC_NICKNAME"),
    ];

    for &(ch, key, env_var) in env_mappings {
        if channel_id == ch {
            if let Some(val) = config.get(key) {
                if !val.is_empty() {
                    std::env::set_var(env_var, val);
                }
            }
        }
    }

    // Persist in AppState + disk
    {
        let mut configs = state.channel_configs.write().map_err(|e| e.to_string())?;
        configs.insert(channel_id.clone(), config.clone());
        AppState::save_channel_configs(&configs);
    }

    tracing::info!(channel = %channel_id, "Channel config saved — attempting hot-start");

    // Hot-start the channel adapter (create, register, start)
    if let Err(e) = AppState::hot_start_channel(
        &channel_id,
        &config,
        &state.channel_factory,
        &state.channel_registry,
        &app,
    ) {
        tracing::warn!(channel = %channel_id, error = %e, "Hot-start failed — channel will start on next app restart");
    } else {
        // Register the newly started channel in the A2A directory
        // so other agents can discover it for cross-channel communication.
        if let Ok(mut dir) = state.agent_directory.write() {
            let card_id = format!("channel:{}", channel_id);
            let mut card = clawdesk_acp::AgentCard::new(
                card_id,
                format!("{} Channel", channel_id),
                "local://channel",
            );
            card.description = format!("Active {} messaging channel.", channel_id);
            card = card.with_capability(clawdesk_acp::CapabilityId::Messaging);
            dir.register(card);
            tracing::info!(channel = %channel_id, "Registered channel in A2A directory");
        }
    }

    Ok(true)
}

/// Disconnect a channel adapter — clears its saved config and persists.
#[tauri::command]
pub async fn disconnect_channel(
    state: tauri::State<'_, AppState>,
    channel_id: String,
) -> Result<bool, String> {
    let mut configs = state.channel_configs.write().map_err(|e| e.to_string())?;
    configs.remove(&channel_id);
    AppState::save_channel_configs(&configs);
    // Remove from A2A directory
    if let Ok(mut dir) = state.agent_directory.write() {
        let card_id = format!("channel:{}", channel_id);
        dir.deregister(&card_id);
    }
    tracing::info!(channel = %channel_id, "Channel disconnected + removed from A2A directory");
    Ok(true)
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
        AgentEvent::ThinkingChunk { text } => TauriAgentEvent::ThinkingChunk {
            text: text.clone(),
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
        AgentEvent::SkillDecision { skill_id, included, reason, token_cost, budget_remaining } => TauriAgentEvent::SkillDecision {
            skill_id: skill_id.clone(),
            included: *included,
            reason: reason.clone(),
            token_cost: *token_cost,
            budget_remaining: *budget_remaining,
        },
        AgentEvent::RetryStatus { attempt, max_attempts, reason } => TauriAgentEvent::RetryStatus {
            attempt: *attempt,
            max_attempts: *max_attempts,
            reason: reason.clone(),
        },
        AgentEvent::ToolBlocked { name, reason } => TauriAgentEvent::ToolBlocked {
            name: name.clone(),
            reason: reason.clone(),
        },
        AgentEvent::ToolExecutionResult { name, tool_call_id, is_error, preview, duration_ms } => TauriAgentEvent::ToolExecutionResult {
            name: name.clone(),
            tool_call_id: tool_call_id.clone(),
            is_error: *is_error,
            preview: preview.clone(),
            duration_ms: *duration_ms,
        },
        AgentEvent::InputRequired { question, options, urgent } => TauriAgentEvent::InputRequired {
            question: question.clone(),
            options: options.clone(),
            urgent: *urgent,
        },
    };

    app.emit(
        "agent-event",
        FrontendAgentEvent {
            agent_id: agent_id.to_string(),
            event: mapped,
        },
    )
}

pub(crate) fn emit_metrics_updated(app: &AppHandle, state: &AppState) {
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

pub(crate) async fn emit_security_changed(app: &AppHandle, state: &AppState) {
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

#[tauri::command]
pub async fn test_llm_connection(
    provider: String,
    model: String,
    api_key: Option<String>,
    base_url: Option<String>,
    project: Option<String>,
    location: Option<String>,
) -> Result<String, String> {
    use clawdesk_providers::anthropic::AnthropicProvider;
    use clawdesk_providers::gemini::GeminiProvider;
    use clawdesk_providers::ollama::OllamaProvider;
    use clawdesk_providers::openai::OpenAiProvider;
    use clawdesk_providers::azure::AzureOpenAiProvider;
    use clawdesk_providers::cohere::CohereProvider;
    use clawdesk_providers::vertex::VertexProvider;
    use clawdesk_providers::{ChatMessage, MessageRole, Provider};
    use std::sync::Arc;

    let prov: Arc<dyn Provider> = match provider.as_str() {
        "Anthropic" => {
            let key = api_key.unwrap_or_else(|| std::env::var("ANTHROPIC_API_KEY").unwrap_or_default());
            Arc::new(AnthropicProvider::new(key, Some(model.clone())))
        }
        "OpenAI" => {
            let key = api_key.unwrap_or_else(|| std::env::var("OPENAI_API_KEY").unwrap_or_default());
            Arc::new(OpenAiProvider::new(key, base_url.clone(), Some(model.clone())))
        }
        "Azure OpenAI" => {
            let key = api_key.unwrap_or_else(|| std::env::var("AZURE_OPENAI_API_KEY").unwrap_or_default());
            let url = base_url.clone().unwrap_or_else(|| std::env::var("AZURE_OPENAI_ENDPOINT").unwrap_or_else(|_| "https://example.openai.azure.com".into()));
            Arc::new(AzureOpenAiProvider::new(key, url, None, Some(model.clone())))
        }
        "Google" => {
            let key = api_key.unwrap_or_else(|| std::env::var("GOOGLE_API_KEY").unwrap_or_default());
            Arc::new(GeminiProvider::new(key, Some(model.clone())))
        }
        "Vertex AI" => {
            let proj = project.unwrap_or_else(|| std::env::var("VERTEX_PROJECT_ID").unwrap_or_default());
            let loc = location.unwrap_or_else(|| std::env::var("VERTEX_LOCATION").unwrap_or_else(|_| "us-central1".into()));
            Arc::new(VertexProvider::new(proj, loc, Some(model.clone())))
        }
        "Cohere" => {
            let key = api_key.unwrap_or_else(|| std::env::var("COHERE_API_KEY").unwrap_or_default());
            Arc::new(CohereProvider::new(key, base_url.clone(), Some(model.clone())))
        }
        "Ollama (Local)" => {
            Arc::new(OllamaProvider::new(base_url.clone(), Some(model.clone())))
        }
        "Local (OpenAI Compatible)" => {
            use clawdesk_providers::compatible::{CompatibleConfig, OpenAiCompatibleProvider};
            let url = base_url.clone().unwrap_or_else(|| "http://localhost:8080/v1".into());
            let key = api_key.unwrap_or_default();
            let config = CompatibleConfig::new("local_compatible", &url, &key)
                .with_default_model(model.clone());
            Arc::new(OpenAiCompatibleProvider::new(config))
        }
        _ => return Err(format!("Unknown testing provider: {}", provider)),
    };

    let request = clawdesk_providers::ProviderRequest {
        model,
        messages: vec![ChatMessage::new(MessageRole::User, "Reply with exactly 'Hello World' and nothing else. No formatting, no extra words.")],
        system_prompt: None,
        max_tokens: Some(10),
        temperature: Some(0.0),
        tools: vec![],
        stream: false,
    };

    match prov.complete(&request).await {
        Ok(res) => Ok(res.content),
        Err(e) => Err(format!("Connection failed: {}", e)),
    }
}

// ═══════════════════════════════════════════════════════════
// Dynamic Agent Spawning — closure factory
// ═══════════════════════════════════════════════════════════

/// Build a `dynamic_spawn` callback closure with the given `parent_depth`.
///
/// The returned closure creates an ephemeral `AgentRunner` from a
/// `DynamicSpawnRequest`, executing the child agent to completion and
/// returning its response text. When the child itself needs spawn
/// capability (`depth + 1 < max_depth`), a recursive call to this factory
/// produces the child's own closure with an incremented depth — forming a
/// bounded, depth-tracked closure chain.
///
/// ## Depth propagation
///
/// ```text
/// Root (depth 0) → build_dynamic_spawn_fn(depth=0)
///   → Child (depth 1) → if depth < max_depth, build_dynamic_spawn_fn(depth=1)
///     → Grandchild (depth 2) → depth >= max_depth → NO dynamic_spawn tool
/// ```
fn build_dynamic_spawn_fn(
    negotiator_ref: Arc<std::sync::RwLock<clawdesk_providers::negotiator::ProviderNegotiator>>,
    cancel_ref: tokio_util::sync::CancellationToken,
    base_tools: Arc<clawdesk_agents::ToolRegistry>,
    sandbox_engine_ref: Arc<clawdesk_security::sandbox_policy::SandboxPolicyEngine>,
    sub_mgr: Arc<clawdesk_gateway::subagent_manager::SubAgentManager>,
    parent_agent_id: String,
    parent_model: String,
    parent_depth: u32,
    parent_provider: Arc<dyn clawdesk_providers::Provider>,
) -> Arc<
    dyn Fn(
            clawdesk_agents::builtin_tools::DynamicSpawnRequest,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
> {
    Arc::new(move |req: clawdesk_agents::builtin_tools::DynamicSpawnRequest| {
        let negotiator: Arc<std::sync::RwLock<clawdesk_providers::negotiator::ProviderNegotiator>> = Arc::clone(&negotiator_ref);
        let cancel = cancel_ref.clone();
        let tools: Arc<clawdesk_agents::ToolRegistry> = Arc::clone(&base_tools);
        let sandbox_eng: Arc<clawdesk_security::sandbox_policy::SandboxPolicyEngine> = Arc::clone(&sandbox_engine_ref);
        let mgr: Arc<clawdesk_gateway::subagent_manager::SubAgentManager> = Arc::clone(&sub_mgr);
        let parent_id = parent_agent_id.clone();
        let parent_mdl = parent_model.clone();
        let fallback_provider = Arc::clone(&parent_provider);

        // Snapshot the closure-factory references for recursive child construction
        let neg_for_child = Arc::clone(&negotiator_ref);
        let cancel_for_child = cancel_ref.clone();
        let tools_for_child = Arc::clone(&base_tools);
        let sandbox_for_child = Arc::clone(&sandbox_engine_ref);
        let mgr_for_child = Arc::clone(&sub_mgr);
        let provider_for_child = Arc::clone(&parent_provider);

        Box::pin(async move {
            let child_depth = parent_depth + 1;
            let label = req.label.clone().unwrap_or_else(|| "ephemeral".into());
            let ephemeral_id = format!("dyn:{}:{}", label, uuid::Uuid::new_v4().as_simple());

            // ── Phase 1: Admission control ───────────────────────────
            let sub_id = mgr
                .register(&parent_id, &ephemeral_id, child_depth)
                .map_err(|e| format!("Spawn rejected: {e}"))?;

            mgr.update_state(
                &sub_id,
                clawdesk_gateway::subagent_manager::ManagedState::Running,
            )
            .ok();

            // ── Phase 2: Model resolution ────────────────────────────
            let model_id = if let Some(ref m) = req.model {
                let resolved = crate::state::AppState::resolve_model_id(m);
                // Treat "default"/"auto" as inherit-from-parent
                if resolved == "default" || resolved == "auto" {
                    crate::state::AppState::resolve_model_id(&parent_mdl)
                } else {
                    resolved
                }
            } else {
                crate::state::AppState::resolve_model_id(&parent_mdl)
            };

            let required = clawdesk_providers::capability::ProviderCaps::TEXT_COMPLETION
                .union(clawdesk_providers::capability::ProviderCaps::SYSTEM_PROMPT);
            let provider = {
                let neg = negotiator
                    .read()
                    .map_err(|e| format!("negotiator lock: {e}"))?;
                match neg.resolve_model(&model_id, required) {
                    Some((p, _)) => Arc::clone(p),
                    None => {
                        tracing::info!(
                            label = %label,
                            model = %model_id,
                            "Dynamic sub-agent: negotiator miss — inheriting parent provider"
                        );
                        fallback_provider
                    }
                }
            };

            // ── Phase 3: Tool registry construction ──────────────────
            let child_tools = match &req.tools {
                clawdesk_agents::builtin_tools::ToolAccess::Inherit => {
                    // Start with parent's full registry
                    let mut registry = (*tools).clone();
                    // If the child can spawn further, register its own dynamic_spawn
                    let max_depth = mgr_for_child
                        .stats()
                        .total; // use config's max_depth via SubAgentManager
                    // SubAgentManager already enforces max_depth in register(), so
                    // we just need to decide whether to give the child the tool.
                    // We'll always give it if depth + 1 hasn't hit the hard limit.
                    if child_depth < 5 {
                        // default max depth from SubAgentManagerConfig
                        let child_spawn_fn = build_dynamic_spawn_fn(
                            neg_for_child,
                            cancel_for_child,
                            tools_for_child,
                            sandbox_for_child,
                            mgr_for_child,
                            ephemeral_id.clone(),
                            model_id.clone(),
                            child_depth,
                            provider_for_child.clone(),
                        );
                        clawdesk_agents::builtin_tools::register_dynamic_spawn_tool(
                            &mut registry,
                            child_spawn_fn,
                        );
                    }
                    Arc::new(registry)
                }
                clawdesk_agents::builtin_tools::ToolAccess::None => {
                    Arc::new(clawdesk_agents::ToolRegistry::new())
                }
                clawdesk_agents::builtin_tools::ToolAccess::Only(names) => {
                    let mut registry = tools.filter_by_names(names);
                    // If the child can spawn and "dynamic_spawn" is in the allowlist
                    if names.iter().any(|n| n == "dynamic_spawn") && child_depth < 5 {
                        let child_spawn_fn = build_dynamic_spawn_fn(
                            neg_for_child,
                            cancel_for_child,
                            tools_for_child,
                            sandbox_for_child,
                            mgr_for_child,
                            ephemeral_id.clone(),
                            model_id.clone(),
                            child_depth,
                            provider_for_child.clone(),
                        );
                        clawdesk_agents::builtin_tools::register_dynamic_spawn_tool(
                            &mut registry,
                            child_spawn_fn,
                        );
                    }
                    Arc::new(registry)
                }
            };

            // ── Phase 4: Prompt assembly ─────────────────────────────
            let prompt_params = clawdesk_agents::dynamic_prompt::EphemeralPromptParams {
                task: req.task.clone(),
                label: req.label.clone(),
                depth: child_depth,
                max_depth: 5,
                has_tools: child_tools.total_count() > 0,
                tool_names: child_tools.list(),
                parent_session: None,
            };
            let system_prompt =
                clawdesk_agents::dynamic_prompt::build_ephemeral_system_prompt(&prompt_params);

            // ── Phase 5: Runner execution ────────────────────────────
            let config = clawdesk_agents::AgentConfig {
                model: model_id,
                system_prompt,
                max_tool_rounds: req.effective_tool_rounds(),
                ..Default::default()
            };

            let runner =
                clawdesk_agents::AgentRunner::new(provider, child_tools, config, cancel)
                    .with_sandbox_gate(Arc::new(crate::commands::SandboxGateAdapter {
                        engine: sandbox_eng,
                    }));

            let history = vec![clawdesk_providers::ChatMessage::new(
                clawdesk_providers::MessageRole::User,
                req.task.as_str(),
            )];

            let timeout = tokio::time::Duration::from_secs(req.effective_timeout());
            let result = match tokio::time::timeout(timeout, runner.run(history, String::new()))
                .await
            {
                Ok(Ok(response)) => {
                    mgr.update_state(
                        &sub_id,
                        clawdesk_gateway::subagent_manager::ManagedState::Completed,
                    )
                    .ok();
                    mgr.set_output(&sub_id, &response.content).ok();
                    Ok(response.content)
                }
                Ok(Err(e)) => {
                    mgr.update_state(
                        &sub_id,
                        clawdesk_gateway::subagent_manager::ManagedState::Failed,
                    )
                    .ok();
                    Err(format!("Dynamic agent error: {e}"))
                }
                Err(_) => {
                    mgr.update_state(
                        &sub_id,
                        clawdesk_gateway::subagent_manager::ManagedState::TimedOut,
                    )
                    .ok();
                    Err(format!(
                        "Dynamic agent timed out after {}s",
                        req.effective_timeout()
                    ))
                }
            };

            // ── Phase 6: Deferred GC ───────────────────────
            let gc_mgr = Arc::clone(&mgr);
            tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                gc_mgr.gc();
            });

            result
        })
    })
}

// ─────────────────────────────────────────────────────────────────────
// Channel provider sync — lets the UI push its active provider config
// to the Rust backend so channel adapters (Discord, Telegram, etc.)
// can use the same provider/model the user picked in the UI.
// ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SyncChannelProviderRequest {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub base_url: String,
}

#[tauri::command]
pub async fn sync_channel_provider(
    request: SyncChannelProviderRequest,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let override_cfg = ChannelProviderOverride {
        provider: request.provider.clone(),
        model: request.model.clone(),
        api_key: request.api_key.clone(),
        base_url: request.base_url.clone(),
    };
    tracing::info!(
        provider = %request.provider,
        model = %request.model,
        base_url = %request.base_url,
        "Channel provider override synced from UI"
    );
    // Persist to disk so the override survives app restarts
    AppState::save_channel_provider(&override_cfg);
    *state.channel_provider.write().map_err(|e| format!("Lock poisoned: {e}"))? = Some(override_cfg);
    Ok("ok".into())
}
