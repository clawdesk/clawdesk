//! Test runner with mock provider for deterministic replay.

use crate::assertions::Assertion;
use crate::suite::{CaseResult, StepResult, SuiteResults, TestCase, TestSuite};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info};

// ---------------------------------------------------------------------------
// Mock responder
// ---------------------------------------------------------------------------

/// A mock responder that produces deterministic agent responses.
///
/// Responses are consumed in order. If exhausted, returns a fallback.
pub struct MockResponder {
    responses: Vec<String>,
    index: AtomicUsize,
    pub fallback: String,
}

impl MockResponder {
    /// Create with a scripted list of responses.
    pub fn scripted(responses: Vec<impl Into<String>>) -> Self {
        Self {
            responses: responses.into_iter().map(|r| r.into()).collect(),
            index: AtomicUsize::new(0),
            fallback: "[mock: no more responses]".to_string(),
        }
    }

    /// Create with a single repeated response.
    pub fn echo(response: impl Into<String>) -> Self {
        let resp = response.into();
        Self {
            responses: vec![resp.clone()],
            index: AtomicUsize::new(0),
            fallback: resp,
        }
    }

    /// Get the next response.
    pub fn next_response(&self) -> String {
        let idx = self.index.fetch_add(1, Ordering::SeqCst);
        if idx < self.responses.len() {
            self.responses[idx].clone()
        } else {
            self.fallback.clone()
        }
    }

