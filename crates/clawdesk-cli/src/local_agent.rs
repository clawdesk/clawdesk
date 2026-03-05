//! Local Agent Mode — Run Agent Loop In-Process.
//!
//! Provides a Claude Code-equivalent CLI experience: the full agent loop
//! runs in the same process as the terminal with direct tool execution
//! and terminal-based approval prompts.
//!
//! ```text
//! clawdesk agent run [--model MODEL] [--allow-all-tools] [--workspace PATH]
//! ```
//!
//! The agent reads from stdin, executes tools locally, and streams responses
//! to stdout. No gateway, no HTTP, no Tauri dependency.

use crate::cli_approval::CliApprovalGate;
use crate::permission_modes::{PermissionConfig, PermissionDecision, PermissionEngine, PermissionMode};
use clawdesk_agents::runner::{AgentConfig, AgentEvent, AgentRunner, ApprovalDecision, ApprovalGate};
use clawdesk_agents::tools::ToolPolicy;
use clawdesk_agents::ToolRegistry;
use clawdesk_providers::Provider;
use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Configuration for the local agent mode.
pub struct LocalAgentConfig {
    /// Model to use (e.g. "claude-sonnet-4-20250514").
    pub model: Option<String>,
    /// Auto-approve all tools (--allow-all-tools / CI mode).
    pub allow_all_tools: bool,
    /// Workspace directory for file operations.
    pub workspace: Option<PathBuf>,
    /// System prompt override.
    pub system_prompt: Option<String>,
    /// Maximum tool rounds per turn.
    pub max_tool_rounds: usize,
    /// Context window size.
    pub context_limit: usize,
    /// Permission configuration (from config.toml).
    pub permission_config: PermissionConfig,
}

impl Default for LocalAgentConfig {
    fn default() -> Self {
        Self {
            model: None,
            allow_all_tools: false,
            workspace: None,
            system_prompt: None,
            max_tool_rounds: 25,
            context_limit: 128_000,
            permission_config: PermissionConfig::default(),
        }
    }
}

/// Build the approval gate based on configuration.
fn build_approval_gate(config: &LocalAgentConfig) -> Arc<dyn ApprovalGate> {
    if config.allow_all_tools {
        info!("running in permissive mode — all tools auto-approved");
        Arc::new(CliApprovalGate::permissive())
    } else {
        match config.permission_config.mode {
            PermissionMode::Unattended => {
                info!("running in unattended mode — all tools auto-approved");
                Arc::new(CliApprovalGate::permissive())
            }
            PermissionMode::Allowlist => {
                info!("running in allowlist mode — matching patterns auto-approved");
                // In allowlist mode, we still use the interactive gate for non-matching tools
                // The permission engine is checked first in the wrapper gate
                let engine = PermissionEngine::new(&config.permission_config);
                Arc::new(AllowlistAwareGate {
                    engine,
                    fallback: CliApprovalGate::new(),
                })
            }
            PermissionMode::Interactive => {
                info!("running in interactive mode — dangerous tools prompt for approval");
                Arc::new(CliApprovalGate::new())
            }
        }
    }
}

/// Gate that checks the permission engine first, then falls back to interactive.
struct AllowlistAwareGate {
    engine: PermissionEngine,
    fallback: CliApprovalGate,
}

#[async_trait::async_trait]
impl ApprovalGate for AllowlistAwareGate {
    async fn request_approval(
        &self,
        tool_name: &str,
        arguments: &str,
    ) -> Result<ApprovalDecision, String> {
        match self.engine.evaluate(tool_name, arguments) {
            PermissionDecision::AutoApprove => {
                debug!(tool = tool_name, "auto-approved by allowlist");
                Ok(ApprovalDecision::AllowForSession)
            }
            PermissionDecision::NeedsPrompt => {
                self.fallback.request_approval(tool_name, arguments).await
            }
            PermissionDecision::Denied(reason) => {
                warn!(tool = tool_name, reason = %reason, "denied by permission engine");
                Ok(ApprovalDecision::Deny)
            }
        }
    }
}

