//! Enhanced domain commands — context guard, prompt builder, provider negotiation,
//! skill promotion (Tasks 25, 26, 27, 28).

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

// ═══════════════════════════════════════════════════════════
// Context Guard — expose utilization + compaction info
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
pub struct ContextGuardStatus {
    pub current_tokens: usize,
    pub available_budget: usize,
    pub utilization: f64,
    pub context_limit: usize,
    pub trigger_threshold: f64,
}

#[tauri::command]
pub async fn get_context_guard_status(
    agent_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<ContextGuardStatus, String> {
    let guards = state.context_guards.read().map_err(|e| e.to_string())?;
    let key = agent_id.unwrap_or_else(|| "__global__".to_string());
    match guards.get(&key) {
        Some(guard) => Ok(ContextGuardStatus {
            current_tokens: guard.current_tokens(),
            available_budget: guard.available_budget(),
            utilization: guard.utilization(),
            context_limit: 128_000,
            trigger_threshold: 0.80,
        }),
        None => Ok(ContextGuardStatus {
            current_tokens: 0,
            available_budget: 128_000,
            utilization: 0.0,
            context_limit: 128_000,
            trigger_threshold: 0.80,
        }),
    }
}

// ═══════════════════════════════════════════════════════════
// Prompt Builder — expose last manifest
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
pub struct PromptManifestInfo {
    pub total_tokens: usize,
    pub budget_total: usize,
    pub budget_utilization: f64,
    pub sections: Vec<PromptSectionInfo>,
    pub skills_included: Vec<String>,
    pub skills_excluded: Vec<(String, String)>,
    pub memory_fragments: usize,
}

#[derive(Debug, Serialize)]
pub struct PromptSectionInfo {
    pub name: String,
    pub tokens: usize,
    pub included: bool,
    pub reason: String,
}

#[tauri::command]
pub async fn get_prompt_manifest(
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<Option<PromptManifestInfo>, String> {
    let manifests = state.prompt_manifests.read().map_err(|e| e.to_string())?;
    Ok(manifests.get(&agent_id).map(|m| PromptManifestInfo {
        total_tokens: m.total_tokens,
        budget_total: m.budget_total,
        budget_utilization: m.budget_utilization,
        sections: m.sections.iter().map(|s| PromptSectionInfo {
            name: s.name.clone(),
            tokens: s.tokens,
            included: s.included,
            reason: s.reason.clone(),
        }).collect(),
        skills_included: m.skills_included.clone(),
        skills_excluded: m.skills_excluded.clone(),
        memory_fragments: m.memory_fragments,
    }))
}

// ═══════════════════════════════════════════════════════════
// Provider Negotiation — expose capability matrix
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
pub struct ProviderCapabilityInfo {
    pub provider: String,
    pub capabilities: Vec<String>,
    pub models: Vec<String>,
}

#[tauri::command]
pub async fn list_provider_capabilities(
    state: State<'_, AppState>,
) -> Result<Vec<ProviderCapabilityInfo>, String> {
    let negotiator = state.negotiator.read().map_err(|e| e.to_string())?;
    let providers = negotiator.list_providers();
    let all_models = negotiator.list_models();
    Ok(providers.iter().map(|name| {
        let caps = negotiator.capabilities(name);
        let cap_names = caps.map(|c| {
            use clawdesk_providers::capability::ProviderCaps;
            let mut names = vec![];
            if c.has(ProviderCaps::TEXT_COMPLETION) { names.push("text_completion"); }
            if c.has(ProviderCaps::STREAMING) { names.push("streaming"); }
            if c.has(ProviderCaps::TOOL_USE) { names.push("tool_use"); }
            if c.has(ProviderCaps::VISION) { names.push("vision"); }
            if c.has(ProviderCaps::EMBEDDINGS) { names.push("embeddings"); }
            if c.has(ProviderCaps::JSON_MODE) { names.push("json_mode"); }
            if c.has(ProviderCaps::SYSTEM_PROMPT) { names.push("system_prompt"); }
            if c.has(ProviderCaps::EXTENDED_THINKING) { names.push("extended_thinking"); }
            if c.has(ProviderCaps::STRUCTURED_OUTPUT) { names.push("structured_output"); }
            if c.has(ProviderCaps::CACHING) { names.push("caching"); }
            if c.has(ProviderCaps::BATCH_API) { names.push("batch_api"); }
            if c.has(ProviderCaps::CODE_EXECUTION) { names.push("code_execution"); }
            if c.has(ProviderCaps::IMAGE_GENERATION) { names.push("image_generation"); }
            names.into_iter().map(|s| s.to_string()).collect::<Vec<_>>()
        }).unwrap_or_default();
        let prefix_str = format!("{}/", name);
        let provider_models: Vec<String> = all_models.iter()
            .filter(|m| m.starts_with(&prefix_str) || m.as_str() == *name)
            .map(|m| m.strip_prefix(&prefix_str).unwrap_or(m).to_string())
            .collect();
        ProviderCapabilityInfo {
            provider: name.to_string(),
            capabilities: cap_names,
            models: provider_models,
        }
    }).collect())
}

#[derive(Debug, Serialize)]
pub struct RoutingDecisionInfo {
    pub selected_provider: Option<String>,
    pub selected_model: Option<String>,
    pub reason: String,
}

#[tauri::command]
pub async fn get_provider_routing(
    model: String,
    required_caps: Vec<String>,
    state: State<'_, AppState>,
) -> Result<RoutingDecisionInfo, String> {
    use clawdesk_providers::capability::ProviderCaps;

    let mut caps = ProviderCaps::NONE;
    for cap in &required_caps {
        caps = caps.union(match cap.as_str() {
            "text_completion" => ProviderCaps::TEXT_COMPLETION,
            "streaming" => ProviderCaps::STREAMING,
            "tool_use" => ProviderCaps::TOOL_USE,
            "vision" => ProviderCaps::VISION,
            "embeddings" => ProviderCaps::EMBEDDINGS,
            "json_mode" => ProviderCaps::JSON_MODE,
            "system_prompt" => ProviderCaps::SYSTEM_PROMPT,
            "extended_thinking" => ProviderCaps::EXTENDED_THINKING,
            _ => ProviderCaps::NONE,
        });
    }

    let negotiator = state.negotiator.read().map_err(|e| e.to_string())?;
    match negotiator.resolve_model(&model, caps) {
        Some((provider, resolved_model)) => Ok(RoutingDecisionInfo {
            selected_provider: Some(provider.name().to_string()),
            selected_model: Some(resolved_model),
            reason: "Capability-matched provider found".into(),
        }),
        None => Ok(RoutingDecisionInfo {
            selected_provider: None,
            selected_model: None,
            reason: format!(
                "No provider satisfies all required capabilities: {:?}",
                required_caps
            ),
        }),
    }
}

// ═══════════════════════════════════════════════════════════
// Skill Promotion — trust level + trigger info
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
pub struct SkillTrustInfo {
    pub skill_id: String,
    pub trust_level: String,
    pub publisher_key: Option<String>,
    pub verified: bool,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn get_skill_trust_level(
    skill_id: String,
    state: State<'_, AppState>,
) -> Result<SkillTrustInfo, String> {
    let reg = state.skill_registry.read().map_err(|e| e.to_string())?;
    let skill_key = clawdesk_skills::definition::SkillId::from(skill_id.as_str());
    let entry = reg.get(&skill_key);
    match entry {
        Some(e) => {
            let result = state.skill_verifier.verify(&e.skill.manifest, &e.source);
            Ok(SkillTrustInfo {
                skill_id,
                trust_level: format!("{:?}", result.trust_level),
                publisher_key: result.publisher_key,
                verified: matches!(result.trust_level,
                    clawdesk_skills::TrustLevel::Builtin |
                    clawdesk_skills::TrustLevel::SignedTrusted),
                error: result.error,
            })
        }
        None => Err(format!("Skill {} not found", skill_id)),
    }
}

#[derive(Debug, Serialize)]
pub struct SkillTriggerInfo {
    pub skill_id: String,
    pub trigger_type: String,
    pub matched: bool,
    pub relevance: f64,
}

#[tauri::command]
pub async fn evaluate_skill_triggers(
    message_text: String,
    state: State<'_, AppState>,
) -> Result<Vec<SkillTriggerInfo>, String> {
    use clawdesk_skills::trigger::{TurnContext, TriggerEvaluator};
    let ctx = TurnContext {
        channel_id: Some("tauri".to_string()),
        message_keywords: TurnContext::extract_keywords(&message_text),
        message_text,
        current_time: chrono::Utc::now(),
        requested_skill_ids: vec![],
        triggered_this_turn: std::collections::HashSet::new(),
        memory_signals: vec![],
    };

    let reg = state.skill_registry.read().map_err(|e| e.to_string())?;
    let skills = reg.active_skills();

    Ok(skills.iter().map(|s| {
        let result = TriggerEvaluator::evaluate(s, &ctx);
        SkillTriggerInfo {
            skill_id: s.manifest.id.as_str().to_string(),
            trigger_type: format!("{:?}", s.manifest.triggers),
            matched: result.matched,
            relevance: result.relevance,
        }
    }).collect())
}

// ═══════════════════════════════════════════════════════════
// Audit Log — exposes AuditLogger to frontend
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, serde::Serialize)]
pub struct FrontendLogEntry {
    pub id: String,
    pub timestamp: String,
    pub level: String,
    pub subsystem: String,
    pub message: String,
    pub category: String,
    pub actor: String,
    pub outcome: String,
}

/// Get recent audit log entries from the AuditLogger.
///
/// Maps the rich `AuditEntry` type to a simplified `FrontendLogEntry`
/// structure that the LogsPage can render directly.
#[tauri::command]
pub async fn get_audit_logs(
    limit: usize,
    state: State<'_, AppState>,
) -> Result<Vec<FrontendLogEntry>, String> {
    let entries = state.audit_logger.recent(limit).await;
    Ok(entries
        .into_iter()
        .map(|e| {
            let level = match e.outcome {
                clawdesk_types::security::AuditOutcome::Success => "info",
                clawdesk_types::security::AuditOutcome::Denied => "warn",
                clawdesk_types::security::AuditOutcome::Failed => "error",
                clawdesk_types::security::AuditOutcome::Blocked => "warn",
            };
            let subsystem = format!("{:?}", e.category).to_lowercase();
            let actor = match &e.actor {
                clawdesk_types::security::AuditActor::Agent { id, .. } => format!("agent:{}", id),
                clawdesk_types::security::AuditActor::User { sender_id, channel } => {
                    format!("user:{}@{}", sender_id, channel)
                }
                clawdesk_types::security::AuditActor::System => "system".into(),
                clawdesk_types::security::AuditActor::Plugin { name, .. } => format!("plugin:{}", name),
                clawdesk_types::security::AuditActor::Cron { task_id } => format!("cron:{}", task_id),
            };
            FrontendLogEntry {
                id: e.id,
                timestamp: e.timestamp.to_rfc3339(),
                level: level.into(),
                subsystem,
                message: format!("{} — {}", e.action, e.detail),
                category: format!("{:?}", e.category),
                actor,
                outcome: format!("{:?}", e.outcome),
            }
        })
        .collect())
}

/// Get execution logs: merges agent execution traces with audit log entries.
///
/// Returns a unified view of what agents did: tool calls, rounds, delegations,
/// compaction, fallbacks, errors — alongside security/config events.
#[tauri::command]
pub async fn get_execution_logs(
    limit: usize,
    state: State<'_, AppState>,
) -> Result<Vec<FrontendLogEntry>, String> {
    // 1. Get audit log entries (includes persisted tool calls from send_message)
    let audit_entries = state.audit_logger.recent(limit).await;
    let mut result: Vec<FrontendLogEntry> = audit_entries
        .into_iter()
        .map(|e| {
            let level = match e.outcome {
                clawdesk_types::security::AuditOutcome::Success => "info",
                clawdesk_types::security::AuditOutcome::Denied => "warn",
                clawdesk_types::security::AuditOutcome::Failed => "error",
                clawdesk_types::security::AuditOutcome::Blocked => "warn",
            };
            let subsystem = format!("{:?}", e.category).to_lowercase();
            let actor = match &e.actor {
                clawdesk_types::security::AuditActor::Agent { id, .. } => format!("agent:{}", id),
                clawdesk_types::security::AuditActor::User { sender_id, channel } => {
                    format!("user:{}@{}", sender_id, channel)
                }
                clawdesk_types::security::AuditActor::System => "system".into(),
                clawdesk_types::security::AuditActor::Plugin { name, .. } => format!("plugin:{}", name),
                clawdesk_types::security::AuditActor::Cron { task_id } => format!("cron:{}", task_id),
            };
            FrontendLogEntry {
                id: e.id,
                timestamp: e.timestamp.to_rfc3339(),
                level: level.into(),
                subsystem,
                message: format!("{} — {}", e.action, e.detail),
                category: format!("{:?}", e.category),
                actor,
                outcome: format!("{:?}", e.outcome),
            }
        })
        .collect();

    // 2. Merge in-memory execution traces (current session data not yet in audit log)
    if let Ok(all_traces) = state.traces.read() {
        for (agent_id, entries) in all_traces.iter() {
            for entry in entries {
                result.push(FrontendLogEntry {
                    id: format!("trace-{}-{}", agent_id, entry.timestamp),
                    timestamp: chrono::Utc::now().to_rfc3339(), // use current time for ordering
                    level: match entry.event.as_str() {
                        "Error" => "error".into(),
                        "Fallback" | "Compaction" | "ContextGuard" | "ContentScan" => "warn".into(),
                        _ => "info".into(),
                    },
                    subsystem: "execution".into(),
                    message: format!("[{}] {}", entry.event, entry.detail),
                    category: "Execution".into(),
                    actor: format!("agent:{}", agent_id),
                    outcome: if entry.event == "Error" { "Failed".into() } else { "Success".into() },
                });
            }
        }
    }

    // 3. Sort by timestamp descending (newest first)
    result.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    result.truncate(limit);

    Ok(result)
}
