//! CLI Provider — wraps `CliAgentRunner` as a `Provider` for profile rotation/failover.
//!
//! Bridges the CLI agent runner into the hexagonal provider architecture so
//! `ProfileRotator` failover works identically for CLI backends as for API backends.

use crate::cli_runner::{CliAgentRunner, CliBackendConfig};
use async_trait::async_trait;
use clawdesk_providers::{
    ChatMessage, FinishReason, MessageRole, Provider, ProviderResponse, StreamChunk,
    TokenUsage, ToolCall,
};
use clawdesk_security::KeychainProvider;
use clawdesk_types::error::ProviderError;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Provider implementation backed by an external CLI agent (Claude Code, Codex).
pub struct CliProvider {
    runner: Arc<CliAgentRunner>,
    name: String,
    model_name: String,
    keychain: Option<Arc<KeychainProvider>>,
}

impl CliProvider {
    /// Create a new CLI provider wrapping the given runner.
    pub fn new(runner: Arc<CliAgentRunner>, name: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            runner,
            name: name.into(),
            model_name: model_name.into(),
            keychain: None,
        }
    }

    /// Attach a keychain provider for credential auto-discovery.
    pub fn with_keychain(mut self, keychain: Arc<KeychainProvider>) -> Self {
        self.keychain = Some(keychain);
        self
    }

    /// Create a CliProvider for Claude Code CLI with auto-discovered credentials.
    pub fn claude_code(keychain: Arc<KeychainProvider>) -> Result<Self, String> {
        let config = CliBackendConfig {
            binary_path: "claude".to_string(),
            default_args: vec!["--print".to_string()],
            ..Default::default()
        };
        let runner = Arc::new(CliAgentRunner::new(config));
        Ok(Self {
            runner,
            name: "claude-code".into(),
            model_name: "claude-code-cli".into(),
            keychain: Some(keychain),
        })
    }

    /// Create a CliProvider for Codex CLI with auto-discovered credentials.
    pub fn codex(keychain: Arc<KeychainProvider>) -> Result<Self, String> {
        let config = CliBackendConfig {
            binary_path: "codex".to_string(),
            default_args: vec![],
            ..Default::default()
        };
        let runner = Arc::new(CliAgentRunner::new(config));
        Ok(Self {
            runner,
            name: "codex".into(),
            model_name: "codex-cli".into(),
            keychain: Some(keychain),
        })
    }
}

#[async_trait]
impl Provider for CliProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn models(&self) -> Vec<String> {
        vec![self.model_name.clone()]
    }

    async fn complete(
        &self,
        request: &clawdesk_providers::ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        // Extract last user message from request
        let user_msg = request.messages.iter().rev()
            .find(|m| m.role == MessageRole::User)
            .map(|m| m.content.to_string())
            .unwrap_or_default();

        let start = std::time::Instant::now();
        let result = self.runner.run(&user_msg).await
            .map_err(|e| ProviderError::network_error(&self.name, e))?;

        Ok(ProviderResponse {
            content: result.response,
            model: self.model_name.clone(),
            provider: self.name.clone(),
            usage: TokenUsage::default(),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            latency: start.elapsed(),
        })
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        // Check if the CLI binary exists on PATH
        let output = tokio::process::Command::new("which")
            .arg(&self.runner.config().binary_path)
            .output()
            .await
            .map_err(|e| ProviderError::network_error(&self.name, format!("which failed: {e}")))?;

        if output.status.success() {
            Ok(())
        } else {
            Err(ProviderError::network_error(
                &self.name,
                format!("CLI binary '{}' not found on PATH", self.runner.config().binary_path),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name() {
        let config = CliBackendConfig::default();
        let runner = Arc::new(CliAgentRunner::new(config));
        let provider = CliProvider::new(runner, "test-cli", "test-model");
        assert_eq!(provider.name(), "test-cli");
        assert_eq!(provider.models(), vec!["test-model"]);
    }
}
