//! Context assembly — builds the context window for LLM calls.
//!
//! Combines conversation history, vector search results, and system prompts
//! into a context payload that fits within the model's token budget.
//!
//! ## Pluggable Context Sources
//!
//! Implement `ContextSource` and register with `ContextAssembler::register()`
//! to add new context sources (RAG, link understanding, hooks, etc.) without
//! modifying the core assembly pipeline. Each source declares a priority and
//! token budget; the assembler allocates budget proportionally.
//!
//! ## Zero-Copy Design
//!
//! `PromptRope` stores prompt fragments as `Cow<'a, str>` references,
//! deferring concatenation until final IO. Context assembly is O(K)
//! where K is the number of fragments, not O(N) total bytes.

use async_trait::async_trait;
use clawdesk_providers::ChatMessage;
use clawdesk_types::estimate_tokens;
use std::borrow::Cow;
use std::sync::Arc;
use tracing::debug;

/// Token budget configuration for context assembly.
#[derive(Debug, Clone)]
pub struct ContextBudget {
    /// Maximum total tokens for the context window.
    pub max_tokens: usize,
    /// Reserved tokens for the system prompt.
    pub system_reserve: usize,
    /// Reserved tokens for the model's response.
    pub response_reserve: usize,
    /// How much of the budget to allocate to vector search results.
    pub vector_ratio: f32,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_tokens: 128_000,
            system_reserve: 4_000,
            response_reserve: 8_192,
            vector_ratio: 0.2,
        }
    }
}

/// Assembled context ready for an LLM call.
#[derive(Debug)]
pub struct AssembledContext {
    pub system_prompt: String,
    pub messages: Vec<ChatMessage>,
    pub estimated_tokens: usize,
}

// ---------------------------------------------------------------------------
// PromptRope — zero-copy prompt construction
// ---------------------------------------------------------------------------

/// A rope-like structure that holds prompt fragments as borrowed or owned
/// strings, assembling them into a contiguous buffer only at final use.
///
/// Avoids O(N) memcpy intermediates during context construction — each
/// `append` is O(1) (push a Cow pointer). Final `flatten()` is the single
/// allocation.
#[derive(Debug, Clone)]
pub struct PromptRope<'a> {
    fragments: Vec<Cow<'a, str>>,
    /// Running byte count estimate (not a token count).
    byte_len: usize,
}

impl<'a> PromptRope<'a> {
    /// New empty rope.
    pub fn new() -> Self {
        Self {
            fragments: Vec::new(),
            byte_len: 0,
        }
    }

    /// Pre-allocate for an expected number of fragments.
    pub fn with_capacity(n: usize) -> Self {
        Self {
            fragments: Vec::with_capacity(n),
            byte_len: 0,
        }
    }

    /// Append a borrowed string slice — zero-copy.
    #[inline]
    pub fn append_borrowed(&mut self, s: &'a str) {
        self.byte_len += s.len();
        self.fragments.push(Cow::Borrowed(s));
    }

    /// Append an owned String (e.g., from format!).
    #[inline]
    pub fn append_owned(&mut self, s: String) {
        self.byte_len += s.len();
        self.fragments.push(Cow::Owned(s));
    }

    /// Append a newline separator.
    #[inline]
    pub fn newline(&mut self) {
        self.fragments.push(Cow::Borrowed("\n"));
        self.byte_len += 1;
    }

    /// Number of fragments.
    pub fn fragment_count(&self) -> usize {
        self.fragments.len()
    }

    /// Estimated byte length (sum of all fragment lengths).
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// Estimated token count (heuristic: bytes / 4).
    pub fn estimated_tokens(&self) -> usize {
        self.byte_len / 4
    }

    /// Flatten into a single contiguous String. This is the ONLY allocation.
    pub fn flatten(&self) -> String {
        let mut out = String::with_capacity(self.byte_len);
        for frag in &self.fragments {
            out.push_str(frag);
        }
        out
    }

    /// Return fragments as IoSlice-compatible references for vectored IO.
    /// Useful when the downstream transport supports writev/sendmsg.
    pub fn as_slices(&self) -> Vec<&[u8]> {
        self.fragments.iter().map(|f| f.as_bytes()).collect()
    }

    /// Iterate over fragments without allocating.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.fragments.iter().map(|f| f.as_ref())
    }
}

impl<'a> Default for PromptRope<'a> {
    fn default() -> Self {
        Self::new()
    }
}

/// Assembles context for LLM calls from multiple sources.
pub struct ContextAssembler {
    budget: ContextBudget,
    /// Pluggable context sources, sorted by priority at assembly time.
    sources: Vec<Arc<dyn ContextSource>>,
}

// ---------------------------------------------------------------------------
// Pluggable context source trait
// ---------------------------------------------------------------------------

