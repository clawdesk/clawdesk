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
mod config_backup;
mod doctor;
mod gateway_rpc;
mod local_agent;
mod onboard;
mod permission_modes;
mod pipeline_run;
mod policy_audit;
mod security_audit;
mod self_update;
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
    /// Daemon lifecycle management (install, start, stop, status)
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Check for updates and self-update the binary
    Update {
        #[command(subcommand)]
        action: UpdateAction,
    },
}

#[derive(Subcommand)]
enum UpdateAction {
    /// Check if a newer version is available
    Check,
    /// Download and apply the latest update
    Apply {
        /// Allow pre-release versions
        #[arg(long)]
        prerelease: bool,
    },
    /// Rollback to the previous binary version
    Rollback,
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
    /// Create an encrypted backup of ~/.clawdesk/ configuration
    Backup {
        /// Output file path (default: clawdesk-backup.cdbu)
        #[arg(long, default_value = "clawdesk-backup.cdbu")]
        output: String,
        /// Include credential keys directory
        #[arg(long)]
        include_keys: bool,
    },
    /// Restore configuration from an encrypted backup
    Restore {
        /// Backup file path to restore from
        file: String,
        /// Restore directory (default: ~/.clawdesk/)
        #[arg(long)]
        target: Option<String>,
        /// Preview contents without restoring
        #[arg(long)]
        dry_run: bool,
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

#[derive(Subcommand)]
enum DaemonAction {
    /// Run in daemon mode (used by service manager — not for manual use)
    Run {
        /// Port to listen on
        #[arg(long, default_value = "18789")]
        port: u16,
        /// Bind address
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
    },
    /// Install platform-native service (launchd/systemd/Windows Service)
    Install,
    /// Uninstall the platform-native service
    Uninstall,
    /// Start the installed daemon service
    Start,
    /// Stop the running daemon service
    Stop,
    /// Restart the daemon service
    Restart,
    /// Show daemon status (PID, uptime, health)
    Status,
    /// Tail daemon logs
    Logs {
        /// Number of lines to show
        #[arg(long, short = 'n', default_value = "50")]
        lines: usize,
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

                // Initialize storage — use canonical SochDB path
                let sochdb_dir = clawdesk_types::dirs::sochdb();
                std::fs::create_dir_all(&sochdb_dir)?;

                let store = clawdesk_sochdb::SochStore::open(
                    sochdb_dir.to_str().unwrap(),
                ).map_err(|e| format!("failed to open database: {e}"))?;

                // Build registries
                let channels = clawdesk_channel::registry::ChannelRegistry::new();
                let mut providers = clawdesk_providers::registry::ProviderRegistry::new();
                let tools = clawdesk_agents::ToolRegistry::new();

                // Auto-register all providers from env vars + channel_provider.json
                clawdesk_providers::registry::auto_register_from_env(&mut providers);
                let cp_path = clawdesk_types::dirs::dot_clawdesk().join("channel_provider.json");
                clawdesk_providers::registry::register_from_config_file(&mut providers, &cp_path);
                info!(count = providers.list().len(), "providers registered");

                // Plugin host with a no-op factory (plugins loaded at runtime)
                let plugin_host = clawdesk_plugin::PluginHost::new(
                    std::sync::Arc::new(clawdesk_plugin::NoopPluginFactory),
                    128,
                );

                // Cron manager with no-op executor/delivery (wired later)
                let cron_manager = clawdesk_cron::CronManager::new(
                    std::sync::Arc::new(clawdesk_cron::executor::NoopAgentExecutor),
                    std::sync::Arc::new(clawdesk_cron::executor::NoopDeliveryHandler),
                );

                // Skills — load from disk if the directory exists
                let skills_dir = clawdesk_types::dirs::skills();
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
            ConfigAction::Backup { output, include_keys } => {
                eprint!("Backup passphrase: ");
                let mut passphrase = String::new();
                std::io::stdin().read_line(&mut passphrase).unwrap_or_default();
                let passphrase = passphrase.trim();
                let mut config = config_backup::BackupConfig::default();
                config.include_keys = include_keys;
                let output_path = std::path::PathBuf::from(&output);
                match config_backup::create_backup(&config, &passphrase, &output_path).await {
                    Ok(count) => println!("✅ Backup created: {output} ({count} files)"),
                    Err(e) => eprintln!("❌ Backup failed: {e}"),
                }
            }
            ConfigAction::Restore { file, target, dry_run } => {
                eprint!("Backup passphrase: ");
                let mut passphrase = String::new();
                std::io::stdin().read_line(&mut passphrase).unwrap_or_default();
                let passphrase = passphrase.trim();
                let backup_path = std::path::PathBuf::from(&file);
                let restore_dir = target
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| config_backup::BackupConfig::default().source_dir);
                match config_backup::restore_backup(&passphrase, &backup_path, &restore_dir, dry_run).await {
                    Ok(manifest) => {
                        if dry_run {
                            println!("📋 Backup contents ({} files):", manifest.files.len());
                        } else {
                            println!("✅ Restored {} files to {}", manifest.files.len(), restore_dir.display());
                        }
                    }
                    Err(e) => eprintln!("❌ Restore failed: {e}"),
                }
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
        Commands::Daemon { action } => {
            cmd_daemon(action, cancel).await?;
        }
        Commands::Update { action } => {
            cmd_update(action).await?;
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

/// Delete a cron task via the admin API.
async fn cmd_cron_delete(
    base_url: &str,
    id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/admin/cron/tasks/{}", base_url, id);
    let resp = client.delete(&url).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if resp.status().is_success() {
        println!("Deleted cron task {}", id);
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        eprintln!("Failed to delete task ({}): {}", status, body);
    }
    Ok(())
}

// ── Self-update commands ─────────────────────────────────────

/// Handle all `clawdesk update *` subcommands.
async fn cmd_update(
    action: UpdateAction,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match action {
        UpdateAction::Check => {
            let config = self_update::UpdateConfig::default();
            let updater = self_update::SelfUpdater::new(config);
            match updater.check().await {
                self_update::UpdateCheck::UpToDate { current } => {
                    println!("✅ Already up to date (v{current})");
                }
                self_update::UpdateCheck::UpdateAvailable {
                    current,
                    latest,
                    asset_size,
                    release_notes_url,
                    ..
                } => {
                    println!("🆕 Update available: v{current} → v{latest}");
                    println!("   Size: {:.1} MB", asset_size as f64 / 1_048_576.0);
                    println!("   Release notes: {release_notes_url}");
                    println!("   Run `clawdesk update apply` to install.");
                }
                self_update::UpdateCheck::CheckFailed { error } => {
                    eprintln!("❌ Update check failed: {error}");
                }
            }
        }
        UpdateAction::Apply { prerelease } => {
            let mut config = self_update::UpdateConfig::default();
            config.allow_prerelease = prerelease;
            let updater = self_update::SelfUpdater::new(config);
            match updater.apply().await {
                self_update::UpdateResult::Updated { from_version, to_version, binary_path } => {
                    println!("✅ Updated: v{from_version} → v{to_version}");
                    println!("   Binary: {}", binary_path.display());
                    println!("   Restart the daemon if running: `clawdesk daemon restart`");
                }
                self_update::UpdateResult::AlreadyCurrent { version } => {
                    println!("✅ Already up to date (v{version})");
                }
                self_update::UpdateResult::Failed { error } => {
                    eprintln!("❌ Update failed: {error}");
                    eprintln!("   You can rollback with `clawdesk update rollback`");
                }
            }
        }
        UpdateAction::Rollback => {
            let config = self_update::UpdateConfig::default();
            let updater = self_update::SelfUpdater::new(config);
            match updater.rollback() {
                Ok(()) => println!("✅ Rolled back to previous version"),
                Err(e) => eprintln!("❌ Rollback failed: {e}"),
            }
        }
    }
    Ok(())
}

// ── Daemon commands ──────────────────────────────────────────

/// Handle all `clawdesk daemon *` subcommands.
async fn cmd_daemon(
    action: DaemonAction,
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let binary_path = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("clawdesk"));

    match action {
        DaemonAction::Run { port, bind } => {
            // Daemon mode: PID file + watchdog + graceful shutdown.
            let config = clawdesk_daemon::DaemonConfig {
                port,
                bind: bind.clone(),
                ..Default::default()
            };
            let runner = clawdesk_daemon::DaemonRunner::new(config);

            // Acquire PID file (prevents multiple instances).
            runner.acquire_pid().map_err(|e| format!("{e}"))?;

            // Start watchdog heartbeat (systemd integration).
            let _watchdog = runner.spawn_watchdog(cancel.clone());

            // Build and run the gateway (same as `gateway run`).
            info!(port, %bind, "starting daemon gateway");

            let host = bind;
            let sochdb_dir = clawdesk_types::dirs::sochdb();
            std::fs::create_dir_all(&sochdb_dir)?;

            let store = clawdesk_sochdb::SochStore::open(
                sochdb_dir.to_str().unwrap(),
            ).map_err(|e| format!("failed to open database: {e}"))?;

            let channels = clawdesk_channel::registry::ChannelRegistry::new();
            let mut providers = clawdesk_providers::registry::ProviderRegistry::new();
            let tools = clawdesk_agents::ToolRegistry::new();

            // Auto-register all providers from env vars
            clawdesk_providers::registry::auto_register_from_env(&mut providers);

            let plugin_host = clawdesk_plugin::PluginHost::new(
                std::sync::Arc::new(clawdesk_plugin::NoopPluginFactory),
                128,
            );
            let cron_manager = clawdesk_cron::CronManager::new(
                std::sync::Arc::new(clawdesk_cron::executor::NoopAgentExecutor),
                std::sync::Arc::new(clawdesk_cron::executor::NoopDeliveryHandler),
            );
            let skills_dir = clawdesk_types::dirs::skills();
            let _ = std::fs::create_dir_all(&skills_dir);
            let skill_loader = clawdesk_skills::loader::SkillLoader::new(skills_dir);
            let load_result = skill_loader.load_fresh(true).await;
            let skills = load_result.registry;
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

            let gw_config = clawdesk_gateway::GatewayConfig {
                host,
                port,
                ..Default::default()
            };

            // Startup recovery: scan for orphaned runs from a previous crash.
            {
                let checkpoint_store = std::sync::Arc::new(
                    clawdesk_runtime::CheckpointStore::new(state.store.clone()),
                );
                let lease_manager = std::sync::Arc::new(
                    clawdesk_runtime::LeaseManager::new(state.store.clone(), 300),
                );
                let dlq = std::sync::Arc::new(
                    clawdesk_runtime::DeadLetterQueue::new(state.store.clone()),
                );
                let recovery =
                    clawdesk_runtime::RecoveryManager::new(checkpoint_store, lease_manager, dlq);

                match recovery.scan_and_recover().await {
                    Ok(actions) if !actions.is_empty() => {
                        info!(recovered = actions.len(), "startup recovery: orphaned runs processed");
                    }
                    Ok(_) => {
                        info!("startup recovery: no orphaned runs found");
                    }
                    Err(e) => {
                        warn!(%e, "startup recovery: scan failed (non-fatal)");
                    }
                }

                // Restore cron tasks from shutdown snapshot.
                let prefix = "cron:shutdown:task:";
                if let Ok(entries) = state.store.scan(prefix) {
                    let mut restored = 0u32;
                    for (_key, val) in &entries {
                        if let Ok(task) = serde_json::from_slice::<clawdesk_types::cron::CronTask>(val) {
                            let _ = state.cron_manager.upsert_task(task).await;
                            restored += 1;
                        }
                    }
                    if restored > 0 {
                        info!(restored, "startup recovery: cron tasks restored");
                        // Clean up the shutdown snapshot.
                        for (key, _) in &entries {
                            let _ = state.store.delete(key);
                        }
                    }
                }
            }

            // Notify systemd we're ready.
            runner.notify_ready();
            info!("daemon ready, serving on port {port}");

            // Initial agent loading from ~/.clawdesk/agents/.
            {
                let (loaded, _changed, errors) = state.reload_agents();
                if !errors.is_empty() {
                    warn!(loaded, errors = ?errors, "some agent definitions failed to load");
                } else if loaded > 0 {
                    info!(loaded, "agent definitions loaded");
                }
            }

            // SIGHUP handler for manual hot-reload.
            let sighup_state = state.clone();
            let _sighup = clawdesk_daemon::DaemonRunner::spawn_sighup_handler(
                cancel.clone(),
                move || {
                    let (loaded, changed, errors) = sighup_state.reload_agents();
                    if !errors.is_empty() {
                        warn!(loaded, changed, errors = ?errors, "SIGHUP: agent reload had errors");
                    } else if changed > 0 {
                        info!(loaded, changed, "SIGHUP: agents reloaded");
                    }
                    // Skills are auto-reloaded by ConfigWatcher; SIGHUP
                    // gives an explicit trigger for agents specifically.
                },
            );

            // Run gateway until cancellation.
            clawdesk_gateway::serve(gw_config, state.clone(), cancel.clone()).await?;

            // Graceful shutdown with real state checkpointing.
            let callbacks = GatewayShutdownCallbacks {
                state: state.clone(),
                cancel: cancel.clone(),
            };
            runner.graceful_shutdown(&callbacks).await;

            info!("daemon exited cleanly");
        }
        DaemonAction::Install => {
            let ctl = clawdesk_daemon::DaemonCtl::new(binary_path, 18789);
            ctl.install().await.map_err(|e| format!("{e}"))?;
            println!("✓ ClawDesk daemon service installed");
            println!("  Run: clawdesk daemon start");
        }
        DaemonAction::Uninstall => {
            let ctl = clawdesk_daemon::DaemonCtl::new(binary_path, 18789);
            ctl.uninstall().await.map_err(|e| format!("{e}"))?;
            println!("✓ ClawDesk daemon service uninstalled");
        }
        DaemonAction::Start => {
            let ctl = clawdesk_daemon::DaemonCtl::new(binary_path, 18789);
            ctl.start().await.map_err(|e| format!("{e}"))?;
            println!("✓ ClawDesk daemon started");
        }
        DaemonAction::Stop => {
            let ctl = clawdesk_daemon::DaemonCtl::new(binary_path, 18789);
            ctl.stop().await.map_err(|e| format!("{e}"))?;
            println!("✓ ClawDesk daemon stopped");
        }
        DaemonAction::Restart => {
            let ctl = clawdesk_daemon::DaemonCtl::new(binary_path, 18789);
            ctl.restart().await.map_err(|e| format!("{e}"))?;
            println!("✓ ClawDesk daemon restarted");
        }
        DaemonAction::Status => {
            let ctl = clawdesk_daemon::DaemonCtl::new(binary_path, 18789);
            let status = ctl.status().await.map_err(|e| format!("{e}"))?;
            println!("{}", status.display());
        }
        DaemonAction::Logs { lines } => {
            let ctl = clawdesk_daemon::DaemonCtl::new(binary_path, 18789);
            let output = ctl.logs(lines).await.map_err(|e| format!("{e}"))?;
            println!("{}", output);
        }
    }

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
    let creds_dir = clawdesk_types::dirs::dot_clawdesk().join("credentials");
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
    let sochdb_dir = clawdesk_types::dirs::sochdb();
    std::fs::create_dir_all(&sochdb_dir)?;
    let store = clawdesk_sochdb::SochStore::open(
        sochdb_dir.to_str().unwrap(),
    ).map_err(|e| format!("failed to open database: {e}"))?;
    Ok(store)
}

// Directory helpers (dirs_home, dirs_data, sochdb, skills) now live in
// clawdesk_types::dirs — the single source of truth for path conventions.

// ── Stub implementations for boot ───────────────────────────

// Noop stubs for PluginFactory, AgentExecutor, DeliveryHandler now live in
// their trait-defining crates: clawdesk_plugin::NoopPluginFactory,
// clawdesk_cron::executor::{NoopAgentExecutor, NoopDeliveryHandler}.

// ── Graceful shutdown callbacks ──────────────────────────────

/// Concrete shutdown callbacks that wire the daemon's 6-phase shutdown
/// protocol to actual gateway state operations.
///
/// Phase 1 (StopAccepting): Cancel token triggered (HTTP server already stopped).
/// Phase 2 (DrainInFlight): Wait for running workflow runs to complete.
/// Phase 3 (CheckpointSessions): Save all active session state.
/// Phase 4 (FlushEventBus): Drain pending WFQ dispatch items to DLQ.
/// Phase 5 (PersistCronState): Stop cron manager & persist next-fire times.
/// Phase 6 (CloseStorage): SochDB checkpoint + fsync.
struct GatewayShutdownCallbacks {
    state: std::sync::Arc<clawdesk_gateway::state::GatewayState>,
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl clawdesk_daemon::ShutdownCallbacks for GatewayShutdownCallbacks {
    async fn execute_phase(
        &self,
        phase: clawdesk_daemon::ShutdownPhase,
    ) -> Result<(), String> {
        use clawdesk_daemon::ShutdownPhase;

        match phase {
            ShutdownPhase::StopAccepting => {
                // Ensure the cancellation token is cancelled (HTTP server
                // should already have stopped, but this acts as a safety net).
                if !self.cancel.is_cancelled() {
                    self.cancel.cancel();
                }

                // Stop inbound adapters from producing new messages.
                {
                    let registry = self.state.inbound_registry.lock().await;
                    registry.stop_all().await;
                }

                info!("shutdown: stop accepting — inbound adapters stopped");
                Ok(())
            }

            ShutdownPhase::DrainInFlight => {
                // Wait for active workflow runs to complete.
                // We scan for runs in "running" state and wait until they
                // either complete or we time out (the DaemonRunner applies
                // the drain_timeout_secs to this phase).
                let store = self.state.store.clone();
                let checkpoint_store =
                    clawdesk_runtime::CheckpointStore::new(store.clone());

                let running_ids = checkpoint_store
                    .load_runs_by_state("running")
                    .await
                    .map_err(|e| format!("failed to scan running runs: {e}"))?;

                if running_ids.is_empty() {
                    info!("shutdown: drain — no in-flight runs");
                    return Ok(());
                }

                info!(
                    count = running_ids.len(),
                    "shutdown: drain — waiting for in-flight runs"
                );

                // Poll every 500ms until all running runs have completed.
                let poll_interval = std::time::Duration::from_millis(500);
                loop {
                    tokio::time::sleep(poll_interval).await;
                    let still_running = checkpoint_store
                        .load_runs_by_state("running")
                        .await
                        .unwrap_or_default();
                    if still_running.is_empty() {
                        break;
                    }
                    tracing::debug!(
                        remaining = still_running.len(),
                        "shutdown: drain — still waiting"
                    );
                }

                info!("shutdown: drain — all in-flight runs completed");
                Ok(())
            }

            ShutdownPhase::CheckpointSessions => {
                // Checkpoint all runs that are in a non-terminal state.
                // This captures pending/running/waiting runs so they can
                // be recovered on next startup.
                let store = self.state.store.clone();
                let checkpoint_store =
                    clawdesk_runtime::CheckpointStore::new(store.clone());

                let mut checkpointed = 0u32;
                for state_label in &["pending", "running", "waiting"] {
                    let run_ids = checkpoint_store
                        .load_runs_by_state(state_label)
                        .await
                        .unwrap_or_default();

                    for run_id in &run_ids {
                        if let Ok(Some(mut run)) = checkpoint_store.load_run(run_id).await {
                            // Mark interrupted runs as pending so recovery
                            // can pick them up.
                            if matches!(run.state, clawdesk_runtime::RunState::Running { .. }) {
                                run.state = clawdesk_runtime::RunState::Pending;
                                run.worker_id = None;
                                run.updated_at = chrono::Utc::now();
                                let _ = checkpoint_store.save_run(&run).await;
                            }
                            checkpointed += 1;
                        }
                    }
                }

                info!(
                    checkpointed,
                    "shutdown: checkpoint — sessions saved"
                );
                Ok(())
            }

            ShutdownPhase::FlushEventBus => {
                // Drain all pending items from the WFQ dispatch queue
                // and record them as dead letter entries for re-processing
                // on next startup.
                let bus = &self.state.event_bus;
                let pending = bus.pending_dispatch_count().await;

                if pending == 0 {
                    info!("shutdown: flush event bus — queue empty");
                    return Ok(());
                }

                let items = bus.drain_prioritized(pending).await;
                let store = self.state.store.clone();
                let dlq = clawdesk_runtime::DeadLetterQueue::new(store);

                let mut flushed = 0u32;
                for item in &items {
                    // Create a DLQ entry so the event can be re-dispatched
                    // after restart.
                    let entry = clawdesk_runtime::DeadLetterEntry {
                        run_id: clawdesk_runtime::RunId(format!(
                            "bus:{}:{}",
                            item.topic, item.offset
                        )),
                        workflow_type: clawdesk_runtime::WorkflowType::A2ATask {
                            task_id: format!(
                                "event-flush:{}:{}",
                                item.topic, item.offset
                            ),
                        },
                        error: "shutdown: event bus flush".into(),
                        attempts: 0,
                        first_attempt_at: chrono::Utc::now(),
                        last_attempt_at: chrono::Utc::now(),
                        last_checkpoint: None,
                        total_input_tokens: 0,
                        total_output_tokens: 0,
                    };
                    if dlq.enqueue(&entry).await.is_ok() {
                        flushed += 1;
                    }
                }

                info!(
                    pending,
                    flushed,
                    "shutdown: flush event bus — items moved to DLQ"
                );
                Ok(())
            }

            ShutdownPhase::PersistCronState => {
                // Stop the cron scheduler and persist task state.
                self.state.cron_manager.stop();

                // Persist all task definitions through the cron persistence
                // backend (if configured). The CronManager already persists
                // on upsert, but we do one final sync of the in-memory state.
                let tasks = self.state.cron_manager.list_tasks().await;

                // Save next-fire metadata via SochDB.
                let store = self.state.store.clone();
                for task in &tasks {
                    let key = format!("cron:shutdown:task:{}", task.id);
                    if let Ok(bytes) = serde_json::to_vec(task) {
                        let _ = store.put(&key, &bytes);
                    }
                }

                info!(
                    tasks = tasks.len(),
                    "shutdown: persist cron state — saved"
                );
                Ok(())
            }

            ShutdownPhase::CloseStorage => {
                // Graceful SochDB shutdown: WAL checkpoint + fsync.
                self.state.store.shutdown().map_err(|e| {
                    format!("SochDB shutdown failed: {e}")
                })?;
                info!("shutdown: storage closed");
                Ok(())
            }
        }
    }
}