/// Build a tool policy that marks dangerous tools as require_approval.
fn build_tool_policy(allow_all: bool) -> ToolPolicy {
    if allow_all {
        ToolPolicy::default() // Empty require_approval = no prompts needed
    } else {
        let mut policy = ToolPolicy::default();
        policy.require_approval = [
            "shell_exec", "shell", "file_write", "http", "http_fetch",
            "message_send", "sessions_send", "spawn_subagent", "dynamic_spawn",
            "email_send", "process_start",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<HashSet<_>>();
        policy
    }
}

/// Resolve the LLM provider from environment variables.
///
/// Priority: ANTHROPIC_API_KEY → OPENAI_API_KEY → AZURE_OPENAI_API_KEY → OPENROUTER_API_KEY
fn resolve_provider(model: &str) -> Result<Arc<dyn Provider>, String> {
    // Anthropic
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        info!(provider = "anthropic", "using Anthropic API");
        return Ok(Arc::new(clawdesk_providers::anthropic::AnthropicProvider::new(
            key,
            Some(model.to_string()),
        )));
    }

    // OpenAI
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let base_url = std::env::var("OPENAI_BASE_URL").ok();
        info!(provider = "openai", "using OpenAI API");
        return Ok(Arc::new(clawdesk_providers::openai::OpenAiProvider::new(
            key,
            base_url,
            Some(model.to_string()),
        )));
    }

    // Azure OpenAI
    if let Ok(key) = std::env::var("AZURE_OPENAI_API_KEY") {
        let endpoint = std::env::var("AZURE_OPENAI_ENDPOINT")
            .map_err(|_| "AZURE_OPENAI_API_KEY is set but AZURE_OPENAI_ENDPOINT is missing")?;
        let api_version = std::env::var("AZURE_OPENAI_API_VERSION").ok();
        info!(provider = "azure_openai", "using Azure OpenAI API");
        return Ok(Arc::new(clawdesk_providers::azure::AzureOpenAiProvider::new(
            key,
            endpoint,
            api_version,
            Some(model.to_string()),
        )));
    }

    // OpenRouter
    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        info!(provider = "openrouter", "using OpenRouter API");
        return Ok(Arc::new(clawdesk_providers::openrouter::OpenRouterProvider::new(
            key,
        )));
    }

    Err(
        "No API key found. Set one of: ANTHROPIC_API_KEY, OPENAI_API_KEY, AZURE_OPENAI_API_KEY, OPENROUTER_API_KEY"
            .to_string(),
    )
}

