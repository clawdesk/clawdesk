//! YAML test case and suite definitions.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Test case schema (YAML-serializable)
// ---------------------------------------------------------------------------

/// A single test case — a conversation scenario with expectations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    /// Human-readable name (used as test identifier).
    pub name: String,
    /// Optional description.
    #[serde(default)]
    pub description: String,
    /// Agent ID to test (matches agent_loader definitions).
    #[serde(default = "default_agent")]
    pub agent: String,
    /// Maximum time for the entire test case (seconds).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Optional tags for filtering.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Setup configuration.
    #[serde(default)]
    pub setup: TestSetup,
    /// Conversation steps (in order).
    pub steps: Vec<TestStep>,
    /// Teardown assertions.
    #[serde(default)]
    pub teardown: TestTeardown,
}

fn default_agent() -> String { "assistant".to_string() }
fn default_timeout() -> u64 { 30 }

/// Pre-test configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestSetup {
    /// Override agent's system prompt.
    pub system_prompt: Option<String>,
    /// Override model name.
    pub model: Option<String>,
    /// Seed messages to pre-populate the conversation.
    #[serde(default)]
    pub seed_messages: Vec<SeedMessage>,
    /// Mock responses to inject (in order). Each step consumes one.
    #[serde(default)]
    pub mock_responses: Vec<String>,
    /// Environment variables to set for the test.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// A pre-seeded message for conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedMessage {
    pub role: String,
    pub content: String,
}

/// A single conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestStep {
    /// User message to send.
    pub user: String,
    /// Expected properties of the agent's response.
    #[serde(default)]
    pub expect: StepExpectation,
    /// Optional mock response override for this step.
    pub mock_response: Option<String>,
    /// Optional delay before sending (ms).
    #[serde(default)]
    pub delay_ms: u64,
}

/// Expectations about an agent response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StepExpectation {
    /// Response must contain ALL of these substrings (case-insensitive).
    #[serde(default)]
    pub contains: Vec<String>,
    /// Response must NOT contain any of these substrings.
    #[serde(default)]
    pub not_contains: Vec<String>,
    /// Response body must match this regex.
    pub matches: Option<String>,
    /// Response must NOT match this regex.
    pub not_matches: Option<String>,
    /// Response must be at most this many tokens (approximate: word count).
    pub max_tokens: Option<usize>,
    /// Response must be at least this many tokens.
    pub min_tokens: Option<usize>,
    /// Response must contain valid JSON.
    #[serde(default)]
    pub is_json: bool,
    /// JSON path expectations (path → expected value string).
    #[serde(default)]
    pub json_values: std::collections::HashMap<String, String>,
    /// Custom assertion labels (checked by registered assertion fns).
    #[serde(default)]
    pub custom: Vec<String>,
}

/// Post-test assertions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestTeardown {
    /// Assert no errors occurred during the conversation.
    #[serde(default)]
    pub assert_no_errors: bool,
    /// Assert total token usage was under this limit.
    pub max_total_tokens: Option<u64>,
    /// Assert total conversation turns.
    pub expected_turns: Option<usize>,
}

// ---------------------------------------------------------------------------
// Test suite
// ---------------------------------------------------------------------------

/// A collection of test cases loaded from YAML files.
#[derive(Debug, Clone)]
pub struct TestSuite {
    pub cases: Vec<TestCase>,
    pub source_dir: Option<PathBuf>,
}

/// Aggregated results for a test suite run.
#[derive(Debug, Clone, Serialize)]
pub struct SuiteResults {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub duration_ms: u64,
    pub cases: Vec<CaseResult>,
}

/// Result of a single test case.
#[derive(Debug, Clone, Serialize)]
pub struct CaseResult {
    pub name: String,
    pub passed: bool,
    pub duration_ms: u64,
    pub steps: Vec<StepResult>,
    pub errors: Vec<String>,
}

/// Result of a single step.
#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub step_index: usize,
    pub user_input: String,
    pub agent_response: String,
    pub passed: bool,
    pub assertion_results: Vec<crate::assertions::AssertionResult>,
}

impl SuiteResults {
    pub fn all_passed(&self) -> bool {
        self.failed == 0
    }

    pub fn summary(&self) -> String {
        format!(
            "{}/{} passed, {} failed, {} skipped ({}ms)",
            self.passed, self.total, self.failed, self.skipped, self.duration_ms
        )
    }
}

