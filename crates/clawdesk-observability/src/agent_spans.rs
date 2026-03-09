//! Agent-level OpenTelemetry span conventions.
//!
//! Extends the GenAI semantic conventions with agent-specific attributes
//! following the OpenTelemetry Gen AI Agent specification draft.
//!
//! These attributes are attached to spans representing agent execution:
//! - Agent identity and configuration
//! - Tool invocation tracking
//! - Turn/iteration counting
//! - Quality and safety signals
//!
//! Reference: <https://opentelemetry.io/docs/specs/semconv/gen-ai/>

/// Agent-specific span attribute keys.
pub mod keys {
    // Agent identity
    pub const AGENT_ID: &str = "gen_ai.agent.id";
    pub const AGENT_NAME: &str = "gen_ai.agent.name";
    pub const AGENT_DESCRIPTION: &str = "gen_ai.agent.description";
    pub const AGENT_VERSION: &str = "gen_ai.agent.version";

    // Execution context
    pub const THREAD_ID: &str = "gen_ai.thread.id";
    pub const RUN_ID: &str = "gen_ai.agent.run_id";
    pub const PARENT_RUN_ID: &str = "gen_ai.agent.parent_run_id";
    pub const ITERATION: &str = "gen_ai.agent.iteration";
    pub const MAX_ITERATIONS: &str = "gen_ai.agent.max_iterations";

    // Tool tracking
    pub const TOOL_NAME: &str = "gen_ai.tool.name";
    pub const TOOL_ID: &str = "gen_ai.tool.id";
    pub const TOOL_CALL_ID: &str = "gen_ai.tool.call_id";
    pub const TOOL_RESULT_STATUS: &str = "gen_ai.tool.result_status";
    pub const TOOL_DURATION_MS: &str = "gen_ai.tool.duration_ms";
    pub const TOOLS_AVAILABLE: &str = "gen_ai.agent.tools_available";
    pub const TOOLS_INVOKED: &str = "gen_ai.agent.tools_invoked";

    // Context window
    pub const CONTEXT_TOKENS_USED: &str = "gen_ai.context.tokens_used";
    pub const CONTEXT_TOKENS_LIMIT: &str = "gen_ai.context.tokens_limit";
    pub const CONTEXT_UTILIZATION: &str = "gen_ai.context.utilization";
    pub const CONTEXT_MESSAGES_COUNT: &str = "gen_ai.context.messages_count";

    // Safety
    pub const SAFETY_SCAN_RESULT: &str = "gen_ai.safety.scan_result";
    pub const SAFETY_RISK_SCORE: &str = "gen_ai.safety.risk_score";
    pub const SAFETY_BLOCKED: &str = "gen_ai.safety.blocked";

    // Cost (agent-level aggregation)
    pub const AGENT_TOTAL_COST_USD: &str = "gen_ai.agent.total_cost_usd";
    pub const AGENT_TOTAL_INPUT_TOKENS: &str = "gen_ai.agent.total_input_tokens";
    pub const AGENT_TOTAL_OUTPUT_TOKENS: &str = "gen_ai.agent.total_output_tokens";
    pub const AGENT_LLM_CALLS: &str = "gen_ai.agent.llm_calls";

    // Quality
    pub const FINISH_REASON: &str = "gen_ai.agent.finish_reason";
    pub const ERROR_TYPE: &str = "gen_ai.agent.error_type";
}

/// Agent execution span builder using tracing crate spans.
///
/// Produces spans compatible with OpenTelemetry GenAI conventions.
pub struct AgentSpanBuilder {
    agent_id: String,
    agent_name: String,
    run_id: String,
    thread_id: Option<String>,
    iteration: Option<u32>,
    max_iterations: Option<u32>,
    parent_run_id: Option<String>,
}

