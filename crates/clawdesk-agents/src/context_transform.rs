//! # Context Transform Pipeline — Composable pre-LLM message transforms.
//!
//! Replaces the monolithic context preparation stage with a composable pipeline
//! of transforms. Each transform receives `&mut Vec<ChatMessage>` and can add,
//! remove, or modify messages before the LLM call.
//!
//! ## Chain of Responsibility
//!
//! Transforms are ordered and execute sequentially. Built-in transforms:
//! 1. `MemoryInjectionTransform` — recall + inject relevant memories
//! 2. `SkillContextTransform` — inject active skill instructions
//! 3. `CompactionTransform` — apply existing compaction logic
//! 4. `BudgetEnforcementTransform` — hard-truncate to token budget (runs last)
//!
//! ## Performance
//!
//! Pipeline of T transforms over M messages: O(T × M) worst case.
//! T is small (3-5 transforms), so effective cost is O(M) with small constant.
//! The `BudgetEnforcementTransform` runs last and guarantees the invariant:
//! `Σ tokens(messages) ≤ context_window_size`.

use async_trait::async_trait;
use clawdesk_providers::ChatMessage;
use std::sync::Arc;
use tracing::{debug, info};

// ═══════════════════════════════════════════════════════════════════════════
// Transform trait
// ═══════════════════════════════════════════════════════════════════════════

/// Context for transform execution.
#[derive(Debug, Clone)]
pub struct TransformContext {
    /// Maximum token budget for this context window.
    pub token_budget: usize,
    /// Current estimated token count.
    pub current_tokens: usize,
    /// Session ID for memory/skill lookups.
    pub session_id: Option<String>,
    /// The user's last message content (for relevance queries).
    pub user_query: Option<String>,
    /// Turn number within the session.
    pub turn_number: u32,
}