impl TestSuite {
    /// Load all `.yaml` / `.yml` test files from a directory.
    pub async fn from_yaml_dir(dir: impl AsRef<Path>) -> Result<Self, String> {
        let dir = dir.as_ref();
        if !dir.exists() {
            return Err(format!("test directory not found: {}", dir.display()));
        }

        let mut cases = Vec::new();
        let mut entries = tokio::fs::read_dir(dir)
            .await
            .map_err(|e| format!("read dir: {e}"))?;

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "yaml" || ext == "yml" {
                match Self::load_case(&path).await {
                    Ok(tc) => {
                        debug!(name = %tc.name, path = %path.display(), "loaded test case");
                        cases.push(tc);
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), %e, "skipping invalid test case");
                    }
                }
            }
        }

        cases.sort_by(|a, b| a.name.cmp(&b.name));
        info!(count = cases.len(), dir = %dir.display(), "test suite loaded");

        Ok(Self {
            cases,
            source_dir: Some(dir.to_path_buf()),
        })
    }

    /// Load a single test case from a YAML file.
    pub async fn load_case(path: &Path) -> Result<TestCase, String> {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("read file: {e}"))?;
        Self::parse_yaml(&content)
    }

    /// Parse a YAML string into a TestCase.
    pub fn parse_yaml(yaml: &str) -> Result<TestCase, String> {
        serde_yaml::from_str(yaml).map_err(|e| format!("YAML parse: {e}"))
    }

    /// Create a suite from a list of test cases.
    pub fn from_cases(cases: Vec<TestCase>) -> Self {
        Self {
            cases,
            source_dir: None,
        }
    }

    /// Filter cases by tag.
    pub fn filter_by_tag(&self, tag: &str) -> Self {
        let cases: Vec<_> = self.cases.iter()
            .filter(|c| c.tags.contains(&tag.to_string()))
            .cloned()
            .collect();
        Self {
            cases,
            source_dir: self.source_dir.clone(),
        }
    }

    /// Filter cases by name pattern (regex).
    pub fn filter_by_name(&self, pattern: &str) -> Result<Self, String> {
        let re = regex::Regex::new(pattern).map_err(|e| format!("regex: {e}"))?;
        let cases: Vec<_> = self.cases.iter()
            .filter(|c| re.is_match(&c.name))
            .cloned()
            .collect();
        Ok(Self {
            cases,
            source_dir: self.source_dir.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_YAML: &str = r#"
name: "greeting-test"
description: "Test that the agent greets properly"
agent: "assistant"
timeout_secs: 10
tags: ["smoke", "greeting"]

setup:
  system_prompt: "You are a helpful assistant."
  model: "mock"
  mock_responses:
    - "Hello Alice! How can I help you?"
    - "The answer is 4."

steps:
  - user: "Hi, my name is Alice"
    expect:
      contains: ["Alice"]
      not_contains: ["error"]

  - user: "What is 2+2?"
    expect:
      matches: ".*4.*"
      max_tokens: 100

teardown:
  assert_no_errors: true
"#;

    #[test]
    fn parse_yaml_test_case() {
        let tc = TestSuite::parse_yaml(SAMPLE_YAML).unwrap();
        assert_eq!(tc.name, "greeting-test");
        assert_eq!(tc.steps.len(), 2);
        assert_eq!(tc.setup.mock_responses.len(), 2);
        assert!(tc.teardown.assert_no_errors);
        assert_eq!(tc.tags, vec!["smoke", "greeting"]);
    }

    #[test]
    fn step_expectation_defaults() {
        let yaml = r#"
name: "minimal"
steps:
  - user: "hello"
"#;
        let tc = TestSuite::parse_yaml(yaml).unwrap();
        assert_eq!(tc.steps.len(), 1);
        assert!(tc.steps[0].expect.contains.is_empty());
    }

    #[test]
    fn filter_by_tag() {
        let tc1 = TestSuite::parse_yaml(SAMPLE_YAML).unwrap();
        let tc2 = TestSuite::parse_yaml(r#"
name: "other-test"
steps:
  - user: "hello"
tags: ["regression"]
"#).unwrap();
        let suite = TestSuite::from_cases(vec![tc1, tc2]);

        let filtered = suite.filter_by_tag("smoke");
        assert_eq!(filtered.cases.len(), 1);
        assert_eq!(filtered.cases[0].name, "greeting-test");
    }

    #[test]
    fn filter_by_name_regex() {
        let tc1 = TestSuite::parse_yaml(SAMPLE_YAML).unwrap();
        let tc2 = TestSuite::parse_yaml(r#"
name: "math-test"
steps:
  - user: "hello"
"#).unwrap();
        let suite = TestSuite::from_cases(vec![tc1, tc2]);

        let filtered = suite.filter_by_name("greeting").unwrap();
        assert_eq!(filtered.cases.len(), 1);
        assert_eq!(filtered.cases[0].name, "greeting-test");
    }

    #[test]
    fn suite_results_summary() {
        let results = SuiteResults {
            total: 5,
            passed: 4,
            failed: 1,
            skipped: 0,
            duration_ms: 1500,
            cases: vec![],
        };
        assert!(!results.all_passed());
        assert!(results.summary().contains("4/5 passed"));
    }
}
