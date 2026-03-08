//! Browser REST API routes — session list, state, screenshot, tabs, profiles.
//!
//! Provides HTTP endpoints for monitoring and debugging browser sessions.

use axum::extract::{Path, State};
use axum::response::Json;
use axum::routing::get;
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
pub struct SessionInfo {
    pub agent_id: String,
}

#[derive(Serialize)]
pub struct SessionState {
    pub agent_id: String,
    pub url: String,
    pub title: String,
    pub pages_visited: u32,
    pub idle_secs: u64,
    pub profile: String,
}

#[derive(Serialize)]
pub struct TabSummary {
    pub target_id: String,
    pub title: String,
    pub url: String,
    pub attached: bool,
}

#[derive(Serialize)]
pub struct ProfileSummary {
    pub name: String,
    pub description: String,
    pub is_default: bool,
    pub last_used: Option<String>,
}

/// Build the browser API router.
///
/// Mount at `/api/browser` in the gateway.
pub fn browser_routes(
    manager: Arc<clawdesk_browser::manager::BrowserManager>,
) -> axum::Router {
    axum::Router::new()
        .route("/sessions", get(list_sessions))
        .route("/sessions/{agent_id}", get(session_state))
        .route(
            "/sessions/{agent_id}/screenshot",
            get(session_screenshot),
        )
        .route(
            "/sessions/{agent_id}/tabs",
            get(session_tabs),
        )
        .route(
            "/sessions/{agent_id}/console",
            get(session_console),
        )
        .route("/profiles", get(list_profiles))
        .with_state(manager)
}

async fn list_sessions(
    State(mgr): State<Arc<clawdesk_browser::manager::BrowserManager>>,
) -> Json<Vec<String>> {
    Json(mgr.list_sessions())
}

async fn session_state(
    State(mgr): State<Arc<clawdesk_browser::manager::BrowserManager>>,
    Path(agent_id): Path<String>,
) -> Result<Json<SessionState>, String> {
    let session = mgr.get_or_create(&agent_id).await?;
    let s = session.lock().await;

    let url = s
        .cdp
        .eval("window.location.href")
        .await
        .ok()
        .and_then(|v| {
            v.get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str().map(String::from))
        })
        .unwrap_or_default();

    let title = s
        .cdp
        .eval("document.title")
        .await
        .ok()
        .and_then(|v| {
            v.get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str().map(String::from))
        })
        .unwrap_or_default();

    Ok(Json(SessionState {
        agent_id,
        url,
        title,
        pages_visited: s.pages_visited,
        idle_secs: s.last_active.elapsed().as_secs(),
        profile: s.profile_name.clone(),
    }))
}

async fn session_screenshot(
    State(mgr): State<Arc<clawdesk_browser::manager::BrowserManager>>,
    Path(agent_id): Path<String>,
) -> Result<String, String> {
    let session = mgr.get_or_create(&agent_id).await?;
    let s = session.lock().await;
    s.cdp.take_screenshot().await
}

async fn session_tabs(
    State(mgr): State<Arc<clawdesk_browser::manager::BrowserManager>>,
    Path(agent_id): Path<String>,
) -> Result<Json<Vec<TabSummary>>, String> {
    let session = mgr.get_or_create(&agent_id).await?;
    let s = session.lock().await;
    let tabs = clawdesk_browser::tabs::list_tabs(&s.cdp, false).await?;

    Ok(Json(
        tabs.into_iter()
            .map(|t| TabSummary {
                target_id: t.target_id,
                title: t.title,
                url: t.url,
                attached: t.attached,
            })
            .collect(),
    ))
}

async fn session_console(
    State(mgr): State<Arc<clawdesk_browser::manager::BrowserManager>>,
    Path(agent_id): Path<String>,
) -> Result<Json<Vec<clawdesk_browser::manager::ConsoleEntry>>, String> {
    let session = mgr.get_or_create(&agent_id).await?;
    let s = session.lock().await;
    // Return accumulated console log
    Ok(Json(s.console_log.clone()))
}

async fn list_profiles(
    State(mgr): State<Arc<clawdesk_browser::manager::BrowserManager>>,
) -> Result<Json<Vec<ProfileSummary>>, String> {
    let pm = mgr
        .profile_manager()
        .ok_or("profile manager not available")?;
    let profiles = pm.list()?;

    Ok(Json(
        profiles
            .into_iter()
            .map(|p| ProfileSummary {
                name: p.name,
                description: p.description,
                is_default: p.is_default,
                last_used: p.last_used,
            })
            .collect(),
    ))
}
