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

use crate::agent_compose::{self, AgentDefinition};
use crate::cli_approval::CliApprovalGate;
use crate::permission_modes::{PermissionConfig, PermissionDecision, PermissionEngine, PermissionMode};
use clawdesk_agents::runner::{AgentConfig, AgentEvent, AgentRunner, ApprovalDecision, ApprovalGate};
use clawdesk_agents::tools::ToolPolicy;
use clawdesk_agents::ToolRegistry;
use clawdesk_providers::{ChatMessage, MessageRole, Provider};
use clawdesk_runtime::{ActivityJournal, Checkpoint, CheckpointStore, DurableAgentRunner, GuardSnapshot, LeaseManager, RetryPolicy, RunId, RunnerFactory, WorkflowRun, WorkflowType};
use clawdesk_sochdb::SochStore;
use clawdesk_types::channel::ChannelId;
use clawdesk_types::dirs;
use clawdesk_types::error::ClawDeskError;
use clawdesk_types::session::SessionKey;
use std::collections::{HashMap, HashSet};
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
    /// Optional team directory containing agent.toml files for multi-agent mode.
    pub team_dir: Option<PathBuf>,
    /// Consciousness preset: autonomous, balanced, supervised, paranoid.
    pub consciousness_preset: String,
    /// Enable post-turn validation to independently verify agent claims.
    pub validate: bool,
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
            team_dir: None,
            consciousness_preset: "balanced".to_string(),
            validate: false,
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

/// Auto-detect the best available model when none is explicitly specified.
///
/// Priority: channel_provider.json → Ollama first model → error
pub(crate) fn resolve_default_model() -> String {
    // 1. Check channel_provider.json for user-configured model
    let cp_path = clawdesk_types::dirs::dot_clawdesk().join("channel_provider.json");
    if let Ok(raw) = std::fs::read_to_string(&cp_path) {
        if let Ok(cp) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(model) = cp.get("model").and_then(|v| v.as_str()) {
                if !model.is_empty() {
                    return model.to_string();
                }
            }
        }
    }

    // 2. Check env vars for provider hints
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        return "claude-sonnet-4-20250514".to_string();
    }
    if std::env::var("OPENAI_API_KEY").is_ok() {
        return "gpt-4o".to_string();
    }

    // 3. Fallback to Ollama default
    "llama3.2".to_string()
}

/// Resolve the LLM provider from environment variables.
///
/// Priority: channel_provider.json → ANTHROPIC_API_KEY → OPENAI_API_KEY → AZURE_OPENAI_API_KEY → OPENROUTER_API_KEY → Ollama
pub(crate) fn resolve_provider(model: &str) -> Result<Arc<dyn Provider>, String> {
    // Handle "default"/"auto" sentinel: re-resolve to an actual model
    let effective_model = if model == "default" || model == "auto" || model.is_empty() {
        resolve_default_model()
    } else {
        model.to_string()
    };
    resolve_provider_for_model(&effective_model)
}

fn resolve_provider_for_model(model: &str) -> Result<Arc<dyn Provider>, String> {
    // Check channel_provider.json first for OpenAI-compatible servers
    let cp_path = clawdesk_types::dirs::dot_clawdesk().join("channel_provider.json");
    if let Ok(raw) = std::fs::read_to_string(&cp_path) {
        if let Ok(cp) = serde_json::from_str::<serde_json::Value>(&raw) {
            let cp_model = cp.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let base_url = cp.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
            let api_key = cp.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
            let provider_name = cp.get("provider").and_then(|v| v.as_str()).unwrap_or("");

            // If model matches the configured model, or model is the default,
            // use this provider
            if !base_url.is_empty() && (model == cp_model || model == "default") {
                if provider_name.contains("Local") || provider_name == "local_compatible" {
                    info!(provider = "local_compatible", base_url = %base_url, model = %model, "using Local (OpenAI Compatible)");
                    let config = clawdesk_providers::compatible::CompatibleConfig::new(
                        "local_compatible",
                        base_url,
                        api_key,
                    )
                    .with_default_model(model.to_string());
                    return Ok(Arc::new(
                        clawdesk_providers::compatible::OpenAiCompatibleProvider::new(config),
                    ));
                }
            }
        }
    }

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

    // Ollama (local models — always available if Ollama is running)
    {
        let host = std::env::var("OLLAMA_HOST")
            .unwrap_or_else(|_| "http://localhost:11434".to_string());
        // Quick TCP check to see if Ollama is listening
        let check_addr = host
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        if let Ok(_) = std::net::TcpStream::connect_timeout(
            &check_addr.parse().unwrap_or_else(|_| "127.0.0.1:11434".parse().unwrap()),
            std::time::Duration::from_secs(2),
        ) {
            info!(provider = "ollama", host = %host, model = %model, "using Ollama (local)");
            return Ok(Arc::new(clawdesk_providers::ollama::OllamaProvider::new(
                Some(host),
                Some(model.to_string()),
            )));
        }
    }

    Err(
        "No API key found and Ollama is not running. Set one of: ANTHROPIC_API_KEY, OPENAI_API_KEY, AZURE_OPENAI_API_KEY, OPENROUTER_API_KEY — or start Ollama"
            .to_string(),
    )
}

