//! GAP-A: Inbound Webhook Ingestion Layer.
//!
//! Receives webhooks from external services (GitHub, Stripe, Jira, etc.),
//! verifies HMAC-SHA256 signatures, extracts message content via JSONPath-like
//! templates, and routes the resulting message to the configured agent.
//!
//! ## Routes
//!
//! - `POST /api/v1/webhooks/:hook_id` — receive a webhook payload
//! - `GET  /api/v1/webhooks` — list registered webhooks
//! - `POST /api/v1/webhooks` — register a new webhook
//! - `DELETE /api/v1/webhooks/:hook_id` — delete a webhook
//!
//! ## Security
//!
//! HMAC-SHA256 signature verification via `X-Webhook-Signature` header.
//! The signature is `sha256=<hex_digest>` where the digest is HMAC(secret, body).

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Webhook configuration
// ---------------------------------------------------------------------------

/// Configuration for a registered webhook endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    /// Unique hook identifier (used in the URL path).
    pub hook_id: String,
    /// Human-readable label (e.g. "GitHub Push", "Stripe Payment").
    pub label: String,
    /// HMAC-SHA256 secret for signature verification.
    /// Empty string disables verification (not recommended for production).
    pub secret: String,
    /// JSONPath-like dot-notation path to extract message body from the payload.
    /// E.g. "commits.0.message" for GitHub push events.
    /// If empty, the entire payload is used as the message body.
    pub content_path: String,
    /// JSONPath-like dot-notation path to extract sender name.
    /// E.g. "sender.login" for GitHub events.
    /// If empty, defaults to the hook label.
    pub sender_path: String,
    /// Optional agent ID to route the message to.
    /// If empty, routes to the default agent.
    pub agent_id: Option<String>,
    /// Optional callback URL for outbound responses.
    /// When set, the agent's response is POSTed back to this URL.
    pub callback_url: Option<String>,
    /// Whether this webhook is enabled.
    pub enabled: bool,
    /// Creation timestamp.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl WebhookConfig {
    pub fn new(hook_id: impl Into<String>, label: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            hook_id: hook_id.into(),
            label: label.into(),
            secret: secret.into(),
            content_path: String::new(),
            sender_path: String::new(),
            agent_id: None,
            callback_url: None,
            enabled: true,
            created_at: chrono::Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Webhook store
// ---------------------------------------------------------------------------

/// In-memory webhook configuration store.
/// Thread-safe via `RwLock`. Webhooks can be persisted to SochDB for durability.
#[derive(Debug, Clone)]
pub struct WebhookStore {
    hooks: Arc<RwLock<HashMap<String, WebhookConfig>>>,
}

impl WebhookStore {
    pub fn new() -> Self {
        Self {
            hooks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register or update a webhook configuration.
    pub async fn upsert(&self, config: WebhookConfig) {
        let id = config.hook_id.clone();
        self.hooks.write().await.insert(id.clone(), config);
        info!(hook_id = %id, "Webhook registered");
    }

    /// Remove a webhook by ID. Returns true if it existed.
    pub async fn remove(&self, hook_id: &str) -> bool {
        self.hooks.write().await.remove(hook_id).is_some()
    }

    /// Get a webhook by ID.
    pub async fn get(&self, hook_id: &str) -> Option<WebhookConfig> {
        self.hooks.read().await.get(hook_id).cloned()
    }

    /// List all webhooks.
    pub async fn list(&self) -> Vec<WebhookConfig> {
        self.hooks.read().await.values().cloned().collect()
    }
}

impl Default for WebhookStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// HMAC-SHA256 verification
// ---------------------------------------------------------------------------

/// Verify HMAC-SHA256 signature from the `X-Webhook-Signature` header.
///
/// Expected format: `sha256=<hex_digest>` (GitHub-style).
/// Also supports raw hex without the `sha256=` prefix.
///
/// Returns `Ok(())` if valid, `Err(reason)` if invalid.
pub fn verify_signature(secret: &str, body: &[u8], signature_header: &str) -> Result<(), String> {
    if secret.is_empty() {
        // No secret configured — skip verification
        return Ok(());
    }

    if signature_header.is_empty() {
        return Err("Missing webhook signature".to_string());
    }

    // Strip "sha256=" prefix if present
    let hex_sig = signature_header
        .strip_prefix("sha256=")
        .unwrap_or(signature_header);

    let expected = hex::decode(hex_sig)
        .map_err(|e| format!("Invalid signature hex: {}", e))?;

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| format!("HMAC key error: {}", e))?;
    mac.update(body);

    // Constant-time comparison
    mac.verify_slice(&expected)
        .map_err(|_| "Signature mismatch".to_string())
}

// ---------------------------------------------------------------------------
// JSONPath-like value extraction
// ---------------------------------------------------------------------------

/// Extract a value from a JSON object using dot-notation path.
///
/// Supports:
/// - `"key"` — top-level field
/// - `"nested.key"` — nested object traversal
/// - `"array.0"` — array index access
/// - `""` (empty) — returns the entire value as a string
///
/// Returns `None` if the path doesn't match.
pub fn extract_json_path(value: &serde_json::Value, path: &str) -> Option<String> {
    if path.is_empty() {
        // Return entire payload as formatted string
        return Some(serde_json::to_string_pretty(value).unwrap_or_default());
    }

    let mut current = value;
    for segment in path.split('.') {
        current = if let Ok(idx) = segment.parse::<usize>() {
            current.get(idx)?
        } else {
            current.get(segment)?
        };
    }

    match current {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Webhook event (emitted to the message sink)
// ---------------------------------------------------------------------------

/// A processed webhook event, ready for injection into the message pipeline.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookEvent {
    pub hook_id: String,
    pub label: String,
    pub sender: String,
    pub content: String,
    pub raw_payload: serde_json::Value,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

// ---------------------------------------------------------------------------
// Axum route handlers
// ---------------------------------------------------------------------------

/// Shared state for webhook routes (added to GatewayState).
pub type SharedWebhookStore = Arc<WebhookStore>;

/// `POST /api/v1/webhooks/:hook_id` — receive an inbound webhook.
///
/// 1. Look up webhook config by hook_id
/// 2. Verify HMAC-SHA256 signature
/// 3. Extract content from payload
/// 4. Route to the inbound adapter registry
pub async fn receive_webhook(
    State(state): State<Arc<crate::state::GatewayState>>,
    Path(hook_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let store = &state.webhook_store;

    // Look up the webhook config
    let config = match store.get(&hook_id).await {
        Some(c) if c.enabled => c,
        Some(_) => {
            return (StatusCode::GONE, Json(serde_json::json!({
                "error": "Webhook is disabled"
            })));
        }
        None => {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({
                "error": "Unknown webhook"
            })));
        }
    };

    // Verify HMAC signature
    let signature = headers
        .get("x-webhook-signature")
        .or_else(|| headers.get("x-hub-signature-256")) // GitHub compat
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Err(reason) = verify_signature(&config.secret, &body, signature) {
        warn!(hook_id = %hook_id, reason = %reason, "Webhook signature verification failed");
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({
            "error": reason
        })));
    }

    // Parse the JSON payload
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            // Accept non-JSON payloads as raw text
            serde_json::json!({
                "raw_body": String::from_utf8_lossy(&body).to_string(),
                "parse_error": e.to_string(),
            })
        }
    };

    // Extract content and sender from payload
    let content = extract_json_path(&payload, &config.content_path)
        .unwrap_or_else(|| {
            // Fallback: use a summary of the payload
            let summary = serde_json::to_string(&payload).unwrap_or_default();
            if summary.len() > 500 {
                format!("{}...", &summary[..500])
            } else {
                summary
            }
        });

    let sender = extract_json_path(&payload, &config.sender_path)
        .unwrap_or_else(|| config.label.clone());

    let event = WebhookEvent {
        hook_id: hook_id.clone(),
        label: config.label.clone(),
        sender: sender.clone(),
        content: content.clone(),
        raw_payload: payload,
        timestamp: chrono::Utc::now(),
    };

    info!(
        hook_id = %hook_id,
        label = %config.label,
        sender = %sender,
        content_len = content.len(),
        "Webhook received and verified"
    );

    // Inject into the inbound adapter registry as a NormalizedMessage.
    // This flows through the same message pipeline as Discord/Telegram messages.
    {
        use clawdesk_types::channel::ChannelId;
        use clawdesk_types::message::{
            MessageOrigin, NormalizedMessage, SenderIdentity,
        };
        use clawdesk_types::session::SessionKey;
        use clawdesk_channel::inbound_adapter::{InboundEnvelope, ReplyPath};

        let origin = MessageOrigin::Internal {
            source: format!("webhook:{}", hook_id),
        };

        let msg = NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key: SessionKey::new(ChannelId::Internal, &format!("webhook:{}", hook_id)),
            body: format!("[Webhook: {}] {}", config.label, content),
            body_for_agent: Some(content),
            sender: SenderIdentity {
                id: format!("webhook:{}", hook_id),
                display_name: sender,
                channel: ChannelId::Internal,
            },
            media: vec![],
            artifact_refs: vec![],
            reply_context: None,
            origin: origin.clone(),
            timestamp: chrono::Utc::now(),
        };

        let envelope = InboundEnvelope {
            message: msg,
            reply_path: ReplyPath {
                channel: ChannelId::Internal,
                origin,
                prefer_thread: false,
                prefer_streaming: false,
            },
            deduplicated: false,
            source_adapter: format!("webhook:{}", hook_id),
        };

        // Get a sender clone from the inbound adapter registry
        let registry = state.inbound_registry.lock().await;
        let tx = registry.sender();
        drop(registry); // Release the lock before sending

        if let Err(e) = tx.send(Ok(envelope)).await {
            warn!(error = %e, "Failed to inject webhook message into adapter registry");
        }
    }

    (StatusCode::OK, Json(serde_json::json!({
        "status": "accepted",
        "hook_id": hook_id,
        "event_id": event.timestamp.timestamp_millis(),
    })))
}

