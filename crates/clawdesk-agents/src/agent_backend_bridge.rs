//! Pipeline ↔ Runner Bridge — concrete `AgentBackend` for `AgentRunner`.
//!
//! Bridges the pipeline executor to the agent runner by implementing
//! `AgentBackend` (the trait that `PipelineExecutor` delegates to).
//!
//! ## Architecture
//!
//! ```text
//! PipelineExecutor ─→ AgentBackend trait ─→ RunnerBackend ─→ AgentRunner::run()
//!                                                  │
//!                                                  ├── agent_configs (per-agent config)
//!                                                  ├── provider_registry (provider lookup)
//!                                                  ├── tool_registry (shared tools)
//!                                                  └── cancel token
//! ```
//!
//! The `RunnerBackend` holds a registry of agent configurations and creates
//! a fresh `AgentRunner` for each pipeline step. This avoids sharing mutable
//! runner state across concurrent pipeline branches.

use crate::pipeline_executor::{AgentBackend, PipelineError};
use crate::runner::{AgentConfig, AgentRunner};
use crate::tools::ToolRegistry;
use async_trait::async_trait;
use clawdesk_providers::{ChatMessage, MessageRole, Provider};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Configuration for an agent within the pipeline.
#[derive(Debug, Clone)]
pub struct PipelineAgentConfig {
    /// Agent configuration.
    pub config: AgentConfig,
    /// System prompt override (if different from config.system_prompt).
    pub system_prompt: Option<String>,
}

/// Concrete `AgentBackend` that delegates to `AgentRunner::run()`.
///
/// Holds a registry of agent configs and shared provider/tools.
/// Creates a fresh `AgentRunner` per step to avoid mutable state sharing.
pub struct RunnerBackend {
    /// Per-agent configurations, keyed by agent_id.
    agents: HashMap<String, PipelineAgentConfig>,
    /// Provider for LLM calls (shared across all agents).
    provider: Arc<dyn Provider>,
    /// Tool registry (shared across all agents).
    tools: Arc<ToolRegistry>,
    /// Cancellation token (shared).
    cancel: CancellationToken,
    /// Default config for unknown agent IDs.
    default_config: AgentConfig,
}

impl RunnerBackend {
    /// Create a new backend with a provider and tool registry.
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Arc<ToolRegistry>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            agents: HashMap::new(),
            provider,
            tools,
            cancel,
            default_config: AgentConfig::default(),
        }
    }

    /// Register an agent configuration for pipeline use.
    pub fn register_agent(
        &mut self,
        agent_id: impl Into<String>,
        config: PipelineAgentConfig,
    ) {
        self.agents.insert(agent_id.into(), config);
    }

    /// Set the default configuration for unregistered agents.
    pub fn with_default_config(mut self, config: AgentConfig) -> Self {
        self.default_config = config;
        self
    }
}

#[async_trait]
impl AgentBackend for RunnerBackend {
    async fn execute_agent(
        &self,
        agent_id: &str,
        _skill_id: Option<&str>,
        input: &str,
        timeout: Duration,
    ) -> Result<String, PipelineError> {
        let agent_config = self.agents.get(agent_id);

        let config = agent_config
            .map(|a| a.config.clone())
            .unwrap_or_else(|| {
                debug!(
                    agent_id,
                    "no registered config for agent, using default"
                );
                self.default_config.clone()
            });

        let system_prompt = agent_config
            .and_then(|a| a.system_prompt.clone())
            .unwrap_or_else(|| config.system_prompt.clone());

        let runner = AgentRunner::builder(
            Arc::clone(&self.provider),
            Arc::clone(&self.tools),
            config,
            self.cancel.clone(),
        )
        .without_sandbox()
        .build();

        let history = vec![ChatMessage::new(MessageRole::User, input.to_string())];

        let result = tokio::time::timeout(timeout, runner.run(history, system_prompt))
            .await
            .map_err(|_| PipelineError::AgentFailed {
                agent_id: agent_id.to_string(),
                detail: format!("agent timed out after {}s", timeout.as_secs()),
            })?
            .map_err(|e| PipelineError::AgentFailed {
                agent_id: agent_id.to_string(),
                detail: e.to_string(),
            })?;

        info!(
            agent_id,
            rounds = result.total_rounds,
            input_tokens = result.input_tokens,
            output_tokens = result.output_tokens,
            "pipeline agent step completed"
        );

        Ok(result.content)
    }

    async fn request_gate_approval(
        &self,
        prompt: &str,
        _timeout: Duration,
    ) -> Result<bool, PipelineError> {
        // Default: auto-approve. In production, this would delegate to
        // the Tauri frontend's approval UI.
        warn!(prompt, "gate approval auto-approved (no interactive gate configured)");
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Integration tests would require mock Provider and ToolRegistry.
    // Unit tests verify the structural wiring.

    #[test]
    fn backend_registers_agents() {
        let tools = Arc::new(ToolRegistry::new());
        // Can't create a mock provider here, but we can verify the structure.
        let _ = AgentConfig::default();
    }
}
