//! Security commands — OAuth2, Approval, ACL, Scoped Tokens (Tasks 16, 17, 23, 24).

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

// ═══════════════════════════════════════════════════════════
// OAuth2 + PKCE
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct OAuthStartRequest {
    pub provider: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub use_pkce: bool,
}

#[derive(Debug, Serialize)]
pub struct OAuthStartResponse {
    pub auth_url: String,
    pub state_param: String,
}

#[tauri::command]
pub async fn start_oauth_flow(
    request: OAuthStartRequest,
    state: State<'_, AppState>,
) -> Result<OAuthStartResponse, String> {
    let config = clawdesk_security::OAuthClientConfig {
        client_id: request.client_id,
        client_secret: request.client_secret,
        auth_url: request.auth_url,
        token_url: request.token_url,
        redirect_uri: request.redirect_uri,
        scopes: request.scopes,
        use_pkce: request.use_pkce,
    };
    let (auth_url, state_param) = state.oauth_flow_manager.start_authorization(&config).await;
    Ok(OAuthStartResponse { auth_url, state_param })
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallbackRequest {
    pub code: String,
    pub state_param: String,
    pub provider: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub use_pkce: bool,
}

#[derive(Debug, Serialize)]
pub struct OAuthTokenResponse {
    pub access_token_preview: String,
    pub has_refresh_token: bool,
    pub expires_at: Option<String>,
    pub scope: Option<String>,
}

#[tauri::command]
pub async fn handle_oauth_callback(
    request: OAuthCallbackRequest,
    state: State<'_, AppState>,
) -> Result<OAuthTokenResponse, String> {
    let config = clawdesk_security::OAuthClientConfig {
        client_id: request.client_id,
        client_secret: request.client_secret,
        auth_url: request.auth_url,
        token_url: request.token_url,
        redirect_uri: request.redirect_uri,
        scopes: request.scopes,
        use_pkce: request.use_pkce,
    };
    let token_set = state.oauth_flow_manager
        .exchange_code(&config, &request.code, &request.state_param)
        .await
        .map_err(|e| format!("OAuth token exchange failed: {:?}", e))?;

    // Store as auth profile
    let profile = clawdesk_security::AuthProfile {
        id: Uuid::new_v4().to_string(),
        provider: request.provider.clone(),
        tokens: token_set.clone(),
        priority: 0,
        failure_count: 0,
        cooldown_until: None,
        created_at: chrono::Utc::now(),
        last_used: None,
    };
    state.auth_profile_manager.add_profile(profile).await;

    Ok(OAuthTokenResponse {
        access_token_preview: format!("{}...", &token_set.access_token[..8.min(token_set.access_token.len())]),
        has_refresh_token: token_set.refresh_token.is_some(),
        expires_at: token_set.expires_at.map(|e| e.to_rfc3339()),
        scope: token_set.scope,
    })
}

#[tauri::command]
pub async fn refresh_oauth_token(
    provider: String,
    state: State<'_, AppState>,
) -> Result<OAuthTokenResponse, String> {
    let profile = state.auth_profile_manager
        .get_profile(&provider).await
        .ok_or_else(|| format!("No auth profile for provider '{}'", provider))?;

    let refresh_token = profile.tokens.refresh_token
        .ok_or("No refresh token available")?;

    // Build a minimal config for refresh
    let config = clawdesk_security::OAuthClientConfig {
        client_id: String::new(),
        client_secret: None,
        auth_url: String::new(),
        token_url: String::new(),
        redirect_uri: String::new(),
        scopes: vec![],
        use_pkce: false,
    };

    let token_set = state.oauth_flow_manager
        .refresh_token(&config, &refresh_token)
        .await
        .map_err(|e| format!("Token refresh failed: {:?}", e))?;

    state.auth_profile_manager.record_success(&provider, &profile.id).await;

    Ok(OAuthTokenResponse {
        access_token_preview: format!("{}...", &token_set.access_token[..8.min(token_set.access_token.len())]),
        has_refresh_token: token_set.refresh_token.is_some(),
        expires_at: token_set.expires_at.map(|e| e.to_rfc3339()),
        scope: token_set.scope,
    })
}

#[derive(Debug, Serialize)]
pub struct AuthProfileInfo {
    pub id: String,
    pub provider: String,
    pub is_expired: bool,
    pub failure_count: u32,
    pub created_at: String,
    pub last_used: Option<String>,
}

#[tauri::command]
pub async fn list_auth_profiles(
    provider: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<AuthProfileInfo>, String> {
    let prov = provider.as_deref().unwrap_or("*");
    let profiles = state.auth_profile_manager.list_profiles(prov).await;
    Ok(profiles.iter().map(|p| AuthProfileInfo {
        id: p.id.clone(),
        provider: p.provider.clone(),
        is_expired: p.tokens.is_expired(),
        failure_count: p.failure_count,
        created_at: p.created_at.to_rfc3339(),
        last_used: p.last_used.map(|t| t.to_rfc3339()),
    }).collect())
}

#[tauri::command]
pub async fn remove_auth_profile(
    provider: String,
    profile_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    Ok(state.auth_profile_manager.remove_profile(&provider, &profile_id).await)
}

// ═══════════════════════════════════════════════════════════
// Execution Approval
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct CreateApprovalRequest {
    pub tool_name: String,
    pub command: String,
    pub risk_level: String,
    pub context: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApprovalRequestInfo {
    pub id: String,
    pub tool_name: String,
    pub command: String,
    pub risk: String,
    pub status: String,
    pub created_at: String,
    pub expires_at: String,
}

fn risk_from_str(s: &str) -> clawdesk_security::RiskLevel {
    match s.to_lowercase().as_str() {
        "low" => clawdesk_security::RiskLevel::Low,
        "medium" => clawdesk_security::RiskLevel::Medium,
        "high" => clawdesk_security::RiskLevel::High,
        _ => clawdesk_security::RiskLevel::Medium,
    }
}

fn approval_status_str(s: &clawdesk_security::ApprovalStatus) -> String {
    match s {
        clawdesk_security::ApprovalStatus::Pending => "pending".into(),
        clawdesk_security::ApprovalStatus::Approved { .. } => "approved".into(),
        clawdesk_security::ApprovalStatus::Denied { .. } => "denied".into(),
        clawdesk_security::ApprovalStatus::TimedOut { .. } => "timed_out".into(),
    }
}

#[tauri::command]
pub async fn create_approval_request(
    request: CreateApprovalRequest,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<ApprovalRequestInfo, String> {
    let risk = risk_from_str(&request.risk_level);
    let approval = state.approval_manager
        .create_request(&request.tool_name, &request.command, risk, request.context.clone())
        .await;
    let info = ApprovalRequestInfo {
        id: approval.id.to_string(),
        tool_name: approval.tool_name.clone(),
        command: approval.command.clone(),
        risk: request.risk_level,
        status: "pending".into(),
        created_at: approval.created_at.to_rfc3339(),
        expires_at: approval.expires_at.to_rfc3339(),
    };
    // Notify UI about new approval request
    let _ = app.emit("approval:pending", serde_json::json!({
        "id": info.id,
        "tool_name": info.tool_name,
        "command": info.command,
        "risk": info.risk,
    }));
    Ok(info)
}

#[tauri::command]
pub async fn approve_request(
    request_id: String,
    approver: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let id = Uuid::parse_str(&request_id).map_err(|e| e.to_string())?;
    state.approval_manager
        .approve(id, approver)
        .await
        .map_err(|e| format!("{:?}", e))?;
    Ok(true)
}

#[tauri::command]
pub async fn deny_request(
    request_id: String,
    approver: String,
    reason: Option<String>,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let id = Uuid::parse_str(&request_id).map_err(|e| e.to_string())?;
    state.approval_manager
        .deny(id, approver, reason)
        .await
        .map_err(|e| format!("{:?}", e))?;
    Ok(true)
}

#[tauri::command]
pub async fn get_approval_status(
    request_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let id = Uuid::parse_str(&request_id).map_err(|e| e.to_string())?;
    let status = state.approval_manager
        .status(id)
        .await
        .ok_or("Approval request not found")?;
    Ok(approval_status_str(&status))
}

// ═══════════════════════════════════════════════════════════
// ACL Engine
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct AclRuleRequest {
    pub principal_type: String,
    pub principal_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub action: String,
    pub effect: String,
}

#[derive(Debug, Serialize)]
pub struct AclCheckResult {
    pub decision: String,
    pub reason: Option<String>,
}

fn parse_principal(ptype: &str, pid: &str) -> clawdesk_security::acl::Principal {
    match ptype {
        "user" => clawdesk_security::acl::Principal::User(pid.to_string()),
        "agent" => clawdesk_security::acl::Principal::Agent(pid.to_string()),
        "plugin" => clawdesk_security::acl::Principal::Plugin(pid.to_string()),
        "role" => clawdesk_security::acl::Principal::Role(pid.to_string()),
        _ => clawdesk_security::acl::Principal::System,
    }
}

fn parse_resource(rtype: &str, rid: &str) -> clawdesk_security::acl::Resource {
    match rtype {
        "path" => clawdesk_security::acl::Resource::Path(rid.to_string()),
        "tool" => clawdesk_security::acl::Resource::Tool(rid.to_string()),
        "channel" => clawdesk_security::acl::Resource::Channel(rid.to_string()),
        "config" => clawdesk_security::acl::Resource::Config(rid.to_string()),
        "endpoint" => clawdesk_security::acl::Resource::Endpoint(rid.to_string()),
        _ => clawdesk_security::acl::Resource::Path(rid.to_string()),
    }
}

fn parse_action(a: &str) -> clawdesk_security::acl::Action {
    match a {
        "read" => clawdesk_security::acl::Action::Read,
        "write" => clawdesk_security::acl::Action::Write,
        "execute" => clawdesk_security::acl::Action::Execute,
        "delete" => clawdesk_security::acl::Action::Delete,
        "admin" => clawdesk_security::acl::Action::Admin,
        _ => clawdesk_security::acl::Action::Read,
    }
}

#[tauri::command]
pub async fn add_acl_rule(
    request: AclRuleRequest,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let effect = if request.effect == "deny" {
        clawdesk_security::acl::Effect::Deny
    } else {
        clawdesk_security::acl::Effect::Allow
    };
    let perm = clawdesk_security::acl::Permission {
        principal: parse_principal(&request.principal_type, &request.principal_id),
        resource: parse_resource(&request.resource_type, &request.resource_id),
        action: parse_action(&request.action),
        effect,
        conditions: vec![],
    };
    state.acl_manager.add_permission(perm).await;
    Ok(true)
}

#[tauri::command]
pub async fn check_permission(
    principal_type: String,
    principal_id: String,
    resource_type: String,
    resource_id: String,
    action: String,
    state: State<'_, AppState>,
) -> Result<AclCheckResult, String> {
    let principal = parse_principal(&principal_type, &principal_id);
    let resource = parse_resource(&resource_type, &resource_id);
    let act = parse_action(&action);
    let decision = state.acl_manager.check(&principal, &resource, act).await;
    match decision {
        clawdesk_security::acl::AccessDecision::Allow => Ok(AclCheckResult {
            decision: "allow".into(),
            reason: None,
        }),
        clawdesk_security::acl::AccessDecision::Deny { reason } => Ok(AclCheckResult {
            decision: "deny".into(),
            reason: Some(reason),
        }),
        clawdesk_security::acl::AccessDecision::ConditionalAllow { conditions } => Ok(AclCheckResult {
            decision: "conditional_allow".into(),
            reason: Some(format!("{} conditions required", conditions.len())),
        }),
    }
}

#[tauri::command]
pub async fn revoke_acl_rules(
    principal_type: String,
    principal_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let principal = parse_principal(&principal_type, &principal_id);
    state.acl_manager.revoke_all(&principal).await;
    Ok(true)
}

// ═══════════════════════════════════════════════════════════
// Scoped Tokens
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct GenerateTokenRequest {
    pub scopes: Vec<String>,
    pub ttl_hours: u64,
    pub peer_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TokenInfo {
    pub encoded: String,
    pub scopes: Vec<String>,
    pub expires_in_secs: u64,
    pub is_peer_bound: bool,
}

fn parse_scopes(scopes: &[String]) -> clawdesk_security::TokenScope {
    let mut scope = clawdesk_security::TokenScope::from_bits(0);
    for s in scopes {
        let cap = match s.as_str() {
            "chat" => clawdesk_security::TokenScope::CHAT,
            "admin" => clawdesk_security::TokenScope::ADMIN,
            "skills" => clawdesk_security::TokenScope::SKILLS,
            "tools" => clawdesk_security::TokenScope::TOOLS,
            "cron" => clawdesk_security::TokenScope::CRON,
            "channels" => clawdesk_security::TokenScope::CHANNELS,
            "audit" => clawdesk_security::TokenScope::AUDIT,
            "tunnel" => clawdesk_security::TokenScope::TUNNEL,
            "all" => clawdesk_security::TokenScope::ALL,
            _ => continue,
        };
        scope = scope.union(cap);
    }
    scope
}

#[tauri::command]
pub async fn generate_token(
    request: GenerateTokenRequest,
    state: State<'_, AppState>,
) -> Result<TokenInfo, String> {
    let scope = parse_scopes(&request.scopes);
    let ttl = std::time::Duration::from_secs(request.ttl_hours * 3600);
    let token = if let Some(ref peer_hex) = request.peer_id {
        let mut peer = [0u8; 32];
        let bytes: Vec<u8> = (0..peer_hex.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(&peer_hex[i..i + 2], 16).ok())
            .collect();
        peer[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
        state.server_secret.create_token(scope, ttl, peer)
    } else {
        clawdesk_security::ScopedToken::create_unbound(scope, ttl, state.server_secret.key())
    };
    let encoded = token.encode();
    let remaining = token.remaining().map(|d| d.as_secs()).unwrap_or(0);
    Ok(TokenInfo {
        encoded,
        scopes: scope.capability_names().into_iter().map(|s| s.to_string()).collect(),
        expires_in_secs: remaining,
        is_peer_bound: token.is_peer_bound(),
    })
}

#[tauri::command]
pub async fn validate_token(
    encoded_token: String,
    state: State<'_, AppState>,
) -> Result<TokenInfo, String> {
    let token = clawdesk_security::ScopedToken::decode(&encoded_token)
        .map_err(|e| format!("Token decode failed: {:?}", e))?;
    let scope = state.server_secret.verify_token(&token)
        .map_err(|e| format!("Token verification failed: {:?}", e))?;
    let remaining = token.remaining().map(|d| d.as_secs()).unwrap_or(0);
    Ok(TokenInfo {
        encoded: encoded_token,
        scopes: scope.capability_names().into_iter().map(|s| s.to_string()).collect(),
        expires_in_secs: remaining,
        is_peer_bound: token.is_peer_bound(),
    })
}

// ── ask_human response handler ──────────────────────────────────────

/// Called by the frontend when the user responds to an `ask_human` tool call.
/// Wakes the blocked tool and delivers the human's answer.
#[tauri::command]
pub async fn respond_to_ask_human(
    request_id: String,
    response: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let id = uuid::Uuid::parse_str(&request_id).map_err(|e| e.to_string())?;
    if let Some(entry) = state.ask_human_pending.get(&id) {
        if let Ok(mut slot) = entry.response.lock() {
            *slot = Some(response);
        }
        entry.notify.notify_one();
        Ok(true)
    } else {
        Err(format!("ask_human request '{}' not found or already answered", request_id))
    }
}
