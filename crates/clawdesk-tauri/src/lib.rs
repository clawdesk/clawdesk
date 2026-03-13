//! # clawdesk-tauri
//!
//! Tauri 2.0 desktop application shell for ClawDesk.
//!
//! Embeds the gateway server and provides a native desktop UI
//! with system tray, menubar, and WebView.

pub mod canvas;
pub mod bus_integration;
pub mod commands;
pub mod commands_a2a;
pub mod commands_canvas;
pub mod commands_debug;
pub mod commands_discovery;
pub mod commands_domain;
pub mod commands_infra;
pub mod commands_journal;
pub mod commands_media;
pub mod commands_memory;
pub mod commands_observability;
pub mod commands_plugin;
pub mod commands_runtime;
pub mod commands_security;
pub mod commands_sochdb;
pub mod commands_terminal;
pub mod commands_threads;
pub mod commands_voice;
pub mod commands_sandbox;
pub mod commands_config_reload;
pub mod commands_mcp;
pub mod commands_extensions;
pub mod commands_orchestration;
pub mod pty_session;
pub mod commands_migrate;
pub mod commands_tunnel;
pub mod commands_browser;
pub mod commands_files;
pub mod commands_canvas_a2ui;
pub mod commands_skills_admin;
pub mod commands_local_models;
pub mod commands_rag;
pub mod commands_preview;
pub mod deep_link;
pub mod engine;
pub mod enriched_backend;
pub mod error;
pub mod message_pipeline;
pub mod i18n;
pub mod persistence;
pub mod pipeline_bridge;
pub mod session_cache;
pub mod state;
pub mod state_aggregates;
pub mod streaming_response;
pub mod tray;
pub mod updater;

use state::AppState;
use tauri::{Emitter, Manager};
use tracing::{error, info, warn};

