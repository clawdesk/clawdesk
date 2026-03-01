//! ACP Behavioral Contracts — pre/postcondition validation and cost envelopes.
//!
//! Contracts attach to A2A tasks and enforce:
//! - **Preconditions**: input schema + context validations before execution starts
//! - **Postconditions**: output schema + quality assertions after completion
//! - **Cost envelopes**: max_tokens, max_time, max_delegation_depth
//! - **Idempotency**: deterministic keys for safe retry
//!
//! ## Design
//!
//! A contract is evaluated at two points in the task lifecycle:
//! 1. **Pre-flight** (before `Submitted → Working`): validate preconditions
//! 2. **Post-flight** (before `Working → Completed`): validate postconditions
//!
//! Violations abort the task with structured error info.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

// ─── Contract Definition ────────────────────────────────────────────────────

/// A behavioral contract attached to an A2A task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehavioralContract {
    /// Unique contract identifier.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// Preconditions that must hold before task execution.
    #[serde(default)]
    pub preconditions: Vec<Condition>,
    /// Postconditions that must hold after task completion.
    #[serde(default)]
    pub postconditions: Vec<Condition>,
    /// Resource cost envelope.
    pub cost_envelope: CostEnvelope,
    /// Idempotency configuration.
    #[serde(default)]
    pub idempotency: Option<IdempotencyConfig>,
}

impl BehavioralContract {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            preconditions: vec![],
            postconditions: vec![],
            cost_envelope: CostEnvelope::default(),
            idempotency: None,
        }
    }

    /// Add a precondition.
    pub fn with_precondition(mut self, cond: Condition) -> Self {
        self.preconditions.push(cond);
        self
    }

    /// Add a postcondition.
    pub fn with_postcondition(mut self, cond: Condition) -> Self {
        self.postconditions.push(cond);
        self
    }

    /// Set cost envelope.
    pub fn with_cost_envelope(mut self, envelope: CostEnvelope) -> Self {
        self.cost_envelope = envelope;
        self
    }

    /// Set idempotency config.
    pub fn with_idempotency(mut self, config: IdempotencyConfig) -> Self {
        self.idempotency = Some(config);
        self
    }
}

// ─── Conditions ─────────────────────────────────────────────────────────────

/// A condition that can be evaluated against task data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    /// Human-readable label for this condition.
    pub label: String,
    /// The kind of check to perform.
    pub check: ConditionCheck,
}

/// Types of condition checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConditionCheck {
    /// A field must exist in the JSON input/output.
    FieldPresent {
        /// JSON pointer path (e.g., "/data/query").
        path: String,
    },
    /// A field must match a specific value.
    FieldEquals {
        path: String,
        expected: serde_json::Value,
    },
    /// A field must be a non-empty string.
    FieldNonEmpty {
        path: String,
    },
    /// A numeric field must be within a range.
    NumericRange {
        path: String,
        min: Option<f64>,
        max: Option<f64>,
    },
    /// Output must contain at least N artifacts.
    MinArtifacts {
        count: usize,
    },
    /// Custom expression (for extensibility — stored but not auto-evaluated).
    Custom {
        expression: String,
    },
}

