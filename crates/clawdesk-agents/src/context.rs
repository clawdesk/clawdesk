//! Context assembly — builds the context window for LLM calls.
//!
//! Combines conversation history, vector search results, and system prompts
//! into a context payload that fits within the model's token budget.
//!
//! ## Zero-Copy Design
//!
//! `PromptRope` stores prompt fragments as `Cow<'a, str>` references,
//! deferring concatenation until final IO. Context assembly is O(K)
//! where K is the number of fragments, not O(N) total bytes.

use clawdesk_providers::ChatMessage;
use clawdesk_types::estimate_tokens;
use std::borrow::Cow;
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
}

impl ContextAssembler {
    pub fn new(budget: ContextBudget) -> Self {
        Self { budget }
    }

    /// Build context from conversation history and optional vector search results.
    ///
    /// Uses a greedy approach: recent messages have priority, then vector results
    /// fill remaining budget.
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
