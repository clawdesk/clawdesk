//! Context assembly — builds the context window for LLM calls.
//!
//! Combines conversation history, vector search results, and system prompts
//! into a context payload that fits within the model's token budget.

use clawdesk_providers::ChatMessage;
use clawdesk_types::estimate_tokens;
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
