//! # clawdesk-tauri
//!
//! Tauri 2.0 desktop application shell for ClawDesk.
//!
//! Embeds the gateway server and provides a native desktop UI
//! with system tray, menubar, and WebView.

pub mod canvas;
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
pub mod deep_link;
pub mod error;
pub mod i18n;
pub mod persistence;
pub mod state;
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
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            // A second instance was launched — focus the existing window instead
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            tracing::info!(?args, %cwd, "Second instance blocked — focused existing window");
        }))
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            // ── Core commands ──────────────────────────────────────
            commands::get_health,
            commands::create_agent,
            commands::list_agents,
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
            commands::list_skills,
            commands::activate_skill,
            commands::deactivate_skill,
            commands::delete_skill,
            commands::get_skill_detail,
            commands::register_skill,
            commands::validate_skill_md,
            commands::list_pipelines,
            commands::create_pipeline,
            commands::run_pipeline,
            commands::get_pipeline_runs,
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
            // ── Debug: storage diagnostics ───────────────────────
            commands_debug::toggle_debug_mode,
            commands_debug::get_debug_mode,
            commands_debug::debug_storage_snapshot,
            commands_debug::debug_force_persist,
            commands_debug::debug_rehydrate,
            // ── Terminal ───────────────────────────────────────────
            commands_terminal::run_shell_command,
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
                138 // 22 core + 63 service + 5 memory + 46 SochDB advanced + 2 tray
            );

            // C1: System tray with gateway health indicator
            if let Err(e) = tray::setup_tray(app) {
                warn!("System tray initialization failed: {}", e);
            }
            // T21: Post-init health verification
            let state: tauri::State<'_, AppState> = app.state();
            let warnings = state.verify_health();
            if !warnings.is_empty() {
                warn!("Startup health check: {} warning(s)", warnings.len());
            }

            // T25: Initialize OTEL tracer if endpoint is configured
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

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building ClawDesk")
        .run(|app_handle, event| {
            match event {
                tauri::RunEvent::ExitRequested { .. } => {
                    // Flush all pending state to SochDB + ThreadStore before exit
                    let state: tauri::State<'_, AppState> = app_handle.state();
                    info!("App exit requested — flushing state to SochDB + ThreadStore");
                    // Capture session count before persist for debug logging
                    let session_count = state.sessions.read().map(|s| s.len()).unwrap_or(0);
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
                // BEFORE ExitRequested. We flush here too as a safety net —
                // if the process is killed between CloseRequested and
                // ExitRequested, at least this flush happened.
                tauri::RunEvent::WindowEvent {
                    event: tauri::WindowEvent::CloseRequested { .. },
                    ..
                } => {
                    let state: tauri::State<'_, AppState> = app_handle.state();
                    info!("Window close requested — flushing state (macOS safety)");
                    state.persist();
                    if let Err(e) = state.soch_store.checkpoint() {
                        error!(error = %e, "SochDB checkpoint failed on window close");
                    }
                    if let Err(e) = state.soch_store.sync() {
                        error!(error = %e, "SochDB sync failed on window close");
                    }
                    if let Err(e) = state.thread_store.checkpoint_and_gc() {
                        error!(error = %e, "ThreadStore checkpoint failed on window close");
                    }
                    if let Err(e) = state.thread_store.sync() {
                        error!(error = %e, "ThreadStore sync failed on window close");
                    }
                }
                _ => {}
            }
        });
}