/// Run the local agent REPL.
///
/// This is the main entry point for `clawdesk agent run`. It:
/// 1. Resolves the LLM provider from env vars
/// 2. Registers all builtin tools
/// 3. Opens SochDB for memory persistence
/// 4. Starts an interactive REPL loop
pub async fn run_local_agent(
    config: LocalAgentConfig,
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Resolve provider
    let model = config.model.clone().unwrap_or_else(|| "claude-sonnet-4-20250514".to_string());
    let provider = resolve_provider(&model)?;

    // Register tools
    let mut tool_registry = ToolRegistry::new();
    clawdesk_agents::builtin_tools::register_builtin_tools(
        &mut tool_registry,
        config.workspace.clone(),
    );
    let tool_registry = Arc::new(tool_registry);

    // Build agent config
    let agent_config = AgentConfig {
        model: model.clone(),
        system_prompt: config.system_prompt.clone().unwrap_or_else(|| {
            "You are a powerful AI assistant running in a local terminal. \
             You have access to tools for file reading/writing, shell command execution, \
             HTTP requests, and web search. Use tools proactively to accomplish tasks. \
             When the user asks you to do something, DO it — don't just describe what \
             you would do. Execute commands, write files, and take concrete actions."
                .to_string()
        }),
        max_tool_rounds: config.max_tool_rounds,
        context_limit: config.context_limit,
        workspace_path: config.workspace.as_ref().map(|p| p.display().to_string()),
        ..Default::default()
    };

    // Build approval gate
    let gate = build_approval_gate(&config);

    // Build tool policy
    let policy = build_tool_policy(config.allow_all_tools);

    // Event channel for streaming output
    let (event_tx, _) = broadcast::channel::<AgentEvent>(256);

    // Conversation history
    let mut history: Vec<clawdesk_providers::ChatMessage> = Vec::new();
    let system_prompt = agent_config.system_prompt.clone();

    // Print header
    println!("ClawDesk Agent (local mode)");
    println!("Model: {}", model);
    if let Some(ref ws) = config.workspace {
        println!("Workspace: {}", ws.display());
    }
    let mode_str = if config.allow_all_tools {
        "permissive (all tools auto-approved)"
    } else {
        match config.permission_config.mode {
            PermissionMode::Interactive => "interactive",
            PermissionMode::Allowlist => "allowlist",
            PermissionMode::Unattended => "unattended",
        }
    };
    println!("Permission mode: {}", mode_str);
    println!("Type 'exit' or Ctrl+C to quit.");
    println!("─────────────────────────────────────");
    println!();

    // REPL loop
    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Read user input
        print!("You: ");
        io::stdout().flush()?;

        let input = {
            let stdin = io::stdin();
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => line.trim().to_string(),
                Err(e) => {
                    error!(error = %e, "failed to read input");
                    break;
                }
            }
        };

        if input.is_empty() {
            continue;
        }
        if input == "exit" || input == "quit" || input == "/exit" || input == "/quit" {
            println!("Goodbye.");
            break;
        }

        // Add user message to history
        history.push(clawdesk_providers::ChatMessage::new(
            clawdesk_providers::MessageRole::User,
            input.as_str(),
        ));

        // Build runner for this turn
        let runner = AgentRunner::new(
            Arc::clone(&provider),
            Arc::clone(&tool_registry),
            agent_config.clone(),
            cancel.child_token(),
        )
        .with_approval_gate(Arc::clone(&gate))
        .with_tool_policy(Arc::new(policy.clone()))
        .with_events(event_tx.clone());

        // Subscribe to events for streaming output
        let mut event_rx = event_tx.subscribe();
        let stream_handle = tokio::spawn(async move {
            let mut streaming = false;
            while let Ok(event) = event_rx.recv().await {
                match event {
                    AgentEvent::StreamChunk { text, done } => {
                        if !streaming {
                            print!("\nAgent: ");
                            streaming = true;
                        }
                        print!("{}", text);
                        let _ = io::stdout().flush();
                        if done {
                            println!();
                        }
                    }
                    AgentEvent::ToolStart { name, .. } => {
                        eprintln!("  [tool] {} ...", name);
                    }
                    AgentEvent::ToolEnd { name, success, duration_ms } => {
                        if success {
                            eprintln!("  [tool] {} ✓ ({}ms)", name, duration_ms);
                        } else {
                            eprintln!("  [tool] {} ✗ ({}ms)", name, duration_ms);
                        }
                    }
                    AgentEvent::Error { error } => {
                        eprintln!("  [error] {}", error);
                    }
                    AgentEvent::Done { total_rounds } => {
                        debug!(rounds = total_rounds, "turn complete");
                        break;
                    }
                    _ => {}
                }
            }
        });

        // Run the agent
        match runner.run(history.clone(), system_prompt.clone()).await {
            Ok(response) => {
                // Wait for stream handler to finish
                let _ = stream_handle.await;

                // If streaming didn't print the response, print it now
                if !response.content.is_empty() {
                    // Check if we already streamed (the stream handler will have printed)
                    // For non-streaming providers, print the full response
                    println!("\nAgent: {}", response.content);
                }

                // Add assistant message to history
                history.push(clawdesk_providers::ChatMessage::new(
                    clawdesk_providers::MessageRole::Assistant,
                    response.content.as_str(),
                ));

                // Add tool messages to history (preserves multi-turn context)
                for msg in response.tool_messages {
                    history.push(msg);
                }

                println!();
            }
            Err(e) => {
                // Cancel the stream handler
                let _ = stream_handle.abort();
                eprintln!("\n[error] {}", e);
                println!();
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = LocalAgentConfig::default();
        assert!(config.model.is_none());
        assert!(!config.allow_all_tools);
        assert!(config.workspace.is_none());
        assert_eq!(config.max_tool_rounds, 25);
    }

    #[test]
    fn tool_policy_permissive() {
        let policy = build_tool_policy(true);
        assert!(policy.require_approval.is_empty());
    }

    #[test]
    fn tool_policy_interactive() {
        let policy = build_tool_policy(false);
        assert!(policy.requires_approval("shell_exec"));
        assert!(policy.requires_approval("file_write"));
        assert!(policy.requires_approval("http"));
        assert!(policy.requires_approval("message_send"));
        assert!(policy.requires_approval("email_send"));
        assert!(!policy.requires_approval("file_read"));
        assert!(!policy.requires_approval("web_search"));
    }

    #[test]
    fn gate_permissive_when_allow_all() {
        let config = LocalAgentConfig {
            allow_all_tools: true,
            ..Default::default()
        };
        // The gate should be permissive
        let _gate = build_approval_gate(&config);
        // Can't easily test async behavior in sync test, but construction shouldn't panic
    }

    #[test]
    fn gate_allowlist_mode() {
        let config = LocalAgentConfig {
            permission_config: PermissionConfig {
                mode: PermissionMode::Allowlist,
                allowlist: {
                    let mut m = std::collections::HashMap::new();
                    m.insert("shell_exec".to_string(), vec!["git *".to_string()]);
                    m
                },
            },
            ..Default::default()
        };
        let _gate = build_approval_gate(&config);
    }
}