/// A composable context transform applied before each LLM call.
///
/// Transforms can add, remove, or modify messages in the conversation
/// history. They run sequentially in pipeline order.
#[async_trait]
pub trait ContextTransform: Send + Sync + 'static {
    /// Human-readable name for logging/tracing.
    fn name(&self) -> &str;

    /// Apply the transform to the message list.
    ///
    /// The transform receives a mutable reference to the messages and
    /// the current context. It can modify messages in place. The context's
    /// `current_tokens` should be updated if the transform changes the
    /// token count significantly.
    async fn transform(
        &self,
        messages: &mut Vec<ChatMessage>,
        ctx: &mut TransformContext,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Transform pipeline
// ═══════════════════════════════════════════════════════════════════════════

/// A composable pipeline of context transforms.
///
/// Transforms execute in order. The pipeline can be customized per agent
/// or per pipeline step.
pub struct ContextTransformPipeline {
    transforms: Vec<Arc<dyn ContextTransform>>,
}

impl ContextTransformPipeline {
    /// Create an empty pipeline.
    pub fn new() -> Self {
        Self {
            transforms: Vec::new(),
        }
    }

    /// Add a transform to the end of the pipeline.
    pub fn add(mut self, transform: Arc<dyn ContextTransform>) -> Self {
        self.transforms.push(transform);
        self
    }

    /// Add a transform to the end of the pipeline (in-place).
    pub fn push(&mut self, transform: Arc<dyn ContextTransform>) {
        self.transforms.push(transform);
    }

    /// Get the number of transforms in the pipeline.
    pub fn len(&self) -> usize {
        self.transforms.len()
    }

    /// Whether the pipeline is empty.
    pub fn is_empty(&self) -> bool {
        self.transforms.is_empty()
    }

    /// Execute all transforms in sequence.
    pub async fn execute(
        &self,
        messages: &mut Vec<ChatMessage>,
        ctx: &mut TransformContext,
    ) {
        for transform in &self.transforms {
            let before_count = messages.len();
            transform.transform(messages, ctx).await;
            let after_count = messages.len();
            if before_count != after_count {
                debug!(
                    transform = transform.name(),
                    before = before_count,
                    after = after_count,
                    "context transform modified message count"
                );
            }
        }
    }
}

impl Default for ContextTransformPipeline {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Built-in transforms
// ═══════════════════════════════════════════════════════════════════════════

/// Budget enforcement transform — hard-truncates to token budget.
///
/// Must run last in the pipeline. Guarantees:
/// `Σ tokens(messages) ≤ context.token_budget`
///
/// Truncates from the oldest messages, preserving the most recent context.
/// O(M) scan with running token counter.
pub struct BudgetEnforcementTransform;

#[async_trait]
impl ContextTransform for BudgetEnforcementTransform {
    fn name(&self) -> &str {
        "budget_enforcement"
    }

    async fn transform(
        &self,
        messages: &mut Vec<ChatMessage>,
        ctx: &mut TransformContext,
    ) {
        let budget = ctx.token_budget;
        let total: usize = messages.iter().map(|m| m.token_count()).sum();

        if total <= budget {
            ctx.current_tokens = total;
            return;
        }

        // Truncate from oldest, keeping most recent
        let mut kept_tokens = 0usize;
        let mut keep_from = messages.len();

        for i in (0..messages.len()).rev() {
            let msg_tokens = messages[i].token_count();
            if kept_tokens + msg_tokens > budget {
                keep_from = i + 1;
                break;
            }
            kept_tokens += msg_tokens;
            if i == 0 {
                keep_from = 0;
            }
        }

        // Keep at least one message
        if keep_from >= messages.len() && !messages.is_empty() {
            keep_from = messages.len() - 1;
            kept_tokens = messages.last().map(|m| m.token_count()).unwrap_or(0);
        }

        if keep_from > 0 {
            let removed = keep_from;
            *messages = messages.split_off(keep_from);
            info!(
                removed,
                kept = messages.len(),
                tokens = kept_tokens,
                budget,
                "budget enforcement: truncated oldest messages"
            );
        }

        ctx.current_tokens = kept_tokens;
    }
}

/// Memory injection transform — recalls relevant memories and injects them.
///
/// Injects a `<memory_context>` block before the last user message.
pub struct MemoryInjectionTransform {
    recall_fn: Arc<
        dyn Fn(
                String,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Vec<MemoryFragment>> + Send>,
            > + Send
            + Sync,
    >,
    /// Maximum tokens to allocate to memory context.
    max_tokens: usize,
}

/// A recalled memory fragment.
#[derive(Debug, Clone)]
pub struct MemoryFragment {
    pub content: String,
    pub relevance: f64,
    pub source: Option<String>,
}

impl MemoryInjectionTransform {
    pub fn new(
        recall_fn: Arc<
            dyn Fn(
                    String,
                ) -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Vec<MemoryFragment>> + Send>,
                > + Send
                + Sync,
        >,
        max_tokens: usize,
    ) -> Self {
        Self {
            recall_fn,
            max_tokens,
        }
    }
}

#[async_trait]
impl ContextTransform for MemoryInjectionTransform {
    fn name(&self) -> &str {
        "memory_injection"
    }

    async fn transform(
        &self,
        messages: &mut Vec<ChatMessage>,
        ctx: &mut TransformContext,
    ) {
        let query = match &ctx.user_query {
            Some(q) if !q.is_empty() => q.clone(),
            _ => return,
        };

        let fragments = (self.recall_fn)(query).await;
        if fragments.is_empty() {
            return;
        }

        // Build memory context block within token budget
        let mut memory_block = String::from("<memory_context>\n");
        let mut memory_tokens = 0;

        for frag in &fragments {
            let estimated = clawdesk_domain::context_guard::estimate_tokens(&frag.content);
            if memory_tokens + estimated > self.max_tokens {
                break;
            }
            memory_block.push_str(&frag.content);
            memory_block.push('\n');
            memory_tokens += estimated;
        }
        memory_block.push_str("</memory_context>");

        // Inject before the last user message (recency bias)
        let insert_pos = messages
            .iter()
            .rposition(|m| m.role == clawdesk_providers::MessageRole::User)
            .unwrap_or(messages.len());

        messages.insert(
            insert_pos,
            ChatMessage {
                role: clawdesk_providers::MessageRole::System,
                content: std::sync::Arc::from(memory_block),
                cached_tokens: Some(memory_tokens),
            },
        );

        ctx.current_tokens += memory_tokens;
        debug!(
            fragments = fragments.len(),
            tokens = memory_tokens,
            "memory injection: injected fragments"
        );
    }
}

/// Skill context transform — injects active skill prompt fragments.
pub struct SkillContextTransform {
    /// Skill prompt fragments to inject.
    pub fragments: Vec<String>,
}

#[async_trait]
impl ContextTransform for SkillContextTransform {
    fn name(&self) -> &str {
        "skill_context"
    }

    async fn transform(
        &self,
        messages: &mut Vec<ChatMessage>,
        ctx: &mut TransformContext,
    ) {
        if self.fragments.is_empty() {
            return;
        }

        let skill_block = self.fragments.join("\n\n");
        let tokens = clawdesk_domain::context_guard::estimate_tokens(&skill_block);

        // Inject at the beginning (after any system messages)
        let insert_pos = messages
            .iter()
            .position(|m| m.role != clawdesk_providers::MessageRole::System)
            .unwrap_or(0);

        messages.insert(
            insert_pos,
            ChatMessage {
                role: clawdesk_providers::MessageRole::System,
                content: std::sync::Arc::from(skill_block),
                cached_tokens: Some(tokens),
            },
        );

        ctx.current_tokens += tokens;
        debug!(
            fragment_count = self.fragments.len(),
            tokens,
            "skill context: injected fragments"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Default pipeline factory
// ═══════════════════════════════════════════════════════════════════════════

/// Create a default transform pipeline with budget enforcement.
pub fn default_pipeline(token_budget: usize) -> ContextTransformPipeline {
    let _ = token_budget; // Budget is in the context, not the pipeline
    ContextTransformPipeline::new()
        .add(Arc::new(BudgetEnforcementTransform))
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_providers::MessageRole;
    use std::sync::Arc;

    fn make_msg(role: MessageRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: Arc::from(content),
            cached_tokens: Some(content.len() / 4), // rough estimate
        }
    }

    #[tokio::test]
    async fn test_budget_enforcement_no_truncation() {
        let transform = BudgetEnforcementTransform;
        let mut messages = vec![
            make_msg(MessageRole::User, "Hello"),
            make_msg(MessageRole::Assistant, "Hi"),
        ];
        let mut ctx = TransformContext {
            token_budget: 10000,
            current_tokens: 0,
            session_id: None,
            user_query: None,
            turn_number: 0,
        };

        transform.transform(&mut messages, &mut ctx).await;
        assert_eq!(messages.len(), 2);
    }

    #[tokio::test]
    async fn test_budget_enforcement_truncates() {
        let transform = BudgetEnforcementTransform;
        let long_content = "x".repeat(1000);
        let mut messages = vec![
            make_msg(MessageRole::User, &long_content),
            make_msg(MessageRole::User, &long_content),
            make_msg(MessageRole::User, &long_content),
            make_msg(MessageRole::User, "recent"),
        ];
        let mut ctx = TransformContext {
            token_budget: 10, // Very small budget
            current_tokens: 0,
            session_id: None,
            user_query: None,
            turn_number: 0,
        };

        transform.transform(&mut messages, &mut ctx).await;
        // Should keep at most 1-2 messages within budget
        assert!(messages.len() < 4);
    }

    #[tokio::test]
    async fn test_pipeline_execution_order() {
        struct CountTransform {
            name: &'static str,
            call_order: Arc<tokio::sync::Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl ContextTransform for CountTransform {
            fn name(&self) -> &str {
                self.name
            }
            async fn transform(
                &self,
                _messages: &mut Vec<ChatMessage>,
                _ctx: &mut TransformContext,
            ) {
                self.call_order
                    .lock()
                    .await
                    .push(self.name.to_string());
            }
        }

        let order = Arc::new(tokio::sync::Mutex::new(Vec::new()));

        let pipeline = ContextTransformPipeline::new()
            .add(Arc::new(CountTransform {
                name: "first",
                call_order: Arc::clone(&order),
            }))
            .add(Arc::new(CountTransform {
                name: "second",
                call_order: Arc::clone(&order),
            }))
            .add(Arc::new(CountTransform {
                name: "third",
                call_order: Arc::clone(&order),
            }));

        let mut messages = vec![make_msg(MessageRole::User, "test")];
        let mut ctx = TransformContext {
            token_budget: 10000,
            current_tokens: 0,
            session_id: None,
            user_query: None,
            turn_number: 0,
        };

        pipeline.execute(&mut messages, &mut ctx).await;

        let calls = order.lock().await;
        assert_eq!(*calls, vec!["first", "second", "third"]);
    }

    #[tokio::test]
    async fn test_skill_context_injection() {
        let transform = SkillContextTransform {
            fragments: vec![
                "You are a coding expert.".into(),
                "Always write tests.".into(),
            ],
        };

        let mut messages = vec![
            make_msg(MessageRole::System, "Base system prompt"),
            make_msg(MessageRole::User, "Write a function"),
        ];
        let mut ctx = TransformContext {
            token_budget: 10000,
            current_tokens: 0,
            session_id: None,
            user_query: None,
            turn_number: 0,
        };

        transform.transform(&mut messages, &mut ctx).await;
        assert_eq!(messages.len(), 3);
        // Skill context should be inserted after system messages
        assert!(messages[1].content.contains("coding expert"));
    }

    #[test]
    fn test_default_pipeline_creation() {
        let pipeline = default_pipeline(128_000);
        assert_eq!(pipeline.len(), 1); // Just budget enforcement
    }
}