/// A context block produced by a `ContextSource`.
#[derive(Debug, Clone)]
pub struct ContextBlock {
    /// Source identifier for debugging/observability.
    pub source_id: String,
    /// The rendered content to inject into the context.
    pub content: String,
    /// Actual tokens used.
    pub tokens_used: usize,
}

/// Session context available to context sources during rendering.
pub struct SessionContext<'a> {
    /// Current conversation history.
    pub history: &'a [ChatMessage],
    /// Session key/ID.
    pub session_key: &'a str,
    /// Agent ID.
    pub agent_id: &'a str,
}

/// Trait for pluggable context sources.
///
/// Implement this trait and register with `ContextAssembler::register()` to
/// inject context from new sources (RAG results, link previews, hooks, etc.)
/// without modifying the core assembly pipeline.
#[async_trait]
pub trait ContextSource: Send + Sync {
    /// Unique identifier for this source (used in logs and budgeting).
    fn id(&self) -> &str;

    /// Priority (lower = higher priority, rendered first). System prompt = 0,
    /// conversation history = 10, vector results = 20. User sources should
    /// use 30+ unless overriding core behavior.
    fn priority(&self) -> u32;

    /// Minimum tokens this source needs to be useful. If the budget cannot
    /// accommodate this minimum, the source is dropped entirely.
    fn min_tokens(&self) -> usize { 0 }

    /// Preferred token budget (the source will get up to this much).
    fn preferred_tokens(&self) -> usize { 2000 }

    /// Render the context content within the given token budget.
    async fn render(&self, ctx: &SessionContext<'_>, budget: usize) -> Option<ContextBlock>;
}

// ---------------------------------------------------------------------------
// Built-in context sources
// ---------------------------------------------------------------------------

/// Built-in: conversation history (priority 10).
pub struct HistorySource;

#[async_trait]
impl ContextSource for HistorySource {
    fn id(&self) -> &str { "history" }
    fn priority(&self) -> u32 { 10 }
    fn min_tokens(&self) -> usize { 100 }
    fn preferred_tokens(&self) -> usize { 64_000 }

    async fn render(&self, ctx: &SessionContext<'_>, budget: usize) -> Option<ContextBlock> {
        let mut messages_text = Vec::new();
        let mut used = 0;
        for msg in ctx.history.iter().rev() {
            let tokens = estimate_tokens(&msg.content);
            if used + tokens > budget { break; }
            messages_text.push(msg.content.clone());
            used += tokens;
        }
        messages_text.reverse();
        if messages_text.is_empty() { return None; }
        Some(ContextBlock {
            source_id: self.id().to_string(),
            content: messages_text.join("\n"),
            tokens_used: used,
        })
    }
}

/// Built-in: vector search results (priority 20).
pub struct VectorSource {
    pub results: Vec<String>,
}

#[async_trait]
impl ContextSource for VectorSource {
    fn id(&self) -> &str { "vector_search" }
    fn priority(&self) -> u32 { 20 }
    fn preferred_tokens(&self) -> usize { 4_000 }

    async fn render(&self, _ctx: &SessionContext<'_>, budget: usize) -> Option<ContextBlock> {
        if self.results.is_empty() { return None; }
        let mut content = String::from("<relevant_context>\n");
        let mut used = 0;
        for result in &self.results {
            let tokens = estimate_tokens(result);
            if used + tokens > budget { break; }
            content.push_str(result);
            content.push('\n');
            used += tokens;
        }
        content.push_str("</relevant_context>");
        if used == 0 { return None; }
        Some(ContextBlock {
            source_id: self.id().to_string(),
            content,
            tokens_used: used,
        })
    }
}

impl ContextAssembler {
    pub fn new(budget: ContextBudget) -> Self {
        Self { budget, sources: Vec::new() }
    }

    /// Register a pluggable context source.
    pub fn register(&mut self, source: Arc<dyn ContextSource>) {
        self.sources.push(source);
    }

