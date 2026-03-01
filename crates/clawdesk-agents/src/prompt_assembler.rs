//! Prompt assembly pipeline — builds the enriched system prompt and messages
//! for an LLM request.
//!
//! Extracts the linear ~300-line prompt assembly pipeline from `AgentRunner::run()`
//! into a composable, testable unit. The assembler processes inputs through
//! sequential stages:
//!
//! 1. **Bootstrap context** — discover workspace files (CLAUDE.md, README, etc.)
//! 2. **Channel context** — inject channel capabilities/formatting hints
//! 3. **Skill selection** — dynamic per-turn skill prompt injection
//! 4. **Output discipline** — append output formatting rules
//! 5. **Memory recall** — inject relevant memories before the last user message
//!
//! Each stage enriches the `system_prompt` and/or `messages` in a pure-functional
//! style (no runner state dependency).
//!
//! # Usage
//!
//! ```ignore
//! let assembler = PromptAssembler::new();
//! let output = assembler.assemble(AssemblyInput {
//!     system_prompt,
//!     messages,
//!     context_limit: 128_000,
//!     workspace_path: Some("/path/to/workspace"),
//!     bootstrap_config: None,
//!     channel_context: None,
//!     skill_provider: None,
//!     session_id: None,
//!     turn: 0,
//!     memory_recall_fn: None,
//! }).await;
//! ```

use crate::bootstrap::{self, BootstrapConfig};
use crate::runner::{
    AgentEvent, ChannelContext, MemoryRecallFn, SkillProvider,
};
use clawdesk_domain::context_guard::estimate_tokens;
use clawdesk_providers::{ChatMessage, MessageRole};
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info};

/// Input to the prompt assembly pipeline.
///
/// All assembly dependencies are passed explicitly — no runner state
/// is required, enabling standalone testing and reuse across different
/// runner implementations.
pub struct AssemblyInput<'a> {
    /// Base system prompt (may include hook overrides already applied).
    pub system_prompt: String,
    /// Sanitized conversation history.
    pub messages: Vec<ChatMessage>,
    /// Total context window size in tokens (e.g., 128_000).
    pub context_limit: usize,
    /// Workspace path for bootstrap file discovery. `None` skips bootstrap.
    pub workspace_path: Option<&'a str>,
    /// Bootstrap configuration; `None` uses defaults.
    pub bootstrap_config: Option<BootstrapConfig>,
    /// Channel context for format/capability injection. `None` skips.
    pub channel_context: Option<&'a ChannelContext>,
    /// Per-turn skill selector. `None` skips skill injection.
    pub skill_provider: Option<&'a dyn SkillProvider>,
    /// Session ID for skill selection context.
    pub session_id: Option<&'a str>,
    /// Turn number within the session.
    pub turn: u32,
    /// Async memory recall callback. `None` skips memory injection.
    pub memory_recall_fn: Option<&'a MemoryRecallFn>,
    /// Optional event sink for assembly telemetry.
    pub event_sink: Option<&'a (dyn Fn(AgentEvent) + Send + Sync)>,
}

/// Output of the prompt assembly pipeline.
pub struct AssemblyOutput {
    /// Enriched system prompt (with bootstrap, channel, skills, output rules).
    pub system_prompt: String,
    /// Messages with optional memory context injected.
    pub messages: Vec<ChatMessage>,
    /// IDs of skills that were selected and injected.
    pub active_skills: Vec<String>,
}

/// Stateless prompt assembly pipeline.
///
/// Each stage is a pure transformation of (system_prompt, messages) →
/// (system_prompt', messages'). The assembler holds no mutable state
/// and can be shared across threads.
pub struct PromptAssembler;

impl PromptAssembler {
    pub fn new() -> Self {
        Self
    }

