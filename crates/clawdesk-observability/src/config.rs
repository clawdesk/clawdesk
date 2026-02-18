//! OpenTelemetry Configuration
//!
//! Supports standard OTEL environment variables for zero-config deployment.
//!
//! # Env Vars
//!
//! | Variable | Default | Description |
//! |---|---|---|
//! | `OTEL_SDK_DISABLED` | `false` | Kill switch |
//! | `OTEL_SERVICE_NAME` | `clawdesk` | Service name |
//! | `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4317` | OTLP endpoint |
//! | `OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc` | `grpc` / `http/protobuf` / `http/json` |
//! | `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` | `false` | Capture prompts |
//! | `OTEL_TRACES_SAMPLER_ARG` | `0.1` | Trace sampling rate |
//! | `CLAWDESK_ENDPOINT` | — | Optional ClawDesk-specific endpoint |
//! | `CLAWDESK_DUAL_EXPORT` | `false` | Export to both OTLP + ClawDesk |

use std::env;

/// Observability configuration with OTEL standard env vars.
#[derive(Debug, Clone)]
pub struct ObservabilityConfig {
    // OTEL standard env vars
    pub otel_sdk_disabled: bool,
    pub otel_service_name: String,
    pub otel_exporter_otlp_endpoint: String,
    pub otel_exporter_otlp_protocol: Protocol,

    // GenAI-specific
    pub capture_message_content: bool,
    pub sampling_rate: f64,

    // ClawDesk custom
    pub clawdesk_endpoint: Option<String>,
    pub enable_dual_export: bool,
}

/// OTLP export protocol.
#[derive(Debug, Clone, Copy)]
pub enum Protocol {
    Grpc,
    HttpProtobuf,
    HttpJson,
}

impl ObservabilityConfig {
    /// Build from environment variables.
    pub fn from_env() -> Self {
        Self {
            otel_sdk_disabled: env::var("OTEL_SDK_DISABLED")
                .map(|v| v == "true")
                .unwrap_or(false),

            otel_service_name: env::var("OTEL_SERVICE_NAME")
                .unwrap_or_else(|_| "clawdesk".to_string()),

            otel_exporter_otlp_endpoint: env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap_or_else(|_| "http://localhost:4317".to_string()),

            otel_exporter_otlp_protocol: match env::var("OTEL_EXPORTER_OTLP_PROTOCOL")
                .unwrap_or_else(|_| "grpc".to_string())
                .as_str()
            {
                "http/protobuf" => Protocol::HttpProtobuf,
                "http/json" => Protocol::HttpJson,
                _ => Protocol::Grpc,
            },

            capture_message_content: env::var("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT")
                .map(|v| v == "true")
                .unwrap_or(false),

            sampling_rate: env::var("OTEL_TRACES_SAMPLER_ARG")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.1),

            clawdesk_endpoint: env::var("CLAWDESK_ENDPOINT").ok(),

            enable_dual_export: env::var("CLAWDESK_DUAL_EXPORT")
                .map(|v| v == "true")
                .unwrap_or(false),
        }
    }
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            otel_sdk_disabled: false,
            otel_service_name: "clawdesk".to_string(),
            otel_exporter_otlp_endpoint: "http://localhost:4317".to_string(),
            otel_exporter_otlp_protocol: Protocol::Grpc,
            capture_message_content: false,
            sampling_rate: 0.1,
            clawdesk_endpoint: None,
            enable_dual_export: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = ObservabilityConfig::default();
        assert!(!cfg.otel_sdk_disabled);
        assert_eq!(cfg.otel_service_name, "clawdesk");
        assert!(!cfg.capture_message_content);
        assert!((cfg.sampling_rate - 0.1).abs() < f64::EPSILON);
    }
}
