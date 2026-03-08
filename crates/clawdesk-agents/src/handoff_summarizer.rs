//! # Handoff Summarizer — LLM-summarized context handoff for sub-agent spawns.
//!
//! Before spawning a child agent, the parent's conversation history is
//! compressed into a focused summary (max 10 lines, 700 chars) retaining
//! only details relevant to the child's specific task. This eliminates
//! the "cold start" problem.
//!
//! ## Compression
//!
//! Input: parent conversation (potentially thousands of tokens).
//! Output: ~200 tokens focused summary.
//! Compression ratio: 10-100× typical.
//!
//! ## Break-even Analysis
//!
//! Cost: single LLM call for summary.
//! Benefit: saves k additional rounds of re-establishing context.
//! For k ≥ 1 (common case), summarization pays for itself.

use async_trait::async_trait;
use clawdesk_providers::{ChatMessage, MessageRole, Provider, ProviderRequest};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Maximum lines in a handoff summary.
const SUMMARY_MAX_LINES: usize = 10;

/// Maximum characters in a handoff summary.
const SUMMARY_MAX_CHARS: usize = 700;

// ═══════════════════════════════════════════════════════════════════════════
// Summarizer trait
// ═══════════════════════════════════════════════════════════════════════════

/// Trait for generating context handoff summaries.
///
/// Async to allow both LLM-based (production) and deterministic (testing)
/// implementations.
#[async_trait]
pub trait HandoffSummarizer: Send + Sync + 'static {
    /// Generate a focused summary of the parent context relevant to the child task.
    ///
    /// Returns `None` if the parent context has no information relevant to
    /// the child task (equivalent to pi-mono's "NONE" response).
    async fn summarize(
        &self,
        parent_context: &[ChatMessage],
        child_task: &str,
    ) -> Option<String>;
}

// ═══════════════════════════════════════════════════════════════════════════
// LLM-based summarizer (production)
// ═══════════════════════════════════════════════════════════════════════════

/// LLM-powered handoff summarizer.
///
/// Serializes the parent conversation, sends it with the child task to the LLM
/// with a specialized system prompt, and produces a focused summary.
pub struct LlmHandoffSummarizer {
    provider: Arc<dyn Provider>,
    model: String,
}

impl LlmHandoffSummarizer {
    pub fn new(provider: Arc<dyn Provider>, model: String) -> Self {
        Self { provider, model }
    }
}

