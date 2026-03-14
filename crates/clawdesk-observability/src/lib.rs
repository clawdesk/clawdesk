//! Observability for ClawDesk AI Agent Gateway
//!
//! Provides GenAI-specific instrumentation, tracers, and semantic conventions.
//!
//! # Delineation with `clawdesk-telemetry`
//!
//! - **`clawdesk-telemetry`** owns OTel initialization (TracerProvider, MeterProvider,
//!   tracing_subscriber, OTLP export). It provides the `Metrics` registry and
//!   `init_telemetry()` entry point.
//! - **`clawdesk-observability`** (this crate) owns GenAI-specific spans, cost
//!   calculation, semantic convention constants, and domain tracers (`LLMTracer`,
//!   `AgentTracer`, `EvaluationTracer`). It does NOT initialize the subscriber.
//!
//! Call `clawdesk_telemetry::init_telemetry()` first, then optionally call
//! `init_observability()` to register the observability config.

pub mod agent_spans;
pub mod audit;
pub mod batcher;
pub mod config;
pub mod genai_conventions;
pub mod genai_instrumentation;
pub mod metrics;
pub mod slo;
pub mod span_mapper;
pub mod storage_metrics;
pub mod tracer;
pub mod provider_usage;

pub use agent_spans::{AgentSpanBuilder, ToolSpanBuilder, record_agent_completion, record_tool_result};
pub use metrics::{MetricKey, MetricValue, MetricsAggregator};
pub use slo::{SloAlert, SloDefinition, SloMonitor, SloStatus, AlertSeverity};
pub use storage_metrics::{WriteAmplificationMetrics, WriteAmplificationReport};
pub use audit::{AuditLogger, AuditConfig, AuditEvent, AuditActor, AuditCategory, AuditOutcome};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::{span, Level, Span};

// ─────────────────────────────────────────────────────────────────────────────
// Observability configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Top-level observability configuration.
///
/// Environment variables:
/// - `CLAWDESK_OBSERVABILITY_ENABLED` — enable/disable (default: true)
/// - `CLAWDESK_API_KEY` — API key for authentication
/// - `CLAWDESK_ENDPOINT` — endpoint (default: `http://localhost:47100`)
/// - `CLAWDESK_PROJECT` — project identifier
/// - `CLAWDESK_SERVICE_NAME` — service name
/// - `CLAWDESK_ENVIRONMENT` — environment (production/staging/development)
#[derive(Debug, Clone)]
pub struct ObservabilityConfig {
    /// Service name for identification.
    pub service_name: String,

    /// ClawDesk server endpoint (e.g., `http://localhost:47100`).
    pub clawdesk_endpoint: String,

    /// Environment (production, staging, development).
    pub environment: String,

    /// Service version.
    pub version: String,

    /// Whether observability is enabled.
    pub enabled: bool,

    /// API key for authentication.
    pub api_key: Option<String>,

    /// Project identifier.
    pub project: Option<String>,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            service_name: "clawdesk-app".to_string(),
            clawdesk_endpoint: "http://localhost:47100".to_string(),
            environment: "development".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            enabled: true,
            api_key: None,
            project: None,
        }
    }
}

impl ObservabilityConfig {
    /// Build configuration from environment variables.
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(enabled) = std::env::var("CLAWDESK_OBSERVABILITY_ENABLED") {
            config.enabled = enabled.to_lowercase() == "true" || enabled == "1";
        }
        if let Ok(api_key) = std::env::var("CLAWDESK_API_KEY") {
            config.api_key = Some(api_key);
        }
        if let Ok(endpoint) = std::env::var("CLAWDESK_ENDPOINT") {
            config.clawdesk_endpoint = endpoint;
        }
        if let Ok(project) = std::env::var("CLAWDESK_PROJECT") {
            config.project = Some(project);
        }
        if let Ok(service_name) = std::env::var("CLAWDESK_SERVICE_NAME") {
            config.service_name = service_name;
        }
        if let Ok(environment) = std::env::var("CLAWDESK_ENVIRONMENT") {
            config.environment = environment;
        }

