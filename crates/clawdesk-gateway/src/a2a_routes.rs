//! A2A HTTP route handlers — mounted into the gateway's Axum router.
//!
//! These handlers bridge the gateway's `GatewayState` to the A2A protocol
//! handlers in `clawdesk-acp`. They translate between Axum's extraction
//! types and the A2A handler functions.
//!
//! ## Routes
//!
//! | Method | Path                     | Handler                |
//! |--------|--------------------------|------------------------|
//! | GET    | /.well-known/agent.json  | `agent_card`           |
//! | POST   | /a2a/tasks/send          | `send_task`            |
//! | GET    | /a2a/tasks/:id           | `get_task`             |
//! | POST   | /a2a/tasks/:id/cancel    | `cancel_task`          |
//! | POST   | /a2a/tasks/:id/input     | `provide_input`        |
//! | GET    | /a2a/agents              | `list_agents`          |
//! | POST   | /a2a/agents/register     | `register_agent`       |
//! | POST   | /a2a/agents/discover     | `discover_agent`       |

use crate::state::GatewayState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use clawdesk_acp::agent_card::AgentCard;
use clawdesk_acp::server::{A2AHandler, TaskSendRequest};
use serde::Deserialize;
use std::sync::Arc;

/// GET /.well-known/agent.json — serve this agent's card.
pub async fn agent_card(State(state): State<Arc<GatewayState>>) -> impl IntoResponse {
    let a2a = state.a2a_state.load();
    let card = A2AHandler::agent_card(&a2a).await;
    Json(card)
}

/// Request body for send_task.
#[derive(Deserialize)]
pub struct SendTaskBody {
    #[serde(flatten)]
    pub inner: TaskSendRequest,
    /// The requester agent ID. If omitted, uses "self".
    pub requester_id: Option<String>,
}

/// POST /a2a/tasks/send — create a new A2A task.
pub async fn send_task(
    State(state): State<Arc<GatewayState>>,
    Json(body): Json<SendTaskBody>,
) -> impl IntoResponse {
    let a2a = state.a2a_state.load();
    let requester = body.requester_id.as_deref().unwrap_or("self");

    match A2AHandler::send_task(&a2a, requester, body.inner).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

/// GET /a2a/tasks/:id — get task status.
pub async fn get_task(
    State(state): State<Arc<GatewayState>>,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    let a2a = state.a2a_state.load();

    match A2AHandler::get_task(&a2a, &task_id).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

/// Request body for cancel_task.
#[derive(Deserialize)]
pub struct CancelTaskBody {
    pub reason: Option<String>,
}

/// POST /a2a/tasks/:id/cancel — cancel a task.
pub async fn cancel_task(
    State(state): State<Arc<GatewayState>>,
    Path(task_id): Path<String>,
    Json(body): Json<CancelTaskBody>,
) -> impl IntoResponse {
    let a2a = state.a2a_state.load();

    match A2AHandler::cancel_task(&a2a, &task_id, body.reason).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

/// POST /a2a/tasks/:id/input — provide input for a task.
pub async fn provide_input(
    State(state): State<Arc<GatewayState>>,
    Path(task_id): Path<String>,
    Json(input): Json<serde_json::Value>,
) -> impl IntoResponse {
    let a2a = state.a2a_state.load();

    match A2AHandler::provide_input(&a2a, &task_id, input).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

/// GET /a2a/agents — list known agents.
pub async fn list_agents(State(state): State<Arc<GatewayState>>) -> impl IntoResponse {
    let a2a = state.a2a_state.load();
    let list = A2AHandler::list_agents(&a2a).await;
    Json(serde_json::to_value(list).unwrap())
}

/// POST /a2a/agents/register — register an external agent.
pub async fn register_agent(
    State(state): State<Arc<GatewayState>>,
    Json(card): Json<AgentCard>,
) -> impl IntoResponse {
    let a2a = state.a2a_state.load();

    match A2AHandler::register_agent(&a2a, card).await {
        Ok(summary) => (
            StatusCode::CREATED,
            Json(serde_json::to_value(summary).unwrap()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

/// Request body for discover_agent.
#[derive(Deserialize)]
pub struct DiscoverAgentBody {
    pub url: String,
}

/// POST /a2a/agents/discover — discover an agent by URL.
pub async fn discover_agent(
    State(state): State<Arc<GatewayState>>,
    Json(body): Json<DiscoverAgentBody>,
) -> impl IntoResponse {
    let a2a = state.a2a_state.load();

    match A2AHandler::discover_agent(&a2a, &body.url).await {
        Ok(card) => (StatusCode::OK, Json(serde_json::to_value(card).unwrap())).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}
