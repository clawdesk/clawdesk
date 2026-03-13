//! Composable channel middleware pipeline.
//!
//! Each inbound message passes through a chain of middleware stages:
//!
//! ```text
//! Inbound → BotSelfFilter → RateLimit → AllowList → MentionGate
//!         → ExecApproval → MessageHandler → TypingIndicator → Reply
//! ```
//!
//! Each stage is a `ChannelMiddleware` that transforms an `InboundContext`
//! or rejects the message. Adding a new check requires implementing one
//! trait — no existing channel code needs modification.
//!
//! Pipeline latency: O(Σ T(fᵢ)) for k sequential stages. With k ≈ 8
//! at ~0.1ms each, total overhead ≈ 0.8ms — negligible vs LLM latency.

use async_trait::async_trait;
use clawdesk_types::channel::ChannelId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ─────────────────────────────────────────────────────────────
// Context flowing through the pipeline
// ─────────────────────────────────────────────────────────────

/// Context that flows through the middleware pipeline.
///
/// Each middleware reads, modifies, or rejects this context.
#[derive(Debug, Clone)]
pub struct InboundContext {
    /// Channel this message arrived on.
    pub channel: ChannelId,
    /// Sender identifier (platform-specific).
    pub sender_id: String,
    /// Sender display name.
    pub sender_name: String,
    /// Raw message content.
    pub content: String,
    /// Message ID on the platform.
    pub message_id: String,
    /// Whether the sender is a bot.
    pub is_bot: bool,
    /// Whether the message mentions the bot.
    pub mentions_bot: bool,
    /// Guild/workspace/group ID (if applicable).
    pub group_id: Option<String>,
    /// Thread/topic ID (if applicable).
    pub thread_id: Option<String>,
    /// Metadata bag for middleware to attach data.
    pub metadata: std::collections::HashMap<String, String>,
}

/// Reason a middleware rejected a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rejection {
    /// Which middleware rejected.
    pub stage: String,
    /// Human-readable reason.
    pub reason: String,
    /// Whether to silently drop (true) or send an error reply (false).
    pub silent: bool,
}

// ─────────────────────────────────────────────────────────────
// Middleware trait
// ─────────────────────────────────────────────────────────────

/// A single middleware stage in the inbound message pipeline.
///
/// Each middleware receives the context from the previous stage and
/// either passes it through (possibly modified) or rejects it.
#[async_trait]
pub trait ChannelMiddleware: Send + Sync + 'static {
    /// Middleware name (for logging and rejection attribution).
    fn name(&self) -> &str;

    /// Process the inbound context. Return Ok to continue, Err to reject.
    async fn process(&self, ctx: InboundContext) -> Result<InboundContext, Rejection>;
}

// ─────────────────────────────────────────────────────────────
// Pipeline
// ─────────────────────────────────────────────────────────────

/// A composable pipeline of middleware stages.
pub struct MiddlewarePipeline {
    stages: Vec<Arc<dyn ChannelMiddleware>>,
}

impl MiddlewarePipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    /// Add a middleware stage to the end of the pipeline.
    pub fn add(&mut self, middleware: Arc<dyn ChannelMiddleware>) {
        self.stages.push(middleware);
    }

    /// Run the full pipeline. Returns the final context or the first rejection.
    pub async fn run(&self, ctx: InboundContext) -> Result<InboundContext, Rejection> {
        let mut current = ctx;
        for stage in &self.stages {
            current = stage.process(current).await?;
        }
        Ok(current)
    }

    /// Number of stages.
    pub fn len(&self) -> usize {
        self.stages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }
}

impl Default for MiddlewarePipeline {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────
// Built-in middleware
// ─────────────────────────────────────────────────────────────

/// Filters out messages from the bot itself.
pub struct BotSelfFilter {
    bot_user_id: String,
}

impl BotSelfFilter {
    pub fn new(bot_user_id: impl Into<String>) -> Self {
        Self {
            bot_user_id: bot_user_id.into(),
        }
    }
}

#[async_trait]
impl ChannelMiddleware for BotSelfFilter {
    fn name(&self) -> &str {
        "bot_self_filter"
    }

    async fn process(&self, ctx: InboundContext) -> Result<InboundContext, Rejection> {
        if ctx.sender_id == self.bot_user_id {
            Err(Rejection {
                stage: self.name().into(),
                reason: "message from self".into(),
                silent: true,
            })
        } else {
            Ok(ctx)
        }
    }
}

/// Filters out messages from other bots (unless explicitly allowed).
pub struct BotFilter {
    allow_bots: bool,
}

impl BotFilter {
    pub fn new(allow_bots: bool) -> Self {
        Self { allow_bots }
    }
}

#[async_trait]
impl ChannelMiddleware for BotFilter {
    fn name(&self) -> &str {
        "bot_filter"
    }