/// `GET /api/v1/webhooks` — list registered webhooks.
pub async fn list_webhooks(
    State(state): State<Arc<crate::state::GatewayState>>,
) -> impl IntoResponse {
    let hooks = state.webhook_store.list().await;
    // Redact secrets in the response
    let redacted: Vec<serde_json::Value> = hooks.iter().map(|h| {
        serde_json::json!({
            "hook_id": h.hook_id,
            "label": h.label,
            "has_secret": !h.secret.is_empty(),
            "content_path": h.content_path,
            "sender_path": h.sender_path,
            "agent_id": h.agent_id,
            "callback_url": h.callback_url,
            "enabled": h.enabled,
            "created_at": h.created_at,
        })
    }).collect();

    Json(serde_json::json!({ "webhooks": redacted }))
}

/// Request body for creating a webhook.
#[derive(Debug, Deserialize)]
pub struct CreateWebhookRequest {
    pub label: String,
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub content_path: String,
    #[serde(default)]
    pub sender_path: String,
    pub agent_id: Option<String>,
    pub callback_url: Option<String>,
}

/// `POST /api/v1/webhooks` — register a new webhook.
pub async fn create_webhook(
    State(state): State<Arc<crate::state::GatewayState>>,
    Json(req): Json<CreateWebhookRequest>,
) -> impl IntoResponse {
    let hook_id = format!("wh_{}", uuid::Uuid::new_v4().as_simple());
    let config = WebhookConfig {
        hook_id: hook_id.clone(),
        label: req.label,
        secret: req.secret,
        content_path: req.content_path,
        sender_path: req.sender_path,
        agent_id: req.agent_id,
        callback_url: req.callback_url,
        enabled: true,
        created_at: chrono::Utc::now(),
    };

    state.webhook_store.upsert(config).await;

    (StatusCode::CREATED, Json(serde_json::json!({
        "hook_id": hook_id,
        "url": format!("/api/v1/webhooks/{}", hook_id),
    })))
}