impl Condition {
    pub fn field_present(label: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            check: ConditionCheck::FieldPresent { path: path.into() },
        }
    }

    pub fn field_non_empty(label: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            check: ConditionCheck::FieldNonEmpty { path: path.into() },
        }
    }

    pub fn numeric_range(
        label: impl Into<String>,
        path: impl Into<String>,
        min: Option<f64>,
        max: Option<f64>,
    ) -> Self {
        Self {
            label: label.into(),
            check: ConditionCheck::NumericRange {
                path: path.into(),
                min,
                max,
            },
        }
    }

    pub fn min_artifacts(label: impl Into<String>, count: usize) -> Self {
        Self {
            label: label.into(),
            check: ConditionCheck::MinArtifacts { count },
        }
    }

    /// Evaluate this condition against a JSON value.
    pub fn evaluate(&self, data: &serde_json::Value) -> ConditionResult {
        match &self.check {
            ConditionCheck::FieldPresent { path } => {
                let exists = data.pointer(path).is_some();
                ConditionResult {
                    label: self.label.clone(),
                    passed: exists,
                    detail: if exists {
                        None
                    } else {
                        Some(format!("field '{}' not found", path))
                    },
                }
            }
            ConditionCheck::FieldEquals { path, expected } => {
                let actual = data.pointer(path);
                let passed = actual == Some(expected);
                ConditionResult {
                    label: self.label.clone(),
                    passed,
                    detail: if passed {
                        None
                    } else {
                        Some(format!(
                            "field '{}': expected {:?}, got {:?}",
                            path, expected, actual
                        ))
                    },
                }
            }
            ConditionCheck::FieldNonEmpty { path } => {
                let value = data.pointer(path);
                let passed = match value {
                    Some(serde_json::Value::String(s)) => !s.is_empty(),
                    Some(serde_json::Value::Array(a)) => !a.is_empty(),
                    Some(serde_json::Value::Object(o)) => !o.is_empty(),
                    _ => false,
                };
                ConditionResult {
                    label: self.label.clone(),
                    passed,
                    detail: if passed {
                        None
                    } else {
                        Some(format!("field '{}' is empty or missing", path))
                    },
                }
            }
            ConditionCheck::NumericRange { path, min, max } => {
                let value = data.pointer(path).and_then(|v| v.as_f64());
                let passed = match value {
                    Some(v) => {
                        min.map_or(true, |lo| v >= lo) && max.map_or(true, |hi| v <= hi)
                    }
                    None => false,
                };
                ConditionResult {
                    label: self.label.clone(),
                    passed,
                    detail: if passed {
                        None
                    } else {
                        Some(format!(
                            "field '{}': value {:?} outside range [{:?}, {:?}]",
                            path, value, min, max
                        ))
                    },
                }
            }
            ConditionCheck::MinArtifacts { count } => {
                let artifacts = data
                    .pointer("/artifacts")
                    .and_then(|v| v.as_array())
                    .map_or(0, |a| a.len());
                let passed = artifacts >= *count;
                ConditionResult {
                    label: self.label.clone(),
                    passed,
                    detail: if passed {
                        None
                    } else {
                        Some(format!(
                            "expected ≥{} artifacts, got {}",
                            count, artifacts
                        ))
                    },
                }
            }
            ConditionCheck::Custom { expression } => {
                // Custom expressions are logged but not auto-evaluated.
                ConditionResult {
                    label: self.label.clone(),
                    passed: true,
                    detail: Some(format!("custom expression (not evaluated): {}", expression)),
                }
            }
        }
    }
}

/// Result of evaluating a single condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionResult {
    pub label: String,
    pub passed: bool,
    pub detail: Option<String>,
}

// ─── Cost Envelope ──────────────────────────────────────────────────────────

/// Resource cost envelope — bounds on what a task may consume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEnvelope {
    /// Maximum tokens (input + output) the task may consume.
    pub max_tokens: Option<u64>,
    /// Maximum wall-clock time for task execution.
    #[serde(with = "duration_seconds")]
    pub max_duration: Option<Duration>,
    /// Maximum delegation depth (how many sub-tasks deep).
    pub max_depth: Option<u32>,
    /// Maximum number of tool calls.
    pub max_tool_calls: Option<u32>,
    /// Maximum monetary cost in USD (for metered APIs).
    pub max_cost_usd: Option<f64>,
}

impl Default for CostEnvelope {
    fn default() -> Self {
        Self {
            max_tokens: Some(100_000),
            max_duration: Some(Duration::seconds(300)),
            max_depth: Some(3),
            max_tool_calls: Some(50),
            max_cost_usd: None,
        }
    }
}

impl CostEnvelope {
    /// Unlimited envelope (no constraints).
    pub fn unlimited() -> Self {
        Self {
            max_tokens: None,
            max_duration: None,
            max_depth: None,
            max_tool_calls: None,
            max_cost_usd: None,
        }
    }

    /// Check if token usage is within bounds.
    pub fn check_tokens(&self, used: u64) -> bool {
        self.max_tokens.map_or(true, |max| used <= max)
    }

    /// Check if delegation depth is within bounds.
    pub fn check_depth(&self, depth: u32) -> bool {
        self.max_depth.map_or(true, |max| depth <= max)
    }

    /// Check if tool call count is within bounds.
    pub fn check_tool_calls(&self, count: u32) -> bool {
        self.max_tool_calls.map_or(true, |max| count <= max)
    }

