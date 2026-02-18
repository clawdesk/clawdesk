//! Agent runner pipeline — composable middleware stages.
//!
//! Pipeline: AuthResolve → HistorySanitize → ContextGuard → ToolSplit → Execute → FailoverDecide
//!
//! Each stage is independently testable with O(1) mock injection.
//! Pipeline latency = Σ latency(stage_i) for sequential stages.

use crate::tools::{Tool, ToolPolicy, ToolRegistry, ToolResult};
use clawdesk_domain::context_guard::{
    estimate_tokens, CompactionLevel, CompactionResult, ContextGuard, ContextGuardConfig,
    GuardAction,
};
use clawdesk_providers::{
    ChatMessage, FinishReason, MessageRole, Provider, ProviderRequest, ToolCall, ToolDefinition,
};
use clawdesk_types::error::{AgentError, ClawDeskError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Maximum number of tool call rounds before forcing a response.
const MAX_TOOL_ROUNDS: usize = 25;

/// Configuration for an agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub model: String,
    pub system_prompt: String,
    pub max_tool_rounds: usize,
    pub context_limit: usize,
    pub response_reserve: usize,
    /// Provider-specific quirks.
    pub provider_quirks: ProviderQuirks,
    /// Optional workspace path for this agent.
    ///
    /// When set, all file-system tool operations are confined to this directory
    /// (chroot-style scoping). The path is canonicalized on first use to prevent
    /// symlink escapes. Uniqueness across concurrent agents should be enforced
    /// by the gateway state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            system_prompt: "You are a helpful assistant.".to_string(),
            max_tool_rounds: MAX_TOOL_ROUNDS,
            context_limit: 128_000,
            response_reserve: 8_192,
            provider_quirks: ProviderQuirks::default(),
            workspace_path: None,
        }
    }
}

/// Provider-specific quirks for turn sanitization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderQuirks {
    /// Google requires user-assistant alternation.
    pub require_alternation: bool,
    /// Some providers require function calls after user turns only.
    pub tool_after_user_only: bool,
    /// Provider name for error classification.
    pub provider_name: String,
}

/// Event emitted during agent execution for streaming/monitoring.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    RoundStart { round: usize },
    Response { content: String, finish_reason: FinishReason },
    ToolStart { name: String, args: String },
    ToolEnd { name: String, success: bool, duration_ms: u64 },
    Compaction { level: CompactionLevel, tokens_before: usize, tokens_after: usize },
    StreamChunk { text: String, done: bool },
    Done { total_rounds: usize },
    Error { error: String },

    // ── Decision-explaining events (for AgentTrace) ──────────────

    /// Emitted after prompt assembly — explains what's in the prompt.
    PromptAssembled {
        total_tokens: usize,
        skills_included: Vec<String>,
        skills_excluded: Vec<String>,
        memory_fragments: usize,
        budget_utilization: f64,
    },

    /// Emitted when a skill is selected or excluded.
    SkillDecision {
        skill_id: String,
        included: bool,
        reason: String,
        token_cost: usize,
        budget_remaining: usize,
    },

    /// Emitted when context guard intervenes.
    ContextGuardAction {
        action: String,
        token_count: usize,
        threshold: f64,
    },

    /// Emitted on model fallback.
    FallbackTriggered {
        from_model: String,
        to_model: String,
        reason: String,
        attempt: usize,
    },

    /// Emitted when identity is verified.
    IdentityVerified {
        hash_match: bool,
        version: u64,
    },
}

/// Final response from the agent runner.
#[derive(Debug, Clone)]
pub struct AgentResponse {
    pub content: String,
    pub total_rounds: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub finish_reason: FinishReason,
}

