//! # clawdesk CLI
//!
//! Entry point for the ClawDesk multi-channel AI agent gateway.
//!
//! ```text
//! clawdesk gateway run [--port 18789] [--bind loopback]
//! clawdesk message send <text> [--session <id>]
//! clawdesk channels status [--probe]
//! clawdesk plugins list
//! clawdesk plugins reload <name>
//! clawdesk cron list
//! clawdesk cron create <name> <schedule> <prompt>
//! clawdesk cron trigger <id>
//! clawdesk agent message <text> [--thinking low]
//! clawdesk config set <key> <value>
//! clawdesk config get <key>
//! clawdesk login
//! clawdesk doctor
//! ```

mod agent_compose;
mod cli_approval;
mod completions;
mod doctor;
mod gateway_rpc;
mod local_agent;
mod onboard;
mod permission_modes;
mod pipeline_run;
mod policy_audit;
mod security_audit;
mod skill_author;
mod skill_cli;
mod skill_registry;

use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser)]
#[command(
    name = "clawdesk",
    about = "ClawDesk — multi-channel AI agent gateway",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Gateway URL for client commands
    #[arg(long, global = true, default_value = "http://127.0.0.1:18789")]
    gateway_url: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the gateway server
    Gateway {
        #[command(subcommand)]
        action: GatewayAction,
    },
    /// Send a message to the agent
    Message {
        #[command(subcommand)]
        action: MessageAction,
    },
    /// Channel management
    Channels {
        #[command(subcommand)]
        action: ChannelAction,
    },
    /// Plugin management
    Plugins {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// Cron / scheduled task management
    Cron {
        #[command(subcommand)]
        action: CronAction,
    },
    /// Agent interaction (local agent mode)
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Configuration management
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Authenticate with provider APIs
    Login,
    /// Run diagnostics
    Doctor {
        /// Verbose output with timing info
        #[arg(long)]
        verbose: bool,
    },
    /// Interactive first-time setup wizard
    Init,
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for (bash, zsh, fish, powershell)
        shell: String,
    },
    /// Run security audit checks
    Security {
        #[command(subcommand)]
        action: SecurityAction,
    },
    /// Skill lifecycle management
    Skill {
        #[command(subcommand)]
        action: skill_cli::SkillAction,
    },
    /// Launch the terminal UI (ratatui-based interactive dashboard)
    #[cfg(feature = "tui")]
    Tui {
        /// Gateway URL to connect to
        #[arg(long, default_value = "http://127.0.0.1:18789")]
        gateway: String,
        /// Color theme (dark, light, high-contrast)
        #[arg(long, default_value = "dark")]
        theme: String,
    },
}