    /// Run the full assembly pipeline.
    ///
    /// Stages execute sequentially, each enriching the prompt/messages:
    /// 1. Bootstrap → 2. Channel → 3. Skills → 4. Output rules → 5. Memory
    pub async fn assemble(&self, input: AssemblyInput<'_>) -> AssemblyOutput {
        let AssemblyInput {
            system_prompt,
            messages,
            context_limit,
            workspace_path,
            bootstrap_config,
            channel_context,
            skill_provider,
            session_id,
            turn,
            memory_recall_fn,
            event_sink: _,
        } = input;

        // Stage 1: Bootstrap context — discover workspace files
        let system_prompt = self.inject_bootstrap(
            system_prompt,
            workspace_path,
            bootstrap_config,
            context_limit,
        );

        // Stage 2: Channel context — inject capabilities/formatting
        let system_prompt = self.inject_channel_context(system_prompt, channel_context);

        // Stage 3: Skill selection — dynamic per-turn injection
        let (system_prompt, active_skills) = self
            .inject_skills(
                system_prompt,
                &messages,
                skill_provider,
                session_id,
                channel_context.map(|c| c.channel_name.as_str()),
                turn,
                context_limit,
            )
            .await;

        // Stage 4: Output discipline — formatting rules
        let system_prompt = Self::append_output_discipline(system_prompt);

        // Stage 5: Memory recall — inject context before last user message
        let messages = self
            .inject_memory(messages, memory_recall_fn, context_limit)
            .await;

        AssemblyOutput {
            system_prompt,
            messages,
            active_skills,
        }
    }

    // ── Stage implementations ──────────────────────────────────────────

    /// Stage 1: Bootstrap context injection.
    ///
    /// Discovers workspace files (CLAUDE.md, README.md, Cargo.toml, etc.)
    /// and prepends them to the system prompt. Budget: 15% of context_limit.
    fn inject_bootstrap(
        &self,
        system_prompt: String,
        workspace_path: Option<&str>,
        bootstrap_config: Option<BootstrapConfig>,
        context_limit: usize,
    ) -> String {
        let Some(ws_path_str) = workspace_path else {
            return system_prompt;
        };
        let ws_path = Path::new(ws_path_str);
        if !ws_path.is_dir() {
            debug!(path = ws_path_str, "workspace path not a directory, skipping bootstrap");
            return system_prompt;
        }

        let boot_config = bootstrap_config.unwrap_or_default();
        let bootstrap_budget = context_limit * 15 / 100;
        let boot_config = BootstrapConfig {
            max_total_chars: boot_config.max_total_chars.min(bootstrap_budget * 4),
            ..boot_config
        };

        let boot_result = bootstrap::discover_bootstrap_files(ws_path, &boot_config);
        if boot_result.files.is_empty() {
            return system_prompt;
        }

        let bootstrap_section = bootstrap::assemble_bootstrap_prompt(&boot_result);
        info!(
            files = boot_result.files.len(),
            tokens = boot_result.total_tokens,
            budget = bootstrap_budget,
            "injected bootstrap context into system prompt"
        );
        format!("{}\n\n{}", bootstrap_section, system_prompt)
    }

    /// Stage 2: Channel context injection.
    ///
    /// Appends channel capabilities and formatting hints to the system prompt.
    fn inject_channel_context(
        &self,
        system_prompt: String,
        channel_context: Option<&ChannelContext>,
    ) -> String {
        let Some(ch_ctx) = channel_context else {
            return system_prompt;
        };
        let channel_section = ch_ctx.to_prompt_section();
        info!(
            channel = %ch_ctx.channel_name,
            markup = %ch_ctx.markup_format,
            "injected channel context into system prompt"
        );
        format!("{}\n\n{}", system_prompt, channel_section)
    }