    /// Check if cost is within bounds.
    pub fn check_cost(&self, cost_usd: f64) -> bool {
        self.max_cost_usd.map_or(true, |max| cost_usd <= max)
    }

    /// Check if elapsed time is within bounds.
    pub fn check_duration(&self, elapsed: Duration) -> bool {
        self.max_duration.map_or(true, |max| elapsed <= max)
    }
}

/// Helper module for serializing `Option<Duration>` as seconds.
mod duration_seconds {
    use chrono::Duration;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match duration {
            Some(d) => serializer.serialize_some(&d.num_seconds()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<i64> = Option::deserialize(deserializer)?;
        Ok(opt.map(Duration::seconds))
    }
}

// ─── Idempotency ─────────────────────────────────────────────────────────────

/// Idempotency configuration for safe task retries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyConfig {
    /// Deterministic key for deduplication.
    /// Typically: SHA-256(requester_id || skill_id || canonical_input).
    pub key: String,
    /// How long the idempotency key is valid.
    #[serde(with = "duration_seconds")]
    pub ttl: Option<Duration>,
    /// Behavior on duplicate submission.
    pub on_duplicate: DuplicatePolicy,
}

/// What to do when a duplicate idempotency key is detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicatePolicy {
    /// Return the cached result from the original execution.
    ReturnCached,
    /// Reject the duplicate with an error.
    Reject,
    /// Allow re-execution (idempotency is advisory only).
    Allow,
}

impl Default for DuplicatePolicy {
    fn default() -> Self {
        Self::ReturnCached
    }
}

// ─── Contract Evaluation ────────────────────────────────────────────────────

/// Result of evaluating a contract against task data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractEvaluation {
    pub contract_id: String,
    pub phase: EvaluationPhase,
    pub passed: bool,
    pub results: Vec<ConditionResult>,
    pub evaluated_at: DateTime<Utc>,
}

/// When in the task lifecycle the evaluation was performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationPhase {
    /// Before task starts (precondition check).
    PreFlight,
    /// After task completes (postcondition check).
    PostFlight,
}

impl BehavioralContract {
    /// Evaluate preconditions against task input.
    pub fn evaluate_preconditions(&self, input: &serde_json::Value) -> ContractEvaluation {
        let results: Vec<ConditionResult> = self
            .preconditions
            .iter()
            .map(|cond| cond.evaluate(input))
            .collect();
        let passed = results.iter().all(|r| r.passed);
        ContractEvaluation {
            contract_id: self.id.clone(),
            phase: EvaluationPhase::PreFlight,
            passed,
            results,
            evaluated_at: Utc::now(),
        }
    }

    /// Evaluate postconditions against task output.
    pub fn evaluate_postconditions(&self, output: &serde_json::Value) -> ContractEvaluation {
        let results: Vec<ConditionResult> = self
            .postconditions
            .iter()
            .map(|cond| cond.evaluate(output))
            .collect();
        let passed = results.iter().all(|r| r.passed);
        ContractEvaluation {
            contract_id: self.id.clone(),
            phase: EvaluationPhase::PostFlight,
            passed,
            results,
            evaluated_at: Utc::now(),
        }
    }
}

// ─── Contract Registry ──────────────────────────────────────────────────────

/// Registry of contracts keyed by skill/task type.
pub struct ContractRegistry {
    contracts: std::collections::HashMap<String, BehavioralContract>,
}

impl ContractRegistry {
    pub fn new() -> Self {
        Self {
            contracts: std::collections::HashMap::new(),
        }
    }

    /// Register a contract by its ID. 
    pub fn register(&mut self, contract: BehavioralContract) {
        self.contracts.insert(contract.id.clone(), contract);
    }

    /// Look up a contract by ID.
    pub fn get(&self, id: &str) -> Option<&BehavioralContract> {
        self.contracts.get(id)
    }

    /// All registered contract IDs.
    pub fn contract_ids(&self) -> Vec<&str> {
        self.contracts.keys().map(|s| s.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.contracts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.contracts.is_empty()
    }
}

impl Default for ContractRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_precondition_field_present() {
        let contract = BehavioralContract::new("test", "test contract")
            .with_precondition(Condition::field_present("query exists", "/query"));

