//! A2A Protocol commands — agent discovery, task delegation (Task 15).

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
        let cap = match cap_str.as_str() {
            "text_generation" => clawdesk_acp::AgentCapability::TextGeneration,
            "code_execution" => clawdesk_acp::AgentCapability::CodeExecution,
            "web_search" => clawdesk_acp::AgentCapability::WebSearch,
            "file_processing" => clawdesk_acp::AgentCapability::FileProcessing,
            "image_processing" => clawdesk_acp::AgentCapability::ImageProcessing,
            "audio_processing" => clawdesk_acp::AgentCapability::AudioProcessing,
            "api_integration" => clawdesk_acp::AgentCapability::ApiIntegration,
            "data_management" => clawdesk_acp::AgentCapability::DataManagement,
            "mathematics" => clawdesk_acp::AgentCapability::Mathematics,
            "scheduling" => clawdesk_acp::AgentCapability::Scheduling,
            "messaging" => clawdesk_acp::AgentCapability::Messaging,
            other => clawdesk_acp::AgentCapability::Custom(other.to_string()),
        };
        card = card.with_capability(cap);
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
        .with_capability(clawdesk_acp::AgentCapability::TextGeneration)
        .with_capability(clawdesk_acp::AgentCapability::CodeExecution)
        .with_capability(clawdesk_acp::AgentCapability::Custom(
            format!("{} agents, {} skills", agents.len(), skill_count),
        ));

    serde_json::to_value(&card).map_err(|e| e.to_string())
}