/// Run the Tauri application.
///
/// # Panics
/// Panics if the Tauri application fails to build.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            // A second instance was launched — focus the existing window instead
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            tracing::info!(?args, %cwd, "Second instance blocked — focused existing window");
        }))
        .manage(AppState::new())
        .manage(commands_canvas_a2ui::CanvasA2uiState::new())
        .manage(commands_preview::PreviewRegistry::new())
        .manage(pty_session::PtySessionManager::new())
        .invoke_handler(tauri::generate_handler![
            // ── Core commands ──────────────────────────────────────
            commands::get_health,
            commands::create_agent,
            commands::list_agents,
            commands::update_agent,
            commands::delete_agent,
            commands::import_openclaw_config,
            commands::send_message,
            commands::cancel_active_run,
            commands::get_session_messages,
            commands::get_chat_messages,
            commands::list_sessions,
            commands::create_chat,
            commands::delete_chat,
            commands::clear_all_chats,
            commands::update_chat_title,
            commands::debug_session_storage,
            commands::list_skills,
            commands::activate_skill,
            commands::deactivate_skill,
            commands::delete_skill,
            commands::get_skill_detail,
            commands::register_skill,
            commands::validate_skill_md,
            commands::list_pipelines,
            commands::create_pipeline,
            commands::delete_pipeline,
            commands::update_pipeline,
            commands::run_pipeline,
            commands::get_pipeline_runs,
            commands::list_cron_tasks,
            commands::trigger_cron_task,
            commands::get_cron_logs,
            commands::get_metrics,
            commands::get_security_status,
            commands::get_agent_trace,
            commands::get_tunnel_status,
            commands::create_invite,
            commands::get_config,
            commands::list_models,
            commands::list_channels,
            commands::update_channel,
            commands::disconnect_channel,
            commands::test_llm_connection,
            commands::sync_channel_provider,
            // ── T15: Session Export ────────────────────────────────
            commands::export_session_markdown,
            commands::export_session_json,
            // ── T16: Agent Clone ───────────────────────────────────
            commands::clone_agent,
            // ── Durable runtime ───────────────────────────
            commands_runtime::get_runtime_status,
            commands_runtime::cancel_durable_run,
            commands_runtime::get_durable_run_status,
            commands_runtime::resume_durable_run,
            commands_runtime::list_durable_runs,
            commands_runtime::list_checkpoints,
            commands_runtime::get_dlq,
            // ── Media pipeline ────────────────────────────
            commands_media::get_media_pipeline_status,
            commands_media::get_link_preview,
            commands_media::tts_synthesize,
            commands_media::tts_list_voices,
            // ── Plugin system ─────────────────────────────
            commands_plugin::list_plugins,
            commands_plugin::get_plugin_info,
            commands_plugin::enable_plugin,
            commands_plugin::disable_plugin,
            // ── A2A protocol ──────────────────────────────
            commands_a2a::list_a2a_agents,
            commands_a2a::register_a2a_agent,
            commands_a2a::deregister_a2a_agent,
            commands_a2a::get_agent_card,
            commands_a2a::get_self_agent_card,
            commands_a2a::send_a2a_task,
            commands_a2a::get_a2a_task,
            commands_a2a::list_a2a_tasks,
            commands_a2a::cancel_a2a_task,
            commands_a2a::provide_a2a_task_input,
            // ── Tasks 16,17: OAuth2 + Exec approval ────────────────
            commands_security::start_oauth_flow,
            commands_security::handle_oauth_callback,
            commands_security::refresh_oauth_token,
            commands_security::list_auth_profiles,
            commands_security::remove_auth_profile,
            commands_security::create_approval_request,
            commands_security::approve_request,
            commands_security::deny_request,
            commands_security::get_approval_status,
            commands_security::respond_to_ask_human,
            // ── Discovery + pairing ───────────────────────
            commands_discovery::get_mdns_service_info,
            commands_discovery::start_pairing,
            commands_discovery::complete_pairing,
            commands_discovery::get_pairing_status,
            commands_discovery::list_discovered_peers,
            // ── Observability ─────────────────────────────
            commands_observability::get_observability_config,
            commands_observability::configure_observability,
            // ── Tasks 20-22,29: Infra (notif/clipboard/voice/idle) ─
            commands_infra::send_notification,
            commands_infra::list_notifications,
            commands_infra::read_clipboard,
            commands_infra::write_clipboard,
            commands_infra::get_clipboard_history,
            commands_infra::configure_voice_wake,
            commands_infra::get_voice_wake_status,
            commands_infra::get_idle_status,
            commands_infra::record_activity,
            // ── Tasks 23,24: ACL + scoped tokens ───────────────────
            commands_security::add_acl_rule,
            commands_security::check_permission,
            commands_security::revoke_acl_rules,
            commands_security::generate_token,
            commands_security::validate_token,
            // ── Tasks 25-28: Domain (ctx guard/prompt/negotiate) ───
            commands_domain::get_context_guard_status,
            commands_domain::get_prompt_manifest,
            commands_domain::list_provider_capabilities,
            commands_domain::get_provider_routing,
            commands_domain::get_skill_trust_level,
            commands_domain::evaluate_skill_triggers,
            commands_domain::get_audit_logs,
            commands_domain::get_execution_logs,
            // ── Canvas workspace ──────────────────────────
            commands_canvas::create_canvas,
            commands_canvas::get_canvas,
            commands_canvas::list_canvases,
            commands_canvas::add_canvas_block,
            commands_canvas::remove_canvas_block,
            commands_canvas::connect_canvas_blocks,
            commands_canvas::export_canvas_markdown,
            // ── Memory: remember/recall/forget backed by SochDB ────
            commands_memory::remember_memory,
            commands_memory::remember_batch,
            commands_memory::recall_memories,
            commands_memory::forget_memory,
            commands_memory::get_memory_stats,
            // ── Memory Schema: episodes/events/entities (A4) ────────
            commands_memory::create_episode,
            commands_memory::get_episode,
            commands_memory::search_episodes,
            commands_memory::append_event,
            commands_memory::get_timeline,
            commands_memory::upsert_entity,
            commands_memory::get_entity,
            commands_memory::search_entities,
            commands_memory::get_entity_facts,
            // ── Context query (A1), task queue (A8), views (A5) ─────
            commands_memory::build_context,
            commands_memory::enqueue_task,
            commands_memory::claim_task,
            commands_memory::ack_task,
            commands_memory::nack_task,
            commands_memory::queue_stats,
            commands_memory::list_views,
            commands_memory::query_view,
            // ── SochDB advanced: cache/trace/checkpoint/graph/policy ─
            commands_sochdb::cache_lookup,
            commands_sochdb::cache_store,
            commands_sochdb::cache_invalidate_source,
            commands_sochdb::trace_start_run,
            commands_sochdb::trace_end_run,
            commands_sochdb::trace_start_span,
            commands_sochdb::trace_end_span,
            commands_sochdb::trace_get_spans,
            commands_sochdb::trace_get_run,
            commands_sochdb::trace_update_metrics,
            commands_sochdb::trace_log_tool_call,
            commands_sochdb::checkpoint_create_run,
            commands_sochdb::checkpoint_save,
            commands_sochdb::checkpoint_load,
            commands_sochdb::checkpoint_list,
            commands_sochdb::checkpoint_get_run,
            commands_sochdb::checkpoint_delete_run,
            commands_sochdb::graph_add_node,
            commands_sochdb::graph_get_node,
            commands_sochdb::graph_delete_node,
            commands_sochdb::graph_add_edge,
            commands_sochdb::graph_get_edges,
            commands_sochdb::graph_shortest_path,
            commands_sochdb::graph_get_subgraph,
            commands_sochdb::graph_get_nodes_by_type,
            commands_sochdb::temporal_add_edge,
            commands_sochdb::temporal_invalidate_edge,
            commands_sochdb::temporal_edges_at,
            commands_sochdb::temporal_edge_history,
            commands_sochdb::policy_enable_audit,
            commands_sochdb::policy_get_audit_log,
            commands_sochdb::policy_add_rate_limit,
            commands_sochdb::atomic_memory_write,
            commands_sochdb::atomic_memory_recover,
            commands_sochdb::registry_register_agent,
            commands_sochdb::registry_list_agents,
            commands_sochdb::registry_find_capable,
            commands_sochdb::registry_unregister_agent,
            commands_sochdb::sochdb_checkpoint,
            commands_sochdb::sochdb_sync,
            // ── Storage health, lifecycle, structured tracing, session indexes ─
            commands_sochdb::storage_health,
            commands_sochdb::lifecycle_delete_session,
            commands_sochdb::lifecycle_delete_thread,
            commands_sochdb::lifecycle_delete_agent,
            commands_sochdb::trace_set_span_attributes,
            commands_sochdb::trace_add_span_event,
            commands_sochdb::trace_get_span_attributes,
            commands_sochdb::trace_query_spans_by_attribute,
            commands_sochdb::trace_set_run_attributes,
            commands_sochdb::sessions_by_activity,
            commands_sochdb::sessions_by_channel,
            commands_sochdb::sessions_by_agent,
            commands_sochdb::sessions_rebuild_indexes,
            // ── Debug: storage diagnostics ───────────────────────
            commands_debug::toggle_debug_mode,
            commands_debug::get_debug_mode,
            commands_debug::debug_storage_snapshot,
            commands_debug::debug_force_persist,
            commands_debug::debug_rehydrate,
            // ── Terminal ───────────────────────────────────────────
            commands_terminal::run_shell_command,
            commands_terminal::pty_create_session,
            commands_terminal::pty_write_input,
            commands_terminal::pty_resize,
            commands_terminal::pty_kill_session,
            commands_terminal::pty_list_sessions,
            // ── Threads: namespaced chat-thread persistence ────────
            commands_threads::thread_create,
            commands_threads::thread_get,
            commands_threads::thread_list,
            commands_threads::thread_update,
            commands_threads::thread_delete,
            commands_threads::thread_append_message,
            commands_threads::thread_get_messages,
            commands_threads::thread_get_recent_messages,
            commands_threads::thread_get_messages_range,
            commands_threads::thread_delete_message,
            commands_threads::thread_append_message_with_attachment,
            commands_threads::thread_get_attachment,
            commands_threads::thread_store_checkpoint,
            commands_threads::thread_store_sync,
            commands_threads::thread_store_stats,
            // ── T8: Templates ──────────────────────────────────────────
            commands::list_persona_templates,
            commands::list_life_os_templates,
            commands::instantiate_life_os_template,
            // ── T9: Journal ────────────────────────────────────────────
            commands_journal::add_journal_entry,
            commands_journal::list_journal_entries,
            commands_journal::get_journal_entry,
            commands_journal::delete_journal_entry,
            commands_journal::analyze_journal_triggers,
            commands_journal::get_journal_daily_values,
            // ── Voice Input (local Whisper) ───────────────────────────
            commands_voice::start_voice_recording,
            commands_voice::stop_voice_recording,
            commands_voice::cancel_voice_recording,
            commands_voice::transcribe_audio,
            commands_voice::get_whisper_models,
            commands_voice::download_whisper_model,
            commands_voice::delete_whisper_model,
            commands_voice::get_voice_input_status,
            // ── C1: System tray ────────────────────────────────────────
            tray::get_gateway_health,
            tray::refresh_gateway_health,
            // ── Sandbox: Multi-modal code execution isolation ──────────
            commands_sandbox::get_sandbox_status,
            commands_sandbox::list_sandbox_backends,
            commands_sandbox::execute_sandboxed,
            commands_sandbox::get_sandbox_resource_limits,
            commands_sandbox::cleanup_sandboxes,
            // ── Config Reload: MVCC hot-reload + canary + rollback ────
            commands_config_reload::config_get_reload_policy,
            commands_config_reload::config_get_reload_status,
            commands_config_reload::config_trigger_reload,
            commands_config_reload::config_rollback,
            // ── MCP: Model Context Protocol ───────────────────────────
            commands_mcp::list_mcp_servers,
            commands_mcp::connect_mcp_server,
            commands_mcp::disconnect_mcp_server,
            commands_mcp::list_mcp_tools,
            commands_mcp::call_mcp_tool,
            commands_mcp::get_mcp_server_status,
            commands_mcp::list_mcp_templates,
            commands_mcp::list_mcp_categories,
            commands_mcp::install_mcp_template,
            commands_mcp::disconnect_all_mcp,
            // ── Extensions: Integration registry + vault + health ─────
            commands_extensions::list_integrations,
            commands_extensions::get_integration_detail,
            commands_extensions::list_integration_categories,
            commands_extensions::enable_integration,
            commands_extensions::disable_integration,
            commands_extensions::get_integration_stats,
            commands_extensions::get_extension_config,
            commands_extensions::save_extension_config,
            commands_extensions::validate_extension_config,
            commands_extensions::store_extension_credential,
            commands_extensions::check_extension_credentials,
            commands_extensions::vault_status,
            commands_extensions::vault_initialize,
            commands_extensions::vault_unlock,
            commands_extensions::vault_lock,
            commands_extensions::vault_store_credential,
            commands_extensions::vault_get_credential,
            commands_extensions::vault_delete_credential,
            commands_extensions::vault_list_credentials,
            commands_extensions::get_all_health_statuses,
            commands_extensions::get_integration_health,
            commands_extensions::check_integration_health,
            commands_extensions::start_extension_oauth,
            commands_extensions::run_extension_oauth,
            commands_extensions::complete_extension_oauth,
            // ── Migration: Import from other AI apps ──────────────────
            commands_migrate::list_migration_sources,
            commands_migrate::validate_migration_source,
            commands_migrate::run_migration,
            commands_migrate::preview_migration,            // ── Tunnel: WireGuard remote access ───────────────────────
            commands_tunnel::get_tunnel_detail,
            commands_tunnel::start_tunnel,
            commands_tunnel::stop_tunnel,
            commands_tunnel::list_tunnel_peers,
            commands_tunnel::add_tunnel_peer,
            commands_tunnel::remove_tunnel_peer,
            commands_tunnel::generate_tunnel_invite,
            commands_tunnel::list_tunnel_invites,
            commands_tunnel::prune_tunnel_invites,
            commands_tunnel::validate_invite_code,
            // ── Browser: CDP automation ────────────────────────────────
            commands_browser::list_browser_tools,
            commands_browser::get_browser_status,
            commands_browser::execute_browser_action,
            // ── Skills Admin: hot-reload, activate/deactivate ──────────
            commands_skills_admin::admin_list_skills,
            commands_skills_admin::admin_reload_skills,
            commands_skills_admin::admin_activate_skill,
            commands_skills_admin::admin_deactivate_skill,
            commands_skills_admin::admin_get_skills_dir,
            // ── Canvas A2UI: Agent-controlled WebView + A2UI protocol ──
            commands_canvas_a2ui::canvas_a2ui_present,
            commands_canvas_a2ui::canvas_a2ui_hide,
            commands_canvas_a2ui::canvas_a2ui_navigate,
            commands_canvas_a2ui::canvas_a2ui_eval,
            commands_canvas_a2ui::canvas_a2ui_snapshot,
            commands_canvas_a2ui::canvas_a2ui_push,
            commands_canvas_a2ui::canvas_a2ui_reset,
            commands_canvas_a2ui::canvas_a2ui_status,
            // ── Device: Info, status, location, capabilities ──────────
            commands_canvas_a2ui::device_get_info,
            commands_canvas_a2ui::device_get_status,
            commands_canvas_a2ui::device_get_location,
            commands_canvas_a2ui::device_capabilities,
            // ── Orchestration: task DAG, dispatch, capabilities ─────
            commands_orchestration::list_capabilities,
            commands_orchestration::get_orchestration_status,
            commands_orchestration::list_agent_flows,
            commands_orchestration::create_agent_flow,
            commands_orchestration::update_agent_flow,
            commands_orchestration::delete_agent_flow,
            commands_orchestration::list_flow_templates,
            commands_orchestration::run_orchestration,
            commands_orchestration::list_orchestration_tasks,
            commands_orchestration::send_orchestrated,
            // ── Files: workspace file browser ─────────────────────────
            commands_files::get_workspace_root,
            commands_files::list_workspace_files,
            commands_files::read_workspace_file,
            // ── Local Models: manage local LLMs ───────────────────────
            commands_local_models::local_models_status,
            commands_local_models::local_models_system_info,
            commands_local_models::local_models_recommend,
            commands_local_models::local_models_start,
            commands_local_models::local_models_stop,
            commands_local_models::local_models_running,
            commands_local_models::local_models_download,
            commands_local_models::local_models_delete,
            commands_local_models::local_models_set_server_path,
            commands_local_models::local_models_scan_directory,
            commands_local_models::local_models_import,
            commands_local_models::local_models_set_ttl,
            // ── RAG: document ingestion & retrieval ───────────────────
            commands_rag::rag_ingest_document,
            commands_rag::rag_list_documents,
            commands_rag::rag_delete_document,
            commands_rag::rag_search,
            commands_rag::rag_get_chunks,
            commands_rag::rag_build_context,
            // ── Preview: live web app preview ─────────────────────────
            commands_preview::preview_register,
            commands_preview::preview_list,
            commands_preview::preview_remove,
            commands_preview::preview_check_port,
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.set_decorations(true);
                    let _ = window.set_title_bar_style(tauri::TitleBarStyle::Overlay);
                    let _ = window.set_theme(Some(tauri::Theme::Light));
                }
            }
            info!(
                "ClawDesk Tauri app starting — {} IPC commands registered",
                177 // 138 existing + 5 sandbox + 10 MCP + 19 extensions + 4 migrate + 1 padding
            );

            // C1: System tray with gateway health indicator
            if let Err(e) = tray::setup_tray(app) {
                warn!("System tray initialization failed: {}", e);
            }
            // Post-init health verification
            let state: tauri::State<'_, AppState> = app.state();
            let warnings = state.verify_health();
            if !warnings.is_empty() {
                warn!("Startup health check: {} warning(s)", warnings.len());
            }

            // Start the TTL reaper for local models (needs Tokio runtime)
            {
                let lm_guard = state.local_model_manager.read().unwrap();
                if let Some(ref mgr) = *lm_guard {
                    clawdesk_local_models::LocalModelManager::start_ttl_reaper(
                        std::sync::Arc::clone(&mgr.server),
                    );
                }
            }

            // ── Embedded gateway server ──────────────────────────────
            // Spawn the HTTP/WS gateway on localhost:18789 so channel
            // webhooks, the OpenAI-compatible API, and A2A protocol
            // work without running `clawdesk-cli serve` separately.
            {
                use clawdesk_channel::inbound_adapter::InboundAdapterRegistry;
                use clawdesk_channel::registry::ChannelRegistry;
                use clawdesk_channels::factory::ChannelFactory;
                use clawdesk_gateway::{GatewayConfig, state::GatewayState};

                // Build fresh registries for the gateway — the gateway uses ArcSwap
                // for lock-free reads, while AppState uses RwLock. They operate
                // independently; the gateway handles external HTTP/WS traffic.
                let gw_channels = ChannelRegistry::new();
                let gw_providers = clawdesk_providers::registry::ProviderRegistry::new();
                let gw_tools = clawdesk_agents::ToolRegistry::new();

                // Open an in-memory SochStore for the gateway (the desktop app
                // owns the on-disk store — the gateway is just the HTTP layer).
                let gw_store = match clawdesk_sochdb::SochStore::open_ephemeral_quiet() {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("Gateway SochStore failed, skipping embedded gateway: {}", e);
                        // Skip gateway but continue the rest of setup
                        return Ok(());
                    }
                };

                let gw_plugin_host = clawdesk_plugin::PluginHost::new(
                    std::sync::Arc::new(clawdesk_plugin::NoopPluginFactory),
                    128,
                );
                let gw_cron = clawdesk_cron::CronManager::new(
                    std::sync::Arc::new(clawdesk_cron::executor::NoopAgentExecutor),
                    std::sync::Arc::new(state::NoOpDelivery),
                );

                // Skills
                let skills_dir = clawdesk_types::dirs::skills();
                let _ = std::fs::create_dir_all(&skills_dir);
                let gw_skill_loader = clawdesk_skills::loader::SkillLoader::new(skills_dir);
                let gw_skills = clawdesk_skills::registry::SkillRegistry::new();

                let gw_factory = ChannelFactory::with_builtins();
                let gw_inbound = InboundAdapterRegistry::new(256);

                let cancel = state.cancel.clone();

                let gw_state = std::sync::Arc::new(GatewayState::new(
                    gw_channels,
                    gw_providers,
                    gw_tools,
                    gw_store,
                    gw_plugin_host,
                    gw_cron,
                    gw_skills,
                    gw_skill_loader,
                    gw_factory,
                    cancel.clone(),
                    gw_inbound,
                ));

                let port: u16 = std::env::var("CLAWDESK_GATEWAY_PORT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(18789);

                let config = GatewayConfig {
                    host: "127.0.0.1".to_string(),
                    port,
                    ..Default::default()
                };

                // Spawn on a dedicated thread with its own Tokio runtime.
                // The Tauri .setup() hook runs before Tauri's internal runtime
                // is available, so we can't use tokio::spawn() here.
                std::thread::Builder::new()
                    .name("gateway-server".into())
                    .spawn(move || {
                        let rt = tokio::runtime::Builder::new_multi_thread()
                            .worker_threads(2)
                            .enable_all()
                            .build()
                            .expect("gateway tokio runtime");
                        rt.block_on(async move {
                            info!(port, "Starting embedded gateway server");
                            if let Err(e) = clawdesk_gateway::serve(config, gw_state, cancel).await {
                                error!("Embedded gateway exited with error: {}", e);
                            }
                        });
                    })
                    .expect("failed to spawn gateway thread");

                info!(port, "Embedded gateway server spawned on localhost");
            }

            // ── Start inbound channel adapters ───────────────────────
            // For channels that support inbound messages (Discord, Telegram, etc.),
            // call Channel::start() with a MessageSink that routes messages through
            // the agent runner and sends responses back via the channel.
            {
                use clawdesk_channel::Channel;
                use clawdesk_types::channel::ChannelId;

                let sink = std::sync::Arc::new(state::ChannelMessageSink {
                    negotiator: std::sync::Arc::clone(&state.negotiator),
                    tool_registry: std::sync::Arc::clone(&state.tool_registry),
                    app_handle: app.handle().clone(),
                    channel_registry: std::sync::Arc::clone(&state.channel_registry),
                    cancel: state.cancel.clone(),
                    conversation_histories: std::sync::Arc::new(dashmap::DashMap::new()),
                    last_channel_origins: std::sync::Arc::clone(&state.last_channel_origins),
                });

                // Check which channels are registered and start their inbound loops
                let channels_to_start: Vec<(ChannelId, std::sync::Arc<dyn Channel>)> = {
                    let reg = state.channel_registry.read().expect("channel registry lock");
                    reg.iter()
                        .filter(|(id, _)| matches!(id,
                            ChannelId::Discord | ChannelId::Telegram | ChannelId::Slack
                            | ChannelId::WhatsApp | ChannelId::Email
                            | ChannelId::IMessage | ChannelId::Irc
                        ))
                        .map(|(id, ch)| (*id, std::sync::Arc::clone(ch)))
                        .collect()
                };

                for (id, ch) in channels_to_start {
                    let ch_sink = std::sync::Arc::clone(&sink);
                    // Spawn on a background thread with its own runtime so
                    // Channel::start() can do async work (token validation,
                    // WebSocket connection, etc.) without blocking the setup hook.
                    let ch_name = format!("{id}");
                    std::thread::Builder::new()
                        .name(format!("channel-{id}"))
                        .spawn(move || {
                            let rt = tokio::runtime::Builder::new_multi_thread()
                                .worker_threads(1)
                                .enable_all()
                                .build()
                                .expect("channel runtime");
                            rt.block_on(async move {
                                info!(channel = %ch_name, "Starting inbound channel adapter");
                                match ch.start(ch_sink).await {
                                    Ok(()) => info!(channel = %ch_name, "Channel adapter started"),
                                    Err(e) => error!(channel = %ch_name, error = %e, "Channel adapter failed to start"),
                                }
                                // Keep the runtime alive so spawned gateway tasks continue running
                                tokio::signal::ctrl_c().await.ok();
                            });
                        })
                        .ok();
                }
            }

            // Initialize OTEL tracer if endpoint is configured
            if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
                info!(endpoint = %endpoint, "Initializing OpenTelemetry tracer");
                let otel_config = clawdesk_observability::tracer::OtelConfig {
                    otlp_endpoint: endpoint,
                    service_name: "clawdesk-desktop".to_string(),
                    service_version: env!("CARGO_PKG_VERSION").to_string(),
                    ..Default::default()
                };
                match clawdesk_observability::tracer::init_tracer(otel_config) {
                    Ok(_tracer) => info!("OpenTelemetry tracer initialized"),
                    Err(e) => warn!("Failed to initialize OTEL tracer: {}", e),
                }
            }

            // Periodic SochDB + ThreadStore checkpoint every 30 seconds.
            // Reduced from 60s to 30s for better crash safety.
            // Protects against data loss from crashes or force-quit by ensuring
            // WAL entries are checkpointed regularly, not just on message send.

            // ── Spawn orchestration event bridge ─────────────────────
            // Forwards OrchestrationEvents to the Tauri frontend and the
            // reactive event bus so skills/pipelines can react.
            commands_orchestration::spawn_orchestration_bridge(
                app.handle().clone(),
                &state,
            );

            // ── Initialize event bus standard topics ──────────────────
            // Pre-create standard topics including orchestrator topics
            // so they're ready for publish without lazy creation on hot path.
            {
                let bus = state.event_bus.clone();
                let cancel = state.cancel.clone();
                std::thread::Builder::new()
                    .name("bus-init".into())
                    .spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("bus init runtime");
                        rt.block_on(async move {
                            let topics = [
                                "channel.inbound.telegram",
                                "channel.inbound.discord",
                                "channel.inbound.slack",
                                "channel.inbound.webchat",
                                "channel.inbound.internal",
                                "agent.message.sent",
                                "cron.task.executed",
                                "pipeline.completed",
                                "memory.stored",
                                "skill.lifecycle",
                                "system.startup",
                                "system.shutdown",
                                "orchestrator.task.completed",
                                "orchestrator.task.failed",
                                "orchestrator.dag.created",
                                "orchestrator.finished",
                                "orchestrator.escalated",
                                "orchestrator.event",
                            ];
                            for topic in &topics {
                                bus.topic(topic).await;
                            }
                            info!(topics = topics.len(), "Event bus standard topics initialized");

                            // Emit startup event so bus subscribers can react
                            bus_integration::emit_startup(&bus).await;

                            // Spawn inbound bridge for channel adapter → bus event conversion
                            let _inbound_tx = bus_integration::spawn_inbound_bridge(
                                bus.clone(),
                                cancel,
                            );
                            info!("Event bus fully initialized with inbound bridge");
                        });
                    })
                    .ok();
            }

            // Periodic SochDB + ThreadStore checkpoint every 30 seconds.
            // Reduced from 60s to 30s for better crash safety.
            // Protects against data loss from crashes or force-quit by ensuring
            // WAL entries are checkpointed regularly, not just on message send.
            {
                let soch_store = std::sync::Arc::clone(&state.soch_store);
                let thread_store = std::sync::Arc::clone(&state.thread_store);
                std::thread::spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(30));
                        match soch_store.checkpoint_and_gc() {
                            Ok(seq) => tracing::debug!(seq, "Periodic SochDB checkpoint complete"),
                            Err(e) => tracing::warn!(error = %e, "Periodic SochDB checkpoint failed"),
                        }
                        match thread_store.checkpoint_and_gc() {
                            Ok(seq) => tracing::debug!(seq, "Periodic ThreadStore checkpoint complete"),
                            Err(e) => tracing::warn!(error = %e, "Periodic ThreadStore checkpoint failed"),
                        }
                    }
                });
                info!("Periodic SochDB + ThreadStore checkpoint thread started (30s interval)");
            }

            // ── Initialize deferred AppHandle for CronAgentExecutor ───
            // CronAgentExecutor routes through send_message which needs
            // the AppHandle. It wasn't available during AppState::new(),
            // so we set it here.
            let _ = state.cron_app_handle.set(app.handle().clone());

            // ── Restore cron schedules from persisted pipelines ───────
            // On startup we do a two-phase restore:
            //  1. load_persisted() — rehydrate all cron tasks from SochDB
            //     (preserves user edits like enabled/disabled, custom timeout).
            //  2. Merge pipeline-derived tasks — update schedule/prompt from
            //     pipeline definitions (which may have changed), but preserve
            //     persisted `enabled` and `timeout_secs` states.
            //
            // Collect scheduled pipelines synchronously here; the actual
            // upsert_task calls happen in the cron-manager thread where
            // an async runtime is available.
            let cron_restore_tasks: Vec<clawdesk_types::cron::CronTask> = {
                let p = state.pipelines.read().expect("pipelines lock");
                p.iter()
                    .filter_map(|desc| {
                        let schedule = desc.schedule.as_deref()?;
                        // Build prompt from pipeline steps (same logic as sync_pipeline_cron_schedule)
                        let step_instructions: Vec<String> = desc.steps.iter()
                            .enumerate()
                            .filter(|(_, s)| s.node_type == "agent")
                            .map(|(i, step)| {
                                let custom_prompt = step.config.get("prompt").cloned().unwrap_or_default();
                                if !custom_prompt.is_empty() {
                                    format!("Step {}: {} — {}", i + 1, step.label, custom_prompt)
                                } else {
                                    format!("Step {}: {}", i + 1, step.label)
                                }
                            })
                            .collect();

                        let prompt = if step_instructions.is_empty() {
                            format!(
                                "Execute the scheduled pipeline '{}'. {}",
                                desc.name, desc.description
                            )
                        } else {
                            format!(
                                "Execute the scheduled pipeline '{}'. {}\n\nSteps to perform:\n{}",
                                desc.name, desc.description, step_instructions.join("\n")
                            )
                        };

                        let now = chrono::Utc::now();
                        Some(clawdesk_types::cron::CronTask {
                            id: format!("pipeline:{}", desc.id),
                            name: format!("Pipeline: {}", desc.name),
                            schedule: schedule.to_string(),
                            enabled: true,
                            prompt,
                            agent_id: Some(format!("pipeline:{}", desc.id)),
                            delivery_targets: vec![],
                            skip_if_running: true,
                            timeout_secs: 600,
                            created_at: now,
                            updated_at: now,
                            depends_on: vec![],
                            chain_mode: Default::default(),
                            max_retained_logs: 0,
                        })
                    })
                    .collect()
            };
            if !cron_restore_tasks.is_empty() {
                info!(count = cron_restore_tasks.len(), "Collected {} pipeline schedule(s) for cron restoration", cron_restore_tasks.len());
            }

            // ── Spawn CronManager tick loop ───────────────────────────
            // The cron manager runs a 60-second tick loop checking for
            // scheduled pipeline tasks. Without this spawn, cron schedules
            // are registered but never actually fire.
            {
                let cron_mgr = std::sync::Arc::clone(&state.cron_manager);
                std::thread::Builder::new()
                    .name("cron-manager".into())
                    .spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("cron-manager runtime");
                        rt.block_on(async move {
                            // Phase 1: Rehydrate persisted cron tasks from SochDB.
                            // This restores user edits (enabled/disabled, timeouts, standalone tasks).
                            match cron_mgr.load_persisted().await {
                                Ok(count) => info!(count, "Loaded persisted cron tasks from SochDB"),
                                Err(e) => warn!(error = %e, "Failed to load persisted cron tasks"),
                            }

                            // Phase 2: Merge pipeline-derived tasks.
                            // For each pipeline task, check if a persisted version exists.
                            // If so, update schedule/prompt/name from pipeline but preserve
                            // the persisted `enabled` and `timeout_secs` values.
                            for mut task in cron_restore_tasks {
                                if let Some(persisted) = cron_mgr.get_task(&task.id).await {
                                    // Preserve user-editable fields from persisted state.
                                    task.enabled = persisted.enabled;
                                    task.timeout_secs = persisted.timeout_secs;
                                    task.depends_on = persisted.depends_on;
                                    task.created_at = persisted.created_at;
                                    // Keep the pipeline-derived schedule, prompt, name
                                    // (they reflect the latest pipeline definition).
                                }
                                let tid = task.id.clone();
                                match cron_mgr.upsert_task(task).await {
                                    Ok(()) => info!(task_id = %tid, "Restored cron schedule"),
                                    Err(e) => warn!(task_id = %tid, error = %e, "Failed to restore cron schedule"),
                                }
                            }
                            info!("CronManager tick loop started");
                            cron_mgr.run().await;
                        });
                    })
                    .expect("failed to spawn cron-manager thread");
                info!("CronManager tick loop thread spawned (60s interval)");
            }

            // ── Launch enabled MCP integrations + health monitor ──
            // Runs on a background thread (Tauri setup doesn't have an async
            // runtime yet). Connects MCP servers for previously-enabled
            // integrations and starts the health monitor loop.
            {
                let handle = app.handle().clone();
                std::thread::Builder::new()
                    .name("extensions-launch".into())
                    .spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("extensions-launch runtime");
                        rt.block_on(async {
                            let app_state: tauri::State<'_, AppState> = handle.state();
                            crate::commands_extensions::launch_enabled_integrations(&app_state).await;
                        });
                    })
                    .expect("failed to spawn extensions-launch thread");
                info!("Extension MCP launch thread spawned");
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building ClawDesk")
        .run(|app_handle, event| {
            match event {
                tauri::RunEvent::ExitRequested { .. } => {
                    // Flush all pending state to SochDB + ThreadStore before exit
                    let state: tauri::State<'_, AppState> = app_handle.state();
                    // Signal the embedded gateway (and any other CancellationToken
                    // consumers) to shut down gracefully.
                    state.cancel.cancel();
                    info!("App exit requested — flushing state to SochDB + ThreadStore");
                    // Capture session count before persist for debug logging
                    let session_count = state.sessions.len();
                    commands_debug::emit_debug(app_handle, commands_debug::DebugEvent::info(
                        "shutdown", "exit_persist_start",
                        format!("ExitRequested — persisting {} sessions to SochDB", session_count),
                    ));
                    state.persist();
                    commands_debug::emit_debug(app_handle, commands_debug::DebugEvent::info(
                        "shutdown", "exit_persist_done", "persist() complete, running checkpoint+sync",
                    ));
                    // Belt-and-suspenders: checkpoint + fsync to ensure WAL is durable
                    if let Err(e) = state.soch_store.checkpoint() {
                        error!(error = %e, "Final SochDB checkpoint failed on exit");
                    }
                    if let Err(e) = state.soch_store.sync() {
                        error!(error = %e, "Final SochDB sync failed on exit");
                    }
                    if let Err(e) = state.thread_store.checkpoint_and_gc() {
                        error!(error = %e, "Final ThreadStore checkpoint failed on exit");
                    }
                    if let Err(e) = state.thread_store.sync() {
                        error!(error = %e, "Final ThreadStore sync failed on exit");
                    }
                    info!("Exit flush complete — checkpoint + fsync done");
                }
                // On macOS, Cmd+Q sends CloseRequested on the window
                // BEFORE ExitRequested. When the user closes the window via
                // the red button, we hide to tray instead of exiting so the
                // gateway, channels, and background services keep running.
                tauri::RunEvent::WindowEvent {
                    event: tauri::WindowEvent::CloseRequested { api, .. },
                    label,
                    ..
                } => {
                    // Prevent the window from actually closing — hide to tray
                    api.prevent_close();
                    if let Some(window) = app_handle.get_webview_window(&label) {
                        let _ = window.hide();
                    }
                    info!("Window hidden to tray — background services still running");

                    // Flush state as a safety measure
                    let state: tauri::State<'_, AppState> = app_handle.state();
                    state.persist();
                    if let Err(e) = state.soch_store.checkpoint() {
                        error!(error = %e, "SochDB checkpoint failed on window hide");
                    }
                }
                _ => {}
            }
        });
}
