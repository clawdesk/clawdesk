//! Observability for ClawDesk AI Agent Gateway
//!
//! Provides comprehensive tracing, metrics, and instrumentation following
//! OpenTelemetry semantic conventions for GenAI operations.
//!
//! # Architecture
//!
//! ClawDesk observes itself using structured tracing with optional OTLP export:
//!
//! ```text
//! ┌─────────────────────┐
//! │  Agent / LLM Call    │  ← #[instrument] spans
//! └──────────┬──────────┘
//!            │
//!     ┌──────▼──────┐
//!     │  Batcher     │  ← async batch span collector
//!     └──────┬──────┘
//!            │
//!   ┌────────▼────────┐
//!   │  OTLP Exporter   │  ← gRPC/HTTP to collector
//!   └────────┬────────┘
//!            │
//!   ┌────────▼────────┐
//!   │  Jaeger / Tempo  │  ← or any OTLP-compatible backend
//!   └─────────────────┘
//! ```
//!
//! # Modules
//!
//! - [`config`] — OTEL-standard env var configuration
//! - [`tracer`] — TracerProvider initialization with OTLP export
//! - [`genai_conventions`] — GenAI semantic convention constants (OTEL v1.36)
//! - [`genai_instrumentation`] — Ergonomic span builders for LLM calls
//! - [`batcher`] — Async batch exporter with circuit breaker
//! - [`metrics`] — Pre-aggregated metrics for dashboards
//! - [`span_mapper`] — Map span types to OTEL GenAI operations
//! - [`storage_metrics`] — Write amplification tracking for storage engine

pub mod batcher;
pub mod config;
pub mod genai_conventions;
pub mod genai_instrumentation;
pub mod metrics;
pub mod span_mapper;
pub mod storage_metrics;
pub mod tracer;

pub use metrics::{MetricKey, MetricValue, MetricsAggregator};
pub use storage_metrics::{WriteAmplificationMetrics, WriteAmplificationReport};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::{span, Level, Span};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

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

/// Initialize observability with structured logging.
///
/// Sets up a `tracing_subscriber` with `EnvFilter` + pretty-printed output.
/// Traces can optionally be batched and shipped to an OTLP backend.
pub fn init_observability(config: ObservabilityConfig) -> Result<()> {
    if !config.enabled {
        tracing::info!("ClawDesk observability disabled");
        return Ok(());
    }

    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!(
        service = %config.service_name,
        environment = %config.environment,
        endpoint = %config.clawdesk_endpoint,
        "ClawDesk observability initialized"
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: f64,
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
/// Pricing is per-1K tokens. Call [`CostCalculator::for_model`] to auto-detect.
pub struct CostCalculator {
    input_cost_per_1k: f64,
    output_cost_per_1k: f64,
}

impl CostCalculator {
    /// Auto-detect pricing from model name.
    ///
    /// Covers OpenAI, Anthropic, Google Gemini, Cohere, and Mistral families.
    pub fn for_model(model: &str) -> Self {
        let m = model.to_lowercase();

        match () {
            // ── OpenAI ──────────────────────────────────────────────
            _ if m.contains("gpt-4o-mini") => Self { input_cost_per_1k: 0.000_15, output_cost_per_1k: 0.000_6 },
            _ if m.contains("gpt-4o")      => Self { input_cost_per_1k: 0.002_5,  output_cost_per_1k: 0.01  },
            _ if m.contains("gpt-4-turbo") || m.contains("gpt-4-1106") =>
                Self { input_cost_per_1k: 0.01, output_cost_per_1k: 0.03 },
            _ if m.contains("gpt-4")       => Self { input_cost_per_1k: 0.03, output_cost_per_1k: 0.06 },
            _ if m.contains("gpt-3.5-turbo") => Self { input_cost_per_1k: 0.000_5, output_cost_per_1k: 0.001_5 },

            // ── Anthropic Claude ────────────────────────────────────
            _ if m.contains("claude-opus-4") || m.contains("claude-3-opus") =>
                Self { input_cost_per_1k: 0.015, output_cost_per_1k: 0.075 },
            _ if m.contains("claude-sonnet-4.5") || m.contains("claude-sonnet-4")
              || m.contains("claude-3.5-sonnet") || m.contains("claude-3-5-sonnet") =>
                Self { input_cost_per_1k: 0.003, output_cost_per_1k: 0.015 },
            _ if m.contains("claude-3-sonnet") =>
                Self { input_cost_per_1k: 0.003, output_cost_per_1k: 0.015 },
            _ if m.contains("claude-haiku-4") || m.contains("claude-3-haiku") =>
                Self { input_cost_per_1k: 0.000_25, output_cost_per_1k: 0.001_25 },
            _ if m.contains("claude-2") =>
                Self { input_cost_per_1k: 0.008, output_cost_per_1k: 0.024 },

            // ── Google Gemini ───────────────────────────────────────
            _ if m.contains("gemini-1.5-pro") || m.contains("gemini-pro-1.5") =>
                Self { input_cost_per_1k: 0.003_5, output_cost_per_1k: 0.010_5 },
            _ if m.contains("gemini-1.5-flash") || m.contains("gemini-flash-1.5") =>
                Self { input_cost_per_1k: 0.000_075, output_cost_per_1k: 0.000_3 },
            _ if m.contains("gemini-pro") =>
                Self { input_cost_per_1k: 0.000_5, output_cost_per_1k: 0.001_5 },

            // ── Cohere ──────────────────────────────────────────────
            _ if m.contains("command-r-plus") || m.contains("command-r+") =>
                Self { input_cost_per_1k: 0.003, output_cost_per_1k: 0.015 },
            _ if m.contains("command-r") =>
                Self { input_cost_per_1k: 0.000_5, output_cost_per_1k: 0.001_5 },

            // ── Mistral ─────────────────────────────────────────────
            _ if m.contains("mistral-large")  => Self { input_cost_per_1k: 0.008,   output_cost_per_1k: 0.024  },
            _ if m.contains("mistral-medium") => Self { input_cost_per_1k: 0.002_7, output_cost_per_1k: 0.008_1 },
            _ if m.contains("mistral-small")  => Self { input_cost_per_1k: 0.001,   output_cost_per_1k: 0.003  },

            // ── Unknown ─────────────────────────────────────────────
            _ => {
                tracing::warn!(model = %model, "Unknown model for cost calculation, using $0");
                Self { input_cost_per_1k: 0.0, output_cost_per_1k: 0.0 }
            }
        }
    }

    /// Calculate total cost for a given usage.
    pub fn calculate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        (input_tokens as f64 / 1000.0) * self.input_cost_per_1k
            + (output_tokens as f64 / 1000.0) * self.output_cost_per_1k
    }

    /// Human-readable pricing string.
    pub fn pricing_info(&self) -> String {
        format!(
            "${:.6}/1K input, ${:.6}/1K output",
            self.input_cost_per_1k, self.output_cost_per_1k
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
        let cost = calc.calculate(1000, 1000);
        // $0.003 + $0.015 = $0.018
        assert!((cost - 0.018).abs() < 0.001);
    }

    #[test]
    fn test_cost_calculator_gpt4o() {
        let calc = CostCalculator::for_model("gpt-4o");
        let cost = calc.calculate(1000, 1000);
        // $0.0025 + $0.01 = $0.0125
        assert!((cost - 0.0125).abs() < 0.001);
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