#[async_trait]
impl HandoffSummarizer for LlmHandoffSummarizer {
    async fn summarize(
        &self,
        parent_context: &[ChatMessage],
        child_task: &str,
    ) -> Option<String> {
        if parent_context.is_empty() {
            return None;
        }

        // Serialize parent conversation (truncate individual messages for budget)
        let serialized = serialize_conversation(parent_context, 4000);
        if serialized.is_empty() {
            return None;
        }

        let system_prompt = format!(
            "You are writing a minimal handoff summary for a background coding agent. \
             The parent agent is spawning a child agent to handle the following task:\n\n\
             CHILD TASK: {}\n\n\
             Use the parent conversation below only as context. Include only details that are \
             directly relevant to the child task. If no details from the parent conversation are \
             relevant, respond with exactly \"NONE\".\n\n\
             Rules:\n\
             - Maximum {} lines\n\
             - Maximum {} characters\n\
             - Focus on: decisions already made, constraints established, relevant files/paths, \
               key facts the child needs\n\
             - Do NOT include general conversation, greetings, or irrelevant context\n\
             - Be extremely concise",
            child_task, SUMMARY_MAX_LINES, SUMMARY_MAX_CHARS
        );

        let request = ProviderRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: MessageRole::User,
                content: Arc::from(format!(
                    "Parent conversation:\n---\n{}\n---\n\nGenerate the handoff summary.",
                    serialized
                )),
                cached_tokens: None,
            }],
            system_prompt: Some(system_prompt),
            max_tokens: Some(200),
            temperature: Some(0.2),
            tools: vec![],
            stream: false,
        };

        match self.provider.complete(&request).await {
            Ok(response) => {
                let summary = response.content.trim().to_string();

                // Check for "NONE" or near-empty responses
                if summary.is_empty()
                    || summary.eq_ignore_ascii_case("none")
                    || summary.eq_ignore_ascii_case("n/a")
                    || summary.len() < 10
                {
                    debug!("handoff summary was trivial — skipping injection");
                    return None;
                }

                // Enforce character limit
                let summary = if summary.len() > SUMMARY_MAX_CHARS {
                    let mut end = SUMMARY_MAX_CHARS;
                    while end > 0 && !summary.is_char_boundary(end) {
                        end -= 1;
                    }
                    summary[..end].to_string()
                } else {
                    summary
                };

                // Enforce line limit
                let lines: Vec<&str> = summary.lines().take(SUMMARY_MAX_LINES).collect();
                let summary = lines.join("\n");

                info!(
                    chars = summary.len(),
                    lines = lines.len(),
                    "generated handoff summary"
                );

                Some(summary)
            }
            Err(e) => {
                warn!(error = %e, "handoff summarization failed — child starts cold");
                None
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Static summarizer (testing/fallback)
// ═══════════════════════════════════════════════════════════════════════════

/// Deterministic summarizer for testing — returns a fixed summary.
pub struct StaticHandoffSummarizer {
    summary: Option<String>,
}

impl StaticHandoffSummarizer {
    /// Create a summarizer that always returns the given summary.
    pub fn with_summary(summary: impl Into<String>) -> Self {
        Self {
            summary: Some(summary.into()),
        }
    }

    /// Create a summarizer that always returns None (no relevant context).
    pub fn none() -> Self {
        Self { summary: None }
    }
}

#[async_trait]
impl HandoffSummarizer for StaticHandoffSummarizer {
    async fn summarize(
        &self,
        _parent_context: &[ChatMessage],
        _child_task: &str,
    ) -> Option<String> {
        self.summary.clone()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Kickoff prompt builder
// ═══════════════════════════════════════════════════════════════════════════

/// Build the complete kickoff prompt for a child agent, including
/// optional parent context summary.
///
/// If the summarizer produces a non-trivial summary, it's prepended
/// under a `## Relevant parent context` section.
pub async fn build_kickoff_prompt(
    summarizer: &dyn HandoffSummarizer,
    parent_context: &[ChatMessage],
    child_task: &str,
    child_system_prompt: &str,
) -> String {
    let summary = summarizer.summarize(parent_context, child_task).await;

    match summary {
        Some(summary) => {
            format!(
                "{}\n\n## Relevant parent context\n\n{}\n\n## Your task\n\n{}",
                child_system_prompt, summary, child_task
            )
        }
        None => {
            format!("{}\n\n## Your task\n\n{}", child_system_prompt, child_task)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Serialize a conversation into a string, truncating each message
/// to keep the total within a character budget.
fn serialize_conversation(messages: &[ChatMessage], max_total_chars: usize) -> String {
    let per_msg_budget = if messages.is_empty() {
        max_total_chars
    } else {
        max_total_chars / messages.len()
    };

    let mut output = String::with_capacity(max_total_chars);

    for msg in messages {
        let role = msg.role.as_str();
        let content = if msg.content.len() > per_msg_budget {
            let mut end = per_msg_budget;
            while end > 0 && !msg.content.is_char_boundary(end) {
                end -= 1;
            }
            &msg.content[..end]
        } else {
            &msg.content
        };

        output.push_str(role);
        output.push_str(": ");
        output.push_str(content);
        output.push('\n');

        if output.len() >= max_total_chars {
            break;
        }
    }

    output
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: MessageRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: Arc::from(content),
            cached_tokens: None,
        }
    }

    #[tokio::test]
    async fn test_static_summarizer_with_summary() {
        let summarizer = StaticHandoffSummarizer::with_summary("Parent found file X is relevant");
        let context = vec![
            make_msg(MessageRole::User, "Find the config file"),
            make_msg(MessageRole::Assistant, "Found config.toml"),
        ];

        let summary = summarizer.summarize(&context, "update the config").await;
        assert_eq!(summary.as_deref(), Some("Parent found file X is relevant"));
    }

    #[tokio::test]
    async fn test_static_summarizer_none() {
        let summarizer = StaticHandoffSummarizer::none();
        let result = summarizer.summarize(&[], "task").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_build_kickoff_with_summary() {
        let summarizer = StaticHandoffSummarizer::with_summary("Config is at /etc/app.toml");
        let context = vec![make_msg(MessageRole::User, "Find config")];

        let prompt = build_kickoff_prompt(
            &summarizer,
            &context,
            "Update the database settings",
            "You are a coding agent",
        )
        .await;

        assert!(prompt.contains("Relevant parent context"));
        assert!(prompt.contains("Config is at /etc/app.toml"));
        assert!(prompt.contains("Update the database settings"));
    }

    #[tokio::test]
    async fn test_build_kickoff_without_summary() {
        let summarizer = StaticHandoffSummarizer::none();

        let prompt = build_kickoff_prompt(
            &summarizer,
            &[],
            "Do something new",
            "You are a coding agent",
        )
        .await;

        assert!(!prompt.contains("Relevant parent context"));
        assert!(prompt.contains("Your task"));
        assert!(prompt.contains("Do something new"));
    }

    #[test]
    fn test_serialize_conversation() {
        let messages = vec![
            make_msg(MessageRole::User, "Hello"),
            make_msg(MessageRole::Assistant, "Hi there, how can I help?"),
            make_msg(MessageRole::User, "Write a function"),
        ];

        let serialized = serialize_conversation(&messages, 1000);
        assert!(serialized.contains("user: Hello"));
        assert!(serialized.contains("assistant: Hi there"));
    }

    #[test]
    fn test_serialize_truncates() {
        let long_msg = "x".repeat(5000);
        let messages = vec![
            make_msg(MessageRole::User, &long_msg),
            make_msg(MessageRole::Assistant, &long_msg),
        ];

        let serialized = serialize_conversation(&messages, 200);
        assert!(serialized.len() <= 300); // Some overhead for role prefixes
    }
}