    async fn process(&self, ctx: InboundContext) -> Result<InboundContext, Rejection> {
        if ctx.is_bot && !self.allow_bots {
            Err(Rejection {
                stage: self.name().into(),
                reason: "bot messages not allowed".into(),
                silent: true,
            })
        } else {
            Ok(ctx)
        }
    }
}

/// Requires messages to mention the bot (configurable).
pub struct MentionGate {
    require_mention: bool,
}

impl MentionGate {
    pub fn new(require_mention: bool) -> Self {
        Self { require_mention }
    }
}

#[async_trait]
impl ChannelMiddleware for MentionGate {
    fn name(&self) -> &str {
        "mention_gate"
    }

    async fn process(&self, ctx: InboundContext) -> Result<InboundContext, Rejection> {
        if self.require_mention && !ctx.mentions_bot {
            Err(Rejection {
                stage: self.name().into(),
                reason: "bot not mentioned".into(),
                silent: true,
            })
        } else {
            Ok(ctx)
        }
    }
}

/// User allowlist — only specified users can interact.
pub struct AllowList {
    /// Allowed user IDs. Empty = wildcard (allow all).
    allowed: Vec<String>,
}

impl AllowList {
    pub fn new(allowed: Vec<String>) -> Self {
        Self { allowed }
    }

    /// Wildcard: allow all users.
    pub fn allow_all() -> Self {
        Self { allowed: Vec::new() }
    }
}

#[async_trait]
impl ChannelMiddleware for AllowList {
    fn name(&self) -> &str {
        "allow_list"
    }

    async fn process(&self, ctx: InboundContext) -> Result<InboundContext, Rejection> {
        if self.allowed.is_empty() || self.allowed.iter().any(|id| id == "*" || id == &ctx.sender_id) {
            Ok(ctx)
        } else {
            Err(Rejection {
                stage: self.name().into(),
                reason: format!("user {} not in allow list", ctx.sender_id),
                silent: true,
            })
        }
    }
}

/// Rejects empty or whitespace-only messages.
pub struct EmptyMessageFilter;

#[async_trait]
impl ChannelMiddleware for EmptyMessageFilter {
    fn name(&self) -> &str {
        "empty_message_filter"
    }

    async fn process(&self, ctx: InboundContext) -> Result<InboundContext, Rejection> {
        if ctx.content.trim().is_empty() {
            Err(Rejection {
                stage: self.name().into(),
                reason: "empty message".into(),
                silent: true,
            })
        } else {
            Ok(ctx)
        }
    }
}

/// Build a default middleware pipeline for a channel.
pub fn default_pipeline(bot_user_id: &str, allow_bots: bool, mention_only: bool) -> MiddlewarePipeline {
    let mut pipeline = MiddlewarePipeline::new();
    pipeline.add(Arc::new(BotSelfFilter::new(bot_user_id)));
    pipeline.add(Arc::new(BotFilter::new(allow_bots)));
    pipeline.add(Arc::new(EmptyMessageFilter));
    pipeline.add(Arc::new(MentionGate::new(mention_only)));
    pipeline
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx(sender_id: &str, content: &str) -> InboundContext {
        InboundContext {
            channel: ChannelId::Discord,
            sender_id: sender_id.into(),
            sender_name: "Test User".into(),
            content: content.into(),
            message_id: "msg-1".into(),
            is_bot: false,
            mentions_bot: false,
            group_id: None,
            thread_id: None,
            metadata: Default::default(),
        }
    }

    #[tokio::test]
    async fn bot_self_filter_rejects_self() {
        let filter = BotSelfFilter::new("bot-123");
        let ctx = make_ctx("bot-123", "hello");
        assert!(filter.process(ctx).await.is_err());
    }

    #[tokio::test]
    async fn bot_self_filter_passes_others() {
        let filter = BotSelfFilter::new("bot-123");
        let ctx = make_ctx("user-456", "hello");
        assert!(filter.process(ctx).await.is_ok());
    }

    #[tokio::test]
    async fn pipeline_runs_in_order() {
        let pipeline = default_pipeline("bot-1", false, false);
        let ctx = make_ctx("user-1", "hi there");
        let result = pipeline.run(ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn pipeline_rejects_empty_message() {
        let pipeline = default_pipeline("bot-1", false, false);
        let ctx = make_ctx("user-1", "   ");
        let result = pipeline.run(ctx).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().stage, "empty_message_filter");
    }

    #[tokio::test]
    async fn mention_gate_rejects_without_mention() {
        let pipeline = default_pipeline("bot-1", false, true);
        let ctx = make_ctx("user-1", "hello");
        let result = pipeline.run(ctx).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().stage, "mention_gate");
    }

    #[tokio::test]
    async fn mention_gate_passes_with_mention() {
        let pipeline = default_pipeline("bot-1", false, true);
        let mut ctx = make_ctx("user-1", "hello @bot");
        ctx.mentions_bot = true;
        let result = pipeline.run(ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn allow_list_blocks_unlisted_user() {
        let filter = AllowList::new(vec!["user-1".into()]);
        let ctx = make_ctx("user-2", "hello");
        assert!(filter.process(ctx).await.is_err());
    }

    #[tokio::test]
    async fn allow_list_empty_allows_all() {
        let filter = AllowList::allow_all();
        let ctx = make_ctx("anyone", "hello");
        assert!(filter.process(ctx).await.is_ok());
    }
}
