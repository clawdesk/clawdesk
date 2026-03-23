//! # Post-Turn Validation
//!
//! Verifies the agent's claims before publishing the response.
//!
//! When an agent says "all tests pass" or "file created", the validator
//! independently runs verification commands to confirm. If claims fail,
//! the response is annotated with a validation report.
//!
//! ## Architecture
//!
//! ```text
//! Agent completes → extract claims → spawn validator → verify each → annotate response
//! ```
//!
//! The validator uses a lightweight sub-agent with read-only tools
//! (shell_exec, file_read, file_list) to independently check claims.
//! It never modifies files — only reads and runs test commands.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::runner::AgentResponse;
use crate::AgentConfig;
use clawdesk_providers::Provider;

// ───────────────────────────────────────────────────────────────
// Configuration
// ───────────────────────────────────────────────────────────────

/// Controls when and how post-turn validation runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    /// Whether validation is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Minimum number of tool calls in the turn to trigger validation.
    /// Skips validation for simple Q&A turns with no tool use.
    #[serde(default = "default_min_tool_calls")]
    pub min_tool_calls: usize,
    /// Maximum time (seconds) for the validator sub-agent.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Maximum tool rounds the validator can use.
    #[serde(default = "default_max_rounds")]
    pub max_validator_rounds: usize,
}

fn default_min_tool_calls() -> usize { 2 }
fn default_timeout() -> u64 { 60 }
fn default_max_rounds() -> usize { 8 }

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_tool_calls: default_min_tool_calls(),
            timeout_secs: default_timeout(),
            max_validator_rounds: default_max_rounds(),
        }
    }
}

// ───────────────────────────────────────────────────────────────
// Claim types
// ───────────────────────────────────────────────────────────────

/// A single verifiable claim extracted from the agent's response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    #[serde(alias = "claim")]
    pub text: String,
    pub status: ClaimStatus,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ClaimStatus {
    Pass,
    Fail,
    Skipped,
    /// LLMs sometimes return PARTIAL — treat as Skipped.
    Partial,
    /// Catch-all for unexpected statuses.
    #[serde(other)]
    Unknown,
}

/// Full validation report appended to the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub verified: bool,
    pub claims: Vec<Claim>,
    pub summary: String,
}

impl std::fmt::Display for ValidationReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let passed = self.claims.iter().filter(|c| c.status == ClaimStatus::Pass).count();
        let failed = self.claims.iter().filter(|c| c.status == ClaimStatus::Fail).count();
        let icon = if self.verified { "✅" } else { "⚠️" };

        writeln!(f)?;
        writeln!(f, "─── Validation Report {icon} ───")?;
        for claim in &self.claims {
            let status_icon = match claim.status {
                ClaimStatus::Pass => "✓",
                ClaimStatus::Fail => "✗",
                ClaimStatus::Skipped | ClaimStatus::Partial | ClaimStatus::Unknown => "○",
            };
            writeln!(f, "  {status_icon} {}", claim.text)?;
            if !claim.evidence.is_empty() && claim.status == ClaimStatus::Fail {
                // Indent evidence lines
                for line in claim.evidence.lines().take(3) {
                    writeln!(f, "    │ {line}")?;
                }
            }
        }
        writeln!(f, "  Result: {passed} passed, {failed} failed")?;
        writeln!(f, "───────────────────────────")?;
        Ok(())
    }
}

// ───────────────────────────────────────────────────────────────
// Validator
// ───────────────────────────────────────────────────────────────

/// Post-turn validator that spawns a sub-agent to verify claims.
pub struct PostTurnValidator {
    pub config: ValidationConfig,
    provider: Arc<dyn Provider>,
    workspace: Option<PathBuf>,
}

impl PostTurnValidator {
    pub fn new(
        config: ValidationConfig,
        provider: Arc<dyn Provider>,
        workspace: Option<PathBuf>,
    ) -> Self {
        Self { config, provider, workspace }
    }

    /// Check whether this response warrants validation.
    fn should_validate(&self, response: &AgentResponse) -> bool {
        if !self.config.enabled {
            return false;
        }
        // Only validate turns that used tools (skip pure text responses)
        if response.total_rounds < self.config.min_tool_calls {
            return false;
        }
        // Must have substantive content to extract claims from
        if response.content.trim().len() < 20 {
            return false;
        }
        true
    }

