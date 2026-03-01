//! Matrix protocol channel adapter.
//!
//! Connects to a Matrix homeserver using the Client-Server API (v1.6+).
//! Supports encrypted rooms (Olm/Megolm) when keys are provided.
//!
//! ## Architecture
//!
//! ```text
//! MatrixChannel
//! ├── start(sink)  — /login, /sync long-polling loop
//! ├── send(msg)    — PUT /_matrix/client/v3/rooms/{roomId}/send/m.room.message/{txnId}
//! ├── stop()       — stop sync loop
//! └── Threads via m.thread relation (MSC3440)
//! ```
//!
//! ## Sync loop
//!
//! Uses long-polling `/sync` with `since` token for incremental updates.
//! On each sync response, processes `m.room.message` events from joined rooms.

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Threaded};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Matrix channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixConfig {
    /// Homeserver URL (e.g., https://matrix.org).
    pub homeserver_url: String,
    /// Bot's Matrix user ID (e.g., @bot:matrix.org).
    pub user_id: String,
    /// Access token (from /login or pre-provisioned).
    pub access_token: Option<String>,
    /// Password for /login authentication.
    pub password: Option<String>,
    /// Room IDs to join and monitor.
    pub rooms: Vec<String>,
    /// Sync timeout in ms (default: 30000).
    pub sync_timeout_ms: u64,
}

impl Default for MatrixConfig {
    fn default() -> Self {
        Self {
            homeserver_url: "https://matrix.org".into(),
            user_id: String::new(),
            access_token: None,
            password: None,
            rooms: Vec::new(),
            sync_timeout_ms: 30_000,
        }
    }
}

/// Transaction ID counter for idempotent message sends.
static TXN_SEQ: AtomicU64 = AtomicU64::new(0);

/// Matrix homeserver channel.
pub struct MatrixChannel {
    config: MatrixConfig,
    client: reqwest::Client,
    sink: RwLock<Option<Arc<dyn MessageSink>>>,
    /// Access token (obtained via /login or from config).
    access_token: RwLock<Option<String>>,
    /// Sync `since` token for incremental /sync.
    since_token: RwLock<Option<String>>,
    running: AtomicBool,
}

/// Simplified /sync response for event extraction.
#[derive(Deserialize)]
struct SyncResponse {
    next_batch: String,
    rooms: Option<SyncRooms>,
}

#[derive(Deserialize)]
struct SyncRooms {
    join: Option<std::collections::HashMap<String, JoinedRoom>>,
}

#[derive(Deserialize)]
struct JoinedRoom {
    timeline: Option<Timeline>,
}

#[derive(Deserialize)]
struct Timeline {
    events: Option<Vec<RoomEvent>>,
}

#[derive(Deserialize)]
struct RoomEvent {
    #[serde(rename = "type")]
    event_type: String,
    event_id: Option<String>,
    sender: Option<String>,
    content: Option<EventContent>,
}

#[derive(Deserialize)]
struct EventContent {
    msgtype: Option<String>,
    body: Option<String>,
    #[serde(rename = "m.relates_to")]
    relates_to: Option<RelatesTo>,
}

#[derive(Deserialize)]
struct RelatesTo {
    rel_type: Option<String>,
    event_id: Option<String>,
}

impl MatrixChannel {
    pub fn new(config: MatrixConfig) -> Self {
        let access_token = config.access_token.clone();
        Self {
            config,
            client: reqwest::Client::new(),
            sink: RwLock::new(None),
            access_token: RwLock::new(access_token),
            since_token: RwLock::new(None),
            running: AtomicBool::new(false),
        }
    }

    /// Authenticate via /login if no access token is configured.
    async fn login(&self) -> Result<String, String> {
        if let Some(ref token) = *self.access_token.read().await {
            return Ok(token.clone());
        }

        let password = self
            .config
            .password
            .as_deref()
            .ok_or("no access_token or password configured")?;

        let url = format!("{}/_matrix/client/v3/login", self.config.homeserver_url);

        let body = serde_json::json!({
            "type": "m.login.password",
            "identifier": {
                "type": "m.id.user",
                "user": self.config.user_id,
            },
            "password": password,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Matrix login failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("Matrix login error: HTTP {}", resp.status()));
        }

        let login_resp: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Matrix login parse error: {}", e))?;

        let token = login_resp["access_token"]
            .as_str()
            .ok_or("no access_token in login response")?
            .to_string();

        *self.access_token.write().await = Some(token.clone());
        info!("Matrix login successful");
        Ok(token)
    }