/// `DELETE /api/v1/webhooks/:hook_id` — delete a webhook.
pub async fn delete_webhook(
    State(state): State<Arc<crate::state::GatewayState>>,
    Path(hook_id): Path<String>,
) -> impl IntoResponse {
    if state.webhook_store.remove(&hook_id).await {
        (StatusCode::OK, Json(serde_json::json!({
            "deleted": true,
            "hook_id": hook_id,
        })))
    } else {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({
            "error": "Webhook not found",
        })))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_signature_valid() {
        let secret = "test_secret_123";
        let body = b"hello world";

        // Compute the expected signature
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let result = mac.finalize();
        let hex_sig = hex::encode(result.into_bytes());

        assert!(verify_signature(secret, body, &format!("sha256={}", hex_sig)).is_ok());
        assert!(verify_signature(secret, body, &hex_sig).is_ok()); // without prefix
    }

    #[test]
    fn test_verify_signature_invalid() {
        assert!(verify_signature("secret", b"body", "sha256=deadbeef").is_err());
    }

    #[test]
    fn test_verify_signature_empty_secret_skips() {
        assert!(verify_signature("", b"body", "").is_ok());
    }

    #[test]
    fn test_extract_json_path_simple() {
        let val = serde_json::json!({"name": "test", "nested": {"key": "value"}});
        assert_eq!(extract_json_path(&val, "name"), Some("test".to_string()));
        assert_eq!(extract_json_path(&val, "nested.key"), Some("value".to_string()));
        assert_eq!(extract_json_path(&val, "missing"), None);
    }

    #[test]
    fn test_extract_json_path_array() {
        let val = serde_json::json!({"items": ["a", "b", "c"]});
        assert_eq!(extract_json_path(&val, "items.0"), Some("a".to_string()));
        assert_eq!(extract_json_path(&val, "items.2"), Some("c".to_string()));
        assert_eq!(extract_json_path(&val, "items.5"), None);
    }

    #[test]
    fn test_extract_json_path_empty_returns_full() {
        let val = serde_json::json!({"key": "val"});
        let result = extract_json_path(&val, "");
        assert!(result.is_some());
        assert!(result.unwrap().contains("key"));
    }

    #[test]
    fn test_webhook_store() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let store = WebhookStore::new();
            let config = WebhookConfig::new("test_hook", "Test", "secret123");
            store.upsert(config).await;

            assert!(store.get("test_hook").await.is_some());
            assert_eq!(store.list().await.len(), 1);

            assert!(store.remove("test_hook").await);
            assert!(store.get("test_hook").await.is_none());
        });
    }
}