fn register_spawn_subagent_tool(
    registry: &mut ToolRegistry,
    provider: Arc<dyn Provider>,
    workspace: Option<PathBuf>,
    cancel: CancellationToken,
    agent_map: Arc<HashMap<String, AgentDefinition>>,
) {
    let spawn_fn: clawdesk_agents::port::AsyncPort<
        clawdesk_agents::port::SpawnSubAgentRequest,
        Result<String, String>,
    > = Arc::new(move |req: clawdesk_agents::port::SpawnSubAgentRequest| {
        let agent_id = req.agent_id;
        let task = req.task;
        let timeout_secs = req.timeout_secs;
        let provider = Arc::clone(&provider);
        let workspace = workspace.clone();
        let cancel = cancel.clone();
        let agents = Arc::clone(&agent_map);
        Box::pin(async move {
            let def = agents
                .get(&agent_id)
                .ok_or_else(|| format!("Sub-agent '{}' not found in team directory", agent_id))?;
            let system_prompt = if def.agent.persona.soul.is_empty() {
                format!(
                    "You are {}. {}",
                    def.agent.display_name, def.agent.persona.guidelines
                )
            } else {
                def.agent.persona.soul.clone()
            };

            let mut sub_tools = ToolRegistry::new();
            clawdesk_agents::builtin_tools::register_builtin_tools(&mut sub_tools, workspace);

            let sub_config = AgentConfig {
                model: def.agent.model.clone(),
                system_prompt: String::new(),
                max_tool_rounds: 15,
                ..Default::default()
            };
            let runner = AgentRunner::new(provider, Arc::new(sub_tools), sub_config, cancel);
            let history = vec![ChatMessage::new(MessageRole::User, task.as_str())];
            let timeout = tokio::time::Duration::from_secs(timeout_secs);
            match tokio::time::timeout(timeout, runner.run(history, system_prompt)).await {
                Ok(Ok(response)) => Ok(response.content),
                Ok(Err(error)) => Err(format!("Sub-agent error: {error}")),
                Err(_) => Err(format!("Sub-agent timed out after {}s", timeout_secs)),
            }
        })
    });

    clawdesk_agents::builtin_tools::register_subagent_tool(registry, spawn_fn);
}

struct LocalRunnerFactory {
    provider: Arc<dyn Provider>,
    workspace: Option<PathBuf>,
    approval_gate: Arc<dyn ApprovalGate>,
    tool_policy: Arc<ToolPolicy>,
    cancel: CancellationToken,
    team_agents: Option<Arc<HashMap<String, AgentDefinition>>>,
    conscious_gateway: Arc<clawdesk_conscious::ConsciousGateway>,
    post_turn_validator: Option<Arc<clawdesk_agents::post_turn_validator::PostTurnValidator>>,
}

impl RunnerFactory for LocalRunnerFactory {
    fn create_runner(&self, config: &AgentConfig) -> Result<AgentRunner, ClawDeskError> {
        let mut registry = ToolRegistry::new();
        clawdesk_agents::builtin_tools::register_builtin_tools(&mut registry, self.workspace.clone());
        if let Some(agent_map) = &self.team_agents {
            register_spawn_subagent_tool(
                &mut registry,
                Arc::clone(&self.provider),
                self.workspace.clone(),
                self.cancel.child_token(),
                Arc::clone(agent_map),
            );
        }

        let runner = AgentRunner::new(
                Arc::clone(&self.provider),
                Arc::new(registry),
                config.clone(),
                self.cancel.child_token(),
            )
            .with_approval_gate(Arc::clone(&self.approval_gate))
            .with_tool_policy(Arc::clone(&self.tool_policy))
            .with_conscious_gateway(Arc::clone(&self.conscious_gateway));

        let runner = if let Some(ref validator) = self.post_turn_validator {
            runner.with_post_turn_validator(Arc::clone(validator))
        } else {
            runner
        };

        Ok(runner)
    }
}