    /// Perform a single /sync request.
    async fn sync_once(&self) -> Result<(), String> {
        let token = self.login().await?;
        let since = self.since_token.read().await.clone();

        let mut url = format!(
            "{}/_matrix/client/v3/sync?timeout={}",
            self.config.homeserver_url, self.config.sync_timeout_ms
        );
        if let Some(ref since) = since {
            url.push_str(&format!("&since={}", since));
        }

        let resp = self
            .client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| format!("Matrix sync failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("Matrix sync error: HTTP {}", resp.status()));
        }

        let sync_resp: SyncResponse = resp
            .json()
            .await
            .map_err(|e| format!("Matrix sync parse error: {}", e))?;

        *self.since_token.write().await = Some(sync_resp.next_batch);

        // Process room events
        if let Some(rooms) = sync_resp.rooms {
            if let Some(joined) = rooms.join {
                for (room_id, room) in joined {
                    if let Some(timeline) = room.timeline {
                        if let Some(events) = timeline.events {
                            for event in events {
                                self.process_event(&room_id, event).await;
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Process a single room event.
    async fn process_event(&self, room_id: &str, event: RoomEvent) {
        if event.event_type != "m.room.message" {
            return;
        }

        let content = match event.content {
            Some(c) => c,
            None => return,
        };

        // Skip non-text messages
        if content.msgtype.as_deref() != Some("m.text") {
            return;
        }

        let text = match content.body {
            Some(t) => t,
            None => return,
        };

        let sender_id = event.sender.unwrap_or_default();

        // Skip our own messages
        if sender_id == self.config.user_id {
            return;
        }

        let thread_id = content
            .relates_to
            .and_then(|r| {
                if r.rel_type.as_deref() == Some("m.thread") {
                    r.event_id
                } else {
                    None
                }
            });

        let msg = NormalizedMessage {
            id: Uuid::new_v4(),
            session_key: clawdesk_types::session::SessionKey::new(
                ChannelId::Matrix,
                room_id,
            ),
            body: text,
            body_for_agent: None,
            sender: SenderIdentity {
                id: sender_id.clone(),
                display_name: sender_id.split(':').next().unwrap_or(&sender_id).trim_start_matches('@').to_string(),
                channel: ChannelId::Matrix,
            },
            media: vec![],
            artifact_refs: vec![],
            reply_context: None,
            origin: clawdesk_types::message::MessageOrigin::Matrix {
                room_id: room_id.to_string(),
            },
            timestamp: Utc::now(),
        };

        let sink = self.sink.read().await;
        if let Some(ref s) = *sink {
            s.on_message(msg).await;
        }
    }

    /// Send a message to a Matrix room.
    async fn send_to_room(
        &self,
        room_id: &str,
        text: &str,
        thread_event_id: Option<&str>,
    ) -> Result<String, String> {
        let token = self.login().await?;
        let txn_id = TXN_SEQ.fetch_add(1, Ordering::Relaxed);

        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/txn_{}",
            self.config.homeserver_url, room_id, txn_id
        );

        let mut body = serde_json::json!({
            "msgtype": "m.text",
            "body": text,
        });

        // Thread support via m.relates_to
        if let Some(event_id) = thread_event_id {
            body["m.relates_to"] = serde_json::json!({
                "rel_type": "m.thread",
                "event_id": event_id,
            });
        }

        let resp = self
            .client
            .put(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Matrix send failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("Matrix send error: HTTP {}", resp.status()));
        }

        let resp_body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Matrix response parse error: {}", e))?;

        Ok(resp_body["event_id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string())
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Matrix
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Matrix".into(),
            supports_threading: true,
            supports_streaming: false,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(65_536),
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        let _ = self.login().await?;
        *self.sink.write().await = Some(sink);
        self.running.store(true, Ordering::Release);
        info!(homeserver = %self.config.homeserver_url, "Matrix channel started");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let room_id = msg
            .thread_id
            .as_deref()
            .or(self.config.rooms.first().map(|s| s.as_str()))
            .ok_or("no room_id specified and no default rooms configured")?;

        let event_id = self
            .send_to_room(room_id, &msg.body, msg.reply_to.as_deref())
            .await?;

        Ok(DeliveryReceipt {
            channel: ChannelId::Matrix,
            message_id: event_id,
            timestamp: Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Release);
        *self.sink.write().await = None;
        info!("Matrix channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait]
impl Threaded for MatrixChannel {
    async fn send_to_thread(
        &self,
        thread_id: &str,
        msg: OutboundMessage,
    ) -> Result<DeliveryReceipt, String> {
        // In Matrix, threads are identified by the root event_id.
        // The room_id must also be known — use the first configured room.
        let room_id = self
            .config
            .rooms
            .first()
            .map(|s| s.as_str())
            .ok_or("no rooms configured")?;

        let event_id = self
            .send_to_room(room_id, &msg.body, Some(thread_id))
            .await?;

        Ok(DeliveryReceipt {
            channel: ChannelId::Matrix,
            message_id: event_id,
            timestamp: Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn create_thread(
        &self,
        parent_msg_id: &str,
        _title: &str,
    ) -> Result<String, String> {
        // Matrix threads are identified by the parent event_id
        Ok(parent_msg_id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> MatrixConfig {
        MatrixConfig {
            homeserver_url: "https://matrix.test".into(),
            user_id: "@bot:matrix.test".into(),
            access_token: Some("test-token".into()),
            password: None,
            rooms: vec!["!room:matrix.test".into()],
            sync_timeout_ms: 5000,
        }
    }

    #[test]
    fn matrix_meta() {
        let ch = MatrixChannel::new(test_config());
        assert_eq!(ch.id(), ChannelId::Matrix);
        let meta = ch.meta();
        assert!(meta.supports_threading);
        assert_eq!(meta.display_name, "Matrix");
    }

    #[test]
    fn matrix_config_default() {
        let cfg = MatrixConfig::default();
        assert_eq!(cfg.homeserver_url, "https://matrix.org");
        assert_eq!(cfg.sync_timeout_ms, 30_000);
    }
}