    /// Build context using registered sources with priority-weighted budget allocation.
    ///
    /// Algorithm:
    /// 1. Sort sources by priority (lower = higher priority).
    /// 2. Phase 1: allocate min_tokens to each source; drop lowest-priority
    ///    sources if total exceeds available budget.
    /// 3. Phase 2: distribute remaining budget proportionally by priority.
    /// 4. Phase 3: redistribute unclaimed budget from capped sources.
    /// 5. Render each source within its allocated budget.
    pub async fn assemble_pluggable(
        &self,
        system_prompt: &str,
        session: &SessionContext<'_>,
    ) -> AssembledContext {
        let available = self.budget.max_tokens
            .saturating_sub(self.budget.system_reserve)
            .saturating_sub(self.budget.response_reserve);

        // Sort sources by priority.
        let mut indexed_sources: Vec<(usize, &Arc<dyn ContextSource>)> =
            self.sources.iter().enumerate().collect();
        indexed_sources.sort_by_key(|(_, s)| s.priority());

        // Phase 1: allocate minimums, dropping lowest-priority if over budget.
        let mut allocations: Vec<(usize, usize)> = Vec::new(); // (source_idx, budget)
        let mut total_min = 0;
        for &(idx, ref source) in &indexed_sources {
            let min = source.min_tokens();
            if total_min + min > available {
                // Drop this source — not enough budget.
                continue;
            }
            total_min += min;
            allocations.push((idx, min));
        }

        // Phase 2: distribute remaining budget proportionally.
        let remaining = available.saturating_sub(total_min);
        if remaining > 0 {
            let total_priority: u32 = allocations.iter()
                .map(|(idx, _)| {
                    let p = self.sources[*idx].priority();
                    // Invert priority so lower number = higher weight.
                    100u32.saturating_sub(p).max(1)
                })
                .sum();

            for alloc in allocations.iter_mut() {
                let source = &self.sources[alloc.0];
                let weight = 100u32.saturating_sub(source.priority()).max(1);
                let extra = (remaining as u64 * weight as u64 / total_priority.max(1) as u64) as usize;
                let preferred = source.preferred_tokens();
                let capped_extra = extra.min(preferred.saturating_sub(alloc.1));
                alloc.1 += capped_extra;
            }
        }

        // Render sources.
        let mut context_blocks: Vec<ContextBlock> = Vec::new();
        for (idx, budget) in &allocations {
            let source = &self.sources[*idx];
            if let Some(block) = source.render(session, *budget).await {
                context_blocks.push(block);
            }
        }

        // Build the final assembled context.
        let mut system = system_prompt.to_string();
        let mut total_tokens = estimate_tokens(&system);
        let mut messages = Vec::new();

        for block in &context_blocks {
            if block.source_id == "history" {
                // History goes as messages, not system prompt.
                for msg in session.history.iter().rev() {
                    let tokens = estimate_tokens(&msg.content);
                    if total_tokens + tokens > available + self.budget.system_reserve {
                        break;
                    }
                    messages.push(msg.clone());
                    total_tokens += tokens;
                }
                messages.reverse();
            } else {
                // Other sources get appended to system prompt.
                system.push_str("\n\n");
                system.push_str(&block.content);
                total_tokens += block.tokens_used;
            }
        }

        debug!(
            sources = context_blocks.len(),
            history_msgs = messages.len(),
            estimated_tokens = total_tokens,
            "pluggable context assembled"
        );

        AssembledContext {
            system_prompt: system,
            messages,
            estimated_tokens: total_tokens,
        }
    }

    /// Build context from conversation history and optional vector search results.
    ///
    /// Uses a greedy approach: recent messages have priority, then vector results
    /// fill remaining budget.
    ///
    /// **Preserved for backwards compatibility.** New callers should prefer
    /// `assemble_pluggable()` with registered `ContextSource` implementations.
    pub fn assemble(
        &self,
        system_prompt: &str,
        history: &[ChatMessage],
        vector_results: &[String],
    ) -> AssembledContext {
        let available = self.budget.max_tokens
            - self.budget.system_reserve
            - self.budget.response_reserve;

        let vector_budget = (available as f32 * self.budget.vector_ratio) as usize;
        let history_budget = available - vector_budget;

        // Build messages from history (most recent first, then reverse)
        let mut messages = Vec::new();
        let mut used_tokens = 0;

        for msg in history.iter().rev() {
            let tokens = estimate_tokens(&msg.content);
            if used_tokens + tokens > history_budget {
                break;
            }
            messages.push(msg.clone());
            used_tokens += tokens;
        }
        messages.reverse();

        // Inject vector search results as a system context addendum
        let mut system = system_prompt.to_string();
        if !vector_results.is_empty() {
            let mut context_addition = String::from("\n\n<relevant_context>\n");
            let mut vector_used = 0;
            for result in vector_results {
                let tokens = estimate_tokens(result);
                if vector_used + tokens > vector_budget {
                    break;
                }
                context_addition.push_str(result);
                context_addition.push('\n');
                vector_used += tokens;
            }
            context_addition.push_str("</relevant_context>");
            system.push_str(&context_addition);
            used_tokens += vector_used;
        }

        let total_tokens = used_tokens + estimate_tokens(&system);
        debug!(
            history_msgs = messages.len(),
            vector_results = vector_results.len(),
            estimated_tokens = total_tokens,
            "context assembled"
        );

        AssembledContext {
            system_prompt: system,
            messages,
            estimated_tokens: total_tokens,
        }
    }
}