    /// Stage 3: Per-turn skill selection and injection.
    ///
    /// Selects relevant skills for the user's message and injects their
    /// prompt fragments. Budget: 20% of context_limit.
    async fn inject_skills(
        &self,
        system_prompt: String,
        messages: &[ChatMessage],
        skill_provider: Option<&dyn SkillProvider>,
        session_id: Option<&str>,
        channel_id: Option<&str>,
        turn: u32,
        context_limit: usize,
    ) -> (String, Vec<String>) {
        let Some(sp) = skill_provider else {
            return (system_prompt, Vec::new());
        };

        let user_message = messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .map(|m| m.content.as_ref())
            .unwrap_or("");
        let skill_budget = context_limit / 5;

        let injection = sp
            .select_skills(
                user_message,
                session_id.unwrap_or("unknown"),
                channel_id,
                turn,
                skill_budget,
            )
            .await;

        if injection.prompt_fragments.is_empty() {
            return (system_prompt, Vec::new());
        }

        let active_skills = injection.selected_skill_ids.clone();
        let skills_section = injection.prompt_fragments.join("\n\n");
        info!(
            skills = injection.selected_skill_ids.len(),
            tokens = injection.total_tokens,
            "injected skill prompts into system prompt"
        );
        (format!("{}\n\n{}", system_prompt, skills_section), active_skills)
    }

    /// Stage 4: Append output discipline rules.
    ///
    /// Instructs the model to respond with ONLY the final answer,
    /// suppressing reasoning narration and raw data dumps.
    fn append_output_discipline(system_prompt: String) -> String {
        format!(
            "{}\n\n## Output Rules\n\
             - Respond with ONLY the final answer the user needs.\n\
             - Do NOT repeat or narrate your reasoning, planning, or tool usage in your response.\n\
             - Do NOT echo raw JSON, coordinates, API responses, or intermediate data unless the user explicitly asked for it.\n\
             - Keep responses concise and directly useful.",
            system_prompt
        )
    }

    /// Stage 5: Memory recall injection.
    ///
    /// Recalls relevant memories and injects them as a `<memory_context>`
    /// system message just before the last user message (recency bias).
    /// Budget: min(4096, 15% of context_limit) tokens.
    async fn inject_memory(
        &self,
        messages: Vec<ChatMessage>,
        memory_recall_fn: Option<&MemoryRecallFn>,
        context_limit: usize,
    ) -> Vec<ChatMessage> {
        let Some(recall_fn) = memory_recall_fn else {
            return messages;
        };

        let user_query = messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .map(|m| m.content.to_string())
            .unwrap_or_default();

        if user_query.is_empty() {
            return messages;
        }

        let recall_results = recall_fn(user_query).await;
        if recall_results.is_empty() {
            return messages;
        }

        // Format as XML memory context block
        let mut mem_block = String::from("<memory_context>\n");
        for r in &recall_results {
            mem_block.push_str(&format!(
                "<memory relevance=\"{:.2}\"{}>{}</memory>\n",
                r.relevance,
                r.source
                    .as_ref()
                    .map(|s| format!(" source=\"{}\"", s))
                    .unwrap_or_default(),
                r.content
            ));
        }
        mem_block.push_str("</memory_context>");

        // Budget: cap memory at min(4096, 15% of context_limit) tokens
        let memory_budget = (context_limit * 15 / 100).min(4096);
        let mem_tokens = estimate_tokens(&mem_block);
        if mem_tokens > memory_budget {
            debug!(
                tokens = mem_tokens,
                budget = memory_budget,
                "memory context exceeds budget, skipping"
            );
            return messages;
        }

        // Inject before last user message (recency bias)
        let mut msgs = messages;
        let insert_pos = msgs
            .iter()
            .rposition(|m| matches!(m.role, MessageRole::User))
            .unwrap_or(msgs.len());
        msgs.insert(insert_pos, ChatMessage::new(MessageRole::System, mem_block));
        info!(
            memories = recall_results.len(),
            tokens = mem_tokens,
            budget = memory_budget,
            "injected memory context into runner history"
        );
        msgs
    }
}

impl Default for PromptAssembler {
    fn default() -> Self {
        Self::new()
    }
}
