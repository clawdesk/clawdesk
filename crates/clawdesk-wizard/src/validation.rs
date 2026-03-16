//! Configuration validation — dependency-aware validation graph.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Result of validating a configuration field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub field: String,
    pub valid: bool,
    pub message: String,
    pub severity: ValidationSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationSeverity {
    Error,
    Warning,
    Info,
}

/// Configuration validator with dependency-aware validation graph.
///
/// Dependencies form a DAG: gateway.auth depends on secrets, channels depend
/// on gateway, agents depend on at least one provider key.
pub struct ConfigValidator {
    checks: Vec<ConfigCheck>,
}

struct ConfigCheck {
    #[allow(dead_code)]
    field: String,
    #[allow(dead_code)]
    dependencies: Vec<String>,
    validate: Box<dyn Fn(&HashMap<String, serde_json::Value>) -> ValidationResult + Send + Sync>,
}

impl ConfigValidator {
    pub fn new() -> Self {
        let mut v = Self { checks: Vec::new() };
        v.register_defaults();
        v
    }

    fn register_defaults(&mut self) {
        // Provider keys present.
        self.checks.push(ConfigCheck {
            field: "providers".into(),
            dependencies: vec![],
            validate: Box::new(|config| {
                let has_any = config.contains_key("anthropic_key")
                    || config.contains_key("openai_key")
                    || config.contains_key("gemini_key")
                    || config.contains_key("ollama_url");
                ValidationResult {
                    field: "providers".into(),
                    valid: has_any,
                    message: if has_any { "at least one provider configured".into() }
                             else { "no provider API keys found".into() },
                    severity: if has_any { ValidationSeverity::Info } else { ValidationSeverity::Error },
                }
            }),
        });

        // Gateway port valid.
        self.checks.push(ConfigCheck {
            field: "gateway.port".into(),
            dependencies: vec![],
            validate: Box::new(|config| {
                let port = config.get("gateway_port")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(18789);
                let valid = (1024..=65535).contains(&port);
                ValidationResult {
                    field: "gateway.port".into(),
                    valid,
                    message: if valid { format!("port {} is valid", port) }
                             else { format!("port {} is out of range", port) },
                    severity: if valid { ValidationSeverity::Info } else { ValidationSeverity::Error },
                }
            }),
        });
    }

    /// Run all validation checks, respecting dependency order.
    pub fn validate(&self, config: &HashMap<String, serde_json::Value>) -> Vec<ValidationResult> {
        // Simple topological traversal (all deps are currently independent).
        self.checks.iter().map(|c| (c.validate)(config)).collect()
    }

    /// Validate gateway port availability.
    pub fn check_port_available(port: u16) -> bool {
        std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
    }

    /// Find the next available port starting from `start`.
    pub fn find_available_port(start: u16, max_tries: u16) -> Option<u16> {
        for offset in 0..max_tries {
            let port = start.saturating_add(offset);
            if Self::check_port_available(port) {
                return Some(port);
            }
        }
        None
    }
}

impl Default for ConfigValidator {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_empty_config() {
        let validator = ConfigValidator::new();
        let results = validator.validate(&HashMap::new());
        assert!(results.iter().any(|r| !r.valid));
    }

    #[test]
    fn validate_with_provider() {
        let validator = ConfigValidator::new();
        let mut config = HashMap::new();
        config.insert("anthropic_key".into(), serde_json::json!("sk-ant-xxx"));
        let results = validator.validate(&config);
        let provider_check = results.iter().find(|r| r.field == "providers").unwrap();
        assert!(provider_check.valid);
    }

    #[test]
    fn port_range_validation() {
        let validator = ConfigValidator::new();
        let mut config = HashMap::new();
        config.insert("gateway_port".into(), serde_json::json!(80));
        let results = validator.validate(&config);
        let port_check = results.iter().find(|r| r.field == "gateway.port").unwrap();
        assert!(!port_check.valid);
    }
}
