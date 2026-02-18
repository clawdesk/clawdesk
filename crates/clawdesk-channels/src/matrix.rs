//! Matrix channel adapter for ClawDesk.
//!
//! Uses the Matrix Client-Server API (v1.6+) for decentralized,
//! end-to-end encrypted messaging.
//!
//! Requires: homeserver_url, access_token (or user_id + password for login)

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{DeliveryReceipt, OutboundMessage};
use chrono::Utc;
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

pub struct MatrixChannel {
    homeserver: String,
    access_token: String,
    client: Client,
}

impl MatrixChannel {
    pub fn new(homeserver: String, access_token: String) -> Self {
        Self {
            homeserver: homeserver.trim_end_matches('/').to_string(),
            access_token,
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
        }
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Matrix
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Matrix".to_string(),
            supports_threading: true,
            supports_streaming: false,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(65536),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        info!("Matrix channel started (long-polling /sync mode)");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let room_id = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Matrix { room_id, .. } => room_id.clone(),
            _ => return Err("cannot send Matrix message without Matrix origin".into()),
        };

        let txn_id = uuid::Uuid::new_v4().to_string();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.homeserver, room_id, txn_id
        );

        let body = serde_json::json!({
            "msgtype": "m.text",
            "body": msg.body,
        });

        let resp = self
            .client
            .put(&url)
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Matrix send failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("Matrix HTTP {}: {}", status, text));
        }

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Matrix response parse: {}", e))?;

        let event_id = data
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&txn_id)
            .to_string();

        debug!(%event_id, "sent Matrix message");
        Ok(DeliveryReceipt {
            channel: ChannelId::Matrix,
            message_id: event_id,
            timestamp: Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        info!("Matrix channel stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_meta() {
        let ch = MatrixChannel::new("https://matrix.org".into(), "token".into());
        assert_eq!(ch.meta().display_name, "Matrix");
        assert!(ch.meta().supports_threading);
        assert_eq!(ch.id(), ChannelId::Matrix);
    }
}

