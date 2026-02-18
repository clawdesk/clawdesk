//! OpenTelemetry Tracer Initialization
//!
//! Provides TracerProvider configuration with OTLP exporter support.

use opentelemetry::{global, trace::TracerProvider as _, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    trace::{self, RandomIdGenerator, Sampler},
    Resource,
};
use std::time::Duration;

/// Tracer configuration.
#[derive(Debug, Clone)]
pub struct OtelConfig {
    /// OTLP endpoint (e.g., `localhost:4317` for gRPC).
    pub otlp_endpoint: String,
    /// Service name for resource attribute.
    pub service_name: String,
    /// Service version.
    pub service_version: String,
    /// Sampling strategy.
    pub sampler: SamplerConfig,
    /// Batch export configuration.
    pub batch_config: BatchConfig,
    /// Enable content capture (`gen_ai.input.messages`, `gen_ai.output.messages`).
    pub capture_message_content: bool,
}

/// Sampling strategy.
#[derive(Debug, Clone)]
pub enum SamplerConfig {
    /// Sample everything (development).
    AlwaysOn,
    /// Sample nothing (disabled).
    AlwaysOff,
    /// Sample N% of traces.
    TraceIdRatio(f64),
    /// Parent-based sampling (follow parent's decision).
    ParentBased(Box<SamplerConfig>),
}

/// Batch export tuning.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    pub max_queue_size: usize,
    pub scheduled_delay: Duration,
    pub max_export_batch_size: usize,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            otlp_endpoint: std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap_or_else(|_| "http://localhost:4317".to_string()),
            service_name: "clawdesk".to_string(),
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            sampler: SamplerConfig::TraceIdRatio(0.1),
            batch_config: BatchConfig::default(),
            capture_message_content: std::env::var(
                "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT",
            )
            .map(|v| v == "true")
            .unwrap_or(false),
        }
    }
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_queue_size: 2048,
            scheduled_delay: Duration::from_secs(5),
            max_export_batch_size: 512,
        }
    }
}

/// Initialize OpenTelemetry with OTLP exporter.
pub fn init_tracer(
    config: OtelConfig,
) -> Result<opentelemetry_sdk::trace::Tracer, Box<dyn std::error::Error>> {
    let resource = Resource::new(vec![
        KeyValue::new("service.name", config.service_name.clone()),
        KeyValue::new("service.version", config.service_version.clone()),
        KeyValue::new("telemetry.sdk.language", "rust"),
        KeyValue::new("telemetry.sdk.name", "opentelemetry"),
    ]);

    let sampler = match config.sampler {
        SamplerConfig::AlwaysOn => Sampler::AlwaysOn,
        SamplerConfig::AlwaysOff => Sampler::AlwaysOff,
        SamplerConfig::TraceIdRatio(ratio) => Sampler::TraceIdRatioBased(ratio),
        SamplerConfig::ParentBased(inner) => {
            let root = match *inner {
                SamplerConfig::AlwaysOn => Sampler::AlwaysOn,
                SamplerConfig::AlwaysOff => Sampler::AlwaysOff,
                SamplerConfig::TraceIdRatio(r) => Sampler::TraceIdRatioBased(r),
                _ => Sampler::TraceIdRatioBased(0.1),
            };
            Sampler::ParentBased(Box::new(root))
        }
    };

    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(&config.otlp_endpoint);

    let tracer_provider = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(exporter)
        .with_trace_config(
            trace::Config::default()
                .with_sampler(sampler)
                .with_id_generator(RandomIdGenerator::default())
                .with_max_events_per_span(128)
                .with_max_attributes_per_span(128)
                .with_resource(resource),
        )
        .with_batch_config(
            opentelemetry_sdk::trace::BatchConfigBuilder::default()
                .with_max_queue_size(config.batch_config.max_queue_size)
                .with_scheduled_delay(config.batch_config.scheduled_delay)
                .with_max_export_batch_size(config.batch_config.max_export_batch_size)
                .build(),
        )
        .install_batch(opentelemetry_sdk::runtime::Tokio)?;

    global::set_tracer_provider(tracer_provider.clone());

    let tracer = tracer_provider.tracer("clawdesk-instrumentation");
    Ok(tracer)
}

/// Shutdown the global tracer provider, flushing all pending spans.
pub fn shutdown_tracer() {
    global::shutdown_tracer_provider();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = OtelConfig::default();
        assert_eq!(config.service_name, "clawdesk");
        assert!(!config.capture_message_content);
    }

    #[test]
    fn test_batch_config() {
        let batch = BatchConfig::default();
        assert_eq!(batch.max_queue_size, 2048);
        assert_eq!(batch.scheduled_delay, Duration::from_secs(5));
    }
}
