//! Microsoft Teams channel adapter via Bot Framework.
//!
//! Communicates with Microsoft Teams through the Bot Framework REST API.
//! Inbound: receives Activity objects via webhook callback.
//! Outbound: sends messages via the Bot Framework Connector REST API.
//!
//! ## Architecture
//!
//! ```text
//! TeamsChannel
//! ├── start(sink)  — register webhook, begin polling for activities
//! ├── send(msg)    — POST to /v3/conversations/{id}/activities
//! ├── stop()       — stop polling, cleanup
//! └── Thread support via replyToId
//! ```
//!
//! ## Authentication
//!
//! Uses OAuth 2.0 client credentials flow to obtain tokens from
//! `login.microsoftonline.com`. Tokens are cached and refreshed before expiry.

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Threaded};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Configuration for Microsoft Teams channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamsConfig {
    /// Bot Framework App ID.
    pub app_id: String,
    /// Bot Framework App Secret.
    pub app_secret: String,
    /// Tenant ID (or "common" for multi-tenant).
    pub tenant_id: String,
    /// Service URL override (default: https://smba.trafficmanager.net/teams).
    pub service_url: Option<String>,
}

/// Cached OAuth token.
#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at: chrono::DateTime<Utc>,
}

/// Microsoft Teams Bot Framework channel.
pub struct TeamsChannel {
    config: TeamsConfig,
    client: reqwest::Client,
    sink: RwLock<Option<Arc<dyn MessageSink>>>,
    token: RwLock<Option<CachedToken>>,
    running: AtomicBool,
}

/// Bot Framework Activity (simplified).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    #[serde(rename = "type")]
    pub activity_type: String,
    pub id: Option<String>,
    pub text: Option<String>,
    pub from: Option<ActivityFrom>,
    pub conversation: Option<Conversation>,
    pub service_url: Option<String>,
    pub reply_to_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ActivityFrom {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
}

/// Token response from Azure AD.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