fn durable_task_meta_key(run_id: &RunId) -> String {
    format!("runtime:runs:{}:task_meta", run_id)
}

fn durable_task_checkpoint_note_key(run_id: &RunId) -> String {
    format!("runtime:runs:{}:checkpoint_note", run_id)
}

fn build_task_inputs(
    input: &serde_json::Value,
    fallback_system_prompt: &str,
) -> Result<(Vec<ChatMessage>, String, String), String> {
    let prompt = input
        .get("prompt")
        .or_else(|| input.get("task"))
        .or_else(|| input.get("instructions"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            if input.is_null() {
                None
            } else {
                serde_json::to_string_pretty(input).ok()
            }
        })
        .ok_or_else(|| "input.prompt, input.task, or input.instructions is required".to_string())?;

    let system_prompt = input
        .get("system_prompt")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("{}\n\n{}", fallback_system_prompt, value))
        .unwrap_or_else(|| fallback_system_prompt.to_string());

    let summary: String = prompt.chars().take(120).collect();
    Ok((
        vec![ChatMessage::new(MessageRole::User, prompt.as_str())],
        system_prompt,
        summary,
    ))
}

fn resolve_executor_prompt(
    executor_agent: &str,
    team_agents: Option<&Arc<HashMap<String, AgentDefinition>>>,
    default_system_prompt: &str,
) -> Result<(Option<String>, String), String> {
    if executor_agent == "default" {
        return Ok((None, default_system_prompt.to_string()));
    }

    let agents = team_agents.ok_or_else(|| {
        format!("executor_agent '{}' requested, but no team agents are loaded", executor_agent)
    })?;
    let def = agents
        .get(executor_agent)
        .ok_or_else(|| format!("executor_agent '{}' not found", executor_agent))?;
    let prompt = if def.agent.persona.soul.is_empty() {
        format!(
            "You are {}. {}",
            def.agent.display_name, def.agent.persona.guidelines
        )
    } else {
        def.agent.persona.soul.clone()
    };
    Ok((Some(def.agent.model.clone()), prompt))
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
    let model = config.model.clone().unwrap_or_else(|| resolve_default_model());
    let provider = resolve_provider(&model)?;

    // Register tools
    let mut tool_registry = ToolRegistry::new();
    clawdesk_agents::builtin_tools::register_builtin_tools(
        &mut tool_registry,
        config.workspace.clone(),
    );

    // ── Team agent loading & spawn_subagent registration ──────────────
    // If --team-dir is set, load agent.toml files, build a team roster
    // system prompt, and register spawn_subagent so the LLM can delegate.
    let mut team_roster_prompt = String::new();
    let mut team_agents: Option<Arc<HashMap<String, AgentDefinition>>> = None;
    if let Some(ref team_dir) = config.team_dir {
        match agent_compose::load_all_agents(team_dir) {
            Ok(agents) if !agents.is_empty() => {
                let agent_map = Arc::new(agents
                    .iter()
                    .map(|(_, def)| (def.agent.id.clone(), def.clone()))
                    .collect::<HashMap<_, _>>());
                info!(
                    team_dir = %team_dir.display(),
                    count = agent_map.len(),
                    "loaded team agents"
                );
                team_agents = Some(Arc::clone(&agent_map));

                // Build the team roster system prompt section
                team_roster_prompt.push_str(
                    "\n\n## Your Team — Agentic Delegation\n\n\
**MANDATORY**: You MUST use the `spawn_subagent` tool to delegate to specialists below. \
NEVER write specialist content yourself. For EVERY user request, your workflow is:\n\n\
1. **Analyze** the request and identify which specialist(s) are needed.\n\
2. **Delegate** by calling `spawn_subagent` for each specialist with a SPECIFIC, DETAILED task.\n\
   - Include expected deliverables, format, depth, and scope in each task.\n\
   - You can call `spawn_subagent` multiple times in a SINGLE response to run specialists in parallel.\n\
3. **Review** the results — if a result is incomplete, delegate again with more specifics.\n\
4. **Synthesize** a polished, unified response combining all specialist outputs.\n\n\
**CRITICAL RULES**:\n\
- You MUST call `spawn_subagent` at least once per user request\n\
- You are FORBIDDEN from writing business plans, marketing strategies, code, or other specialist content directly\n\
- If you find yourself writing content that a specialist could produce, STOP and delegate instead\n\n\
### Team Members\n\n"
                );
                for (_, def) in agent_map.iter() {
                    let a = &def.agent;
                    team_roster_prompt.push_str(&format!(
                        "- **{}** — agent_id: `{}`\n",
                        a.display_name, a.id,
                    ));
                    if !a.persona.soul.is_empty() {
                        let hint: String = a.persona.soul.chars().take(300).collect();
                        team_roster_prompt.push_str(&format!("  Expertise: {}\n", hint.trim()));
                    }
                }
                team_roster_prompt.push('\n');

                register_spawn_subagent_tool(
                    &mut tool_registry,
                    Arc::clone(&provider),
                    config.workspace.clone(),
                    cancel.clone(),
                    Arc::clone(&agent_map),
                );
                info!("registered spawn_subagent tool for team delegation");
            }
            Ok(_) => {
                warn!(team_dir = %team_dir.display(), "team directory is empty — no agent.toml files found");
            }
            Err(e) => {
                warn!(error = %e, "failed to load team agents — running without team support");
            }
        }
    }

    // Build agent config
    let base_system_prompt = config.system_prompt.clone().unwrap_or_else(|| {
        if !team_roster_prompt.is_empty() {
            // When running in team mode, use a router-specific base prompt
            "You are a team router agent. Your ONLY job is to delegate tasks to your team \
             specialists using the `spawn_subagent` tool and then synthesize their results. \
             You MUST call `spawn_subagent` for EVERY request — you are NOT allowed to write \
             content yourself. Your value is orchestration, quality control, and synthesis."
                .to_string()
        } else {
            "You are a powerful AI assistant running in a local terminal. \
             You have access to tools for file reading/writing, shell command execution, \
             HTTP requests, and web search. Use tools proactively to accomplish tasks. \
             When the user asks you to do something, DO it — don't just describe what \
             you would do. Execute commands, write files, and take concrete actions. \
             Use `durable_task` for long-running agent work that should remain resumable \
             across turns or process restarts. Use `process_start` only for raw subprocesses \
             that do not need durable agent state."
                .to_string()
        }
    });
    // Append team roster if team agents are loaded
    let full_system_prompt = if team_roster_prompt.is_empty() {
        base_system_prompt
    } else {
        format!("{}{}", base_system_prompt, team_roster_prompt)
    };
    let agent_config = AgentConfig {
        model: model.clone(),
        system_prompt: full_system_prompt,
        max_tool_rounds: config.max_tool_rounds,
        context_limit: config.context_limit,
        workspace_path: config.workspace.as_ref().map(|p| p.display().to_string()),
        ..Default::default()
    };

    // Build approval gate
    let gate = build_approval_gate(&config);

    // Build tool policy
    let policy = Arc::new(build_tool_policy(config.allow_all_tools));

    // ── Conscious Gateway — graduated awareness pipeline ─────────────
    let conscious_gateway = {
        use clawdesk_conscious::awareness::LevelThresholds;
        use clawdesk_conscious::veto::{CliVetoGate, VetoConfig};
        use clawdesk_conscious::workspace::GlobalWorkspace;

        let thresholds = match config.consciousness_preset.as_str() {
            "paranoid" => LevelThresholds::paranoid(),
            "supervised" => LevelThresholds::supervised(),
            "autonomous" => LevelThresholds::autonomous(),
            _ => LevelThresholds::balanced(),
        };

        let workspace = Arc::new(GlobalWorkspace::new(1024));

        let gw = clawdesk_conscious::ConsciousGateway::new()
            .with_veto_gate(Arc::new(CliVetoGate))
            .with_veto_config(VetoConfig {
                timeout_seconds: 30,
                allow_modification: true,
            })
            .with_global_workspace(Arc::clone(&workspace));
        gw.set_thresholds(thresholds).await;

        let gw = Arc::new(gw);

        // Spawn the background cognitive event loop
        {
            let rx = workspace.subscribe();
            let gw_loop = Arc::clone(&gw);
            tokio::spawn(clawdesk_agents::cognitive_loop::cognitive_event_loop(rx, gw_loop));
        }

        info!(preset = %config.consciousness_preset, "conscious gateway initialized");
        gw
    };

    // ── Post-turn validator ───────────────────────────────────
    let post_turn_validator = if config.validate {
        info!("post-turn validation enabled");
        let validation_config = clawdesk_agents::post_turn_validator::ValidationConfig {
            enabled: true,
            min_tool_calls: 2,
            timeout_secs: 60,
            max_validator_rounds: 8,
        };
        Some(Arc::new(clawdesk_agents::post_turn_validator::PostTurnValidator::new(
            validation_config,
            Arc::clone(&provider),
            config.workspace.clone(),
        )))
    } else {
        None
    };

    let sochdb_dir = dirs::sochdb();
    std::fs::create_dir_all(&sochdb_dir)?;
    let durable_store = Arc::new(if crate::is_sochdb_lock_held(&sochdb_dir) {
        eprintln!("⚠ SochDB locked by another process (desktop app running?).");
        eprintln!("  Using ephemeral in-memory storage.");
        SochStore::open_in_memory()
            .map_err(|e| format!("failed to open in-memory database: {e}"))?
    } else {
        match SochStore::open(
            sochdb_dir
                .to_str()
                .ok_or("SochDB path contains invalid UTF-8")?,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠ SochDB open failed: {e}");
                eprintln!("  Falling back to ephemeral in-memory storage.");
                SochStore::open_in_memory()
                    .map_err(|e2| format!("failed to open in-memory database: {e2}"))?
            }
        }
    });
    let checkpoint_store = Arc::new(CheckpointStore::new(Arc::clone(&durable_store)));
    let journal = Arc::new(ActivityJournal::new(Arc::clone(&durable_store)));
    let lease_manager = Arc::new(LeaseManager::new(Arc::clone(&durable_store), 300));

    let durable_runner = Arc::new(DurableAgentRunner::new(
        Arc::clone(&checkpoint_store),
        Arc::clone(&journal),
        Arc::clone(&lease_manager),
        Arc::new(LocalRunnerFactory {
            provider: Arc::clone(&provider),
            workspace: config.workspace.clone(),
            approval_gate: Arc::clone(&gate),
            tool_policy: Arc::clone(&policy),
            cancel: cancel.clone(),
            team_agents: team_agents.clone(),
            conscious_gateway: Arc::clone(&conscious_gateway),
            post_turn_validator: post_turn_validator.clone(),
        }),
        "cli-local".to_string(),
    ));

    {
        let checkpoint_store = Arc::clone(&checkpoint_store);
        let journal = Arc::clone(&journal);
        let durable_runner = Arc::clone(&durable_runner);
        let durable_store = Arc::clone(&durable_store);
        let default_model = model.clone();
        let default_system_prompt = agent_config.system_prompt.clone();
        let default_max_tool_rounds = agent_config.max_tool_rounds;
        let default_context_limit = agent_config.context_limit;
        let workspace = config.workspace.clone();
        let team_agents = team_agents.clone();

        clawdesk_agents::builtin_tools::register_durable_task_tool(
            &mut tool_registry,
            Arc::new(move |operation: String, args: serde_json::Value| {
                let checkpoint_store = Arc::clone(&checkpoint_store);
                let journal = Arc::clone(&journal);
                let durable_runner = Arc::clone(&durable_runner);
                let durable_store = Arc::clone(&durable_store);
                let default_model = default_model.clone();
                let default_system_prompt = default_system_prompt.clone();
                let workspace = workspace.clone();
                let team_agents = team_agents.clone();
                Box::pin(async move {
                    match operation.as_str() {
                        "create" => {
                            let executor_agent = args
                                .get("executor_agent")
                                .and_then(|value| value.as_str())
                                .unwrap_or("default");
                            let (override_model, executor_prompt) = resolve_executor_prompt(
                                executor_agent,
                                team_agents.as_ref(),
                                &default_system_prompt,
                            )?;
                            let input = args.get("input").cloned().unwrap_or(serde_json::Value::Null);
                            let (history, system_prompt, summary) =
                                build_task_inputs(&input, &executor_prompt)?;

                            let run_config = AgentConfig {
                                model: override_model.unwrap_or_else(|| default_model.clone()),
                                system_prompt: system_prompt.clone(),
                                max_tool_rounds: default_max_tool_rounds,
                                context_limit: default_context_limit,
                                workspace_path: workspace.as_ref().map(|path| path.display().to_string()),
                                ..Default::default()
                            };
                            let session_key = SessionKey::new(
                                ChannelId::Internal,
                                format!("durable-task:{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()).as_str(),
                            );
                            let mut run = WorkflowRun::new(
                                WorkflowType::AgentLoop {
                                    config: run_config,
                                    session_key,
                                },
                                RetryPolicy::default_agent(),
                            );
                            run.updated_at = chrono::Utc::now();
                            checkpoint_store
                                .save_run(&run)
                                .await
                                .map_err(|error| error.to_string())?;
                            checkpoint_store
                                .save_checkpoint(
                                    &run.id,
                                    &Checkpoint::AgentLoop {
                                        round: 0,
                                        messages: history,
                                        system_prompt,
                                        total_input_tokens: 0,
                                        total_output_tokens: 0,
                                        guard_state: GuardSnapshot {
                                            estimated_tokens: 0,
                                            compaction_count: 0,
                                            circuit_breaker_failures: 0,
                                        },
                                    },
                                )
                                .await
                                .map_err(|error| error.to_string())?;

                            let meta = serde_json::json!({
                                "name": args.get("name").and_then(|value| value.as_str()).unwrap_or("Durable task"),
                                "executor_agent": executor_agent,
                                "summary": summary,
                                "created_at": chrono::Utc::now(),
                            });
                            let meta_bytes = serde_json::to_vec(&meta).map_err(|error| error.to_string())?;
                            durable_store
                                .put(&durable_task_meta_key(&run.id), &meta_bytes)
                                .map_err(|error| error.to_string())?;

                            let run_id = run.id.clone();
                            let runner = Arc::clone(&durable_runner);
                            tokio::spawn(async move {
                                if let Err(error) = runner.resume(&run_id).await {
                                    warn!(run_id = %run_id, error = %error, "durable task failed");
                                }
                            });

                            Ok(serde_json::json!({
                                "task_id": run.id,
                                "status": "pending",
                                "durable": true,
                            })
                            .to_string())
                        }
                        "status" => {
                            let task_id = args
                                .get("task_id")
                                .and_then(|value| value.as_str())
                                .ok_or_else(|| "task_id is required for status".to_string())?;
                            let run_id = RunId::from_str(task_id);
                            let run = checkpoint_store
                                .load_run(&run_id)
                                .await
                                .map_err(|error| error.to_string())?
                                .ok_or_else(|| format!("task '{}' not found", task_id))?;
                            let checkpoint = checkpoint_store
                                .load_checkpoint(&run_id)
                                .await
                                .map_err(|error| error.to_string())?;
                            let meta = durable_store
                                .get(&durable_task_meta_key(&run_id))
                                .map_err(|error| error.to_string())?
                                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                                .unwrap_or(serde_json::Value::Null);
                            let note = durable_store
                                .get(&durable_task_checkpoint_note_key(&run_id))
                                .map_err(|error| error.to_string())?
                                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());

                            Ok(serde_json::json!({
                                "task_id": task_id,
                                "state": run.state.label(),
                                "attempt": run.attempt,
                                "total_input_tokens": run.total_input_tokens,
                                "total_output_tokens": run.total_output_tokens,
                                "journal_entries": journal.count(&run_id).await.map_err(|error| error.to_string())?,
                                "has_checkpoint": checkpoint.is_some(),
                                "meta": meta,
                                "checkpoint_note": note,
                            })
                            .to_string())
                        }
                        "resume" => {
                            let task_id = args
                                .get("task_id")
                                .and_then(|value| value.as_str())
                                .ok_or_else(|| "task_id is required for resume".to_string())?;
                            let run_id = RunId::from_str(task_id);
                            let runner = Arc::clone(&durable_runner);
                            tokio::spawn(async move {
                                if let Err(error) = runner.resume(&run_id).await {
                                    warn!(run_id = %run_id, error = %error, "durable task resume failed");
                                }
                            });
                            Ok(serde_json::json!({"task_id": task_id, "status": "resuming"}).to_string())
                        }
                        "cancel" => {
                            let task_id = args
                                .get("task_id")
                                .and_then(|value| value.as_str())
                                .ok_or_else(|| "task_id is required for cancel".to_string())?;
                            durable_runner
                                .cancel(&RunId::from_str(task_id), "Cancelled by durable_task tool".to_string())
                                .await
                                .map_err(|error| error.to_string())?;
                            Ok(serde_json::json!({"task_id": task_id, "status": "cancelled"}).to_string())
                        }
                        "checkpoint" => {
                            let task_id = args
                                .get("task_id")
                                .and_then(|value| value.as_str())
                                .ok_or_else(|| "task_id is required for checkpoint".to_string())?;
                            let run_id = RunId::from_str(task_id);
                            let checkpoint = checkpoint_store
                                .load_checkpoint(&run_id)
                                .await
                                .map_err(|error| error.to_string())?
                                .ok_or_else(|| format!("task '{}' has no checkpoint yet", task_id))?;
                            let note = serde_json::json!({
                                "label": args.get("label").and_then(|value| value.as_str()),
                                "completed_steps": args.get("completed_steps").and_then(|value| value.as_u64()),
                                "step_outputs": args.get("step_outputs").cloned(),
                                "saved_at": chrono::Utc::now(),
                            });
                            let note_bytes = serde_json::to_vec(&note).map_err(|error| error.to_string())?;
                            durable_store
                                .put(&durable_task_checkpoint_note_key(&run_id), &note_bytes)
                                .map_err(|error| error.to_string())?;
                            let round = match checkpoint {
                                Checkpoint::AgentLoop { round, .. } => round,
                                Checkpoint::PipelineStep { .. } => 0,
                            };
                            Ok(serde_json::json!({
                                "task_id": task_id,
                                "status": "checkpointed",
                                "round": round,
                            })
                            .to_string())
                        }
                        "list" => {
                            let mut tasks = Vec::new();
                            let mut seen = HashSet::new();
                            for state in ["pending", "running", "suspended", "completed", "failed", "cancelled"] {
                                for run_id in checkpoint_store
                                    .load_runs_by_state(state)
                                    .await
                                    .map_err(|error| error.to_string())?
                                {
                                    if !seen.insert(run_id.0.clone()) {
                                        continue;
                                    }
                                    let meta = durable_store
                                        .get(&durable_task_meta_key(&run_id))
                                        .map_err(|error| error.to_string())?
                                        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                                        .unwrap_or(serde_json::Value::Null);
                                    tasks.push(serde_json::json!({
                                        "task_id": run_id,
                                        "state": state,
                                        "meta": meta,
                                    }));
                                }
                            }
                            serde_json::to_string_pretty(&tasks).map_err(|error| error.to_string())
                        }
                        _ => Err(format!("unsupported durable task operation: {}", operation)),
                    }
                })
            }),
        );
    }

    // Event channel for streaming output
    let (event_tx, _) = broadcast::channel::<AgentEvent>(256);

    // Conversation history
    let mut history: Vec<ChatMessage> = Vec::new();
    let system_prompt = agent_config.system_prompt.clone();

    let tool_registry = Arc::new(tool_registry);

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
        history.push(ChatMessage::new(MessageRole::User, input.as_str()));

        // Build runner for this turn
        let mut runner = AgentRunner::new(
            Arc::clone(&provider),
            Arc::clone(&tool_registry),
            agent_config.clone(),
            cancel.child_token(),
        )
        .with_approval_gate(Arc::clone(&gate))
        .with_tool_policy(Arc::clone(&policy))
        .with_conscious_gateway(Arc::clone(&conscious_gateway))
        .with_events(event_tx.clone());

        if let Some(ref validator) = post_turn_validator {
            runner = runner.with_post_turn_validator(Arc::clone(validator));
        }

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
                    AgentEvent::ValidationComplete { verified, claims_passed, claims_failed } => {
                        if verified {
                            eprintln!("  [validate] ✓ {claims_passed} claims verified");
                        } else {
                            eprintln!("  [validate] ⚠ {claims_passed} passed, {claims_failed} failed");
                        }
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
                history.push(ChatMessage::new(MessageRole::Assistant, response.content.as_str()));

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
