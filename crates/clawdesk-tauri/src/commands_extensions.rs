//! Extensions commands — integration registry, credential vault, health monitoring, OAuth.
//!
//! Wraps clawdesk-extensions for the Tauri IPC surface. Exposes:
//! - 25+ bundled integrations (GitHub, Slack, Jira, AWS, etc.)
//! - Per-extension configuration with typed schema
//! - AES-256-GCM encrypted credential vault
//! - Health monitoring with exponential backoff
//! - OAuth 2.0 PKCE flows for browser-based auth

use crate::state::AppState;
use serde::Serialize;
use tauri::State;
use std::collections::HashMap;

// ── Response types ────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct IntegrationInfo {
    pub name: String,
    pub description: String,
    pub category: String,
    pub icon: String,
    pub enabled: bool,
    pub credentials_required: Vec<CredentialRequirementInfo>,
    pub has_oauth: bool,
    pub health_check_url: Option<String>,
    /// Per-extension configuration schema (typed fields).
    pub config_fields: Vec<ConfigFieldInfo>,
    /// Current user-configured values (non-secret only).
    pub config_values: HashMap<String, String>,
    /// Transport type: "stdio" | "sse" | "api"
    pub transport_type: String,
}

#[derive(Debug, Serialize)]
pub struct CredentialRequirementInfo {
    pub name: String,
    pub description: String,
    pub env_var: Option<String>,
    pub required: bool,
}