impl TeamsChannel {
    pub fn new(config: TeamsConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            sink: RwLock::new(None),
            token: RwLock::new(None),
            running: AtomicBool::new(false),
        }
    }

    /// Get a valid OAuth token, refreshing if needed.
    async fn get_token(&self) -> Result<String, String> {
        // Check cached token
        {
            let cached = self.token.read().await;
            if let Some(ref t) = *cached {
                if t.expires_at > Utc::now() + chrono::Duration::seconds(60) {
                    return Ok(t.access_token.clone());
                }
            }
        }

        // Fetch new token
        let url = format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.config.tenant_id
        );

        let resp = self
            .client
            .post(&url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.config.app_id),
                ("client_secret", &self.config.app_secret),
                (
                    "scope",
                    "https://api.botframework.com/.default",
                ),
            ])
            .send()
            .await
            .map_err(|e| format!("Teams token request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Teams auth failed ({}): {}", status, body));
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("Teams token parse error: {}", e))?;

        let cached = CachedToken {
            access_token: token_resp.access_token.clone(),
            expires_at: Utc::now() + chrono::Duration::seconds(token_resp.expires_in as i64),
        };

        *self.token.write().await = Some(cached);
        Ok(token_resp.access_token)
    }

    /// Process an inbound Activity from the Bot Framework webhook.
    pub async fn process_activity(&self, activity: Activity) {
        if activity.activity_type != "message" {
            debug!(activity_type = %activity.activity_type, "ignoring non-message activity");
            return;
        }

        let text = match activity.text {
            Some(t) => t,
            None => return,
        };

        let sender = activity.from.as_ref();
        let conversation_id = activity
            .conversation
            .as_ref()
            .map(|c| c.id.clone())
            .unwrap_or_default();

        let sender_id = sender.map(|s| s.id.clone()).unwrap_or_default();
        let msg = NormalizedMessage {
            id: Uuid::new_v4(),
            session_key: clawdesk_types::session::SessionKey::new(
                ChannelId::Teams,
                &conversation_id,
            ),
            body: text,
            body_for_agent: None,
            sender: SenderIdentity {
                id: sender_id,
                display_name: sender
                    .and_then(|s| s.name.clone())
                    .unwrap_or_else(|| "Teams User".into()),
                channel: ChannelId::Teams,
            },
            media: vec![],
            artifact_refs: vec![],
            reply_context: None,
            origin: clawdesk_types::message::MessageOrigin::Teams {
                conversation_id: activity
                    .conversation
                    .map(|c| c.id)
                    .unwrap_or_default(),
            },
            timestamp: Utc::now(),
        };

        let sink = self.sink.read().await;
        if let Some(ref s) = *sink {
            s.on_message(msg).await;
        }
    }

    /// Send a message to a conversation via Bot Framework Connector API.
    async fn send_to_conversation(
        &self,
        conversation_id: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<String, String> {
        let token = self.get_token().await?;
        let service_url = self
            .config
            .service_url
            .as_deref()
            .unwrap_or("https://smba.trafficmanager.net/teams");

        let url = format!(
            "{}/v3/conversations/{}/activities",
            service_url, conversation_id
        );

        let mut activity = serde_json::json!({
            "type": "message",
            "text": text,
        });

        if let Some(reply_id) = reply_to {
            activity["replyToId"] = serde_json::Value::String(reply_id.to_string());
        }

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&activity)
            .send()
            .await
            .map_err(|e| format!("Teams send failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            return Err(format!("Teams send error: HTTP {}", status));
        }

        let resp_body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Teams response parse error: {}", e))?;

        Ok(resp_body["id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string())
    }
}

#[async_trait]
impl Channel for TeamsChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Teams
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Microsoft Teams".into(),
            supports_threading: true,
            supports_streaming: false,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(28_000), // Teams limit
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        // Pre-validate credentials by fetching a token
        let _ = self.get_token().await?;
        *self.sink.write().await = Some(sink);
        self.running.store(true, Ordering::Release);
        info!("Teams channel started");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let conversation_id = msg.thread_id.as_deref().unwrap_or("default");
        let msg_id = self
            .send_to_conversation(conversation_id, &msg.body, msg.reply_to.as_deref())
            .await?;

        Ok(DeliveryReceipt {
            channel: ChannelId::Teams,
            message_id: msg_id,
            timestamp: Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Release);
        *self.sink.write().await = None;
        *self.token.write().await = None;
        info!("Teams channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait]
impl Threaded for TeamsChannel {
    async fn send_to_thread(
        &self,
        thread_id: &str,
        msg: OutboundMessage,
    ) -> Result<DeliveryReceipt, String> {
        let msg_id = self
            .send_to_conversation(thread_id, &msg.body, msg.reply_to.as_deref())
            .await?;
        Ok(DeliveryReceipt {
            channel: ChannelId::Teams,
            message_id: msg_id,
            timestamp: Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn create_thread(
        &self,
        _parent_msg_id: &str,
        _title: &str,
    ) -> Result<String, String> {
        // Teams threads are created implicitly via replyToId
        Ok(uuid::Uuid::new_v4().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> TeamsConfig {
        TeamsConfig {
            app_id: "test-app-id".into(),
            app_secret: "test-secret".into(),
            tenant_id: "test-tenant".into(),
            service_url: None,
        }
    }

    #[test]
    fn teams_meta() {
        let ch = TeamsChannel::new(test_config());
        assert_eq!(ch.id(), ChannelId::Teams);
        let meta = ch.meta();
        assert!(meta.supports_threading);
        assert!(meta.supports_groups);
        assert_eq!(meta.max_message_length, Some(28_000));
    }

    #[test]
    fn teams_display_name() {
        let ch = TeamsChannel::new(test_config());
        assert_eq!(ch.meta().display_name, "Microsoft Teams");
    }
}