    /// Run validation on the agent's response.
    /// Returns the (possibly annotated) response and the validation report.
    pub fn validate(
        &self,
        response: AgentResponse,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = (AgentResponse, Option<ValidationReport>)> + Send + '_>> {
        Box::pin(async move {
        if !self.should_validate(&response) {
            debug!("validation skipped: criteria not met");
            return (response, None);
        }

        info!("post-turn validation: spawning validator sub-agent");

        let validator_prompt = self.build_validator_prompt(&response.content);

        // Build a read-only tool registry for the validator
        let mut sub_tools = crate::tools::ToolRegistry::new();
        crate::builtin_tools::register_builtin_tools(
            &mut sub_tools,
            self.workspace.clone(),
        );

        let sub_config = AgentConfig {
            model: String::new(), // inherits from provider
            system_prompt: String::new(),
            max_tool_rounds: self.config.max_validator_rounds,
            ..Default::default()
        };

        let cancel = tokio_util::sync::CancellationToken::new();
        let runner = crate::runner::AgentRunner::new(
            Arc::clone(&self.provider),
            Arc::new(sub_tools),
            sub_config,
            cancel.clone(),
        );

        let history = vec![clawdesk_providers::ChatMessage::new(
            clawdesk_providers::MessageRole::User,
            validator_prompt.as_str(),
        )];

        let timeout = tokio::time::Duration::from_secs(self.config.timeout_secs);
        let result = tokio::time::timeout(
            timeout,
            runner.run(history, Self::validator_system_prompt().to_string()),
        )
        .await;

        let report = match result {
            Ok(Ok(validator_response)) => {
                self.parse_validation_response(&validator_response.content)
            }
            Ok(Err(e)) => {
                warn!("validator sub-agent error: {e}");
                None
            }
            Err(_) => {
                warn!("validator sub-agent timed out after {}s", self.config.timeout_secs);
                None
            }
        };

        // Annotate the original response if validation found failures
        let mut annotated = response;
        if let Some(ref report) = report {
            if !report.verified {
                let annotation = format!("{report}");
                annotated.content = format!("{}\n{annotation}", annotated.content);
                warn!(
                    "validation FAILED: {}",
                    report.summary
                );
            } else {
                info!("validation PASSED: {}", report.summary);
            }
        }

        (annotated, report)
        }) // end Box::pin
    }

    fn build_validator_prompt(&self, agent_response: &str) -> String {
        let workspace_hint = self.workspace
            .as_ref()
            .map(|w| format!("\nWorkspace: {}", w.display()))
            .unwrap_or_default();

        format!(
            "Verify the following agent response. Extract every testable claim and \
             verify each one by running actual commands.\n\
             {workspace_hint}\n\n\
             --- AGENT RESPONSE ---\n\
             {agent_response}\n\
             --- END AGENT RESPONSE ---\n\n\
             For each claim:\n\
             1. Identify the specific testable assertion\n\
             2. Run the verification command (e.g., pytest, ls, cat, python -c)\n\
             3. Compare actual output vs claimed output\n\n\
             Respond with ONLY a JSON object (no other text):\n\
             ```json\n\
             {{\n\
               \"verified\": true/false,\n\
               \"claims\": [\n\
                 {{\"claim\": \"description\", \"status\": \"PASS\" or \"FAIL\" or \"SKIPPED\", \"evidence\": \"actual output\"}}\n\
               ],\n\
               \"summary\": \"X/Y claims verified\"\n\
             }}\n\
             ```\n\
             IMPORTANT: status must be exactly one of: PASS, FAIL, or SKIPPED. Do not use other values."
        )
    }

