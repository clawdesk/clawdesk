//! # clawdesk-test
//!
//! Agent testing framework with YAML test case definitions and
//! deterministic conversation replay.
//!
//! ## Test Case Format (YAML)
//!
//! ```yaml
//! name: "greet-user"
//! description: "Verify agent greets the user by name"
//! agent: "assistant"
//! timeout_secs: 30
//!
//! setup:
//!   system_prompt: "You are a helpful assistant."
//!   model: "mock"
//!
//! steps:
//!   - user: "Hi, my name is Alice"
//!     expect:
//!       contains: ["Alice", "hello"]
//!       not_contains: ["error"]
//!
//!   - user: "What is 2+2?"
//!     expect:
//!       matches: ".*4.*"
//!       max_tokens: 500
//!
//! teardown:
//!   assert_no_errors: true
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! use clawdesk_test::{TestSuite, MockResponder};
//!
//! let suite = TestSuite::from_yaml_dir("tests/agent_tests/").await?;
//! let responder = MockResponder::scripted(vec!["Hello Alice!", "4"]);
//! let results = suite.run_all(responder).await;
//! assert!(results.all_passed());
//! ```

pub mod runner;
pub mod suite;
pub mod assertions;
pub mod bench;
pub mod chaos;
pub mod contract;
pub mod coverage;
pub mod loadtest;
pub mod property;

pub use bench::{BenchConfig, BenchResult, RegressionReport, bench_sync, bench_async, check_regression};
pub use chaos::{FaultInjector, FaultConfig, ChaosScenario, FaultType};
pub use loadtest::{LoadTestConfig, LoadTestResult, RequestOutcome, run_load_test};

pub use assertions::{Assertion, AssertionResult};
pub use runner::{MockResponder, TestRunner, RunResult};
pub use suite::{TestCase, TestStep, TestSuite, StepExpectation, SuiteResults};
