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
pub mod commands_discovery;
pub mod commands_domain;
pub mod commands_infra;
pub mod commands_media;
pub mod commands_memory;
pub mod commands_observability;
pub mod commands_plugin;
pub mod commands_runtime;
pub mod commands_security;
pub mod commands_sochdb;
pub mod error;
pub mod i18n;
pub mod persistence;
pub mod state;
pub mod updater;

use state::AppState;
use tauri::Manager;
use tracing::info;

/// Run the Tauri application.
///
/// # Panics
/// Panics if the Tauri application fails to build.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            // ── Core commands ──────────────────────────────────────
            commands::get_health,
            commands::create_agent,
            commands::list_agents,
            commands::delete_agent,
            commands::import_openclaw_config,
            commands::send_message,
            commands::get_session_messages,
            commands::list_sessions,
            commands::list_skills,
            commands::activate_skill,
            commands::deactivate_skill,
            commands::list_pipelines,
            commands::create_pipeline,
            commands::run_pipeline,
            commands::get_metrics,
            commands::get_security_status,
            commands::get_agent_trace,
            commands::get_tunnel_status,
            commands::create_invite,
            commands::get_config,
            commands::list_models,
            commands::list_channels,
            // ── Task 12: Durable runtime ───────────────────────────
            commands_runtime::get_runtime_status,
            commands_runtime::cancel_durable_run,
            commands_runtime::get_durable_run_status,
            commands_runtime::resume_durable_run,
            // ── Task 13: Media pipeline ────────────────────────────
            commands_media::get_media_pipeline_status,
            commands_media::get_link_preview,
            // ── Task 14: Plugin system ─────────────────────────────
            commands_plugin::list_plugins,
            commands_plugin::get_plugin_info,
            commands_plugin::enable_plugin,
            commands_plugin::disable_plugin,
            // ── Task 15: A2A protocol ──────────────────────────────
            commands_a2a::list_a2a_agents,
            commands_a2a::register_a2a_agent,
            commands_a2a::deregister_a2a_agent,
            commands_a2a::get_agent_card,
            commands_a2a::get_self_agent_card,
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
            // ── Task 18: Discovery + pairing ───────────────────────
            commands_discovery::get_mdns_service_info,
            commands_discovery::start_pairing,
            commands_discovery::complete_pairing,
            commands_discovery::get_pairing_status,
            commands_discovery::list_discovered_peers,
            // ── Task 19: Observability ─────────────────────────────
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
            // ── Task 30: Canvas workspace ──────────────────────────
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
                136 // 22 core + 63 service + 5 memory + 46 SochDB advanced
            );
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running ClawDesk");
}