    fn validator_system_prompt() -> &'static str {
        "You are a strict verification agent. Your ONLY job is to verify claims made by another agent.\n\n\
         RULES:\n\
         - NEVER trust the agent's word. Always verify independently by running commands.\n\
         - If the agent says 'all tests pass', YOU run the tests and check output.\n\
         - If the agent says 'file created at X', YOU run `ls -la X` or `cat X`.\n\
         - If the agent says 'bug fixed', YOU run the code to confirm.\n\
         - Do NOT modify any files. Only read and test.\n\
         - Be fast. Max 5 verification commands.\n\
         - Always output valid JSON with the exact schema requested."
    }

    /// Parse the validator's response to extract the structured report.
    fn parse_validation_response(&self, content: &str) -> Option<ValidationReport> {
        // Try to extract JSON from the response (may be wrapped in ```json blocks)
        let json_str = if let Some(start) = content.find('{') {
            let depth_start = start;
            let mut depth = 0i32;
            let mut end = start;
            for (i, ch) in content[depth_start..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = depth_start + i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            &content[depth_start..end]
        } else {
            content
        };

        match serde_json::from_str::<ValidationReport>(json_str) {
            Ok(report) => Some(report),
            Err(e) => {
                warn!("failed to parse validator response as JSON: {e}");
                debug!("raw validator output: {content}");
                // Build a fallback report from text analysis
                Some(ValidationReport {
                    verified: false,
                    claims: vec![Claim {
                        text: "validator response".into(),
                        status: ClaimStatus::Skipped,
                        evidence: format!("Could not parse validator output: {e}"),
                    }],
                    summary: "Validation inconclusive — could not parse validator output".into(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claim_status_serialize() {
        let claim = Claim {
            text: "all tests pass".into(),
            status: ClaimStatus::Fail,
            evidence: "5 tests failed".into(),
        };
        let json = serde_json::to_string(&claim).unwrap();
        assert!(json.contains("\"FAIL\""));
    }

    #[test]
    fn test_validation_report_display() {
        let report = ValidationReport {
            verified: false,
            claims: vec![
                Claim {
                    text: "all tests pass".into(),
                    status: ClaimStatus::Fail,
                    evidence: "5/13 tests failed\ntest_load_csv FAILED\ntest_validation FAILED".into(),
                },
                Claim {
                    text: "file created".into(),
                    status: ClaimStatus::Pass,
                    evidence: String::new(),
                },
            ],
            summary: "1/2 claims verified".into(),
        };
        let output = format!("{report}");
        assert!(output.contains("✗ all tests pass"));
        assert!(output.contains("✓ file created"));
        assert!(output.contains("1 passed, 1 failed"));
    }

    #[test]
    fn test_parse_json_from_markdown() {
        let config = ValidationConfig::default();
        // We can't create a real provider for unit tests, so just test the JSON parser
        let validator = PostTurnValidator {
            config,
            provider: Arc::new(MockProvider),
            workspace: None,
        };

        let input = r#"Here's the report:
```json
{"verified": false, "claims": [{"claim": "tests pass", "status": "FAIL", "evidence": "5 failed"}], "summary": "0/1"}
```
"#;
        let report = validator.parse_validation_response(input).unwrap();
        assert!(!report.verified);
        assert_eq!(report.claims.len(), 1);
        assert_eq!(report.claims[0].status, ClaimStatus::Fail);
    }

    /// Minimal mock provider for unit tests.
    struct MockProvider;

    #[async_trait::async_trait]
    impl Provider for MockProvider {
        async fn complete(
            &self,
            _request: clawdesk_providers::ProviderRequest,
        ) -> Result<clawdesk_providers::ProviderResponse, clawdesk_providers::ProviderError> {
            unimplemented!("mock")
        }

        async fn stream(
            &self,
            _request: clawdesk_providers::ProviderRequest,
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<
                            Item = Result<
                                clawdesk_providers::StreamChunk,
                                clawdesk_providers::ProviderError,
                            >,
                        > + Send,
                >,
            >,
            clawdesk_providers::ProviderError,
        > {
            unimplemented!("mock")
        }

        fn name(&self) -> &str {
            "mock"
        }
    }

    #[test]
    fn test_default_config() {
        let config = ValidationConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.min_tool_calls, 2);
        assert_eq!(config.timeout_secs, 60);
        assert_eq!(config.max_validator_rounds, 8);
    }

    #[test]
    fn test_should_validate_disabled() {
        let config = ValidationConfig::default();
        let validator = PostTurnValidator {
            config,
            provider: Arc::new(MockProvider),
            workspace: None,
        };
        let response = AgentResponse {
            content: "all done!".into(),
            total_rounds: 5,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            finish_reason: clawdesk_providers::FinishReason::Stop,
            tool_messages: vec![],
            segments: vec![],
            active_skills: vec![],
            messaging_sends: vec![],
        };
        assert!(!validator.should_validate(&response));
    }
}
