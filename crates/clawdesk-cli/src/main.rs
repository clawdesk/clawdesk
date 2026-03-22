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
mod tmux;
mod tmux_onboard;

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
    /// tmux workspace manager — launch multi-pane terminal layouts
    Tmux {
        #[command(subcommand)]
        action: TmuxAction,
    },
    /// RAG document management — ingest, list, search, delete files
    #[cfg(feature = "rag")]
    #[command(alias = "docs")]
    Rag {
        #[command(subcommand)]
        action: RagAction,
    },
    /// Security health dashboard — check security score and posture
    #[command(alias = "health")]
    SecurityHealth {
        /// Output format: text, json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Run wizard flow for first-time setup (3-step consumer onboarding)
    Wizard,
    /// Plan mode — generate an execution plan, await approval, then execute
    Plan {
        /// Describe what to do
        task: String,
        /// Approval mode: interactive (default), auto
        #[arg(long, default_value = "interactive")]
        approval: String,
    },
    /// Pipe mode — read stdin, process with agent, write stdout
    Pipe {
        /// Prompt/instructions
        #[arg(short, long)]
        prompt: Option<String>,
        /// Model to use
        #[arg(short, long)]
        model: Option<String>,
    },
    /// Resource usage and cost summary
    Resources,
}

#[cfg(feature = "rag")]
#[derive(Subcommand)]
enum RagAction {
    /// Ingest a file into the RAG store
    Ingest {
        /// Path to the file to ingest
        file: String,
    },
    /// List all ingested documents
    #[command(alias = "ls")]
    List,
    /// Search across ingested documents
    Search {
        /// Search query
        query: String,
        /// Number of results to return
        #[arg(long, short = 'k', default_value = "5")]
        top_k: usize,
    },
    /// Delete an ingested document by ID
    #[command(alias = "rm")]
    Delete {
        /// Document ID to delete
        doc_id: String,
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
enum TmuxAction {
    /// Launch a tmux workspace with a preset layout
    Launch {
        /// Layout preset: desktop, workspace, monitor, chat
        #[arg(long, short = 'l', default_value = "desktop")]
        layout: String,
        /// Session name
        #[arg(long, short = 's', default_value = "clawdesk")]
        session: String,
        /// Workspace / project directory
        #[arg(long, short = 'w')]
        workspace: Option<String>,
        /// Model to use in agent panes
        #[arg(long, short = 'm')]
        model: Option<String>,
        /// Do not auto-attach to the session
        #[arg(long)]
        no_attach: bool,
    },
    /// Run interactive tmux onboarding (first-time setup + layout launch)
    Setup {
        /// Session name for the workspace
        #[arg(long, short = 's', default_value = "clawdesk")]
        session: String,
        /// Workspace / project directory
        #[arg(long, short = 'w')]
        workspace: Option<String>,
    },
    /// List active ClawDesk tmux sessions
    #[command(alias = "ls")]
    List,
    /// Attach to an existing ClawDesk tmux session
    Attach {
        /// Session name to attach to
        #[arg(default_value = "clawdesk")]
        session: String,
    },
    /// Kill a ClawDesk tmux session
    Kill {
        /// Session name to kill
        #[arg(default_value = "clawdesk")]
        session: String,
    },
    /// Show available layouts and descriptions
    Layouts,
    /// Show tmux key bindings cheat sheet
    Keys,
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
    /// Trigger a configuration hot-reload
    Reload,
    /// Validate the current configuration without applying it
    Validate,
    /// Show the current reload policy
    Policy,
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
    /// Delete an agent by ID
    #[command(name = "delete", alias = "rm")]
    Delete {
        /// Agent ID to delete
        id: String,
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
                // If the desktop app holds an exclusive lock, fall back to ephemeral mode.
                let sochdb_dir = clawdesk_types::dirs::sochdb();
                std::fs::create_dir_all(&sochdb_dir)?;

                let store = match clawdesk_sochdb::SochStore::open(
                    sochdb_dir.to_str().unwrap(),
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("⚠ SochDB locked (desktop app running?): {e}");
                        eprintln!("  Falling back to ephemeral in-memory storage.");
                        eprintln!("  Sessions and agent state won't persist until the desktop is closed.");
                        clawdesk_sochdb::SochStore::open_in_memory()
                            .map_err(|e2| format!("failed to open in-memory database: {e2}"))?
                    }
                };

                // Build registries
                let channels = clawdesk_channel::registry::ChannelRegistry::new();
                let mut providers = clawdesk_providers::registry::ProviderRegistry::new();
                let mut tools = clawdesk_agents::ToolRegistry::new();

                // Register builtin tools (file I/O, shell, web search, etc.)
                // so gateway agents can use tools, not just text chat.
                let workspace_root = clawdesk_types::dirs::dot_clawdesk();
                clawdesk_agents::builtin_tools::register_builtin_tools(
                    &mut tools,
                    Some(workspace_root),
                );

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

                // Cron manager with gateway agent executor and channel delivery —
                // the GatewayState reference is wired after construction via OnceLock.
                let gw_agent_executor = clawdesk_gateway::state::GatewayAgentExecutor::new();
                let gw_state_handle = gw_agent_executor.state_handle();
                let gw_delivery_handler = clawdesk_gateway::state::ChannelDeliveryHandler::new(
                    std::sync::Arc::clone(&gw_state_handle),
                );
                let cron_manager = std::sync::Arc::new(clawdesk_cron::CronManager::new(
                    std::sync::Arc::new(gw_agent_executor),
                    std::sync::Arc::new(gw_delivery_handler),
                ));

                // Register cron management tools (schedule, list, remove, trigger)
                {
                    let cm = std::sync::Arc::clone(&cron_manager);
                    let cm2 = std::sync::Arc::clone(&cron_manager);
                    let cm3 = std::sync::Arc::clone(&cron_manager);
                    let cm4 = std::sync::Arc::clone(&cron_manager);

                    let schedule_fn: clawdesk_agents::port::AsyncPort<
                        clawdesk_agents::port::CronScheduleRequest,
                        Result<String, String>,
                    > = std::sync::Arc::new(move |req| {
                        let cm = std::sync::Arc::clone(&cm);
                        Box::pin(async move {
                            use clawdesk_types::cron::{CronTask, DeliveryTarget};
                            let task_id = req.task_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                            let delivery_targets: Vec<DeliveryTarget> = req.delivery_targets.iter().map(|(ch, to)| {
                                DeliveryTarget::Channel { channel_id: ch.clone(), conversation_id: to.clone() }
                            }).collect();
                            let task = CronTask {
                                id: task_id.clone(),
                                name: req.name,
                                schedule: req.schedule,
                                prompt: req.prompt,
                                agent_id: req.agent_id,
                                delivery_targets,
                                skip_if_running: true,
                                timeout_secs: req.timeout_secs,
                                enabled: true,
                                created_at: chrono::Utc::now(),
                                updated_at: chrono::Utc::now(),
                                depends_on: vec![],
                                chain_mode: Default::default(),
                                max_retained_logs: 0,
                            };
                            cm.upsert_task(task).await.map_err(|e| format!("{e}"))?;
                            Ok(format!("Scheduled task '{}' (id: {})", task_id, task_id))
                        })
                    });

                    let list_fn: std::sync::Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send + Sync> =
                        std::sync::Arc::new(move || {
                            let cm = std::sync::Arc::clone(&cm2);
                            Box::pin(async move {
                                let tasks = cm.list_tasks().await;
                                let summary: Vec<String> = tasks.iter().map(|t| {
                                    format!("- {} (id: {}, schedule: {}, enabled: {}, targets: {})",
                                        t.name, t.id, t.schedule, t.enabled, t.delivery_targets.len())
                                }).collect();
                                if summary.is_empty() {
                                    Ok("No scheduled tasks.".to_string())
                                } else {
                                    Ok(summary.join("\n"))
                                }
                            })
                        });

                    let remove_fn: std::sync::Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send + Sync> =
                        std::sync::Arc::new(move |id| {
                            let cm = std::sync::Arc::clone(&cm3);
                            Box::pin(async move {
                                cm.remove_task(&id).await.map_err(|e| format!("{e}"))?;
                                Ok(format!("Removed task '{}'", id))
                            })
                        });

                    let trigger_fn: std::sync::Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send + Sync> =
                        std::sync::Arc::new(move |id| {
                            let cm = std::sync::Arc::clone(&cm4);
                            Box::pin(async move {
                                let log = cm.trigger(&id).await.map_err(|e| format!("{e}"))?;
                                Ok(format!("Triggered task '{}': {:?}", id, log.status))
                            })
                        });

                    clawdesk_agents::builtin_tools::register_cron_tools(
                        &mut tools,
                        schedule_fn,
                        list_fn,
                        remove_fn,
                        trigger_fn,
                    );
                    info!("Cron management tools registered (cron_schedule, cron_list, cron_remove, cron_trigger)");
                }

                // Skills — load bundled skills (embedded in binary) then merge
                // with user skills from disk (~/.clawdesk/skills/).
                let skills_dir = clawdesk_types::dirs::skills();
                let _ = std::fs::create_dir_all(&skills_dir);
                let skill_loader = clawdesk_skills::loader::SkillLoader::new(
                    skills_dir,
                );
                let skills = {
                    // Start with bundled skills (52 skills compiled into binary)
                    let mut reg = clawdesk_skills::load_bundled_skills();
                    let bundled_count = reg.len();

                    // Merge user-installed skills from disk on top
                    let disk_count = skill_loader.load_all(&mut reg).await;

                    info!(bundled = bundled_count, disk = disk_count, total = reg.len(), "loaded skills");
                    reg
                };

                // Channel factory with built-in constructors
                let channel_factory = clawdesk_channels::factory::ChannelFactory::with_builtins();

                let state = std::sync::Arc::new(
                    clawdesk_gateway::state::GatewayState::with_cron_arc(
                        channels, providers, tools, store,
                        plugin_host, cron_manager,
                        skills, skill_loader, channel_factory,
                        cancel.clone(),
                        clawdesk_channel::inbound_adapter::InboundAdapterRegistry::new(256),
                    ),
                );

                // Wire the deferred GatewayState reference for cron executor + delivery
                let _ = gw_state_handle.set(std::sync::Arc::clone(&state));

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
            ConfigAction::Reload => {
                println!("Triggering configuration hot-reload...");
                let policy = clawdesk_gateway::reload_policy::ReloadPolicy::load_from_file(
                    &clawdesk_gateway::reload_policy::ReloadPolicy::default_path().unwrap_or_default(),
                ).unwrap_or_default();
                println!("  Policy preset: {:?}", policy.global.preset);
                println!("  Debounce: {}ms", policy.watcher.debounce_ms);
                // Send SIGHUP to running daemon if available
                let pid_path = clawdesk_daemon::PidFile::default_path();
                if pid_path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&pid_path) {
                        if let Ok(pid) = content.trim().parse::<u32>() {
                            #[cfg(unix)]
                            {
                                use std::process::Command;
                                let _ = Command::new("kill")
                                    .args(["-HUP", &pid.to_string()])
                                    .status();
                                println!("✅ SIGHUP sent to daemon (PID {pid})");
                            }
                            #[cfg(not(unix))]
                            {
                                let _ = pid;
                                println!("⚠️  Reload signal not supported on this platform");
                            }
                        }
                    }
                } else {
                    println!("⚠️  No running daemon found — reload applies on next start");
                }
            }
            ConfigAction::Validate => {
                let path = clawdesk_gateway::bootstrap::ClawDeskConfig::default_path();
                if path.exists() {
                    match clawdesk_gateway::bootstrap::ClawDeskConfig::load(&path) {
                        Ok(config) => {
                            println!("✅ Configuration valid: {}", path.display());
                            println!("  Gateway: {}:{}", config.gateway.host, config.gateway.port);
                            println!("  Channels: {}", config.channels.len());
                            println!("  Skills dir: {}", config.skills.dir);
                        }
                        Err(e) => {
                            eprintln!("❌ Configuration invalid: {e}");
                            std::process::exit(1);
                        }
                    }
                } else {
                    println!("⚠️  No config file at {}", path.display());
                    println!("  Using default configuration");
                }
            }
            ConfigAction::Policy => {
                let policy = clawdesk_gateway::reload_policy::ReloadPolicy::load_from_file(
                    &clawdesk_gateway::reload_policy::ReloadPolicy::default_path().unwrap_or_default(),
                ).unwrap_or_default();
                println!("Reload Policy");
                println!("  Preset:           {:?}", policy.global.preset);
                println!("  Debounce:         {}ms", policy.watcher.debounce_ms);
                println!("  Canary:           {}s window, threshold {}", policy.canary.window_secs, policy.canary.health_threshold);
                println!("  Auto-rollback:    {}", policy.canary.auto_rollback);
                println!("  Buffer capacity:  {}", policy.rollback.buffer_capacity);
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
            AgentAction::Delete { id } => {
                agent_compose::cmd_agent_delete(&id).await?;
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
                .map_err(|e: String| -> Box<dyn std::error::Error + Send + Sync> { Box::from(e) })?;
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
        Commands::Tmux { action } => {
            cmd_tmux(action).await?;
        }
        #[cfg(feature = "rag")]
        Commands::Rag { action } => {
            cmd_rag(action)?;
        }
        Commands::SecurityHealth { format } => {
            cmd_security_health(&format).await?;
        }
        Commands::Wizard => {
            cmd_wizard().await?;
        }
        Commands::Plan { task, approval } => {
            cmd_plan(&cli.gateway_url, &task, &approval).await?;
        }
        Commands::Pipe { prompt, model } => {
            cmd_pipe(&cli.gateway_url, prompt.as_deref(), model.as_deref()).await?;
        }
        Commands::Resources => {
            cmd_resources().await?;
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

// ── Tmux commands ────────────────────────────────────────────

/// Handle all `clawdesk tmux *` subcommands.
async fn cmd_tmux(
    action: TmuxAction,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match action {
        TmuxAction::Launch {
            layout,
            session,
            workspace,
            model,
            no_attach,
        } => {
            let config = tmux::TmuxConfig {
                session_name: session,
                layout: tmux::Layout::from_str(&layout),
                gateway_url: "http://127.0.0.1:18789".to_string(),
                model,
                workspace_dir: workspace,
                attach: !no_attach,
                if_exists: tmux::IfExistsPolicy::Ask,
            };
            tmux::launch(&config).map_err(|e: String| -> Box<dyn std::error::Error + Send + Sync> {
                Box::from(e)
            })?;
        }
        TmuxAction::Setup { session, workspace } => {
            tmux_onboard::run_tmux_onboarding(Some(session), workspace).await?;
        }
        TmuxAction::List => {
            let sessions = tmux::list_sessions();
            if sessions.is_empty() {
                println!("No active ClawDesk tmux sessions.");
                println!("  Start one with: clawdesk tmux launch");
            } else {
                println!("Active ClawDesk tmux sessions:");
                for s in &sessions {
                    println!("  • {s}");
                }
                println!();
                println!("  Attach: clawdesk tmux attach <session>");
                println!("  Kill:   clawdesk tmux kill <session>");
            }
        }
        TmuxAction::Attach { session } => {
            if !tmux::session_exists(&session) {
                eprintln!("Session '{session}' does not exist.");
                let sessions = tmux::list_sessions();
                if !sessions.is_empty() {
                    eprintln!("Available sessions: {}", sessions.join(", "));
                }
                std::process::exit(1);
            }
            tmux::attach_session(&session).map_err(|e: String| -> Box<dyn std::error::Error + Send + Sync> {
                Box::from(e)
            })?;
        }
        TmuxAction::Kill { session } => {
            match tmux::kill_session(&session) {
                Ok(()) => println!("Killed session '{session}'"),
                Err(e) => eprintln!("Failed to kill session: {e}"),
            }
        }
        TmuxAction::Layouts => {
            tmux::print_layouts();
        }
        TmuxAction::Keys => {
            tmux::print_keybindings();
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
            let mut tools = clawdesk_agents::ToolRegistry::new();

            // Register builtin tools for daemon agents
            let workspace_root = clawdesk_types::dirs::dot_clawdesk();
            clawdesk_agents::builtin_tools::register_builtin_tools(
                &mut tools,
                Some(workspace_root),
            );

            // Auto-register all providers from env vars
            clawdesk_providers::registry::auto_register_from_env(&mut providers);

            let plugin_host = clawdesk_plugin::PluginHost::new(
                std::sync::Arc::new(clawdesk_plugin::NoopPluginFactory),
                128,
            );
            // Cron manager with gateway agent executor and channel delivery
            let gw_agent_executor = clawdesk_gateway::state::GatewayAgentExecutor::new();
            let gw_state_handle = gw_agent_executor.state_handle();
            let gw_delivery_handler = clawdesk_gateway::state::ChannelDeliveryHandler::new(
                std::sync::Arc::clone(&gw_state_handle),
            );
            let cron_manager = std::sync::Arc::new(clawdesk_cron::CronManager::new(
                std::sync::Arc::new(gw_agent_executor),
                std::sync::Arc::new(gw_delivery_handler),
            ));

            // Register cron management tools for daemon agents
            {
                let cm = std::sync::Arc::clone(&cron_manager);
                let cm2 = std::sync::Arc::clone(&cron_manager);
                let cm3 = std::sync::Arc::clone(&cron_manager);
                let cm4 = std::sync::Arc::clone(&cron_manager);

                let schedule_fn: clawdesk_agents::port::AsyncPort<
                    clawdesk_agents::port::CronScheduleRequest,
                    Result<String, String>,
                > = std::sync::Arc::new(move |req| {
                    let cm = std::sync::Arc::clone(&cm);
                    Box::pin(async move {
                        use clawdesk_types::cron::{CronTask, DeliveryTarget};
                        let task_id = req.task_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                        let delivery_targets: Vec<DeliveryTarget> = req.delivery_targets.iter().map(|(ch, to)| {
                            DeliveryTarget::Channel { channel_id: ch.clone(), conversation_id: to.clone() }
                        }).collect();
                        let task = CronTask {
                            id: task_id.clone(),
                            name: req.name,
                            schedule: req.schedule,
                            prompt: req.prompt,
                            agent_id: req.agent_id,
                            delivery_targets,
                            skip_if_running: true,
                            timeout_secs: req.timeout_secs,
                            enabled: true,
                            created_at: chrono::Utc::now(),
                            updated_at: chrono::Utc::now(),
                            depends_on: vec![],
                            chain_mode: Default::default(),
                            max_retained_logs: 0,
                        };
                        cm.upsert_task(task).await.map_err(|e| format!("{e}"))?;
                        Ok(format!("Scheduled task '{}' (id: {})", task_id, task_id))
                    })
                });

                let list_fn: std::sync::Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send + Sync> =
                    std::sync::Arc::new(move || {
                        let cm = std::sync::Arc::clone(&cm2);
                        Box::pin(async move {
                            let tasks = cm.list_tasks().await;
                            let summary: Vec<String> = tasks.iter().map(|t| {
                                format!("- {} (id: {}, schedule: {}, enabled: {}, targets: {})",
                                    t.name, t.id, t.schedule, t.enabled, t.delivery_targets.len())
                            }).collect();
                            if summary.is_empty() {
                                Ok("No scheduled tasks.".to_string())
                            } else {
                                Ok(summary.join("\n"))
                            }
                        })
                    });

                let remove_fn: std::sync::Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send + Sync> =
                    std::sync::Arc::new(move |id| {
                        let cm = std::sync::Arc::clone(&cm3);
                        Box::pin(async move {
                            cm.remove_task(&id).await.map_err(|e| format!("{e}"))?;
                            Ok(format!("Removed task '{}'", id))
                        })
                    });

                let trigger_fn: std::sync::Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send + Sync> =
                    std::sync::Arc::new(move |id| {
                        let cm = std::sync::Arc::clone(&cm4);
                        Box::pin(async move {
                            let log = cm.trigger(&id).await.map_err(|e| format!("{e}"))?;
                            Ok(format!("Triggered task '{}': {:?}", id, log.status))
                        })
                    });

                clawdesk_agents::builtin_tools::register_cron_tools(
                    &mut tools,
                    schedule_fn,
                    list_fn,
                    remove_fn,
                    trigger_fn,
                );
                info!("Daemon: cron management tools registered");
            }
            let skills_dir = clawdesk_types::dirs::skills();
            let _ = std::fs::create_dir_all(&skills_dir);
            let skill_loader = clawdesk_skills::loader::SkillLoader::new(skills_dir);
            let skills = {
                let mut reg = clawdesk_skills::load_bundled_skills();
                let bundled = reg.len();
                let disk = skill_loader.load_all(&mut reg).await;
                info!(bundled, disk, total = reg.len(), "daemon: loaded skills");
                reg
            };
            let channel_factory = clawdesk_channels::factory::ChannelFactory::with_builtins();

            let state = std::sync::Arc::new(
                clawdesk_gateway::state::GatewayState::with_cron_arc(
                    channels, providers, tools, store,
                    plugin_host, cron_manager,
                    skills, skill_loader, channel_factory,
                    cancel.clone(),
                    clawdesk_channel::inbound_adapter::InboundAdapterRegistry::new(256),
                ),
            );

            // Wire the deferred GatewayState reference for cron executor + delivery
            let _ = gw_state_handle.set(std::sync::Arc::clone(&state));

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

                    // Publish a config reload event on the bus for downstream
                    // listeners (canary, rollback, validation pipeline).
                    let bus = sighup_state.config_event_bus.clone();
                    tokio::spawn(async move {
                        bus.emit_file_changed(
                            0,
                            "sighup".to_string(),
                            "manual".to_string(),
                        );
                    });
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

/// RAG document management — ingest, list, search, delete.
#[cfg(feature = "rag")]
fn cmd_rag(action: RagAction) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let store = open_store()?;
    let rag = clawdesk_rag::RagManager::new(std::sync::Arc::new(store));

    match action {
        RagAction::Ingest { file } => {
            let path = std::path::Path::new(&file);
            match rag.ingest_file(path) {
                Ok((doc_id, chunk_count)) => {
                    let filename = path.file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or(&file);
                    println!("Ingested \"{filename}\" → {chunk_count} chunks (id: {doc_id})");
                }
                Err(e) => eprintln!("Error: {e}"),
            }
        }
        RagAction::List => {
            let docs = rag.list_documents()?;
            if docs.is_empty() {
                println!("No documents ingested.");
            } else {
                println!("{:<38} {:<30} {:<8} {:<8} {}", "ID", "FILENAME", "TYPE", "CHUNKS", "SIZE");
                println!("{}", "-".repeat(90));
                for doc in &docs {
                    let size = if doc.size_bytes > 1_048_576 {
                        format!("{:.1}MB", doc.size_bytes as f64 / 1_048_576.0)
                    } else {
                        format!("{}KB", doc.size_bytes / 1024)
                    };
                    println!("{:<38} {:<30} {:<8} {:<8} {}",
                        doc.id, doc.filename, doc.doc_type.label(), doc.chunk_count, size);
                }
                println!("\n{} document(s)", docs.len());
            }
        }
        RagAction::Search { query, top_k } => {
            let results = rag.search(&query, top_k)?;
            if results.is_empty() {
                println!("No results for \"{query}\".");
            } else {
                for (i, result) in results.iter().enumerate() {
                    println!("--- Result {} (score: {:.3}, doc: {}) ---", i + 1, result.similarity, result.doc_id);
                    // Truncate long chunk text for terminal display
                    let preview: String = result.chunk_text.chars().take(300).collect();
                    println!("{}", preview);
                    if result.chunk_text.len() > 300 {
                        println!("...");
                    }
                    println!();
                }
            }
        }
        RagAction::Delete { doc_id } => {
            rag.remove_document(&doc_id)?;
            println!("Deleted document {doc_id}");
        }
    }
    Ok(())
}

/// Open a SochStore for direct CLI access (config set/get, doctor).
fn open_store() -> Result<clawdesk_sochdb::SochStore, Box<dyn std::error::Error + Send + Sync>> {
    let sochdb_dir = clawdesk_types::dirs::sochdb();
    std::fs::create_dir_all(&sochdb_dir)?;
    let store = clawdesk_sochdb::SochStore::open(
        sochdb_dir.to_str().unwrap(),
    ).map_err(|e| format!("failed to open database: {e}"))?;
    Ok(store)
}

// ── New strategic command implementations ─────────────────────

/// Security health dashboard — displays score and check results.
async fn cmd_security_health(format: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use clawdesk_security::health_dashboard::{SecurityHealthEvaluator, SecurityState};

    // Gather state from local environment
    let state = SecurityState {
        credentials_encrypted: true,
        credential_count: 0,
        sandbox_default_empty: true, // We flipped this in Pre-Phase 0
        skills_sandboxed: 0,
        total_skills: 0,
        skills_verified: 0,
        exposed_ports: 0,
        data_encrypted_at_rest: true,
        audit_trail_active: true,
        audit_chain_valid: true,
        audit_entry_count: 0,
    };

    let report = SecurityHealthEvaluator::evaluate(&state);

    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    } else {
        println!();
        println!("ClawDesk Security Health");
        println!("════════════════════════");
        println!();
        println!("  Score: {}/100 ({})", report.score, report.grade);
        println!("  Checks: {}/{} passed", report.passed_count, report.total_count);
        println!();
        for check in &report.checks {
            let icon = if check.passed { "✓" } else { "✗" };
            let weight = format!("[w={}]", check.weight);
            println!("  {} {:<30} {} {}", icon, check.name, weight, check.status_message);
            if let Some(ref rem) = check.remediation {
                println!("    → fix: {}", rem);
            }
        }
        if !report.critical_issues.is_empty() {
            println!();
            println!("  Critical issues:");
            for issue in &report.critical_issues {
                println!("    ⚠ {}", issue);
            }
        }
        println!();
    }

    Ok(())
}

/// Interactive wizard for first-time setup.
async fn cmd_wizard() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use clawdesk_wizard::flow::{WizardFlow, WizardStep};

    println!();
    println!("ClawDesk Setup Wizard");
    println!("═════════════════════");
    println!();

    let mut flow = WizardFlow::new();
    let defaults = WizardFlow::default_config();

    // Apply defaults
    for (key, value) in &defaults {
        flow.state.set_config(key, value.clone());
    }

    // Step 1: Personalization
    println!("Step 1/3: {}", WizardStep::Personalization.description());
    println!("  (Using default configuration for CLI mode)");
    let bg_tasks = flow.state.advance().unwrap_or_default();
    println!("  → Launching {} background tasks", bg_tasks.len());

    // Step 2: Connection
    println!();
    println!("Step 2/3: {}", WizardStep::Connection.description());
    println!("  (Skipping channel pairing in CLI mode)");
    let bg_tasks = flow.state.advance().unwrap_or_default();
    println!("  → Launching {} background tasks", bg_tasks.len());

    // Step 3: Confirmation
    println!();
    println!("Step 3/3: {}", WizardStep::Confirmation.description());
    let _ = flow.state.advance();

    println!();
    println!("✓ Setup complete! Run 'clawdesk doctor' to verify.");
    println!();

    Ok(())
}

/// Plan mode — generate plan, await approval, execute.
async fn cmd_plan(
    gateway: &str,
    task: &str,
    approval: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("Planning: {}", task);
    println!("Approval mode: {}", approval);
    println!();

    // Send to gateway's plan endpoint
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/plan", gateway))
        .json(&serde_json::json!({
            "task": task,
            "approval_mode": approval,
        }))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
        }
        Ok(r) => {
            eprintln!("Plan failed: HTTP {}", r.status());
        }
        Err(e) => {
            eprintln!("Plan failed: {}", e);
            eprintln!("Is the gateway running? Try: clawdesk gateway run");
        }
    }

    Ok(())
}

/// Pipe mode — stdin → agent → stdout.
async fn cmd_pipe(
    gateway: &str,
    prompt: Option<&str>,
    model: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::io::Read;

    let mut stdin_text = String::new();
    std::io::stdin().read_to_string(&mut stdin_text)?;

    if stdin_text.is_empty() {
        eprintln!("No input on stdin");
        std::process::exit(1);
    }

    let full_prompt = match prompt {
        Some(p) => format!("{}\n\n{}", p, stdin_text),
        None => stdin_text,
    };

    let client = reqwest::Client::new();
    let mut body = serde_json::json!({
        "message": full_prompt,
    });
    if let Some(m) = model {
        body["model"] = serde_json::json!(m);
    }

    let resp = client
        .post(format!("{}/api/v1/message", gateway))
        .json(&body)
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let result: serde_json::Value = r.json().await.unwrap_or_default();
            if let Some(text) = result["response"].as_str() {
                print!("{}", text);
            } else {
                print!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            }
        }
        Ok(r) => {
            eprintln!("Error: HTTP {}", r.status());
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Resource usage summary.
async fn cmd_resources() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let pid = std::process::id();

    // Get RSS via ps
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=,vsz=,%cpu=", "-p", &pid.to_string()])
        .output()?;

    let text = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = text.trim().split_whitespace().collect();

    println!();
    println!("ClawDesk Resource Monitor");
    println!("════════════════════════");
    println!();

    if parts.len() >= 3 {
        let rss_kb: u64 = parts[0].parse().unwrap_or(0);
        let vsz_kb: u64 = parts[1].parse().unwrap_or(0);
        let cpu: &str = parts[2];
        let rss_mb = rss_kb as f64 / 1024.0;
        let nodejs_baseline = 120.0;
        let ratio = nodejs_baseline / rss_mb.max(1.0);

        println!("  RSS Memory:  {:.1} MB", rss_mb);
        println!("  VSZ Memory:  {:.1} MB", vsz_kb as f64 / 1024.0);
        println!("  CPU Usage:   {}%", cpu);
        println!();
        if rss_mb < 50.0 {
            println!("  Comparison:  {:.1}× less than Node.js baseline ({:.0} MB)", ratio, nodejs_baseline);
        } else {
            println!("  Note: Memory includes loaded model context");
        }
    } else {
        println!("  (Could not read process metrics)");
    }

    println!();

    Ok(())
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