        let input = json!({"query": "hello world"});
        let eval = contract.evaluate_preconditions(&input);
        assert!(eval.passed);

        let bad_input = json!({"other": "value"});
        let eval = contract.evaluate_preconditions(&bad_input);
        assert!(!eval.passed);
    }

    #[test]
    fn test_precondition_non_empty() {
        let contract = BehavioralContract::new("test", "test")
            .with_precondition(Condition::field_non_empty("query non-empty", "/query"));

        let input = json!({"query": "hello"});
        assert!(contract.evaluate_preconditions(&input).passed);

        let empty = json!({"query": ""});
        assert!(!contract.evaluate_preconditions(&empty).passed);
    }

    #[test]
    fn test_postcondition_numeric_range() {
        let contract = BehavioralContract::new("test", "test").with_postcondition(
            Condition::numeric_range("confidence check", "/confidence", Some(0.5), Some(1.0)),
        );

        let good = json!({"confidence": 0.8});
        assert!(contract.evaluate_postconditions(&good).passed);

        let low = json!({"confidence": 0.2});
        assert!(!contract.evaluate_postconditions(&low).passed);

        let high = json!({"confidence": 1.5});
        assert!(!contract.evaluate_postconditions(&high).passed);
    }

    #[test]
    fn test_cost_envelope_checks() {
        let env = CostEnvelope::default();
        assert!(env.check_tokens(50_000));
        assert!(!env.check_tokens(200_000));
        assert!(env.check_depth(2));
        assert!(!env.check_depth(5));
        assert!(env.check_tool_calls(30));
        assert!(!env.check_tool_calls(100));
    }

    #[test]
    fn test_cost_envelope_unlimited() {
        let env = CostEnvelope::unlimited();
        assert!(env.check_tokens(u64::MAX));
        assert!(env.check_depth(u32::MAX));
        assert!(env.check_tool_calls(u32::MAX));
    }

    #[test]
    fn test_contract_serde_roundtrip() {
        let contract = BehavioralContract::new("web-search", "Web search contract")
            .with_precondition(Condition::field_present("query", "/query"))
            .with_postcondition(Condition::min_artifacts("has results", 1))
            .with_cost_envelope(CostEnvelope {
                max_tokens: Some(50_000),
                max_duration: Some(Duration::seconds(60)),
                max_depth: Some(2),
                max_tool_calls: Some(10),
                max_cost_usd: Some(0.10),
            });

        let json = serde_json::to_string(&contract).unwrap();
        let restored: BehavioralContract = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, "web-search");
        assert_eq!(restored.preconditions.len(), 1);
        assert_eq!(restored.postconditions.len(), 1);
        assert_eq!(restored.cost_envelope.max_tokens, Some(50_000));
    }

    #[test]
    fn test_full_contract_lifecycle() {
        let contract = BehavioralContract::new("summarize", "Summarization contract")
            .with_precondition(Condition::field_non_empty("input text", "/text"))
            .with_postcondition(Condition::field_non_empty("summary present", "/summary"))
            .with_postcondition(
                Condition::numeric_range("length check", "/word_count", Some(10.0), Some(500.0)),
            );

        // Pre-flight: valid input
        let input = json!({"text": "A long article about AI..."});
        let pre = contract.evaluate_preconditions(&input);
        assert!(pre.passed);
        assert_eq!(pre.phase, EvaluationPhase::PreFlight);

        // Post-flight: valid output
        let output = json!({"summary": "AI is transformative.", "word_count": 50});
        let post = contract.evaluate_postconditions(&output);
        assert!(post.passed);
        assert_eq!(post.phase, EvaluationPhase::PostFlight);

        // Post-flight: missing summary
        let bad_output = json!({"word_count": 50});
        let post = contract.evaluate_postconditions(&bad_output);
        assert!(!post.passed);
    }

    #[test]
    fn test_contract_registry() {
        let mut reg = ContractRegistry::new();
        reg.register(BehavioralContract::new("search", "Search contract"));
        reg.register(BehavioralContract::new("summarize", "Summarize contract"));
        assert_eq!(reg.len(), 2);
        assert!(reg.get("search").is_some());
        assert!(reg.get("nonexistent").is_none());
    }
}
