//! # Unified Message-Processing Engine
//!
//! Shared pipeline used by **both** the desktop Tauri command (`commands.rs`)
//! and the Discord/channel handler (`ChannelMessageSink` in `state.rs`).
//!
//! Having a single codepath eliminates the class of bugs where a feature
//! (memory recall, skill scoring, prompt building, post-run storage) is
//! implemented in one path but missed in the other.
//!
//! ## Pipeline Stages
//!
//! 1. **Memory recall** — semantic search for relevant memories.
//! 2. **Skill scoring** — trigger-based evaluation + memory signal boost.
//! 3. **Prompt assembly** — `PromptBuilder` knapsack with budget accounting.
//! 4. **Memory injection** — recency-biased `<memory_context>` insertion.
//! 5. **Runner creation** — `AgentRunner` with skill_provider + channel_context.
//! 6. **Post-run storage** — durable memory write with dedup + temporal edges.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, info, warn};

use clawdesk_domain::prompt_builder::{
    AssembledPrompt, MemoryFragment, PromptBudget, PromptBuilder, PromptManifest, RuntimeContext,
    ScoredSkill,
};
use clawdesk_memory::MemoryManager;
use clawdesk_providers::{ChatMessage, MessageRole};
use clawdesk_skills::trigger::{TriggerEvaluator, TurnContext};
use clawdesk_sochdb::{SochMemoryBackend, SochTemporalGraph};
use clawdesk_types::tokenizer::estimate_tokens;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of the prompt-building pipeline.
pub(crate) struct PromptPipelineResult {
    /// Fully assembled system prompt (identity + runtime + skills + memory directive + skill protocol).
    pub system_prompt: String,
    /// Memory context XML block to inject before the last user message.
    /// `None` when no relevant memories were found.
    pub memory_injection: Option<String>,
    /// Token-accounting manifest for debugging / observability.
    pub prompt_manifest: Option<PromptManifest>,
}

/// Input parameters for the prompt pipeline.
pub(crate) struct PromptPipelineInput<'a> {
    /// The user's current message text (used for memory recall + skill trigger matching).
    pub user_content: &'a str,
    /// Agent persona / identity prompt.
    pub persona: &'a str,
    /// Model name (e.g. "gpt-4o", "GLM-4.7-Flash").
    pub model_name: &'a str,
    /// Skill IDs/names explicitly assigned to this agent (boosted 1.5× in scoring).
    pub agent_skill_ids: &'a HashSet<String>,
    /// Channel identifier for trigger context (e.g. "tauri", "discord").
    pub channel_id: Option<&'a str>,
    /// Human-readable channel description for runtime context (e.g. "Tauri desktop", "Discord #general").
    pub channel_description: &'a str,
    /// Token budget for prompt assembly.
    pub budget: PromptBudget,
}

// ---------------------------------------------------------------------------
// 1. Memory Recall
// ---------------------------------------------------------------------------

/// Recall relevant memories for the given query.
///
/// Returns `Vec<MemoryFragment>` ready for `PromptBuilder::memory()`.
/// On failure, logs a warning and returns an empty vec (never blocks the pipeline).
pub(crate) async fn recall_memories(
    memory: &MemoryManager<SochMemoryBackend>,
    query: &str,
    max_results: usize,
) -> Vec<MemoryFragment> {
    match memory.recall(query, Some(max_results)).await {
        Ok(results) => results
            .into_iter()
            .filter_map(|r| {
                let text = r.content?;
                if text.is_empty() {
                    return None;
                }
                Some(MemoryFragment {
                    token_cost: estimate_tokens(&text),
                    relevance: r.score as f64,
                    source: r
                        .metadata
                        .get("source")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    content: text,
                })
            })
            .collect(),
        Err(e) => {
            warn!(error = %e, "Memory recall failed — continuing without memories");
            vec![]
        }
    }
}

/// Extract memory-signal keywords from fragments for skill trigger boosting.
pub(crate) fn extract_memory_signals(fragments: &[MemoryFragment], top_n: usize) -> Vec<String> {
    fragments
        .iter()
        .take(top_n)
        .flat_map(|f| TurnContext::extract_keywords(&f.content))
        .collect::<HashSet<_>>()
        .into_iter()
        .take(20)
        .collect()
}

