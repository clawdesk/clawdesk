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
// Safety / behavioral directive — injected via PromptBuilder::safety()
// ---------------------------------------------------------------------------

/// Behavioral directive that teaches the agent to ask for human approval
/// instead of refusing or restating capabilities.
const HUMAN_DECISION_DIRECTIVE: &str = "\
<decision_protocol>
When a task requires a decision that should be made by the human (e.g. deployment, \
file execution, destructive actions, choosing between options, spending money, \
or anything where the user's explicit preference matters):

1. Do NOT refuse or say \"I cannot do that\".
2. Do NOT just list what you can do.
3. Instead, call the `ask_human` tool with a clear question and suggested options.
4. Wait for the human's response, then proceed based on their decision.

Examples of when to use ask_human:
- \"Should I run this code locally or generate it for you to review first?\"
- \"This will modify 15 files. Proceed, or review the changes first?\"
- \"I can deploy via Option A or Option B. Which do you prefer?\"

Never guess the user's preference for consequential actions — ask.
</decision_protocol>";

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
    /// Actually-connected channel names from ChannelRegistry (e.g. ["telegram", "discord", "webchat"]).
    pub available_channels: Vec<String>,
    /// Optional chat/session ID for temporal memory expansion.
    /// When set, memory recall uses geodesic concentric search:
    /// session-local → cross-session → global corpus.
    pub session_id: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// 1. Memory Recall
// ---------------------------------------------------------------------------

/// Recall relevant memories for the given query.
///
/// Uses temporal expansion: searches the current session first, then expands
/// outward to cross-session memories, then global corpus (geodesic rings).
///
/// Returns `Vec<MemoryFragment>` ready for `PromptBuilder::memory()`.
/// On failure, logs a warning and returns an empty vec (never blocks the pipeline).
pub(crate) async fn recall_memories(
    memory: &MemoryManager<SochMemoryBackend>,
    query: &str,
    max_results: usize,
) -> Vec<MemoryFragment> {
    recall_memories_scoped(memory, query, max_results, None).await
}

/// Recall memories with optional session scope for temporal expansion.
///
/// When `session_id` is provided, uses the geodesic concentric search pattern:
/// Ring 0 (current session) → Ring 1 (cross-session) → Ring 2 (global corpus).
/// Closer rings receive higher relevance boosts.
pub(crate) async fn recall_memories_scoped(
    memory: &MemoryManager<SochMemoryBackend>,
    query: &str,
    max_results: usize,
    session_id: Option<&str>,
) -> Vec<MemoryFragment> {
    let recall_result = if session_id.is_some() {
        memory.recall_with_scope(query, Some(max_results), session_id).await
    } else {
        memory.recall(query, Some(max_results)).await
    };
    match recall_result {
        Ok(results) => {
            let raw_count = results.len();
            let fragments: Vec<MemoryFragment> = results
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
                .collect();
            let dropped = raw_count - fragments.len();
            if dropped > 0 {
                warn!(
                    raw_results = raw_count,
                    kept = fragments.len(),
                    dropped,
                    query = %safe_log_prefix(query, 80),
                    "Memory recall: {} results dropped (content was None or empty)",
                    dropped,
                );
            } else if !fragments.is_empty() {
                debug!(
                    results = fragments.len(),
                    query = %safe_log_prefix(query, 80),
                    "Memory recall returned {} fragments",
                    fragments.len(),
                );
            }
            fragments
        }
        Err(e) => {
            warn!(error = %e, "Memory recall failed — continuing without memories");
            vec![]
        }
    }
}

/// Truncate a string for safe logging (no panics on char boundaries).
fn safe_log_prefix(s: &str, max: usize) -> &str {
    if s.len() <= max { return s; }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    &s[..end]
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
    // Step 1: Memory recall (with temporal expansion when session_id is available)
    let memory_fragments = recall_memories_scoped(memory, input.user_content, 10, input.session_id).await;

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
        available_channels: input.available_channels,
    };

    match PromptBuilder::new(input.budget) {
        Ok(builder) => {
            let (assembled, manifest) = builder
                .identity(input.persona.to_string())
                .safety(HUMAN_DECISION_DIRECTIVE.to_string())
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

/// Minimum content length to consider a turn worth storing.
/// Short turns like "hi", "ok", "thanks" are ephemeral and dilute recall quality.
const MIN_CONTENT_LEN_FOR_MEMORY: usize = 15;

/// Maximum truncation length per turn. Increased from 500 to 2000 to preserve
/// important context from longer assistant responses (decisions, explanations).
const MAX_MEMORY_TURN_CHARS: usize = 2000;

/// Patterns that indicate a user turn contains memorable information.
/// If a user turn is short BUT matches one of these, store it anyway.
const MEMORY_TRIGGER_PATTERNS: &[&str] = &[
    "my name is",
    "i am ",
    "i'm ",
    "i prefer",
    "i like",
    "i don't like",
    "i hate",
    "i love",
    "i work at",
    "i work for",
    "i live in",
    "remember ",
    "don't forget",
    "my email",
    "my phone",
    "my address",
    "call me ",
    "i decided",
    "we decided",
    "the plan is",
    "deadline is",
    "due date",
    "birthday",
    "anniversary",
];

/// Aho-Corasick automaton for memory trigger patterns — built once at startup.
///
/// Matches all 24 trigger patterns in a single O(n + z) pass over input
/// (n = input length, z = match count) instead of O(24 × n) with linear scan.
/// Automaton occupies ~50KB fitting in L1 cache.
static MEMORY_TRIGGER_AC: std::sync::LazyLock<aho_corasick::AhoCorasick> =
    std::sync::LazyLock::new(|| {
        aho_corasick::AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(MEMORY_TRIGGER_PATTERNS)
            .expect("valid Aho-Corasick patterns")
    });

/// Check if content is important enough to store in memory.
fn is_memorable(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.len() >= MIN_CONTENT_LEN_FOR_MEMORY {
        return true;
    }
    // Single-pass Aho-Corasick match — O(n + z) vs old O(24 × n).
    MEMORY_TRIGGER_AC.is_match(trimmed)
}

/// Store a conversation turn (user + assistant) in memory for future recall.
///
/// - Importance gate: trivial turns ("hi", "ok") are skipped
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
    // Importance gate: skip trivial turns that dilute memory quality.
    let user_memorable = is_memorable(user_content);
    let asst_memorable = is_memorable(assistant_content);

    if !user_memorable && !asst_memorable {
        debug!(
            user_len = user_content.len(),
            asst_len = assistant_content.len(),
            "Skipping trivial turn — not memorable enough to store"
        );
        return;
    }

    let mut batch = Vec::new();

    if user_memorable {
        let user_summary = clawdesk_memory::safe_truncate_with_ellipsis(user_content, MAX_MEMORY_TURN_CHARS);
        let user_hash = clawdesk_memory::sha256_hex(&user_summary);
        batch.push((
            user_summary,
            clawdesk_memory::MemorySource::Conversation,
            serde_json::json!({
                "role": "user",
                "agent_id": source_id,
                "agent_name": source_name,
                "content_hash": user_hash,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        ));
    }

    if asst_memorable {
        let asst_summary = clawdesk_memory::safe_truncate_with_ellipsis(assistant_content, MAX_MEMORY_TURN_CHARS);
        let asst_hash = clawdesk_memory::sha256_hex(&asst_summary);
        batch.push((
            asst_summary,
            clawdesk_memory::MemorySource::Conversation,
            serde_json::json!({
                "role": "assistant",
                "agent_id": source_id,
                "agent_name": source_name,
                "content_hash": asst_hash,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        ));
    }

    if batch.is_empty() {
        return;
    }

    match memory.remember_batch(batch).await {
        Ok(ids) => {
            info!(
                count = ids.len(),
                source = %source_id,
                "Memory stored (filtered by importance)"
            );

            // Note: event bus emission for memory.stored would go here
            // but we don't have access to the EventBus in this function.
            // The caller (send_message) should emit if needed.

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