        config
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Initialization
// ─────────────────────────────────────────────────────────────────────────────

/// Initialize observability configuration.
///
/// **Note:** This does NOT initialize the tracing subscriber — that is owned by
/// `clawdesk_telemetry::init_telemetry()`. This function registers the
/// observability config and logs the active settings. Call it after telemetry
/// is already initialized.
pub fn init_observability(config: ObservabilityConfig) -> Result<()> {
    if !config.enabled {
        tracing::info!("ClawDesk observability disabled");
        return Ok(());
    }

    // Tracing subscriber is initialized by clawdesk-telemetry.
    // We only log the config state here.
    tracing::info!(
        service = %config.service_name,
        environment = %config.environment,
        endpoint = %config.clawdesk_endpoint,
        "ClawDesk observability config loaded (tracing owned by clawdesk-telemetry)"
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// GenAI semantic convention attribute constants
// ─────────────────────────────────────────────────────────────────────────────

/// GenAI semantic convention attributes (shorthand submodule).
pub mod gen_ai {
    pub const OPERATION_NAME: &str = "gen_ai.operation.name";
    pub const SYSTEM: &str = "gen_ai.system";
    pub const REQUEST_MODEL: &str = "gen_ai.request.model";
    pub const REQUEST_TEMPERATURE: &str = "gen_ai.request.temperature";
    pub const REQUEST_MAX_TOKENS: &str = "gen_ai.request.max_tokens";
    pub const REQUEST_TOP_P: &str = "gen_ai.request.top_p";
    pub const USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
    pub const USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
    pub const RESPONSE_FINISH_REASON: &str = "gen_ai.response.finish_reason";
    pub const RESPONSE_ID: &str = "gen_ai.response.id";
}

/// Custom agent semantic conventions.
pub mod agent {
    pub const NAME: &str = "agent.name";
    pub const OPERATION: &str = "agent.operation";
    pub const POLICY: &str = "agent.policy";
    pub const VERSION: &str = "agent.version";
}

/// Retrieval semantic conventions.
pub mod retrieval {
    pub const STRATEGY: &str = "retrieval.strategy";
    pub const CANDIDATES: &str = "retrieval.candidates";
    pub const RETURNED: &str = "retrieval.returned";
    pub const VECTOR_SCORE_AVG: &str = "retrieval.vector_score.avg";
    pub const KEYWORD_SCORE_AVG: &str = "retrieval.keyword_score.avg";
}

/// Evaluation semantic conventions.
pub mod evaluation {
    pub const HALLUCINATION_SCORE: &str = "evaluation.hallucination_score";
    pub const RELEVANCE_SCORE: &str = "evaluation.relevance_score";
    pub const GROUNDEDNESS_SCORE: &str = "evaluation.groundedness_score";
    pub const TOXICITY_SCORE: &str = "evaluation.toxicity_score";
    pub const PASSED: &str = "evaluation.passed";
}

// ─────────────────────────────────────────────────────────────────────────────
// LLM usage / cost tracking
// ─────────────────────────────────────────────────────────────────────────────

/// Token and cost usage for a single LLM invocation.
///
/// Wraps the presentation layer around the canonical `clawdesk_types::TokenUsage`.
/// The `cost_usd` field is observability-specific (not part of the core type).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: f64,
}

impl LLMUsage {
    /// Create from raw counts. `cost_usd` is calculated externally.
    pub fn new(input_tokens: u32, output_tokens: u32, cost_usd: f64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
            cost_usd,
        }
    }
}

/// Response wrapper carrying content + usage metadata.
#[derive(Debug, Clone)]
pub struct LLMResponse {
    pub content: String,
    pub model: String,
    pub finish_reason: String,
    pub usage: LLMUsage,
    pub response_id: Option<String>,
}

/// Per-model cost calculator.
///
/// Pricing is per-1M tokens. Call [`CostCalculator::for_model`] to auto-detect.
/// Rates aligned with upstream provider pricing as of 2026-03.
pub struct CostCalculator {
    input_cost_per_1m: f64,
    output_cost_per_1m: f64,
    cache_read_cost_per_1m: f64,
    cache_write_cost_per_1m: f64,
}

impl CostCalculator {
    /// Auto-detect pricing from model name.
    ///
    /// Covers OpenAI (GPT-4x/5.x), Anthropic Claude, Google Gemini,
    /// DeepSeek, Meta Llama, Cohere, Mistral, and local models.
    pub fn for_model(model: &str) -> Self {
        let m = model.to_lowercase();

        // ── Anthropic Claude ────────────────────────────────────
        if m.contains("claude-opus-4") || m.contains("claude-3-opus") {
            return Self { input_cost_per_1m: 15.0, output_cost_per_1m: 75.0, cache_read_cost_per_1m: 1.5, cache_write_cost_per_1m: 18.75 };
        }
        if m.contains("claude-sonnet-4") || m.contains("claude-3.5-sonnet") || m.contains("claude-3-5-sonnet") {
            return Self { input_cost_per_1m: 3.0, output_cost_per_1m: 15.0, cache_read_cost_per_1m: 0.3, cache_write_cost_per_1m: 3.75 };
        }
        if m.contains("claude-3-sonnet") {
            return Self { input_cost_per_1m: 3.0, output_cost_per_1m: 15.0, cache_read_cost_per_1m: 0.3, cache_write_cost_per_1m: 3.75 };
        }
        if m.contains("claude-haiku-4") || m.contains("claude-3-haiku") {
            return Self { input_cost_per_1m: 0.25, output_cost_per_1m: 1.25, cache_read_cost_per_1m: 0.025, cache_write_cost_per_1m: 0.3125 };
        }
        if m.contains("claude-2") {
            return Self { input_cost_per_1m: 8.0, output_cost_per_1m: 24.0, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 };
        }

        // ── OpenAI GPT-5.x ──────────────────────────────────────
        if m.contains("gpt-5.2") {
            return Self { input_cost_per_1m: 1.75, output_cost_per_1m: 14.0, cache_read_cost_per_1m: 0.175, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gpt-5.1-codex-max") {
            return Self { input_cost_per_1m: 1.25, output_cost_per_1m: 10.0, cache_read_cost_per_1m: 0.125, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gpt-5.1-codex-mini") {
            return Self { input_cost_per_1m: 0.25, output_cost_per_1m: 2.0, cache_read_cost_per_1m: 0.025, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gpt-5.1-codex") || m.contains("gpt-5.1") {
            return Self { input_cost_per_1m: 1.07, output_cost_per_1m: 8.5, cache_read_cost_per_1m: 0.107, cache_write_cost_per_1m: 0.0 };
        }

        // ── OpenAI GPT-4x ───────────────────────────────────────
        if m.contains("gpt-4o-mini") {
            return Self { input_cost_per_1m: 0.15, output_cost_per_1m: 0.6, cache_read_cost_per_1m: 0.075, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gpt-4o") {
            return Self { input_cost_per_1m: 2.5, output_cost_per_1m: 10.0, cache_read_cost_per_1m: 1.25, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gpt-4-turbo") || m.contains("gpt-4-1106") {
            return Self { input_cost_per_1m: 10.0, output_cost_per_1m: 30.0, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gpt-4") {
            return Self { input_cost_per_1m: 30.0, output_cost_per_1m: 60.0, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gpt-3.5-turbo") {
            return Self { input_cost_per_1m: 0.5, output_cost_per_1m: 1.5, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 };
        }

        // ── Google Gemini ───────────────────────────────────────
        if m.contains("gemini-3-pro") {
            return Self { input_cost_per_1m: 2.0, output_cost_per_1m: 12.0, cache_read_cost_per_1m: 0.2, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gemini-3-flash") {
            return Self { input_cost_per_1m: 0.5, output_cost_per_1m: 3.0, cache_read_cost_per_1m: 0.05, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gemini-1.5-pro") || m.contains("gemini-pro-1.5") {
            return Self { input_cost_per_1m: 3.5, output_cost_per_1m: 10.5, cache_read_cost_per_1m: 0.875, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gemini-1.5-flash") || m.contains("gemini-flash-1.5") {
            return Self { input_cost_per_1m: 0.075, output_cost_per_1m: 0.3, cache_read_cost_per_1m: 0.01875, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("gemini-pro") {
            return Self { input_cost_per_1m: 0.5, output_cost_per_1m: 1.5, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 };
        }

        // ── DeepSeek ────────────────────────────────────────────
        if m.contains("deepseek-r1") {
            return Self { input_cost_per_1m: 3.0, output_cost_per_1m: 7.0, cache_read_cost_per_1m: 3.0, cache_write_cost_per_1m: 3.0 };
        }
        if m.contains("deepseek-v3") {
            return Self { input_cost_per_1m: 0.6, output_cost_per_1m: 1.25, cache_read_cost_per_1m: 0.6, cache_write_cost_per_1m: 0.6 };
        }

        // ── Cohere ──────────────────────────────────────────────
        if m.contains("command-r-plus") || m.contains("command-r+") {
            return Self { input_cost_per_1m: 3.0, output_cost_per_1m: 15.0, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("command-r") {
            return Self { input_cost_per_1m: 0.5, output_cost_per_1m: 1.5, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 };
        }

        // ── Mistral ─────────────────────────────────────────────
        if m.contains("mistral-large") {
            return Self { input_cost_per_1m: 8.0, output_cost_per_1m: 24.0, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("mistral-medium") {
            return Self { input_cost_per_1m: 2.7, output_cost_per_1m: 8.1, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 };
        }
        if m.contains("mistral-small") {
            return Self { input_cost_per_1m: 1.0, output_cost_per_1m: 3.0, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 };
        }

        // ── Unknown ─────────────────────────────────────────────
        tracing::warn!(model = %model, "Unknown model for cost calculation, using $0");
        Self { input_cost_per_1m: 0.0, output_cost_per_1m: 0.0, cache_read_cost_per_1m: 0.0, cache_write_cost_per_1m: 0.0 }
    }

    /// Calculate total cost for a given usage.
    pub fn calculate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        (input_tokens as f64 * self.input_cost_per_1m / 1_000_000.0)
            + (output_tokens as f64 * self.output_cost_per_1m / 1_000_000.0)
    }

    /// Calculate total cost including cache tokens.
    pub fn calculate_with_cache(&self, input_tokens: u32, output_tokens: u32, cache_read: u32, cache_write: u32) -> f64 {
        (input_tokens as f64 * self.input_cost_per_1m / 1_000_000.0)
            + (output_tokens as f64 * self.output_cost_per_1m / 1_000_000.0)
            + (cache_read as f64 * self.cache_read_cost_per_1m / 1_000_000.0)
            + (cache_write as f64 * self.cache_write_cost_per_1m / 1_000_000.0)
    }

    /// Human-readable pricing string.
    pub fn pricing_info(&self) -> String {
        format!(
            "${:.4}/1M input, ${:.4}/1M output",
            self.input_cost_per_1m, self.output_cost_per_1m
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LLM Tracer
// ─────────────────────────────────────────────────────────────────────────────

/// Convenience tracer for LLM generation calls.
pub struct LLMTracer {
    model: String,
    system: String,
}

impl LLMTracer {
    pub fn new(model: String, system: String) -> Self {
        Self { model, system }
    }

    /// Wrap an async LLM call in a `gen_ai.chat_completion` span.
    pub fn trace_generation<F, Fut, T>(
        &self,
        operation: &str,
        f: F,
    ) -> impl std::future::Future<Output = Result<T>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let span = span!(
            Level::INFO,
            "gen_ai.chat_completion",
            gen_ai.operation.name = operation,
            gen_ai.system = %self.system,
            gen_ai.request.model = %self.model,
        );

        async move {
            let _enter = span.enter();
            let start = Instant::now();
            let result = f().await;
            let duration_ms = start.elapsed().as_millis();
            Span::current().record("llm.latency_ms", duration_ms as i64);
            result
        }
    }

    /// Record token usage on the current span.
    pub fn record_usage(&self, usage: &LLMUsage, finish_reason: &str) {
        let span = Span::current();
        span.record(gen_ai::USAGE_INPUT_TOKENS, usage.input_tokens as i64);
        span.record(gen_ai::USAGE_OUTPUT_TOKENS, usage.output_tokens as i64);
        span.record(gen_ai::RESPONSE_FINISH_REASON, finish_reason);
        span.record("gen_ai.usage.cost_usd", usage.cost_usd);

        tracing::info!(
            model = %self.model,
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            cost_usd = %usage.cost_usd,
            finish_reason = %finish_reason,
            "LLM generation completed"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Agent Tracer
// ─────────────────────────────────────────────────────────────────────────────

/// Convenience tracer for agent-level operations.
pub struct AgentTracer {
    agent_name: String,
}

impl AgentTracer {
    pub fn new(agent_name: String) -> Self {
        Self { agent_name }
    }

    /// Trace a retrieval operation.
    #[tracing::instrument(
        name = "agent.retrieval",
        skip(self, f),
        fields(agent.name = %self.agent_name, agent.operation = "context_retrieval")
    )]
    pub async fn trace_retrieval<F, Fut, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let start = Instant::now();
        let result = f().await;
        let duration_ms = start.elapsed().as_millis();
        Span::current().record("retrieval.latency_ms", duration_ms as i64);
        result
    }

    /// Record retrieval metrics on the current span.
    pub fn record_retrieval_metrics(
        &self,
        strategy: &str,
        total_candidates: usize,
        returned: usize,
        avg_vector_score: f64,
        avg_keyword_score: f64,
    ) {
        let span = Span::current();
        span.record(retrieval::STRATEGY, strategy);
        span.record(retrieval::CANDIDATES, total_candidates as i64);
        span.record(retrieval::RETURNED, returned as i64);
        span.record(retrieval::VECTOR_SCORE_AVG, avg_vector_score);
        span.record(retrieval::KEYWORD_SCORE_AVG, avg_keyword_score);

        tracing::info!(
            agent = %self.agent_name,
            strategy = %strategy,
            candidates = total_candidates,
            returned = returned,
            precision = %(returned as f64 / total_candidates.max(1) as f64),
            "Retrieval completed"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Evaluation Tracer
// ─────────────────────────────────────────────────────────────────────────────

/// Result of an evaluation pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    pub hallucination_score: f64,
    pub relevance_score: f64,
    pub groundedness_score: f64,
    pub toxicity_score: f64,
    pub passed: bool,
}

/// Tracer for evaluation operations.
pub struct EvaluationTracer;

impl EvaluationTracer {
    /// Trace an evaluation run.
    #[tracing::instrument(
        name = "evaluation.run",
        skip(f),
        fields(evaluation.r#type = "llm_response")
    )]
    pub async fn trace_evaluation<F, Fut>(f: F) -> Result<EvaluationResult>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<EvaluationResult>>,
    {
        let start = Instant::now();
        let result = f().await?;
        let duration_ms = start.elapsed().as_millis();

        let span = Span::current();
        span.record(evaluation::HALLUCINATION_SCORE, result.hallucination_score);
        span.record(evaluation::RELEVANCE_SCORE, result.relevance_score);
        span.record(evaluation::GROUNDEDNESS_SCORE, result.groundedness_score);
        span.record(evaluation::TOXICITY_SCORE, result.toxicity_score);
        span.record(evaluation::PASSED, result.passed);
        span.record("evaluation.latency_ms", duration_ms as i64);

        tracing::info!(
            hallucination = %result.hallucination_score,
            relevance = %result.relevance_score,
            groundedness = %result.groundedness_score,
            toxicity = %result.toxicity_score,
            passed = result.passed,
            "Evaluation completed"
        );

        Ok(result)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cost_calculator_claude() {
        let calc = CostCalculator::for_model("claude-sonnet-4.5");
        // per-1M: input=3.0, output=15.0
        // 1000 tokens each: (1000 * 3.0 / 1M) + (1000 * 15.0 / 1M) = 0.003 + 0.015 = 0.018
        let cost = calc.calculate(1000, 1000);
        assert!((cost - 0.018).abs() < 0.001);
    }

    #[test]
    fn test_cost_calculator_gpt4o() {
        let calc = CostCalculator::for_model("gpt-4o");
        // per-1M: input=2.5, output=10.0
        // 1000 tokens each: (1000 * 2.5 / 1M) + (1000 * 10.0 / 1M) = 0.0025 + 0.01 = 0.0125
        let cost = calc.calculate(1000, 1000);
        assert!((cost - 0.0125).abs() < 0.001);
    }

    #[test]
    fn test_cost_calculator_with_cache() {
        let calc = CostCalculator::for_model("claude-opus-4-20250514");
        // per-1M: input=15, output=75, cache_read=1.5, cache_write=18.75
        let cost = calc.calculate_with_cache(1000, 500, 2000, 100);
        let expected = (1000.0 * 15.0 / 1_000_000.0)
            + (500.0 * 75.0 / 1_000_000.0)
            + (2000.0 * 1.5 / 1_000_000.0)
            + (100.0 * 18.75 / 1_000_000.0);
        assert!((cost - expected).abs() < 0.0001);
    }

    #[test]
    fn test_cost_calculator_unknown() {
        let calc = CostCalculator::for_model("custom-local-model");
        let cost = calc.calculate(1000, 1000);
        assert_eq!(cost, 0.0);
    }

    #[tokio::test]
    async fn test_observability_config_default() {
        let config = ObservabilityConfig::default();
        assert_eq!(config.service_name, "clawdesk-app");
        assert!(config.enabled);
    }
}