impl AgentSpanBuilder {
    pub fn new(
        agent_id: impl Into<String>,
        agent_name: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            agent_name: agent_name.into(),
            run_id: run_id.into(),
            thread_id: None,
            iteration: None,
            max_iterations: None,
            parent_run_id: None,
        }
    }

    pub fn thread_id(mut self, id: impl Into<String>) -> Self {
        self.thread_id = Some(id.into());
        self
    }

    pub fn iteration(mut self, iter: u32, max: u32) -> Self {
        self.iteration = Some(iter);
        self.max_iterations = Some(max);
        self
    }

    pub fn parent_run(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_run_id = Some(parent_id.into());
        self
    }

    /// Create a tracing span with all agent attributes.
    pub fn build(&self) -> tracing::Span {
        let span = tracing::info_span!(
            "agent.run",
            { keys::AGENT_ID } = %self.agent_id,
            { keys::AGENT_NAME } = %self.agent_name,
            { keys::RUN_ID } = %self.run_id,
            { keys::THREAD_ID } = tracing::field::Empty,
            { keys::ITERATION } = tracing::field::Empty,
            { keys::MAX_ITERATIONS } = tracing::field::Empty,
            { keys::PARENT_RUN_ID } = tracing::field::Empty,
            { keys::TOOLS_INVOKED } = tracing::field::Empty,
            { keys::AGENT_TOTAL_COST_USD } = tracing::field::Empty,
            { keys::AGENT_LLM_CALLS } = tracing::field::Empty,
            { keys::FINISH_REASON } = tracing::field::Empty,
        );

        if let Some(ref tid) = self.thread_id {
            span.record(keys::THREAD_ID, tid.as_str());
        }
        if let Some(iter) = self.iteration {
            span.record(keys::ITERATION, iter);
        }
        if let Some(max) = self.max_iterations {
            span.record(keys::MAX_ITERATIONS, max);
        }
        if let Some(ref parent) = self.parent_run_id {
            span.record(keys::PARENT_RUN_ID, parent.as_str());
        }

        span
    }
}

/// Builder for tool invocation spans.
pub struct ToolSpanBuilder {
    tool_name: String,
    tool_call_id: String,
    agent_run_id: String,
}

impl ToolSpanBuilder {
    pub fn new(
        tool_name: impl Into<String>,
        tool_call_id: impl Into<String>,
        agent_run_id: impl Into<String>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            tool_call_id: tool_call_id.into(),
            agent_run_id: agent_run_id.into(),
        }
    }

    pub fn build(&self) -> tracing::Span {
        tracing::info_span!(
            "tool.invoke",
            { keys::TOOL_NAME } = %self.tool_name,
            { keys::TOOL_CALL_ID } = %self.tool_call_id,
            { keys::RUN_ID } = %self.agent_run_id,
            { keys::TOOL_RESULT_STATUS } = tracing::field::Empty,
            { keys::TOOL_DURATION_MS } = tracing::field::Empty,
        )
    }
}

/// Record tool completion on a span.
pub fn record_tool_result(span: &tracing::Span, status: &str, duration_ms: u64) {
    span.record(keys::TOOL_RESULT_STATUS, status);
    span.record(keys::TOOL_DURATION_MS, duration_ms);
}

/// Record agent completion on a span.
pub fn record_agent_completion(
    span: &tracing::Span,
    finish_reason: &str,
    tools_invoked: u32,
    total_cost_usd: f64,
    llm_calls: u32,
) {
    span.record(keys::FINISH_REASON, finish_reason);
    span.record(keys::TOOLS_INVOKED, tools_invoked);
    span.record(keys::AGENT_TOTAL_COST_USD, total_cost_usd);
    span.record(keys::AGENT_LLM_CALLS, llm_calls);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_span_builder() {
        let span = AgentSpanBuilder::new("agent-1", "Coder", "run-abc")
            .thread_id("thread-xyz")
            .iteration(3, 10)
            .build();
        // Span is created without panic — attributes are set
        drop(span);
    }

    #[test]
    fn tool_span_builder() {
        let span = ToolSpanBuilder::new("shell_exec", "call-123", "run-abc").build();
        drop(span);
    }
}