#[derive(Debug, Serialize)]
pub struct ConfigFieldInfo {
    pub key: String,
    pub label: String,
    pub description: String,
    pub field_type: String,
    pub default: Option<String>,
    pub required: bool,
    pub placeholder: Option<String>,
    pub validation: Option<String>,
    pub options: Vec<ConfigFieldOptionInfo>,
    pub group: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ConfigFieldOptionInfo {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct IntegrationCategoryInfo {
    pub name: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct VaultStatusInfo {
    pub exists: bool,
    pub unlocked: bool,
    pub credential_count: usize,
}

#[derive(Debug, Serialize)]
pub struct CredentialInfo {
    pub integration: String,
    pub name: String,
    pub label: String,
    pub stored_at: String,
    pub expires_at: Option<String>,
    pub is_expired: bool,
}

#[derive(Debug, Serialize)]
pub struct HealthStatusInfo {
    pub name: String,
    pub state: String,
    pub last_check: Option<String>,
    pub last_success: Option<String>,
    pub consecutive_failures: u32,
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct OAuthFlowInfo {
    pub auth_url: String,
    pub state: String,
}

// ── Integration Registry Commands ─────────────────────────────

/// Convert an `Integration` + its config values into the IPC response type.
fn integration_to_info(
    i: &clawdesk_extensions::Integration,
    config_values: HashMap<String, String>,
) -> IntegrationInfo {
    let transport_type = match &i.transport {
        clawdesk_extensions::registry::TransportConfig::Stdio { .. } => "stdio",
        clawdesk_extensions::registry::TransportConfig::Sse { .. } => "sse",
        clawdesk_extensions::registry::TransportConfig::DirectApi { .. } => "api",
    };
    IntegrationInfo {
        name: i.name.clone(),
        description: i.description.clone(),
        category: format!("{:?}", i.category),
        icon: i.icon.clone().unwrap_or_default(),
        enabled: i.enabled,
        credentials_required: i
            .credentials
            .iter()
            .map(|c| CredentialRequirementInfo {
                name: c.name.clone(),
                description: c.description.clone(),
                env_var: c.env_var.clone(),
                required: c.required,
            })
            .collect(),
        has_oauth: i.oauth.is_some(),
        health_check_url: i.health_check_url.clone(),
        config_fields: i
            .config_fields
            .iter()
            .map(|f| ConfigFieldInfo {
                key: f.key.clone(),
                label: f.label.clone(),
                description: f.description.clone(),
                field_type: format!("{:?}", f.field_type).to_lowercase(),
                default: f.default.clone(),
                required: f.required,
                placeholder: f.placeholder.clone(),
                validation: f.validation.clone(),
                options: f
                    .options
                    .iter()
                    .map(|o| ConfigFieldOptionInfo {
                        label: o.label.clone(),
                        value: o.value.clone(),
                    })
                    .collect(),
                group: f.group.clone(),
            })
            .collect(),
        config_values,
        transport_type: transport_type.to_string(),
    }
}

/// List all available integrations (bundled + user-defined).
#[tauri::command]
pub async fn list_integrations(
    state: State<'_, AppState>,
) -> Result<Vec<IntegrationInfo>, String> {
    let registry = state.integration_registry.read().await;
    let integrations = registry.list();
    Ok(integrations
        .iter()
        .map(|i| {
            let config_values = registry.get_config(&i.name).unwrap_or_default();
            integration_to_info(i, config_values)
        })
        .collect())
}

/// Get detailed info about a specific integration.
#[tauri::command]
pub async fn get_integration_detail(
    name: String,
    state: State<'_, AppState>,
) -> Result<IntegrationInfo, String> {
    let registry = state.integration_registry.read().await;
    let integration = registry
        .get(&name)
        .ok_or_else(|| format!("Integration '{}' not found", name))?;
    let config_values = registry.get_config(&name).unwrap_or_default();
    Ok(integration_to_info(&integration, config_values))
}

/// List integration categories with counts.
#[tauri::command]
pub async fn list_integration_categories(
    state: State<'_, AppState>,
) -> Result<Vec<IntegrationCategoryInfo>, String> {
    let registry = state.integration_registry.read().await;
    use clawdesk_extensions::IntegrationCategory;
    let categories = [
        IntegrationCategory::DevTools,
        IntegrationCategory::Productivity,
        IntegrationCategory::Data,
        IntegrationCategory::Cloud,
        IntegrationCategory::Search,
        IntegrationCategory::Communication,
        IntegrationCategory::Custom,
    ];
    Ok(categories
        .iter()
        .map(|cat| {
            let items = registry.list_by_category(cat);
            IntegrationCategoryInfo {
                name: format!("{:?}", cat),
                count: items.len(),
            }
        })
        .collect())
}

/// Enable an integration by name.
///
/// 1. Marks the integration as enabled in the registry
/// 2. Persists enabled state to SochDB (survives restart)
/// 3. If the integration has an MCP transport (Stdio/SSE), spawns the MCP
///    server process and performs the initialize → tools/list handshake
/// 4. Registers the integration for health monitoring
#[tauri::command]
pub async fn enable_integration(
    name: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    // 1. Enable in registry
    let integration = {
        let registry = state.integration_registry.write().await;
        registry.enable(&name).map_err(|e| format!("{:?}", e))?;
        registry.get(&name).ok_or_else(|| format!("Integration '{}' not found after enable", name))?
    };

    // 2. Persist enabled state to SochDB
    persist_enabled_state(&state).await;

    // 3. Connect MCP transport if applicable
    if integration.is_mcp_connectable() {
        if let Err(e) = connect_mcp_for_integration(&state, &integration).await {
            tracing::warn!(
                name = %name,
                error = %e,
                "MCP connection failed — integration enabled but transport not active"
            );
        }
    }

    // 4. Register for health monitoring
    if integration.health_check_url.is_some() {
        let monitor = state.health_monitor.write().await;
        monitor.register(&name).await;
    }

    tracing::info!(name = %name, mcp = integration.is_mcp_connectable(), "integration enabled");
    Ok(true)
}

/// Disable an integration by name.
///
/// 1. Marks the integration as disabled in the registry
/// 2. Persists state to SochDB
/// 3. Disconnects the MCP server process if running
/// 4. Removes health monitoring
#[tauri::command]
pub async fn disable_integration(
    name: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let integration = {
        let registry = state.integration_registry.write().await;
        let integ = registry.get(&name);
        registry.disable(&name).map_err(|e| format!("{:?}", e))?;
        integ
    };

    // Persist state
    persist_enabled_state(&state).await;

    // Disconnect MCP if connected
    if let Some(ref integ) = integration {
        if integ.is_mcp_connectable() {
            let mcp = state.mcp_client.read().await;
            if let Err(e) = mcp.disconnect(&name).await {
                tracing::warn!(name = %name, error = %e, "MCP disconnect failed");
            }
        }
    }

    // Remove from health monitor
    {
        let monitor = state.health_monitor.write().await;
        monitor.unregister(&name).await;
    }

    tracing::info!(name = %name, "integration disabled");
    Ok(true)
}

/// Get extension registry statistics.
#[tauri::command]
pub async fn get_integration_stats(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let registry = state.integration_registry.read().await;
    let enabled = registry.enabled();
    Ok(serde_json::json!({
        "total": registry.count(),
        "enabled": enabled.len(),
        "disabled": registry.count() - enabled.len(),
    }))
}

// ── Extension Configuration Commands ──────────────────────────

/// Get the current configuration values for an extension.
#[tauri::command]
pub async fn get_extension_config(
    name: String,
    state: State<'_, AppState>,
) -> Result<HashMap<String, String>, String> {
    let registry = state.integration_registry.read().await;
    Ok(registry.get_config(&name).unwrap_or_default())
}

/// Save configuration values for an extension.
///
/// Validates against the integration's config schema, persists to SochDB,
/// and updates the in-memory registry. If the integration is currently
/// enabled with an MCP transport, reconnects with the new config.
#[tauri::command]
pub async fn save_extension_config(
    name: String,
    values: HashMap<String, String>,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    // 1. Update in-memory config (schema-validated)
    {
        let registry = state.integration_registry.read().await;
        registry
            .set_config(&name, values.clone())
            .map_err(|e| format!("{:?}", e))?;
    }

    // 2. Persist to SochDB
    persist_extension_config(&state, &name).await;

    // 3. If enabled + MCP, reconnect with new config
    let integration = {
        let registry = state.integration_registry.read().await;
        registry.get(&name)
    };
    if let Some(ref integ) = integration {
        if integ.enabled && integ.is_mcp_connectable() {
            // Disconnect old, connect new
            let mcp = state.mcp_client.read().await;
            let _ = mcp.disconnect(&name).await;
            drop(mcp);
            if let Err(e) = connect_mcp_for_integration(&state, integ).await {
                tracing::warn!(
                    name = %name, error = %e,
                    "MCP reconnect failed after config change"
                );
            }
        }
    }

    tracing::info!(name = %name, "extension config saved");
    Ok(true)
}

/// Validate an extension's configuration.
///
/// Returns the list of missing required field keys (empty = valid).
#[tauri::command]
pub async fn validate_extension_config(
    name: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let registry = state.integration_registry.read().await;
    registry.validate_config(&name).map_err(|e| format!("{:?}", e))
}

/// Store a credential for a specific extension (combines name + vault key).
///
/// Uses the pattern `{integration_name}_{credential_name}` as the vault key,
/// matching what `resolve_credentials()` expects.
#[tauri::command]
pub async fn store_extension_credential(
    integration_name: String,
    credential_name: String,
    value: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let vault_key = format!("{}_{}", integration_name, credential_name);
    let vault = state.credential_vault.read().await;
    vault
        .store(&vault_key, &value)
        .await
        .map_err(|e| format!("{:?}", e))?;
    tracing::info!(
        integration = %integration_name,
        credential = %credential_name,
        "stored extension credential"
    );
    Ok(true)
}

/// Check which credentials for an extension are already stored in the vault.
///
/// Returns a map of credential_name → true/false.
#[tauri::command]
pub async fn check_extension_credentials(
    name: String,
    state: State<'_, AppState>,
) -> Result<HashMap<String, bool>, String> {
    let registry = state.integration_registry.read().await;
    let integration = registry
        .get(&name)
        .ok_or_else(|| format!("Integration '{}' not found", name))?;
    let user_config = registry.get_config(&name).unwrap_or_default();

    let mut result = HashMap::new();
    let vault = state.credential_vault.read().await;
    let vault_unlocked = vault.is_unlocked().await;

    for cred in &integration.credentials {
        let mut found = false;
        if let Some(ref env_var) = cred.env_var {
            if user_config
                .get(env_var)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                found = true;
            }
        }
        if !found && user_config
            .get(&cred.name)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            found = true;
        }
        // Check env var first
        if !found {
            if let Some(ref env_var) = cred.env_var {
            if std::env::var(env_var).is_ok() {
                found = true;
            }
            }
        }
        // Check vault
        if !found && vault_unlocked {
            let vault_key = format!("{}_{}", name, cred.name);
            if let Ok(Some(_)) = vault.get(&vault_key).await {
                found = true;
            }
        }
        result.insert(cred.name.clone(), found);
    }

    Ok(result)
}

// ── Credential Vault Commands ─────────────────────────────────

/// Get the current vault status (exists, unlocked, credential count).
#[tauri::command]
pub async fn vault_status(
    state: State<'_, AppState>,
) -> Result<VaultStatusInfo, String> {
    let vault = state.credential_vault.read().await;
    let unlocked = vault.is_unlocked().await;
    let count = if unlocked {
        vault.list_names().await.unwrap_or_default().len()
    } else {
        0
    };
    Ok(VaultStatusInfo {
        exists: vault.exists(),
        unlocked,
        credential_count: count,
    })
}

/// Initialize the vault with a master password (first-time setup).
#[tauri::command]
pub async fn vault_initialize(
    password: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let vault = state.credential_vault.read().await;
    vault
        .initialize(&password)
        .await
        .map_err(|e| format!("{:?}", e))?;
    Ok(true)
}

/// Unlock the vault with the master password.
#[tauri::command]
pub async fn vault_unlock(
    password: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let vault = state.credential_vault.read().await;
    vault.unlock(&password).await.map_err(|e| format!("{:?}", e))?;
    Ok(true)
}

/// Lock the vault (clear key from memory).
#[tauri::command]
pub async fn vault_lock(
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let vault = state.credential_vault.read().await;
    vault.lock().await;
    Ok(true)
}

/// Store a credential in the vault.
#[tauri::command]
pub async fn vault_store_credential(
    name: String,
    value: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let vault = state.credential_vault.read().await;
    vault
        .store(&name, &value)
        .await
        .map_err(|e| format!("{:?}", e))?;
    Ok(true)
}

/// Retrieve a credential from the vault (returns the decrypted value).
#[tauri::command]
pub async fn vault_get_credential(
    name: String,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let vault = state.credential_vault.read().await;
    vault.get(&name).await.map_err(|e| format!("{:?}", e))
}

/// Delete a credential from the vault.
#[tauri::command]
pub async fn vault_delete_credential(
    name: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let vault = state.credential_vault.read().await;
    vault.delete(&name).await.map_err(|e| format!("{:?}", e))?;
    Ok(true)
}

/// List all credential names stored in the vault.
#[tauri::command]
pub async fn vault_list_credentials(
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let vault = state.credential_vault.read().await;
    vault.list_names().await.map_err(|e| format!("{:?}", e))
}

// ── Health Monitoring Commands ────────────────────────────────

/// Get health status for all monitored integrations.
#[tauri::command]
pub async fn get_all_health_statuses(
    state: State<'_, AppState>,
) -> Result<Vec<HealthStatusInfo>, String> {
    let monitor = state.health_monitor.read().await;
    let statuses = monitor.all_statuses().await;
    Ok(statuses
        .iter()
        .map(|s| HealthStatusInfo {
            name: s.name.clone(),
            state: format!("{:?}", s.state),
            last_check: s.last_check.map(|t| t.to_rfc3339()),
            last_success: s.last_success.map(|t| t.to_rfc3339()),
            consecutive_failures: s.consecutive_failures,
            latency_ms: s.latency_ms,
        })
        .collect())
}

/// Get health status for a specific integration.
#[tauri::command]
pub async fn get_integration_health(
    name: String,
    state: State<'_, AppState>,
) -> Result<HealthStatusInfo, String> {
    let monitor = state.health_monitor.read().await;
    let status = monitor
        .get_status(&name)
        .await
        .ok_or_else(|| format!("No health status for '{}'", name))?;
    Ok(HealthStatusInfo {
        name: status.name.clone(),
        state: format!("{:?}", status.state),
        last_check: status.last_check.map(|t| t.to_rfc3339()),
        last_success: status.last_success.map(|t| t.to_rfc3339()),
        consecutive_failures: status.consecutive_failures,
        latency_ms: status.latency_ms,
    })
}

/// Trigger a health check for a specific integration.
#[tauri::command]
pub async fn check_integration_health(
    name: String,
    state: State<'_, AppState>,
) -> Result<HealthStatusInfo, String> {
    let registry = state.integration_registry.read().await;
    let integration = registry
        .get(&name)
        .ok_or_else(|| format!("Integration '{}' not found", name))?;
    let url = integration
        .health_check_url
        .as_deref()
        .ok_or_else(|| format!("Integration '{}' has no health check URL", name))?;

    let mut monitor = state.health_monitor.write().await;
    monitor.check_health(&name, url).await;

    let status = monitor
        .get_status(&name)
        .await
        .ok_or("Health check completed but status not found")?;
    Ok(HealthStatusInfo {
        name: status.name.clone(),
        state: format!("{:?}", status.state),
        last_check: status.last_check.map(|t| t.to_rfc3339()),
        last_success: status.last_success.map(|t| t.to_rfc3339()),
        consecutive_failures: status.consecutive_failures,
        latency_ms: status.latency_ms,
    })
}

// ── OAuth PKCE Commands ───────────────────────────────────────

/// Start an OAuth 2.0 PKCE flow for an integration (returns auth URL to open in browser).
#[tauri::command]
pub async fn start_extension_oauth(
    integration_name: String,
    state: State<'_, AppState>,
) -> Result<OAuthFlowInfo, String> {
    let registry = state.integration_registry.read().await;
    let integration = registry
        .get(&integration_name)
        .ok_or_else(|| format!("Integration '{}' not found", integration_name))?;
    let oauth = integration
        .oauth
        .as_ref()
        .ok_or_else(|| format!("Integration '{}' does not support OAuth", integration_name))?;

    let challenge = clawdesk_extensions::oauth::PkceChallenge::generate();
    let oauth_state = clawdesk_extensions::oauth::generate_state();
    let redirect_uri = "http://localhost:18789/oauth/callback";

    let auth_url = clawdesk_extensions::oauth::build_auth_url(
        &oauth.auth_url,
        &oauth.client_id,
        redirect_uri,
        &oauth.scopes,
        &oauth_state,
        &challenge,
    );

    // Store the PKCE verifier + state for the callback exchange
    // We store in the vault if unlocked, otherwise in memory
    {
        let vault = state.credential_vault.read().await;
        if vault.is_unlocked().await {
            let _ = vault.store(
                &format!("_oauth_verifier_{}", integration_name),
                &challenge.verifier,
            ).await;
            let _ = vault.store(
                &format!("_oauth_state_{}", integration_name),
                &oauth_state,
            ).await;
        }
    }

    Ok(OAuthFlowInfo {
        auth_url,
        state: oauth_state,
    })
}

/// Complete an OAuth 2.0 PKCE flow by exchanging the authorization code.
#[tauri::command]
pub async fn complete_extension_oauth(
    integration_name: String,
    code: String,
    state_param: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let registry = state.integration_registry.read().await;
    let integration = registry
        .get(&integration_name)
        .ok_or_else(|| format!("Integration '{}' not found", integration_name))?;
    let oauth = integration
        .oauth
        .as_ref()
        .ok_or_else(|| format!("Integration '{}' does not support OAuth", integration_name))?;

    // Retrieve stored PKCE verifier
    let vault = state.credential_vault.read().await;
    let verifier = vault
        .get(&format!("_oauth_verifier_{}", integration_name))
        .await
        .map_err(|e| format!("{:?}", e))?
        .ok_or("No PKCE verifier found — start the OAuth flow first")?;
    let stored_state = vault
        .get(&format!("_oauth_state_{}", integration_name))
        .await
        .map_err(|e| format!("{:?}", e))?
        .ok_or("No OAuth state found")?;

    if stored_state != state_param {
        return Err("OAuth state mismatch — possible CSRF attack".into());
    }

    let redirect_uri = "http://localhost:18789/oauth/callback";
    let token_response = clawdesk_extensions::oauth::exchange_code(
        &oauth.token_url,
        &oauth.client_id,
        &code,
        redirect_uri,
        &verifier,
    )
    .await
    .map_err(|e| format!("{:?}", e))?;

    // Store the access token in the vault
    drop(vault);
    let vault = state.credential_vault.read().await;
    vault
        .store(
            &format!("{}_access_token", integration_name),
            &token_response.access_token,
        )
        .await
        .map_err(|e| format!("{:?}", e))?;

    if let Some(refresh) = &token_response.refresh_token {
        vault
            .store(&format!("{}_refresh_token", integration_name), refresh)
            .await
            .map_err(|e| format!("{:?}", e))?;
    }

    // Cleanup PKCE temporaries
    let verifier_key = format!("_oauth_verifier_{}", integration_name);
    let state_key = format!("_oauth_state_{}", integration_name);
    let _ = tokio::join!(
        vault.delete(&verifier_key),
        vault.delete(&state_key)
    );

    Ok(true)
}

// ── Internal helpers ──────────────────────────────────────────

/// SochDB key for persisted extension enabled state.
const EXTENSIONS_ENABLED_KEY: &str = "extensions/enabled";

/// Persist the list of enabled integration names to SochDB.
async fn persist_enabled_state(state: &AppState) {
    let names = {
        let registry = state.integration_registry.read().await;
        registry.enabled_names()
    };
    match serde_json::to_vec(&names) {
        Ok(bytes) => {
            if let Err(e) = state.soch_store.put_durable(EXTENSIONS_ENABLED_KEY, &bytes) {
                tracing::warn!(error = %e, "failed to persist extension enabled state");
            } else {
                tracing::debug!(count = names.len(), "persisted extension enabled state");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize extension enabled state");
        }
    }
}

/// Restore persisted enabled state from SochDB into the registry.
///
/// Called once during app startup (after `load_bundled()`). Returns the
/// list of names that were restored so the caller can launch MCP connections.
pub(crate) fn restore_enabled_state(
    registry: &clawdesk_extensions::IntegrationRegistry,
    soch_store: &clawdesk_sochdb::SochStore,
) -> Vec<String> {
    match soch_store.get(EXTENSIONS_ENABLED_KEY) {
        Ok(Some(bytes)) => match serde_json::from_slice::<Vec<String>>(&bytes) {
            Ok(names) => {
                let count = names.len();
                registry.restore_enabled(&names);
                tracing::info!(count, "restored extension enabled state from SochDB");
                names
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to deserialize extension enabled state");
                Vec::new()
            }
        },
        Ok(None) => {
            tracing::debug!("no persisted extension enabled state found");
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to read extension enabled state from SochDB");
            Vec::new()
        }
    }
}

/// SochDB key prefix for per-extension configuration.
const EXTENSIONS_CONFIG_PREFIX: &str = "extensions/config/";

/// Persist a single extension's configuration to SochDB.
async fn persist_extension_config(state: &AppState, name: &str) {
    let config = {
        let registry = state.integration_registry.read().await;
        registry.get_config(name).unwrap_or_default()
    };
    let key = format!("{}{}", EXTENSIONS_CONFIG_PREFIX, name);
    match serde_json::to_vec(&config) {
        Ok(bytes) => {
            if let Err(e) = state.soch_store.put_durable(&key, &bytes) {
                tracing::warn!(name, error = %e, "failed to persist extension config");
            } else {
                tracing::debug!(name, "persisted extension config");
            }
        }
        Err(e) => {
            tracing::warn!(name, error = %e, "failed to serialize extension config");
        }
    }
}

/// Restore all persisted extension configs from SochDB into the registry.
///
/// Called once during app startup (after `restore_enabled_state()`).
pub(crate) fn restore_extension_configs(
    registry: &clawdesk_extensions::IntegrationRegistry,
    soch_store: &clawdesk_sochdb::SochStore,
) {
    // Iterate all integrations and check for stored config
    let names: Vec<String> = registry.list().iter().map(|i| i.name.clone()).collect();
    let mut restored = 0;
    for name in &names {
        let key = format!("{}{}", EXTENSIONS_CONFIG_PREFIX, name);
        if let Ok(Some(bytes)) = soch_store.get(&key) {
            if let Ok(config) = serde_json::from_slice::<HashMap<String, String>>(&bytes) {
                if !config.is_empty() {
                    if let Err(e) = registry.set_config(name, config) {
                        tracing::warn!(name, error = %e, "failed to restore extension config");
                    } else {
                        restored += 1;
                    }
                }
            }
        }
    }
    if restored > 0 {
        tracing::info!(count = restored, "restored extension configs from SochDB");
    }
}

/// Convert a resolved `TransportConfig` to an MCP `McpServerConfig`.
///
/// Uses `IntegrationRegistry::resolve_transport()` to interpolate `${KEY}`
/// placeholders from user config → vault credentials → env vars → defaults.
fn resolved_transport_to_mcp_config(
    integration: &clawdesk_extensions::Integration,
    resolved_transport: &clawdesk_extensions::registry::TransportConfig,
    credential_env: HashMap<String, String>,
) -> Option<clawdesk_mcp::McpServerConfig> {
    let transport = match resolved_transport {
        clawdesk_extensions::registry::TransportConfig::Stdio { command, args } => {
            clawdesk_mcp::McpTransportConfig::Stdio {
                command: command.clone(),
                args: args.clone(),
            }
        }
        clawdesk_extensions::registry::TransportConfig::Sse { url } => {
            clawdesk_mcp::McpTransportConfig::Sse { url: url.clone() }
        }
        clawdesk_extensions::registry::TransportConfig::DirectApi { .. } => {
            // DirectApi integrations don't use MCP transport
            return None;
        }
    };

    Some(clawdesk_mcp::McpServerConfig {
        name: integration.name.clone(),
        transport,
        env: credential_env,
        description: integration.description.clone(),
    })
}

/// Resolve credentials for an integration from environment + vault + user config.
///
/// For each `CredentialRequirement` with an `env_var`, check:
/// 1. User-configured values (stored via `save_extension_config`)
/// 2. Process environment (std::env::var)
/// 3. Vault (if unlocked)
///
/// Returns a map of env_var_name → value for all resolved credentials.
async fn resolve_credentials(
    integration: &clawdesk_extensions::Integration,
    state: &AppState,
) -> HashMap<String, String> {
    let mut env = HashMap::new();

    // Load user config for this integration
    let user_config = {
        let registry = state.integration_registry.read().await;
        registry.get_config(&integration.name).unwrap_or_default()
    };

    for cred in &integration.credentials {
        if let Some(ref env_var) = cred.env_var {
            // 1. Check user config first (key = env_var or credential name)
            if let Some(val) = user_config.get(env_var).or_else(|| user_config.get(&cred.name)) {
                if !val.is_empty() {
                    env.insert(env_var.clone(), val.clone());
                    continue;
                }
            }
            // 2. Check process environment
            if let Ok(val) = std::env::var(env_var) {
                env.insert(env_var.clone(), val);
                continue;
            }
            // 3. Check vault
            let vault_key = format!("{}_{}", integration.name, cred.name);
            let vault = state.credential_vault.read().await;
            if vault.is_unlocked().await {
                if let Ok(Some(val)) = vault.get(&vault_key).await {
                    env.insert(env_var.clone(), val);
                }
            }
        }
    }
    env
}

/// Connect an integration's MCP server (spawn process + handshake + tool discovery).
///
/// Uses `resolve_transport` for `${KEY}` variable interpolation.
async fn connect_mcp_for_integration(
    state: &AppState,
    integration: &clawdesk_extensions::Integration,
) -> Result<(), String> {
    let cred_env = resolve_credentials(integration, state).await;

    // Resolve transport with variable interpolation
    let resolved_transport = {
        let registry = state.integration_registry.read().await;
        registry.resolve_transport(&integration.name, &cred_env)
    };
    let resolved = resolved_transport
        .ok_or_else(|| format!("Integration '{}' not found for transport resolution", integration.name))?;

    let config = resolved_transport_to_mcp_config(integration, &resolved, cred_env)
        .ok_or_else(|| format!("Integration '{}' has no MCP transport", integration.name))?;

    let mcp = state.mcp_client.read().await;
    match mcp.connect(config).await {
        Ok(tools) => {
            tracing::info!(
                name = %integration.name,
                tool_count = tools.len(),
                "MCP server connected and tools discovered"
            );
            Ok(())
        }
        Err(e) => Err(format!("MCP connect failed for '{}': {}", integration.name, e)),
    }
}

/// Launch MCP connections for all enabled integrations that support MCP.
///
/// Called during app startup (non-blocking — errors are logged, not propagated).
pub(crate) async fn launch_enabled_integrations(state: &AppState) {
    let integrations = {
        let registry = state.integration_registry.read().await;
        registry.enabled()
    };

    let mcp_integrations: Vec<_> = integrations
        .iter()
        .filter(|i| i.is_mcp_connectable())
        .collect();

    if mcp_integrations.is_empty() {
        return;
    }

    tracing::info!(
        count = mcp_integrations.len(),
        "launching MCP connections for enabled integrations"
    );

    for integration in mcp_integrations {
        match connect_mcp_for_integration(state, integration).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(
                    name = %integration.name,
                    error = %e,
                    "skipping MCP connection on startup — will retry when credentials are available"
                );
            }
        }
    }

    // Register all enabled integrations with health monitor
    let health_pairs: Vec<(String, String)> = integrations
        .iter()
        .filter_map(|i| {
            i.health_check_url
                .as_ref()
                .map(|url| (i.name.clone(), url.clone()))
        })
        .collect();

    if !health_pairs.is_empty() {
        let monitor = state.health_monitor.write().await;
        for (name, _) in &health_pairs {
            monitor.register(name).await;
        }
        tracing::info!(
            count = health_pairs.len(),
            "registered enabled integrations for health monitoring"
        );
    }
}
