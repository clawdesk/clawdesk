//! # clawdesk-gateway
//!
//! Axum-based HTTP/WebSocket gateway server.
//!
//! Routes:
//! - `POST /api/v1/message` — send a message to the agent
//! - `GET  /api/v1/health` — health check
//! - `GET  /api/v1/sessions` — list sessions
//! - `GET  /api/v1/channels` — list channels and status
//! - `GET  /ws` — WebSocket for streaming agent responses
//!
//! Extended API:
//! - `GET  /api/v1/config` — runtime configuration
//! - `GET  /api/v1/models` — available models
//! - `GET  /api/v1/sessions/:id` — session detail
//! - `GET  /api/v1/sessions/:id/messages` — conversation history
//! - `DELETE /api/v1/sessions/:id` — delete session
//! - `POST /api/v1/sessions/:id/compact` — context compaction
//!
//! OpenAI-compatible API:
//! - `POST /v1/chat/completions` — chat completions (streaming + non-streaming)
//! - `GET  /v1/models` — list models
//!
//! Admin routes:
//! - `GET  /api/v1/admin/plugins` — list installed plugins
//! - `POST /api/v1/admin/plugins/:name/reload` — reload a plugin
//! - `GET  /api/v1/admin/cron/tasks` — list cron tasks
//! - `POST /api/v1/admin/cron/tasks` — create a cron task
//! - `POST /api/v1/admin/cron/tasks/:id/trigger` — manually trigger
//! - `GET  /api/v1/admin/metrics` — runtime metrics
//!
//! Skill admin routes (plug-and-play):
//! - `GET    /api/v1/admin/skills` — list all skills + state
//! - `POST   /api/v1/admin/skills/reload` — hot-reload from filesystem
//! - `POST   /api/v1/admin/skills/:id/activate` — activate a skill
//! - `POST   /api/v1/admin/skills/:id/deactivate` — deactivate a skill
//! - `POST   /api/v1/admin/channels/reload` — rebuild channels from config

pub mod a2a_routes;
pub mod admin;
pub mod agent_loader;
pub mod bootstrap;
#[cfg(feature = "browser")]
pub mod browser_routes;
pub mod connection_mode;
pub mod durable_response_store;
pub mod error;
pub mod fanout;
pub mod fanout_executor;
pub mod grace_window;
pub mod idempotency;
pub mod middleware;
pub mod observability;
pub mod openai_compat;
pub mod orchestrator;
pub mod rate_limiter;
pub mod responses_api;
pub mod routes;
pub mod rpc;
pub mod skills_admin;
pub mod state;
pub mod subagent_manager;
pub mod task_dispatcher;
pub mod thread_ownership;
pub mod wake;
pub mod watcher;
pub mod webhook;
pub mod webhook_queue;
pub mod ws;

use axum::{
    routing::{delete, get, post},
    Router,
};
use state::GatewayState;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Gateway server configuration.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub host: String,
    pub port: u16,
    pub cors_origins: Vec<String>,
    /// Bearer token for admin API. Empty string = no auth.
    pub admin_token: String,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 18789,
            cors_origins: vec!["http://localhost:*".to_string()],
            admin_token: String::new(),
        }
    }
}