// ---------------------------------------------------------------------------
// 2. Skill Scoring
// ---------------------------------------------------------------------------

/// Score active skills using trigger evaluation with memory-signal boosting.
///
/// Agent-assigned skills receive a 1.5× priority weight.
pub(crate) fn score_skills(
    skills: &[Arc<clawdesk_skills::Skill>],
    user_content: &str,
    channel_id: Option<&str>,
    agent_skill_ids: &HashSet<String>,
    memory_signals: Vec<String>,
) -> Vec<ScoredSkill> {
    let trigger_ctx = TurnContext {
        channel_id: channel_id.map(String::from),
        message_keywords: TurnContext::extract_keywords(user_content),
        message_text: user_content.to_string(),
        current_time: Utc::now(),
        requested_skill_ids: vec![],
        triggered_this_turn: HashSet::new(),
        memory_signals,
    };

    skills
        .iter()
        .map(|s| {
            let trigger_result = TriggerEvaluator::evaluate(s, &trigger_ctx);

            let dn = s.manifest.display_name.to_lowercase();
            let id = s.manifest.id.as_str().to_lowercase();
            let short_id = id.rsplit('/').next().unwrap_or(&id).to_string();
            let is_agent_skill = agent_skill_ids.contains(&dn)
                || agent_skill_ids.contains(&id)
                || agent_skill_ids.contains(&short_id);

            let base_weight = if trigger_result.matched { 2.0 } else { 1.0 };
            let priority_weight = if is_agent_skill {
                base_weight * 1.5
            } else {
                base_weight
            };

            ScoredSkill {
                skill_id: s.manifest.id.as_str().to_string(),
                display_name: s.manifest.display_name.clone(),
                prompt_fragment: s.prompt_fragment.clone(),
                token_cost: estimate_tokens(&s.prompt_fragment),
                priority_weight,
                relevance: trigger_result.relevance,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 3. Prompt Assembly (PromptBuilder)
// ---------------------------------------------------------------------------

/// Run the full prompt-building pipeline:
/// 1. Recall memories
/// 2. Extract memory signals
/// 3. Score skills with trigger evaluation
/// 4. Assemble via PromptBuilder (knapsack budget allocation)
///
/// Returns the system prompt, memory injection text, and manifest.
pub(crate) async fn build_prompt_pipeline(
    input: PromptPipelineInput<'_>,
    memory: &MemoryManager<SochMemoryBackend>,
    active_skills: &[Arc<clawdesk_skills::Skill>],
) -> PromptPipelineResult {
    // Step 1: Memory recall
    let memory_fragments = recall_memories(memory, input.user_content, 10).await;

    // Step 2: Memory signals for skill boosting
    let memory_signals = extract_memory_signals(&memory_fragments, 5);

    // Step 3: Score skills
    let scored_skills = score_skills(
        active_skills,
        input.user_content,
        input.channel_id,
        input.agent_skill_ids,
        memory_signals,
    );

    // Step 4: PromptBuilder assembly
    let runtime_ctx = RuntimeContext {
        datetime: Utc::now().to_rfc3339(),
        channel_description: Some(input.channel_description.to_string()),
        model_name: Some(input.model_name.to_string()),
        metadata: vec![],
    };

    match PromptBuilder::new(input.budget) {
        Ok(builder) => {
            let (assembled, manifest) = builder
                .identity(input.persona.to_string())
                .runtime(runtime_ctx)
                .skills(scored_skills)
                .memory(memory_fragments)
                .build();

            PromptPipelineResult {
                system_prompt: assembled.text,
                memory_injection: assembled.memory_text,
                prompt_manifest: Some(manifest),
            }
        }
        Err(e) => {
            warn!(error = %e, "PromptBuilder failed validation — falling back to raw persona");
            PromptPipelineResult {
                system_prompt: input.persona.to_string(),
                memory_injection: None,
                prompt_manifest: None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Memory Injection (pre-user-message)
// ---------------------------------------------------------------------------

/// Inject memory context as a System message just before the last user message.
///
/// This exploits the LLM's recency bias — tokens near the end of the context
/// window receive much higher attention weights than those buried in the
/// system prompt.
pub(crate) fn inject_memory_context(history: &mut Vec<ChatMessage>, memory_text: &str) {
    let insert_pos = history
        .iter()
        .rposition(|m| matches!(m.role, MessageRole::User))
        .unwrap_or(history.len());
    let mem_msg = ChatMessage::new(MessageRole::System, memory_text);
    history.insert(insert_pos, mem_msg);
    debug!(
        insert_pos,
        mem_len = memory_text.len(),
        "Injected memory context pre-user-message"
    );
}

// ---------------------------------------------------------------------------
// 5. Skill Provider (for runner)
// ---------------------------------------------------------------------------

/// Build a `SkillProvider` for the `AgentRunner` from the skill registry.
///
/// Returns `None` if no active skills are available.
pub(crate) fn build_skill_provider(
    active_skills: Vec<Arc<clawdesk_skills::Skill>>,
) -> Option<Arc<dyn clawdesk_agents::runner::SkillProvider>> {
    if active_skills.is_empty() {
        return None;
    }

    use clawdesk_skills::env_injection::EnvResolver;
    use clawdesk_skills::orchestrator::SkillOrchestrator;
    use clawdesk_skills::skill_provider::OrchestratorSkillProvider;

    let orchestrator = SkillOrchestrator::new(active_skills, 8_000);
    let env_resolver = EnvResolver::default();
    Some(Arc::new(OrchestratorSkillProvider::new(
        orchestrator,
        env_resolver,
    )))
}

/// Load active skills from the skill registry.
///
/// Returns an empty vec on lock failure (non-blocking).
pub(crate) fn load_active_skills(
    skill_registry: &std::sync::RwLock<clawdesk_skills::registry::SkillRegistry>,
) -> Vec<Arc<clawdesk_skills::Skill>> {
    skill_registry
        .read()
        .ok()
        .map(|r| r.active_skills().iter().map(|s| Arc::clone(s)).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 6. Post-Run Memory Storage
// ---------------------------------------------------------------------------

/// Store a conversation turn (user + assistant) in memory for future recall.
///
/// - UTF-8 safe truncation (never panics on multi-byte characters)
/// - Content-hash dedup via SHA-256
/// - Batch embedding for efficiency
/// - Temporal graph edges for "what was discussed when?" queries
///
/// Runs in the background (caller should `tokio::spawn` if fire-and-forget).
pub(crate) async fn store_conversation_memory(
    memory: &MemoryManager<SochMemoryBackend>,
    user_content: &str,
    assistant_content: &str,
    source_id: &str,
    source_name: &str,
    temporal_graph: Option<&SochTemporalGraph>,
) {
    let user_summary = clawdesk_memory::safe_truncate_with_ellipsis(user_content, 500);
    let asst_summary = clawdesk_memory::safe_truncate_with_ellipsis(assistant_content, 500);

    let user_hash = clawdesk_memory::sha256_hex(&user_summary);
    let asst_hash = clawdesk_memory::sha256_hex(&asst_summary);

    let batch = vec![
        (
            user_summary,
            clawdesk_memory::MemorySource::Conversation,
            serde_json::json!({
                "role": "user",
                "agent_id": source_id,
                "agent_name": source_name,
                "content_hash": user_hash,
            }),
        ),
        (
            asst_summary,
            clawdesk_memory::MemorySource::Conversation,
            serde_json::json!({
                "role": "assistant",
                "agent_id": source_id,
                "agent_name": source_name,
                "content_hash": asst_hash,
            }),
        ),
    ];

    match memory.remember_batch(batch).await {
        Ok(ids) => {
            info!(
                count = ids.len(),
                source = %source_id,
                "Memory stored (user + assistant)"
            );

            // Temporal edges: record that this source discussed these memories now
            if let Some(tg) = temporal_graph {
                let node = format!("agent:{}", source_id);
                for memory_id in &ids {
                    let _ = tg.add_edge(
                        &node,
                        "discussed",
                        memory_id,
                        Some(std::collections::HashMap::from([(
                            "turn_timestamp".to_string(),
                            serde_json::json!(Utc::now().to_rfc3339()),
                        )])),
                    );
                }
            }
        }
        Err(e) => {
            warn!(
                error = %e,
                source = %source_id,
                "Memory store failed — memories from this turn will be lost"
            );
        }
    }
}
