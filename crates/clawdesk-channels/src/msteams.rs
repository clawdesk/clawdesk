//! Microsoft Teams channel adapter for ClawDesk.
//!
//! Uses the Microsoft Bot Framework REST API for Teams integration.
//! Requires: app_id, app_password (from Azure Bot registration)

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{DeliveryReceipt, MessageOrigin, OutboundMessage};
use reqwest::Client;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::debug;

pub struct MsTeamsChannel {
    app_id: String,
    app_password: String,
    client: Client,
    /// Cached OAuth token
    access_token: RwLock<Option<String>>,
}

impl MsTeamsChannel {
    pub fn new(app_id: String, app_password: String) -> Self {
        Self {
            app_id,
            app_password,
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            access_token: RwLock::new(None),
        }
    }

    /// Get or refresh the Bot Framework OAuth token.
    async fn get_token(&self) -> Result<String, String> {
        // Check cached token
        if let Some(ref token) = *self.access_token.read().unwrap() {
            return Ok(token.clone());
        }

        let resp = self
            .client
            .post("https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token")
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.app_id),
                ("client_secret", &self.app_password),
                (
                    "scope",
                    "https://api.botframework.com/.default",
                ),
            ])
            .send()
            .await
            .map_err(|e| format!("token request failed: {}", e))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("token parse failed: {}", e))?;

        let token = data
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "no access_token in response".to_string())?
            .to_string();

        *self.access_token.write().unwrap() = Some(token.clone());
        Ok(token)
    }
}

#[async_trait]
impl Channel for MsTeamsChannel {
    fn id(&self) -> ChannelId {
        ChannelId::MsTeams
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Microsoft Teams".to_string(),
            supports_threading: true,
            supports_streaming: false,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(28000),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        // Teams uses webhook-based inbound messaging; no polling loop needed.
        // The webhook handler pushes NormalizedMessage through the sink.
        debug!("MsTeams channel started (webhook mode)");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let token = self.get_token().await?;

        // Extract Teams-specific routing info from message origin
        let (service_url, conversation_id) = match &msg.origin {
            MessageOrigin::MsTeams {
                tenant_id: _,
                channel_id,
                message_id: _,
            } => (
                "https://smba.trafficmanager.net/teams/".to_string(),
                channel_id.clone(),
            ),
            _ => return Err("MsTeams channel received non-Teams origin".into()),
        };

        let url = format!(
            "{}v3/conversations/{}/activities",
            service_url.trim_end_matches('/').to_owned() + "/",
            conversation_id
        );

        let body = serde_json::json!({
            "type": "message",
            "text": msg.body,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(format!("HTTP {}: {}", status, err_body));
        }

        let data: serde_json::Value =
            resp.json().await.map_err(|e| e.to_string())?;

        let activity_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        debug!(%activity_id, "sent Teams message");

        Ok(DeliveryReceipt {
            channel: ChannelId::MsTeams,
            message_id: activity_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        debug!("MsTeams channel stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teams_meta() {
        let ch = MsTeamsChannel::new("app_id".into(), "secret".into());
        assert_eq!(ch.id(), ChannelId::MsTeams);
        let meta = ch.meta();
        assert!(meta.supports_threading);
        assert!(meta.supports_groups);
        assert_eq!(meta.max_message_length, Some(28000));
    }
}
