//! A2A Protocol commands — agent discovery, task delegation.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Serialize)]
pub struct AgentCardInfo {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub active_tasks: u32,
    pub is_healthy: bool,
}

#[derive(Debug, Deserialize)]
pub struct RegisterAgentCardRequest {
    pub agent_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub capabilities: Vec<String>,
    pub endpoint: Option<String>,
}

#[tauri::command]
pub async fn list_a2a_agents(state: State<'_, AppState>) -> Result<Vec<AgentCardInfo>, String> {
    let dir = state.agent_directory.read().map_err(|e| e.to_string())?;
    Ok(dir.list().iter().map(|card| {
        let entry = dir.get(&card.id);
        AgentCardInfo {
            id: card.id.clone(),
            name: card.name.clone(),
            capabilities: card.capabilities.iter().map(|c| format!("{:?}", c)).collect(),
            active_tasks: entry.map(|e| e.active_tasks).unwrap_or(0),
            is_healthy: entry.map(|e| e.is_healthy).unwrap_or(false),
        }
    }).collect())
}

#[tauri::command]
pub async fn register_a2a_agent(
    request: RegisterAgentCardRequest,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    use clawdesk_acp::AgentCard;

    let url = request.endpoint.unwrap_or_else(|| "http://localhost:18789".to_string());
    let name = request.name.unwrap_or_else(|| "Agent".to_string());
    let desc = request.description.unwrap_or_default();

    let mut card = AgentCard::new(request.agent_id, name, &url);
    card.description = desc;
    for cap_str in &request.capabilities {
        if let Some(cap) = clawdesk_acp::CapabilityId::from_str_loose(cap_str) {
            card = card.with_capability(cap);
        }
    }

    let mut dir = state.agent_directory.write().map_err(|e| e.to_string())?;
    dir.register(card);
    Ok(true)
}

#[tauri::command]
pub async fn deregister_a2a_agent(
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let mut dir = state.agent_directory.write().map_err(|e| e.to_string())?;
    Ok(dir.deregister(&agent_id).is_some())
}

#[tauri::command]
pub async fn get_agent_card(
    agent_id: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let dir = state.agent_directory.read().map_err(|e| e.to_string())?;
    let entry = dir.get(&agent_id)
        .ok_or_else(|| format!("Agent {} not found in directory", agent_id))?;
    serde_json::to_value(&entry.card).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_self_agent_card(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    use clawdesk_acp::AgentCard;

    let agents = state.agents.read().map_err(|e| e.to_string())?;
    let skill_count = {
        let reg = state.skill_registry.read().map_err(|e| e.to_string())?;
        reg.len()
    };

    let card = AgentCard::new("self", "ClawDesk", "http://127.0.0.1:18789")
        .with_capability(clawdesk_acp::CapabilityId::TextGeneration)
        .with_capability(clawdesk_acp::CapabilityId::CodeExecution);

    // Serialize capabilities as plain strings for UI display.
    let caps: Vec<String> = card.capabilities.iter().map(|c| c.name().to_string()).collect();

    Ok(serde_json::json!({
        "id": card.id,
        "name": card.name,
        "description": card.description,
        "endpoint": card.endpoint,
        "capabilities": caps,
        "agents": agents.len(),
        "skills": skill_count,
    }))
}

#[derive(Debug, Deserialize)]
pub struct TaskSendRequest {
    pub skill_id: Option<String>,
    pub input: serde_json::Value,
    pub target_agent: Option<String>,
    pub required_capabilities: Option<Vec<String>>,
}

#[tauri::command]
pub async fn send_a2a_task(
    requester_id: String,
    req: TaskSendRequest,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    use clawdesk_acp::{Task, TaskEvent};
    
    let target = req.target_agent.unwrap_or_else(|| "self".to_string());
    
    let mut task = Task::new(&requester_id, &target, req.input);
    task.skill_id = req.skill_id;
    let task_id = task.id.as_str().to_string();
    
    let response = serde_json::json!({
        "task_id": task_id,
        "state": task.state,
        "output": task.output,
        "error": task.error,
        "progress": task.progress,
        "artifacts": task.artifacts,
    });
    
    state.a2a_tasks.write().await.insert(task_id, task);
    
    Ok(response)
}

#[tauri::command]
pub async fn get_a2a_task(
    task_id: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let tasks = state.a2a_tasks.read().await;
    let task = tasks.get(&task_id).ok_or_else(|| format!("Task {} not found", task_id))?;
    
    Ok(serde_json::json!({
        "task_id": task.id.as_str(),
        "state": task.state,
        "output": task.output,
        "error": task.error,
        "progress": task.progress,
        "artifacts": task.artifacts,
    }))
}

#[tauri::command]
pub async fn list_a2a_tasks(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let tasks = state.a2a_tasks.read().await;
    let mut result = Vec::new();
    for task in tasks.values() {
        result.push(serde_json::json!({
            "task_id": task.id.as_str(),
            "state": task.state,
            "output": task.output,
            "error": task.error,
            "progress": task.progress,
            "artifacts": task.artifacts,
        }));
    }
    Ok(result)
}

#[tauri::command]
pub async fn cancel_a2a_task(
    task_id: String,
    reason: Option<String>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    use clawdesk_acp::TaskEvent;
    
    let mut tasks = state.a2a_tasks.write().await;
    let task = tasks.get_mut(&task_id).ok_or_else(|| format!("Task {} not found", task_id))?;
    
    task.apply_event(TaskEvent::Cancel { reason }).map_err(|e| e.to_string())?;
    
    Ok(serde_json::json!({
        "task_id": task.id.as_str(),
        "state": task.state,
        "output": task.output,
        "error": task.error,
        "progress": task.progress,
        "artifacts": task.artifacts,
    }))
}

#[tauri::command]
pub async fn provide_a2a_task_input(
    task_id: String,
    input: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    use clawdesk_acp::TaskEvent;
    
    let mut tasks = state.a2a_tasks.write().await;
    let task = tasks.get_mut(&task_id).ok_or_else(|| format!("Task {} not found", task_id))?;
    
    task.apply_event(TaskEvent::ProvideInput { input }).map_err(|e| e.to_string())?;
    
    Ok(serde_json::json!({
        "task_id": task.id.as_str(),
        "state": task.state,
        "output": task.output,
        "error": task.error,
        "progress": task.progress,
        "artifacts": task.artifacts,
    }))
}