#[derive(Subcommand)]
enum GatewayAction {
    /// Start the gateway server
    Run {
        /// Port to listen on
        #[arg(long, default_value = "18789")]
        port: u16,
        /// Bind address (loopback or all)
        #[arg(long, default_value = "loopback")]
        bind: String,
        /// Force start even if another instance is running
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum MessageAction {
    /// Send a message
    Send {
        /// The message text
        text: String,
        /// Session ID (auto-generated if not provided)
        #[arg(long)]
        session: Option<String>,
        /// Model to use
        #[arg(long)]
        model: Option<String>,
    },
}

#[derive(Subcommand)]
enum ChannelAction {
    /// Show channel status
    Status {
        /// Probe channels for connectivity
        #[arg(long)]
        probe: bool,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Set a configuration value
    Set {
        key: String,
        value: String,
    },
    /// Get a configuration value
    Get {
        key: String,
    },
}

#[derive(Subcommand)]
enum PluginAction {
    /// List installed plugins
    List,
    /// Reload a plugin
    Reload {
        /// Plugin name to reload
        name: String,
    },
    /// Show plugin details
    Info {
        /// Plugin name
        name: String,
    },
}

#[derive(Subcommand)]
enum CronAction {
    /// List scheduled tasks
    List,
    /// Create a new scheduled task
    Create {
        /// Task name
        name: String,
        /// Cron expression (e.g., "0 9 * * *")
        schedule: String,
        /// Agent prompt to execute
        prompt: String,
    },
    /// Manually trigger a task
    Trigger {
        /// Task ID
        id: String,
    },
    /// Delete a scheduled task
    Delete {
        /// Task ID
        id: String,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    /// Send a message to the local agent
    #[command(name = "message", alias = "msg")]
    Message {
        /// Message text
        text: String,
        /// Thinking level (low, medium, high)
        #[arg(long, default_value = "medium")]
        thinking: String,
        /// Model to use
        #[arg(long)]
        model: Option<String>,
    },
    /// Run an interactive local agent session (Claude Code equivalent)
    #[command(name = "run")]
    Run {
        /// Model to use (e.g. claude-sonnet-4-20250514)
        #[arg(long)]
        model: Option<String>,
        /// Auto-approve all tool calls (use with caution)
        #[arg(long)]
        allow_all_tools: bool,
        /// Workspace / project root directory
        #[arg(long)]
        workspace: Option<String>,
        /// System prompt override
        #[arg(long)]
        system_prompt: Option<String>,
        /// Permission mode: interactive, allowlist, unattended
        #[arg(long, default_value = "interactive")]
        permission_mode: String,
        /// Maximum tool rounds per turn
        #[arg(long, default_value = "25")]
        max_tool_rounds: usize,
        /// Directory containing agent.toml files for multi-agent team mode
        #[arg(long)]
        team_dir: Option<String>,
    },
    /// Add a new agent from TOML definition or interactive wizard
    #[command(name = "add")]
    Add {
        /// Agent ID (kebab-case)
        id: String,
        /// Path to agent.toml (if omitted, generates interactively)
        #[arg(long)]
        from_toml: Option<String>,
    },
    /// Validate all agent.toml definitions
    #[command(name = "validate")]
    Validate,
    /// List all registered agents with routing table
    #[command(name = "list", alias = "ls")]
    List {
        /// Show channel bindings
        #[arg(long)]
        bindings: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Hot-reload agent definitions without restart
    #[command(name = "apply")]
    Apply {
        /// Specific agent ID to reload (reloads all if omitted)
        id: Option<String>,
    },
    /// Export an agent to agent.toml
    #[command(name = "export")]
    Export {
        /// Agent ID to export
        id: String,
        /// Output path (default: stdout)
        #[arg(long, short = 'o')]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
enum SecurityAction {
    /// Run security audit
    Audit {
        /// Run deep checks (skill scanning, log integrity)
        #[arg(long)]
        deep: bool,
        /// Auto-remediate findings where safe
        #[arg(long)]
        fix: bool,
        /// Custom config directory to audit
        #[arg(long)]
        config_dir: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize tracing
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();

    let cli = Cli::parse();
    let cancel = CancellationToken::new();

    // Handle Ctrl+C
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("received Ctrl+C, shutting down");
        cancel_clone.cancel();
    });

    match cli.command {
        Commands::Gateway { action } => match action {
            GatewayAction::Run { port, bind, force } => {
                info!(%port, %bind, %force, "starting gateway");

                let host = match bind.as_str() {
                    "loopback" => "127.0.0.1".to_string(),
                    "all" => "0.0.0.0".to_string(),
                    other => other.to_string(),
                };

                // Initialize storage
                let data_dir = dirs_home()
                    .join(".clawdesk")
                    .join("data");
                std::fs::create_dir_all(&data_dir)?;

                let store = clawdesk_sochdb::SochStore::open(
                    data_dir.to_str().unwrap(),
                ).map_err(|e| format!("failed to open database: {e}"))?;

                // Build registries
                let channels = clawdesk_channel::registry::ChannelRegistry::new();
                let mut providers = clawdesk_providers::registry::ProviderRegistry::new();
                let tools = clawdesk_agents::ToolRegistry::new();

                // Auto-register providers from environment variables
                if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                    info!("registering Anthropic provider from env");
                    providers.register(
                        std::sync::Arc::new(
                            clawdesk_providers::anthropic::AnthropicProvider::new(key, None),
                        ),
                    );
                }
                if let Ok(key) = std::env::var("OPENAI_API_KEY") {
                    let base_url = std::env::var("OPENAI_BASE_URL").ok();
                    info!("registering OpenAI provider from env");
                    providers.register(
                        std::sync::Arc::new(
                            clawdesk_providers::openai::OpenAiProvider::new(key, base_url, None),
                        ),
                    );
                }
                if let Ok(key) = std::env::var("AZURE_OPENAI_API_KEY") {
                    if let Ok(endpoint) = std::env::var("AZURE_OPENAI_ENDPOINT") {
                        let api_version = std::env::var("AZURE_OPENAI_API_VERSION").ok();
                        info!("registering Azure OpenAI provider from env");
                        providers.register(
                            std::sync::Arc::new(
                                clawdesk_providers::azure::AzureOpenAiProvider::new(
                                    key, endpoint, api_version, None,
                                ),
                            ),
                        );
                    }
                }
                if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
                    info!("registering Gemini provider from env");
                    providers.register(
                        std::sync::Arc::new(
                            clawdesk_providers::gemini::GeminiProvider::new(key, None),
                        ),
                    );
                }
                if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
                    info!("registering OpenRouter provider from env");
                    providers.register(
                        std::sync::Arc::new(
                            clawdesk_providers::openrouter::OpenRouterProvider::new(key),
                        ),
                    );
                }
                // Ollama — always available if running locally
                providers.register(
                    std::sync::Arc::new(
                        clawdesk_providers::ollama::OllamaProvider::new(None, None),
                    ),
                );

                // Also try to load channel_provider.json for Azure/custom overrides
                let cp_path = dirs_home().join(".clawdesk").join("channel_provider.json");
                if cp_path.exists() {
                    if let Ok(raw) = std::fs::read_to_string(&cp_path) {
                        if let Ok(cp) = serde_json::from_str::<serde_json::Value>(&raw) {
                            let provider_name = cp.get("provider").and_then(|v| v.as_str()).unwrap_or("");
                            let api_key = cp.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
                            let base_url = cp.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
                            let model = cp.get("model").and_then(|v| v.as_str());

                            if !api_key.is_empty() && !base_url.is_empty() {
                                if provider_name.contains("Azure") {
                                    info!("registering Azure OpenAI from channel_provider.json");
                                    providers.register(
                                        std::sync::Arc::new(
                                            clawdesk_providers::azure::AzureOpenAiProvider::new(
                                                api_key.to_string(),
                                                base_url.to_string(),
                                                None,
                                                model.map(|m| m.to_string()),
                                            ),
                                        ),
                                    );
                                } else if provider_name.contains("OpenAI") {
                                    info!("registering OpenAI from channel_provider.json");
                                    providers.register(
                                        std::sync::Arc::new(
                                            clawdesk_providers::openai::OpenAiProvider::new(
                                                api_key.to_string(),
                                                Some(base_url.to_string()),
                                                model.map(|m| m.to_string()),
                                            ),
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }

                info!(count = providers.list().len(), "providers registered");

                // Plugin host with a no-op factory (plugins loaded at runtime)
                let plugin_host = clawdesk_plugin::PluginHost::new(
                    std::sync::Arc::new(NoopPluginFactory),
                    128,
                );

                // Cron manager with no-op executor/delivery (wired later)
                let cron_manager = clawdesk_cron::CronManager::new(
                    std::sync::Arc::new(NoopAgentExecutor),
                    std::sync::Arc::new(NoopDeliveryHandler),
                );

                // Skills — load from disk if the directory exists
                let skills_dir = dirs_data().join("skills");
                let _ = std::fs::create_dir_all(&skills_dir);
                let skill_loader = clawdesk_skills::loader::SkillLoader::new(
                    skills_dir,
                );
                let load_result = skill_loader.load_fresh(true).await;
                let skills = {
                    let reg = load_result.registry;
                    if load_result.errors.is_empty() {
                        info!(count = reg.len(), "loaded skills from disk");
                    } else {
                        warn!(
                            count = reg.len(),
                            errors = load_result.errors.len(),
                            "loaded skills with some errors"
                        );
                    }
                    reg
                };

                // Channel factory with built-in constructors
                let channel_factory = clawdesk_channels::factory::ChannelFactory::with_builtins();

                let state = std::sync::Arc::new(
                    clawdesk_gateway::state::GatewayState::new(
                        channels, providers, tools, store,
                        plugin_host, cron_manager,
                        skills, skill_loader, channel_factory,
                        cancel.clone(),
                        clawdesk_channel::inbound_adapter::InboundAdapterRegistry::new(256),
                    ),
                );

                let config = clawdesk_gateway::GatewayConfig {
                    host,
                    port,
                    ..Default::default()
                };

                clawdesk_gateway::serve(config, state, cancel).await?;
            }
        },
        Commands::Message { action } => match action {
            MessageAction::Send { text, session, model } => {
                info!(%text, "sending message");
                cmd_send_message(&cli.gateway_url, &text, session, model).await?;
            }
        },
        Commands::Channels { action } => match action {
            ChannelAction::Status { probe } => {
                info!(%probe, "checking channel status");
                cmd_channels_status(&cli.gateway_url, probe).await?;
            }
        },
        Commands::Config { action } => match action {
            ConfigAction::Set { key, value } => {
                info!(%key, %value, "setting config");
                cmd_config_set(&key, &value).await?;
            }
            ConfigAction::Get { key } => {
                info!(%key, "getting config");
                cmd_config_get(&key).await?;
            }
        },
        Commands::Plugins { action } => match action {
            PluginAction::List => {
                cmd_plugins_list(&cli.gateway_url).await?;
            }
            PluginAction::Reload { name } => {
                cmd_plugin_reload(&cli.gateway_url, &name).await?;
            }
            PluginAction::Info { name } => {
                cmd_plugin_info(&cli.gateway_url, &name).await?;
            }
        },
        Commands::Cron { action } => match action {
            CronAction::List => {
                cmd_cron_list(&cli.gateway_url).await?;
            }
            CronAction::Create { name, schedule, prompt } => {
                cmd_cron_create(&cli.gateway_url, &name, &schedule, &prompt).await?;
            }
            CronAction::Trigger { id } => {
                cmd_cron_trigger(&cli.gateway_url, &id).await?;
            }
            CronAction::Delete { id } => {
                cmd_cron_delete(&cli.gateway_url, &id).await?;
            }
        },
        Commands::Agent { action } => match action {
            AgentAction::Message { text, thinking, model } => {
                cmd_agent_message(&cli.gateway_url, &text, &thinking, model).await?;
            }
            AgentAction::Run {
                model,
                allow_all_tools,
                workspace,
                system_prompt,
                permission_mode,
                max_tool_rounds,
                team_dir,
            } => {
                let perm_mode = match permission_mode.as_str() {
                    "allowlist" => permission_modes::PermissionMode::Allowlist,
                    "unattended" => permission_modes::PermissionMode::Unattended,
                    _ => permission_modes::PermissionMode::Interactive,
                };
                let perm_config = permission_modes::PermissionConfig {
                    mode: perm_mode,
                    ..Default::default()
                };
                let config = local_agent::LocalAgentConfig {
                    model,
                    allow_all_tools,
                    workspace: workspace.map(std::path::PathBuf::from),
                    system_prompt,
                    max_tool_rounds,
                    context_limit: 200_000,
                    permission_config: perm_config,
                    team_dir: team_dir.map(std::path::PathBuf::from),
                };
                local_agent::run_local_agent(config, cancel).await?;
            }
            AgentAction::Add { id, from_toml } => {
                agent_compose::cmd_agent_add(&id, from_toml.as_deref()).await?;
            }
            AgentAction::Validate => {
                agent_compose::cmd_agent_validate().await?;
            }
            AgentAction::List { bindings, json } => {
                agent_compose::cmd_agent_list(bindings, json).await?;
            }
            AgentAction::Apply { id } => {
                agent_compose::cmd_agent_apply(&cli.gateway_url, id.as_deref()).await?;
            }
            AgentAction::Export { id, output } => {
                agent_compose::cmd_agent_export(&id, output.as_deref()).await?;
            }
        },
        Commands::Login => {
            cmd_login().await?;
        }
        Commands::Doctor { verbose } => {
            doctor::run_doctor(verbose).await?;
        }
        Commands::Init => {
            onboard::run_onboarding().await?;
        }
        Commands::Completions { shell } => {
            completions::generate_completions(&shell)
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
        }
        Commands::Skill { action } => {
            skill_cli::execute_skill_command(&cli.gateway_url, action).await?;
        }
        Commands::Security { action } => {
            match action {
                SecurityAction::Audit { deep, fix, config_dir } => {
                    let dir = config_dir
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| {
                            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                            std::path::PathBuf::from(home).join(".clawdesk")
                        });
                    let audit_config = security_audit::AuditConfig {
                        config_dir: dir,
                        deep,
                        fix,
                    };
                    let report = security_audit::run_audit(&audit_config);
                    println!("{}", report.summary());
                    for finding in &report.findings {
                        println!(
                            "  [{}] {}: {}{}",
                            finding.severity,
                            finding.check_id,
                            finding.title,
                            if finding.remediated { " (FIXED)" } else { "" }
                        );
                    }
                    if report.critical_count() > 0 {
                        std::process::exit(1);
                    }
                }
            }
        }
        #[cfg(feature = "tui")]
        Commands::Tui { gateway, theme } => {
            info!(gateway = %gateway, theme = %theme, "Launching terminal UI");
            println!("Starting ClawDesk TUI...");
            println!("  Gateway: {gateway}");
            println!("  Theme:   {theme}");
            println!();

            // Create the TUI app and apply the selected theme
            let mut app = clawdesk_tui::App::new();
            app.theme = clawdesk_tui::Theme::by_name(&theme);

            // Run the TUI event loop (blocks until user exits)
            if let Err(e) = app.run() {
                eprintln!("TUI error: {e}");
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

// ── Client commands ──────────────────────────────────────────

/// Send a message to the running gateway via HTTP.
async fn cmd_send_message(
    base_url: &str,
    text: &str,
    session: Option<String>,
    model: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/message", base_url);
    let body = serde_json::json!({
        "message": text,
        "session_id": session,
        "model": model,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("failed to connect to gateway at {url}: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        eprintln!("Error {status}: {body}");
        return Ok(());
    }

    let data: serde_json::Value = resp.json().await?;
    if let Some(reply) = data.get("reply").and_then(|v| v.as_str()) {
        println!("{reply}");
    }
    if let Some(sid) = data.get("session_id").and_then(|v| v.as_str()) {
        eprintln!("session: {sid}");
    }
    Ok(())
}

/// Query channel status from the running gateway.
async fn cmd_channels_status(
    base_url: &str,
    _probe: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/channels", base_url);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("failed to connect to gateway at {url}: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        eprintln!("Error {status}: {body}");
        return Ok(());
    }

    let channels: Vec<serde_json::Value> = resp.json().await?;
    if channels.is_empty() {
        println!("No channels configured.");
    } else {
        println!("{:<20} {:<15} {}", "ID", "NAME", "STATUS");
        println!("{}", "-".repeat(50));
        for ch in &channels {
            let id = ch.get("id").and_then(|v| v.as_str()).unwrap_or("-");
            let name = ch.get("name").and_then(|v| v.as_str()).unwrap_or("-");
            let status = ch.get("status").and_then(|v| v.as_str()).unwrap_or("-");
            println!("{:<20} {:<15} {}", id, name, status);
        }
    }
    Ok(())
}

/// Set a config value directly in SochDB.
async fn cmd_config_set(
    key: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use clawdesk_storage::config_store::ConfigStore;

    let store = open_store()?;
    let json_value: serde_json::Value = serde_json::from_str(value)
        .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));

    store.set_value(key, json_value.clone()).await.map_err(|e| {
        format!("failed to set config: {e}")
    })?;
    println!("Set {key} = {json_value}");
    Ok(())
}

/// Get a config value directly from SochDB.
async fn cmd_config_get(
    key: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use clawdesk_storage::config_store::ConfigStore;

    let store = open_store()?;
    match store.get_value(key).await {
        Ok(Some(val)) => println!("{key} = {val}"),
        Ok(None) => println!("{key} = (not set)"),
        Err(e) => eprintln!("Error reading config: {e}"),
    }
    Ok(())
}

// ── Plugin commands ──────────────────────────────────────────

/// List plugins via the admin API.
async fn cmd_plugins_list(
    base_url: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/admin/plugins", base_url);
    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if !resp.status().is_success() {
        eprintln!("Error: HTTP {}", resp.status());
        return Ok(());
    }

    let plugins: Vec<serde_json::Value> = resp.json().await?;
    if plugins.is_empty() {
        println!("No plugins installed.");
    } else {
        println!("{:<25} {:<10} {:<10} {}", "NAME", "VERSION", "STATE", "TOOLS");
        println!("{}", "-".repeat(65));
        for p in &plugins {
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("-");
            let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("-");
            let state = p.get("state").and_then(|v| v.as_str()).unwrap_or("-");
            let tools = p
                .get("capabilities")
                .and_then(|c| c.get("tools"))
                .and_then(|t| t.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            println!("{:<25} {:<10} {:<10} {}", name, version, state, tools);
        }
    }
    Ok(())
}

/// Reload a plugin via the admin API.
async fn cmd_plugin_reload(
    base_url: &str,
    name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/admin/plugins/{}/reload", base_url, name);
    let client = reqwest::Client::new();
    let resp = client.post(&url).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if resp.status().is_success() {
        println!("Plugin '{}' reloaded successfully.", name);
    } else {
        let body = resp.text().await.unwrap_or_default();
        eprintln!("Failed to reload plugin '{}': {}", name, body);
    }
    Ok(())
}

/// Show plugin info via the admin API.
async fn cmd_plugin_info(
    base_url: &str,
    name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/admin/plugins", base_url);
    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if !resp.status().is_success() {
        eprintln!("Error: HTTP {}", resp.status());
        return Ok(());
    }

    let plugins: Vec<serde_json::Value> = resp.json().await?;
    match plugins.iter().find(|p| p.get("name").and_then(|v| v.as_str()) == Some(name)) {
        Some(p) => println!("{}", serde_json::to_string_pretty(p)?),
        None => eprintln!("Plugin '{}' not found.", name),
    }
    Ok(())
}

// ── Cron commands ────────────────────────────────────────────

/// List cron tasks via the admin API.
async fn cmd_cron_list(
    base_url: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/admin/cron/tasks", base_url);
    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if !resp.status().is_success() {
        eprintln!("Error: HTTP {}", resp.status());
        return Ok(());
    }

    let tasks: Vec<serde_json::Value> = resp.json().await?;
    if tasks.is_empty() {
        println!("No cron tasks configured.");
    } else {
        println!("{:<36} {:<20} {:<15} {}", "ID", "NAME", "SCHEDULE", "ENABLED");
        println!("{}", "-".repeat(80));
        for t in &tasks {
            let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("-");
            let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("-");
            let sched = t.get("schedule").and_then(|v| v.as_str()).unwrap_or("-");
            let enabled = t.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            println!("{:<36} {:<20} {:<15} {}", id, name, sched, enabled);
        }
    }
    Ok(())
}

/// Create a cron task via the admin API.
async fn cmd_cron_create(
    base_url: &str,
    name: &str,
    schedule: &str,
    prompt: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/admin/cron/tasks", base_url);
    let body = serde_json::json!({
        "name": name,
        "schedule": schedule,
        "prompt": prompt,
    });

    let client = reqwest::Client::new();
    let resp = client.post(&url).json(&body).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if resp.status().is_success() {
        let data: serde_json::Value = resp.json().await?;
        let id = data.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        println!("Created cron task '{}' (ID: {})", name, id);
    } else {
        let body = resp.text().await.unwrap_or_default();
        eprintln!("Failed to create task: {}", body);
    }
    Ok(())
}

/// Trigger a cron task via the admin API.
async fn cmd_cron_trigger(
    base_url: &str,
    id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/admin/cron/tasks/{}/trigger", base_url, id);
    let client = reqwest::Client::new();
    let resp = client.post(&url).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if resp.status().is_success() {
        let data: serde_json::Value = resp.json().await?;
        println!("Triggered task {}: {:?}", id, data);
    } else {
        let body = resp.text().await.unwrap_or_default();
        eprintln!("Failed to trigger task: {}", body);
    }
    Ok(())
}

/// Delete a cron task (via direct store — admin API delete is TODO).
async fn cmd_cron_delete(
    _base_url: &str,
    id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // TODO: Add DELETE endpoint to admin API
    eprintln!("Delete not yet implemented via API. Task ID: {}", id);
    Ok(())
}

// ── Agent commands ───────────────────────────────────────────

/// Send a message to the agent via the gateway.
async fn cmd_agent_message(
    base_url: &str,
    text: &str,
    _thinking: &str,
    model: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Use the standard message endpoint with agent context
    cmd_send_message(base_url, text, None, model).await
}

// ── Login ────────────────────────────────────────────────────

/// Interactive login flow for provider APIs.
async fn cmd_login() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let creds_dir = dirs_home().join(".clawdesk").join("credentials");
    std::fs::create_dir_all(&creds_dir)?;

    println!("ClawDesk Login");
    println!("==============");
    println!();
    println!("Configure API keys for AI providers.");
    println!("Keys are stored in: {}", creds_dir.display());
    println!();

    // Read API key from environment or prompt
    let anthropic_key = std::env::var("ANTHROPIC_API_KEY").ok();
    let openai_key = std::env::var("OPENAI_API_KEY").ok();

    if let Some(key) = anthropic_key {
        let key_path = creds_dir.join("anthropic.json");
        let data = serde_json::json!({ "api_key": key });
        std::fs::write(&key_path, serde_json::to_string_pretty(&data)?)?;
        println!("  Anthropic: saved (from ANTHROPIC_API_KEY)");
    } else {
        println!("  Anthropic: not configured (set ANTHROPIC_API_KEY)");
    }

    if let Some(key) = openai_key {
        let key_path = creds_dir.join("openai.json");
        let data = serde_json::json!({ "api_key": key });
        std::fs::write(&key_path, serde_json::to_string_pretty(&data)?)?;
        println!("  OpenAI:    saved (from OPENAI_API_KEY)");
    } else {
        println!("  OpenAI:    not configured (set OPENAI_API_KEY)");
    }

    println!();
    println!("Done. Run 'clawdesk doctor' to verify.");
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────

/// Open a SochStore for direct CLI access (config set/get, doctor).
fn open_store() -> Result<clawdesk_sochdb::SochStore, Box<dyn std::error::Error + Send + Sync>> {
    let data_dir = dirs_home().join(".clawdesk").join("data");
    std::fs::create_dir_all(&data_dir)?;
    let store = clawdesk_sochdb::SochStore::open(
        data_dir.to_str().unwrap(),
    ).map_err(|e| format!("failed to open database: {e}"))?;
    Ok(store)
}

/// Get the user's home directory — cross-platform.
///
/// Resolution order:
/// 1. `$HOME` (Unix, macOS, WSL)
/// 2. `%USERPROFILE%` (Windows — `C:\Users\<name>`)
/// 3. Fallback to current directory (`.`)
fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Get the platform-appropriate data directory.
///
/// - macOS: `~/Library/Application Support/clawdesk`
/// - Linux: `$XDG_DATA_HOME/clawdesk` or `~/.local/share/clawdesk`
/// - Windows: `%APPDATA%/clawdesk`
/// - Fallback: `~/.clawdesk`
fn dirs_data() -> std::path::PathBuf {
    // macOS
    if cfg!(target_os = "macos") {
        let home = dirs_home();
        return home.join("Library").join("Application Support").join("clawdesk");
    }

    // Linux: respect XDG
    if cfg!(target_os = "linux") {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            return std::path::PathBuf::from(xdg).join("clawdesk");
        }
        return dirs_home().join(".local").join("share").join("clawdesk");
    }

    // Windows: %APPDATA%
    if cfg!(target_os = "windows") {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return std::path::PathBuf::from(appdata).join("clawdesk");
        }
    }

    // Fallback
    dirs_home().join(".clawdesk")
}

// ── Stub implementations for boot ───────────────────────────

/// No-op plugin factory — real implementations are wired at runtime.
struct NoopPluginFactory;

#[async_trait::async_trait]
impl clawdesk_plugin::PluginFactory for NoopPluginFactory {
    async fn create(
        &self,
        manifest: &clawdesk_types::plugin::PluginManifest,
    ) -> Result<std::sync::Arc<dyn clawdesk_plugin::PluginInstance>, clawdesk_types::error::PluginError> {
        Err(clawdesk_types::error::PluginError::LoadFailed {
            name: manifest.name.clone(),
            detail: "No plugin factory configured".into(),
        })
    }
}

/// No-op agent executor for cron — agent runner wired after boot.
struct NoopAgentExecutor;

#[async_trait::async_trait]
impl clawdesk_cron::executor::AgentExecutor for NoopAgentExecutor {
    async fn execute(
        &self,
        _prompt: &str,
        _agent_id: Option<&str>,
    ) -> Result<String, String> {
        Err("Agent executor not configured".into())
    }
}

/// No-op delivery handler for cron — channels wired after boot.
struct NoopDeliveryHandler;

#[async_trait::async_trait]
impl clawdesk_cron::executor::DeliveryHandler for NoopDeliveryHandler {
    async fn deliver(
        &self,
        _target: &clawdesk_types::cron::DeliveryTarget,
        _content: &str,
    ) -> Result<(), String> {
        Err("Delivery handler not configured".into())
    }
}
