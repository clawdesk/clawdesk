//! Network discovery commands — mDNS, SPAKE2 pairing.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Serialize)]
pub struct PeerInfo {
    pub instance_name: String,
    pub host: String,
    pub port: u16,
    pub version: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PairingStatus {
    pub code: String,
    pub state: String,
    pub remaining_secs: u64,
}

/// Get the local mDNS service announcement info.
#[tauri::command]
pub async fn get_mdns_service_info(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let advertiser = state.mdns_advertiser.read().map_err(|e| e.to_string())?;
    let service = advertiser.service();
    Ok(serde_json::json!({
        "instance_name": service.instance_name,
        "host": service.host,
        "port": service.port,
        "version": service.version,
        "capabilities": service.capabilities,
        "full_name": service.full_name(),
        "is_running": advertiser.is_running(),
    }))
}

/// Start a new pairing session (generates a pairing code for the peer).
#[tauri::command]
pub async fn start_pairing(
    state: State<'_, AppState>,
) -> Result<PairingStatus, String> {
    use clawdesk_discovery::PairingSession;
    let session = PairingSession::new();
    let status = PairingStatus {
        code: session.code().to_string(),
        state: format!("{:?}", session.state()),
        remaining_secs: session.remaining().as_secs(),
    };
    let mut pairing = state.pairing_session.write().map_err(|e| e.to_string())?;
    *pairing = Some(session);
    Ok(status)
}

/// Complete pairing by verifying the peer's code.
#[tauri::command]
pub async fn complete_pairing(
    code: String,
    peer_name: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let mut pairing = state.pairing_session.write().map_err(|e| e.to_string())?;
    let session = pairing.as_mut()
        .ok_or("No active pairing session")?;
    Ok(session.verify_code(&code, &peer_name))
}

/// Get the current pairing session status.
#[tauri::command]
pub async fn get_pairing_status(
    state: State<'_, AppState>,
) -> Result<Option<PairingStatus>, String> {
    let pairing = state.pairing_session.read().map_err(|e| e.to_string())?;
    Ok(pairing.as_ref().map(|s| PairingStatus {
        code: s.code().to_string(),
        state: format!("{:?}", s.state()),
        remaining_secs: s.remaining().as_secs(),
    }))
}

/// List discovered peers from the peer registry.
#[tauri::command]
pub async fn list_discovered_peers(
    state: State<'_, AppState>,
) -> Result<Vec<PeerInfo>, String> {
    let registry = state.peer_registry.read().map_err(|e| e.to_string())?;
    let peers = registry.active_peers();
    Ok(peers.iter().map(|p| PeerInfo {
        instance_name: p.name.clone(),
        host: p.host.clone(),
        port: p.port,
        version: p.version.clone(),
        capabilities: p.capabilities.clone(),
    }).collect())
}