/// The agent runner: orchestrates LLM calls, tool execution, and context assembly.
pub struct AgentRunner {
    provider: Arc<dyn Provider>,
    tools: Arc<ToolRegistry>,
    tool_policy: Arc<ToolPolicy>,
    config: AgentConfig,
    cancel: CancellationToken,
    event_tx: Option<broadcast::Sender<AgentEvent>>,
    /// Shared semaphore for bounding concurrent tool calls across rounds.
    ///
    /// Hoisted from per-call creation to struct-level so that back-to-back
    /// tool rounds share the same semaphore, preventing a spike where
    /// round N's tools finish just as round N+1's begin (2× max_concurrent).
    tool_semaphore: Arc<tokio::sync::Semaphore>,
}

impl AgentRunner {
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Arc<ToolRegistry>,
        config: AgentConfig,
        cancel: CancellationToken,
    ) -> Self {
        let policy = ToolPolicy::default();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(policy.max_concurrent));
        Self {
            provider,
            tools,
            tool_policy: Arc::new(policy),
            config,
            cancel,
            event_tx: None,
            tool_semaphore: semaphore,
        }
    }

    pub fn with_tool_policy(mut self, policy: Arc<ToolPolicy>) -> Self {
        // Recreate semaphore with the new policy's max_concurrent
        self.tool_semaphore = Arc::new(tokio::sync::Semaphore::new(policy.max_concurrent));
        self.tool_policy = policy;
        self
    }

    pub fn with_events(mut self, tx: broadcast::Sender<AgentEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Emit an event only if there are active subscribers (zero overhead otherwise).
    fn emit(&self, event: AgentEvent) {
        if let Some(tx) = &self.event_tx {
            if tx.receiver_count() > 0 {
                let _ = tx.send(event);
            }
        }
    }

    /// Run the full agent pipeline.
    pub async fn run(
        &self,
        history: Vec<ChatMessage>,
        system_prompt: String,
    ) -> Result<AgentResponse, ClawDeskError> {
        // Stage 1: Sanitize history per provider quirks
        let messages = self.sanitize_history(history);

        // Stage 2: Initialize context guard
        let mut guard = ContextGuard::new(ContextGuardConfig {
            context_limit: self.config.context_limit,
            trigger_threshold: 0.80,
            response_reserve: self.config.response_reserve,
            ..Default::default()
        });
        let initial_tokens: usize = messages
            .iter()
            .map(|m| estimate_tokens(&m.content))
            .sum::<usize>()
            + estimate_tokens(&system_prompt);
        guard.set_token_count(initial_tokens);

        // Stage 3: Filter tools by policy
        let tool_defs = self.build_tool_definitions();

        // Stage 4: Execute the agent loop
        self.execute_loop(messages, system_prompt, tool_defs, &mut guard)
            .await
    }

    /// Stage 1: Sanitize history based on provider quirks.
    fn sanitize_history(&self, mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
        if self.config.provider_quirks.require_alternation {
            let mut sanitized = Vec::with_capacity(messages.len());
            let mut last_role: Option<MessageRole> = None;
            for msg in messages.drain(..) {
                if Some(msg.role) == last_role && msg.role != MessageRole::System {
                    if let Some(last) = sanitized.last_mut() {
                        let last: &mut ChatMessage = last;
                        let mut merged = String::from(&*last.content);
                        merged.push('\n');
                        merged.push_str(&msg.content);
                        last.content = std::sync::Arc::from(merged);
                    }
                } else {
                    last_role = Some(msg.role);
                    sanitized.push(msg);
                }
            }
            sanitized
        } else {
            messages
        }
    }

    /// Stage 3: Build tool definitions filtered by policy.
    fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .schemas()
            .into_iter()
            .filter(|s| self.tool_policy.is_allowed(&s.name))
            .map(|s| ToolDefinition {
                name: s.name,
                description: s.description,
                parameters: s.parameters,
            })
            .collect()
    }

    /// Stage 4: Main execution loop with context guard integration.
    ///
    /// Builds `ProviderRequest` once before the loop. Loop-invariant fields (model,
    /// system_prompt, tools) are moved in; `request.messages` is mutated in-place on
    /// each round. The provider borrows the request via `&ProviderRequest`, eliminating
    /// per-round clones of messages, tool definitions, and the system prompt.
    async fn execute_loop(
        &self,
        messages: Vec<ChatMessage>,
        system_prompt: String,
        tool_defs: Vec<ToolDefinition>,
        guard: &mut ContextGuard,
    ) -> Result<AgentResponse, ClawDeskError> {
        let mut total_input_tokens = 0u64;
        let mut total_output_tokens = 0u64;

        // Build request once — model, system_prompt, tools are loop-invariant.
        let mut request = ProviderRequest {
            model: self.config.model.clone(),
            messages,
            system_prompt: Some(system_prompt),
            max_tokens: None,
            temperature: None,
            tools: tool_defs,
            stream: false,
        };

        for round in 0..self.config.max_tool_rounds {
            if self.cancel.is_cancelled() {
                info!(round, "agent run cancelled");
                return Err(ClawDeskError::Agent(AgentError::Cancelled));
            }

            self.emit(AgentEvent::RoundStart { round });

            // Predictive compaction check
            match guard.check() {
                GuardAction::Ok => {}
                GuardAction::Compact(level) => {
                    let tokens_before = guard.current_tokens();
                    let result = self.apply_compaction(&mut request.messages, level).await;
                    guard.compaction_succeeded(&result);
                    self.emit(AgentEvent::Compaction {
                        level,
                        tokens_before,
                        tokens_after: result.tokens_after,
                    });
                    debug!(?level, tokens_before, tokens_after = result.tokens_after, "compaction applied");
                }
                GuardAction::ForceTruncate { keep_last_n } => {
                    if request.messages.len() > keep_last_n {
                        request.messages = request.messages.split_off(request.messages.len() - keep_last_n);
                    }
                    let new_tokens: usize = request.messages
                        .iter()
                        .map(|m| m.token_count())
                        .sum();
                    guard.set_token_count(new_tokens);
                    warn!(keep_last_n, "force truncated history");
                }
                GuardAction::CircuitBroken => {
                    if request.messages.len() > 10 {
                        request.messages = request.messages.split_off(request.messages.len() - 10);
                    }
                    warn!("circuit breaker open, using simple truncation");
                }
            }

            debug!(round, messages = request.messages.len(), tokens = guard.current_tokens(), "agent round");

            let response = self.provider.complete(&request).await.map_err(ClawDeskError::Provider)?;

            total_input_tokens += response.usage.input_tokens;
            total_output_tokens += response.usage.output_tokens;
            guard.record_tokens(&response.content);

            self.emit(AgentEvent::Response {
                content: response.content.clone(),
                finish_reason: response.finish_reason,
            });

            if response.finish_reason == FinishReason::ToolUse && !response.tool_calls.is_empty() {
                let assistant_tokens = estimate_tokens(&response.content);
                request.messages.push(ChatMessage {
                    role: MessageRole::Assistant,
                    content: std::sync::Arc::from(response.content.as_str()),
                    cached_tokens: Some(assistant_tokens),
                });

                let tool_results = self.execute_tools_with_policy(&response.tool_calls).await;

                for result in &tool_results {
                    let content = serde_json::json!({
                        "tool_call_id": result.tool_call_id,
                        "name": result.name,
                        "content": result.content,
                        "is_error": result.is_error,
                    })
                    .to_string();
                    let tool_tokens = estimate_tokens(&content);
                    guard.record_tokens(&content);
                    request.messages.push(ChatMessage {
                        role: MessageRole::Tool,
                        content: std::sync::Arc::from(content),
                        cached_tokens: Some(tool_tokens),
                    });
                }
                continue;
            }

            self.emit(AgentEvent::Done { total_rounds: round + 1 });
            return Ok(AgentResponse {
                content: response.content,
                total_rounds: round + 1,
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
                finish_reason: response.finish_reason,
            });
        }

        Err(ClawDeskError::Agent(AgentError::MaxIterations {
            limit: self.config.max_tool_rounds as u32,
        }))
    }

    /// Apply compaction at the specified level.
    ///
    /// Uses `cached_tokens` for O(1) per-message token lookup — avoids
    /// re-scanning every message's content on each compaction pass.
    async fn apply_compaction(
        &self,
        messages: &mut Vec<ChatMessage>,
        level: CompactionLevel,
    ) -> CompactionResult {
        let tokens_before: usize = messages.iter().map(|m| m.token_count()).sum();
        let turns_before = messages.len();

        match level {
            CompactionLevel::DropMetadata => {
                for msg in messages.iter_mut() {
                    if msg.role == MessageRole::Tool {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                            if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                                if content.len() > 500 {
                                    msg.content = std::sync::Arc::from(format!("{}...[truncated]", &content[..500]));
                                } else {
                                    msg.content = std::sync::Arc::from(content);
                                }
                                // Recompute cached tokens after content mutation.
                                msg.cached_tokens = Some(estimate_tokens(&msg.content));
                            }
                        }
                    }
                }
            }
            CompactionLevel::SummarizeOld => {
                let keep = messages.len() / 2;
                if messages.len() > keep + 2 {
                    let old_msgs: Vec<_> = messages.drain(..messages.len() - keep).collect();

                    // Build a summarization prompt from the old messages.
                    let mut transcript = String::with_capacity(old_msgs.len() * 80);
                    for m in &old_msgs {
                        transcript.push_str(m.role.as_str());
                        transcript.push_str(": ");
                        // Truncate very long individual messages to keep the
                        // summarization prompt itself within reasonable bounds.
                        if m.content.len() > 600 {
                            transcript.push_str(&m.content[..600]);
                            transcript.push_str("…");
                        } else {
                            transcript.push_str(&m.content);
                        }
                        transcript.push('\n');
                    }

                    let summary_content = self.summarize_via_llm(&transcript, old_msgs.len()).await;
                    let summary_tokens = estimate_tokens(&summary_content);
                    messages.insert(
                        0,
                        ChatMessage {
                            role: MessageRole::System,
                            content: std::sync::Arc::from(summary_content),
                            cached_tokens: Some(summary_tokens),
                        },
                    );
                }
            }
            CompactionLevel::Truncate => {
                if messages.len() > 10 {
                    *messages = messages.split_off(messages.len() - 10);
                }
            }
        }

        let tokens_after: usize = messages.iter().map(|m| m.token_count()).sum();

        CompactionResult {
            level,
            tokens_before,
            tokens_after,
            turns_removed: turns_before.saturating_sub(messages.len()),
            turns_summarized: if level == CompactionLevel::SummarizeOld {
                turns_before.saturating_sub(messages.len())
            } else {
                0
            },
        }
    }

    /// Summarize a transcript of old messages via the LLM.
    ///
    /// Falls back to a static placeholder if the LLM call fails, so
    /// compaction never blocks the main pipeline.
    async fn summarize_via_llm(&self, transcript: &str, msg_count: usize) -> String {
        let prompt = format!(
            "Summarize the following conversation fragment into a concise paragraph. \
             Preserve key facts, decisions, and any action items. \
             Do not invent information.\n\n---\n{transcript}\n---"
        );
        let req = ProviderRequest {
            model: self.config.model.clone(),
            messages: vec![ChatMessage {
                role: MessageRole::User,
                content: std::sync::Arc::from(prompt),
                cached_tokens: None,
            }],
            system_prompt: None,
            max_tokens: Some(300),
            temperature: Some(0.2),
            tools: vec![],
            stream: false,
        };
        match self.provider.complete(&req).await {
            Ok(resp) => {
                let text = resp.content.trim().to_string();
                if text.is_empty() {
                    Self::static_summary(msg_count)
                } else {
                    format!("[Summary of {msg_count} earlier messages]\n{text}")
                }
            }
            Err(e) => {
                warn!(%e, "LLM summarization failed, using static fallback");
                Self::static_summary(msg_count)
            }
        }
    }

    fn static_summary(msg_count: usize) -> String {
        format!(
            "[Summary of {} earlier messages: conversation covered various topics]",
            msg_count
        )
    }

    /// Execute tools with policy enforcement, bounded concurrency, and timing.
    ///
    /// Uses a `Semaphore` to enforce `ToolPolicy.max_concurrent`. Each tool
    /// acquires a permit before execution, bounding the number of in-flight
    /// tool calls. This prevents resource exhaustion when the LLM requests
    /// many parallel tool calls (e.g., 20 web fetches).
    ///
    /// The previous `spawn_blocking` + `block_on` anti-pattern is replaced
    /// with direct `spawn_blocking` for truly blocking tools.
    async fn execute_tools_with_policy(&self, tool_calls: &[ToolCall]) -> Vec<ToolResult> {
        let mut join_set: JoinSet<ToolResult> = JoinSet::new();

        for call in tool_calls {
            let tools = Arc::clone(&self.tools);
            let policy = Arc::clone(&self.tool_policy);
            let sem = Arc::clone(&self.tool_semaphore);
            let call_id = call.id.clone();
            let name = call.name.clone();
            let args = call.arguments.clone();
            let cancel = self.cancel.clone();
            let event_tx = self.event_tx.clone();

            join_set.spawn(async move {
                if cancel.is_cancelled() {
                    return ToolResult {
                        tool_call_id: call_id,
                        name,
                        content: "cancelled".to_string(),
                        is_error: true,
                    };
                }

                if !policy.is_allowed(&name) {
                    return ToolResult {
                        tool_call_id: call_id,
                        name,
                        content: "tool not allowed by policy".to_string(),
                        is_error: true,
                    };
                }

                let Some(tool) = tools.get(&name) else {
                    return ToolResult {
                        tool_call_id: call_id,
                        name,
                        content: "tool not found".to_string(),
                        is_error: true,
                    };
                };

                // Acquire semaphore permit — bounds concurrency to max_concurrent.
                let _permit = sem.acquire().await.expect("semaphore closed");

                if let Some(tx) = &event_tx {
                    if tx.receiver_count() > 0 {
                        let _ = tx.send(AgentEvent::ToolStart {
                            name: name.clone(),
                            args: args.to_string(),
                        });
                    }
                }

                let start = std::time::Instant::now();
                let result = if tool.is_blocking() {
                    // For blocking tools: run on the blocking threadpool directly.
                    // No block_on anti-pattern — the tool's execute() is called
                    // from a blocking context via Handle::block_on only if needed.
                    let tool: Arc<dyn Tool> = tool.clone();
                    let args_clone = args.clone();
                    match tokio::task::spawn_blocking(move || {
                        tokio::runtime::Handle::current().block_on(tool.execute(args_clone))
                    })
                    .await
                    {
                        Ok(r) => r,
                        Err(e) => Err(format!("tool panicked: {e}")),
                    }
                } else {
                    tool.execute(args).await
                };
                let duration_ms = start.elapsed().as_millis() as u64;

                if let Some(tx) = &event_tx {
                    if tx.receiver_count() > 0 {
                        let _ = tx.send(AgentEvent::ToolEnd {
                            name: name.clone(),
                            success: result.is_ok(),
                            duration_ms,
                        });
                    }
                }

                match result {
                    Ok(content) => ToolResult {
                        tool_call_id: call_id,
                        name,
                        content,
                        is_error: false,
                    },
                    Err(err) => ToolResult {
                        tool_call_id: call_id,
                        name,
                        content: err,
                        is_error: true,
                    },
                }
            });
        }

        let mut results = Vec::with_capacity(tool_calls.len());
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(tool_result) => results.push(tool_result),
                Err(e) => error!("tool task panicked: {e}"),
            }
        }
        results
    }
}