    /// Reset the response index.
    pub fn reset(&self) {
        self.index.store(0, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Result of a full test run (all cases).
pub type RunResult = SuiteResults;

// ---------------------------------------------------------------------------
// Test runner
// ---------------------------------------------------------------------------

/// Executes test cases against a mock responder.
///
/// The runner simulates a conversation loop:
/// 1. Send user message
/// 2. Get agent response (from mock responder)
/// 3. Evaluate assertions
pub struct TestRunner {
    /// Maximum wall-clock time per test case.
    pub timeout: std::time::Duration,
}

impl TestRunner {
    pub fn new() -> Self {
        Self {
            timeout: std::time::Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run an entire test suite.
    pub async fn run_suite(
        &self,
        suite: &TestSuite,
        responder: &MockResponder,
    ) -> RunResult {
        let start = std::time::Instant::now();
        let mut case_results = Vec::new();

        for tc in &suite.cases {
            let result = self.run_case(tc, responder).await;
            case_results.push(result);
        }

        let passed = case_results.iter().filter(|c| c.passed).count();
        let failed = case_results.iter().filter(|c| !c.passed).count();

        RunResult {
            total: case_results.len(),
            passed,
            failed,
            skipped: 0,
            duration_ms: start.elapsed().as_millis() as u64,
            cases: case_results,
        }
    }

    /// Run a single test case.
    pub async fn run_case(
        &self,
        tc: &TestCase,
        responder: &MockResponder,
    ) -> CaseResult {
        let start = std::time::Instant::now();
        let mut step_results = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        debug!(name = %tc.name, steps = tc.steps.len(), "running test case");

        // Build a fresh mock responder from setup if provided.
        let case_responder = if !tc.setup.mock_responses.is_empty() {
            Arc::new(MockResponder::scripted(tc.setup.mock_responses.clone()))
        } else {
            // For the shared responder, don't reset index — let it continue.
            // We'll use a wrapper that delegates.
            Arc::new(MockResponder::scripted(Vec::<String>::new()))
        };

        let mut conversation_history: Vec<(String, String)> = Vec::new();

        for (i, step) in tc.steps.iter().enumerate() {
            // Delay if configured.
            if step.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(step.delay_ms)).await;
            }

            // Get the agent response.
            let response = if let Some(ref mock) = step.mock_response {
                // Per-step override.
                mock.clone()
            } else if !tc.setup.mock_responses.is_empty() {
                case_responder.next_response()
            } else {
                responder.next_response()
            };

            conversation_history.push((step.user.clone(), response.clone()));

            // Evaluate assertions.
            let assertion_results = Assertion::evaluate(&response, &step.expect);
            let step_passed = assertion_results.iter().all(|r| r.passed);

            if !step_passed {
                let failed: Vec<_> = assertion_results.iter()
                    .filter(|r| !r.passed)
                    .map(|r| format!("{}: {}", r.label, r.detail))
                    .collect();
                errors.push(format!("step {}: {}", i, failed.join("; ")));
            }

            step_results.push(StepResult {
                step_index: i,
                user_input: step.user.clone(),
                agent_response: response,
                passed: step_passed,
                assertion_results,
            });
        }

        // Teardown assertions.
        if tc.teardown.assert_no_errors && !errors.is_empty() {
            // Already captured in errors.
        }

        if let Some(expected_turns) = tc.teardown.expected_turns {
            if step_results.len() != expected_turns {
                errors.push(format!(
                    "expected {} turns, got {}",
                    expected_turns,
                    step_results.len()
                ));
            }
        }

        let all_passed = errors.is_empty() && step_results.iter().all(|s| s.passed);

        if all_passed {
            info!(name = %tc.name, "✅ PASSED");
        } else {
            error!(name = %tc.name, errors = ?errors, "❌ FAILED");
        }

        CaseResult {
            name: tc.name.clone(),
            passed: all_passed,
            duration_ms: start.elapsed().as_millis() as u64,
            steps: step_results,
            errors,
        }
    }
}

impl Default for TestRunner {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_simple_test_case() {
        let yaml = r#"
name: "simple-test"
steps:
  - user: "hello"
    expect:
      contains: ["hi"]
setup:
  mock_responses:
    - "Hi there!"
"#;
        let tc = TestSuite::parse_yaml(yaml).unwrap();
        let responder = MockResponder::echo("fallback");
        let runner = TestRunner::new();

        let result = runner.run_case(&tc, &responder).await;
        assert!(result.passed);
        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.steps[0].agent_response, "Hi there!");
    }

    #[tokio::test]
    async fn failing_assertion() {
        let yaml = r#"
name: "fail-test"
steps:
  - user: "hello"
    expect:
      contains: ["goodbye"]
setup:
  mock_responses:
    - "Hi there!"
"#;
        let tc = TestSuite::parse_yaml(yaml).unwrap();
        let responder = MockResponder::echo("fallback");
        let runner = TestRunner::new();

        let result = runner.run_case(&tc, &responder).await;
        assert!(!result.passed);
        assert!(!result.steps[0].passed);
    }

    #[tokio::test]
    async fn multi_step_conversation() {
        let yaml = r#"
name: "multi-step"
setup:
  mock_responses:
    - "Hello Alice!"
    - "The answer is 4."
steps:
  - user: "My name is Alice"
    expect:
      contains: ["Alice"]
  - user: "What is 2+2?"
    expect:
      matches: ".*4.*"
"#;
        let tc = TestSuite::parse_yaml(yaml).unwrap();
        let responder = MockResponder::echo("fallback");
        let runner = TestRunner::new();

        let result = runner.run_case(&tc, &responder).await;
        assert!(result.passed);
        assert_eq!(result.steps.len(), 2);
    }

    #[tokio::test]
    async fn per_step_mock_override() {
        let yaml = r#"
name: "override-test"
steps:
  - user: "hello"
    mock_response: "custom response"
    expect:
      contains: ["custom"]
"#;
        let tc = TestSuite::parse_yaml(yaml).unwrap();
        let responder = MockResponder::echo("unused");
        let runner = TestRunner::new();

        let result = runner.run_case(&tc, &responder).await;
        assert!(result.passed);
    }

    #[tokio::test]
    async fn suite_run_and_results() {
        let yaml1 = r#"
name: "test-a"
setup:
  mock_responses: ["ok"]
steps:
  - user: "hi"
    expect:
      contains: ["ok"]
"#;
        let yaml2 = r#"
name: "test-b"
setup:
  mock_responses: ["fail"]
steps:
  - user: "hi"
    expect:
      contains: ["success"]
"#;
        let tc1 = TestSuite::parse_yaml(yaml1).unwrap();
        let tc2 = TestSuite::parse_yaml(yaml2).unwrap();
        let suite = TestSuite::from_cases(vec![tc1, tc2]);
        let responder = MockResponder::echo("unused");
        let runner = TestRunner::new();

        let results = runner.run_suite(&suite, &responder).await;
        assert_eq!(results.total, 2);
        assert_eq!(results.passed, 1);
        assert_eq!(results.failed, 1);
        assert!(!results.all_passed());
    }

    #[test]
    fn mock_responder_scripted() {
        let r = MockResponder::scripted(vec!["a", "b", "c"]);
        assert_eq!(r.next_response(), "a");
        assert_eq!(r.next_response(), "b");
        assert_eq!(r.next_response(), "c");
        assert_eq!(r.next_response(), "[mock: no more responses]");
    }

    #[test]
    fn mock_responder_reset() {
        let r = MockResponder::scripted(vec!["hello"]);
        assert_eq!(r.next_response(), "hello");
        r.reset();
        assert_eq!(r.next_response(), "hello");
    }
}