/// Build the Axum router with all routes.
pub fn build_router(state: Arc<GatewayState>) -> Router {
    // Public API routes
    let api = Router::new()
        .route("/api/v1/health", get(routes::health))
        .route("/api/v1/message", post(routes::send_message))
        .route("/api/v1/sessions", get(routes::list_sessions))
        .route("/api/v1/channels", get(routes::list_channels))
        .route("/api/v1/config", get(rpc::get_config))
        .route("/api/v1/models", get(rpc::list_models))
        .route(
            "/api/v1/sessions/:id",
            get(rpc::get_session).delete(rpc::delete_session),
        )
        .route(
            "/api/v1/sessions/:id/messages",
            get(rpc::get_session_messages),
        )
        .route(
            "/api/v1/sessions/:id/compact",
            post(rpc::compact_session),
        )
        // Thread-as-Agent routes
        .route("/api/v1/thread-agents", get(routes::list_thread_agents))
        .route(
            "/api/v1/thread-agents/:thread_id/delegate",
            post(routes::delegate_task),
        );

    // OpenAI-compatible API
    let openai = Router::new()
        .route("/v1/chat/completions", post(openai_compat::chat_completions))
        .route("/v1/models", get(openai_compat::list_models))
        // Responses API (OpenAI compatible)
        .route("/v1/responses", post(responses_api::create_response))
        .route("/v1/responses/:id", get(responses_api::get_response));

    // Admin routes
    let admin = Router::new()
        .route("/api/v1/admin/plugins", get(admin::list_plugins))
        .route(
            "/api/v1/admin/plugins/:name/reload",
            post(admin::reload_plugin),
        )
        .route(
            "/api/v1/admin/cron/tasks",
            get(admin::list_cron_tasks).post(admin::create_cron_task),
        )
        .route(
            "/api/v1/admin/cron/tasks/:id",
            delete(admin::delete_cron_task),
        )
        .route(
            "/api/v1/admin/cron/tasks/:id/trigger",
            post(admin::trigger_cron_task),
        )
        .route("/api/v1/admin/metrics", get(admin::metrics_snapshot))
        // Skill admin (plug-and-play)
        .route("/api/v1/admin/skills", get(skills_admin::list_skills))
        .route(
            "/api/v1/admin/skills/reload",
            post(skills_admin::reload_skills),
        )
        .route(
            "/api/v1/admin/skills/:id/activate",
            post(skills_admin::activate_skill),
        )
        .route(
            "/api/v1/admin/skills/:id/deactivate",
            post(skills_admin::deactivate_skill),
        )
        // Channel admin (plug-and-play)
        .route(
            "/api/v1/admin/channels/reload",
            post(skills_admin::reload_channels),
        )
        // Unified skill RPC endpoint
        .route(
            "/api/v1/skills/rpc",
            post(skills_admin::skill_rpc),
        )
        // Observability dashboard & metrics
        .route(
            "/api/v1/admin/observability/metrics",
            get(observability::metrics_full),
        )
        .route(
            "/api/v1/admin/observability/events",
            get(observability::metrics_sse),
        )
        .route(
            "/api/v1/admin/observability/dashboard",
            get(observability::dashboard),
        );

    // WebSocket
    let ws = Router::new().route("/ws", get(ws::ws_handler));

    // A2A protocol routes
    let a2a = Router::new()
        .route("/.well-known/agent.json", get(a2a_routes::agent_card))
        .route("/a2a/tasks/send", post(a2a_routes::send_task))
        .route("/a2a/tasks/:id", get(a2a_routes::get_task))
        .route("/a2a/tasks/:id/cancel", post(a2a_routes::cancel_task))
        .route("/a2a/tasks/:id/input", post(a2a_routes::provide_input))
        .route("/a2a/agents", get(a2a_routes::list_agents))
        .route("/a2a/agents/register", post(a2a_routes::register_agent))
        .route("/a2a/agents/discover", post(a2a_routes::discover_agent));

    // Webhook ingestion routes (GAP-A)
    let webhooks = Router::new()
        .route(
            "/api/v1/webhooks",
            get(webhook::list_webhooks).post(webhook::create_webhook),
        )
        .route(
            "/api/v1/webhooks/:hook_id",
            post(webhook::receive_webhook).delete(webhook::delete_webhook),
        );

    let mut router = api.merge(openai)
        .merge(admin)
        .merge(ws)
        .merge(a2a)
        .merge(webhooks);

    // Browser automation routes (conditionally compiled)
    #[cfg(feature = "browser")]
    {
        let browser_mgr = state.browser_manager.clone();
        // Ensure the idle-session reaper is running now that we have a runtime.
        browser_mgr.ensure_reaper();
        router = router.nest_service("/api/browser", browser_routes::browser_routes(browser_mgr));
    }

    router
        .layer(axum::middleware::from_fn(middleware::request_tracing))
        .with_state(state)
}

/// Start the gateway server.
pub async fn serve(
    config: GatewayConfig,
    state: Arc<GatewayState>,
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = build_router(state);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    info!(%addr, "starting gateway server");

    let listener = tokio::net::TcpListener::bind(addr).await?;

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            cancel.cancelled().await;
            info!("gateway shutting down gracefully");
        })
        .await?;

    Ok(())
}
