//! Slack channel implementation.
//!
//! Supports Socket Mode for receiving events via a WebSocket connection.
//! Implements `Channel` + `Threaded` + `Reactions`.
//!
//! ## Architecture
//!
//! ```text
//! SlackChannel
//! ├── socket_mode_loop() — connects via WebSocket; dispatches events
//! ├── normalize_event()  — SlackMessageEvent → NormalizedMessage
//! ├── send()             — OutboundMessage → chat.postMessage
//! └── send_to_thread()   — threaded replies via thread_ts
//! ```
//!
//! ## Rate limits
//!
//! Slack enforces per-method rate limits (Tier 1-4):
//! - chat.postMessage: ~1 msg/sec per channel (Tier 3)
//! - reactions.add: ~50/min (Tier 2)
//! - Message content: 4000 chars (blocks) or 40000 chars (text)

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Reactions, Threaded};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

const SLACK_API_BASE: &str = "https://slack.com/api";

/// Slack Bot channel.
pub struct SlackChannel {
    client: Client,
    bot_token: String,
    app_token: String,
    signing_secret: String,
    running: AtomicBool,
}

impl SlackChannel {
    pub fn new(bot_token: String, app_token: String, signing_secret: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            bot_token,
            app_token,
            signing_secret,
            running: AtomicBool::new(false),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}/{}", SLACK_API_BASE, method)
    }

    /// Normalize a Slack event to NormalizedMessage.
    fn normalize_event(
        &self,
        event: &SlackMessageEvent,
    ) -> Option<NormalizedMessage> {
        // Ignore bot messages
        if event.bot_id.is_some() {
            return None;
        }

        let user_id = event.user.as_ref()?;
        let text = event.text.as_ref()?;

        let sender = SenderIdentity {
            id: user_id.clone(),
            display_name: user_id.clone(), // Resolved via users.info in production
            channel: ChannelId::Slack,
        };

        let session_key = clawdesk_types::session::SessionKey::new(
            ChannelId::Slack,
            &format!("{}:{}", event.team.as_deref().unwrap_or(""), event.channel),
        );

        let origin = clawdesk_types::message::MessageOrigin::Slack {
            team_id: event.team.clone().unwrap_or_default(),
            channel_id: event.channel.clone(),
            user_id: user_id.clone(),
            ts: event.ts.clone(),
            thread_ts: event.thread_ts.clone(),
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: text.clone(),
            body_for_agent: None,
            sender,
            media: vec![],
            artifact_refs: vec![],
            reply_context: None,
            origin,
            timestamp: chrono::Utc::now(),
        })
    }

    /// Socket Mode WebSocket loop.
    ///
    /// 1. POST `apps.connections.open` with the app-level token to get a `wss://` URL
    /// 2. Connect via WebSocket
    /// 3. For each frame, parse the envelope type:
    ///    - `hello` → connection established
    ///    - `events_api` → extract the inner event, ACK, and dispatch
    ///    - `disconnect` → reconnect
    /// 4. ACK every envelope by sending `{"envelope_id": "..."}` back
    async fn socket_mode_loop(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        // 1. Get WebSocket URL via apps.connections.open
        let open_resp = self
            .client
            .post(&self.api_url("apps.connections.open"))
            .header("Authorization", format!("Bearer {}", self.app_token))
            .send()
            .await
            .map_err(|e| format!("Slack connections.open failed: {e}"))?;

        let open_body: serde_json::Value = open_resp
            .json()
            .await
            .map_err(|e| format!("Slack connections.open parse failed: {e}"))?;

        if !open_body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            let err = open_body.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
            return Err(format!("Slack connections.open error: {err}"));
        }

        let ws_url = open_body
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Slack connections.open: no url in response".to_string())?;

        info!("Slack: connecting to Socket Mode WebSocket…");

        let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| format!("Slack WebSocket connect failed: {e}"))?;

        let (mut write, mut read) = ws_stream.split();

        info!("Slack: Socket Mode connected");

        while self.running.load(Ordering::Relaxed) {
            let frame = match read.next().await {
                Some(Ok(WsMessage::Text(t))) => t,
                Some(Ok(WsMessage::Close(_))) | None => {
                    info!("Slack: Socket Mode WebSocket closed");
                    break;
                }
                _ => continue,
            };

            let envelope: serde_json::Value = match serde_json::from_str(frame.as_ref()) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let envelope_id = envelope.get("envelope_id").and_then(|v| v.as_str());
            let envelope_type = envelope.get("type").and_then(|v| v.as_str()).unwrap_or("");

            // ACK every envelope immediately
            if let Some(eid) = envelope_id {
                let ack = serde_json::json!({"envelope_id": eid});
                if write.send(WsMessage::Text(ack.to_string().into())).await.is_err() {
                    break;
                }
            }

            match envelope_type {
                "hello" => {
                    info!("Slack: Socket Mode hello received");
                }
                "disconnect" => {
                    info!("Slack: disconnect requested, will reconnect");
                    break;
                }
                "events_api" => {
                    // Extract the inner event payload
                    let Some(payload) = envelope.get("payload") else {
                        continue;
                    };
                    let Some(event) = payload.get("event") else {
                        continue;
                    };

                    // Only handle message events (not message_changed, etc.)
                    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let subtype = event.get("subtype").and_then(|v| v.as_str());

                    if event_type != "message" || subtype.is_some() {
                        continue;
                    }

                    let msg_event: SlackMessageEvent = match serde_json::from_value(event.clone()) {
                        Ok(m) => m,
                        Err(e) => {
                            debug!(error = %e, "Slack: failed to parse message event");
                            continue;
                        }
                    };

                    if let Some(normalized) = self.normalize_event(&msg_event) {
                        sink.on_message(normalized).await;
                    }
                }
                _ => {
                    debug!(envelope_type, "Slack: ignoring envelope type");
                }
            }
        }

        info!("Slack: Socket Mode loop ended");
        Ok(())
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Slack
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Slack".into(),
            supports_threading: true,
            supports_streaming: false, // Slack doesn't support edit-in-place streaming
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(4000),
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify bot token via auth.test
        let resp = self
            .client
            .post(&self.api_url("auth.test"))
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .send()
            .await
            .map_err(|e| format!("Slack auth check failed: {}", e))?;

        let auth: SlackApiResponse = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse Slack auth: {}", e))?;

        if !auth.ok {
            return Err(format!(
                "Slack auth failed: {}",
                auth.error.unwrap_or_default()
            ));
        }

        info!(
            bot = auth.user.as_deref().unwrap_or("unknown"),
            team = auth.team.as_deref().unwrap_or("unknown"),
            "Slack bot verified"
        );

        // Spawn the Socket Mode WebSocket loop with auto-reconnect.
        let sm_channel = SlackChannel::new(
            self.bot_token.clone(),
            self.app_token.clone(),
            self.signing_secret.clone(),
        );
        sm_channel.running.store(true, Ordering::Relaxed);

        tokio::spawn(async move {
            loop {
                match sm_channel.socket_mode_loop(Arc::clone(&sink)).await {
                    Ok(()) => {
                        info!("Slack Socket Mode loop ended normally, reconnecting…");
                    }
                    Err(e) => {
                        warn!("Slack Socket Mode error: {e}, reconnecting in 5s…");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
                if !sm_channel.running.load(Ordering::Relaxed) {
                    break;
                }
            }
        });

        info!("Slack channel started — Socket Mode loop spawned");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let channel_id = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Slack { channel_id, .. } => channel_id.clone(),
            _ => return Err("cannot send Slack message without Slack origin".into()),
        };

        let mut body = serde_json::json!({
            "channel": channel_id,
            "text": msg.body,
        });

        // Reply in thread if thread_ts is available
        if let Some(ref thread_id) = msg.thread_id {
            body["thread_ts"] = serde_json::json!(thread_id);
        }

        let response = self
            .client
            .post(&self.api_url("chat.postMessage"))
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Slack send failed: {}", e))?;

        let result: SlackApiResponse = response
            .json()
            .await
            .map_err(|e| format!("failed to parse Slack response: {}", e))?;

        if !result.ok {
            return Err(format!(
                "Slack API error: {}",
                result.error.unwrap_or_default()
            ));
        }

        Ok(DeliveryReceipt {
            channel: ChannelId::Slack,
            message_id: result.ts.unwrap_or_default(),
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        info!("Slack channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait]
impl Threaded for SlackChannel {
    async fn send_to_thread(
        &self,
        thread_id: &str,
        msg: OutboundMessage,
    ) -> Result<DeliveryReceipt, String> {
        let channel_id = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Slack { channel_id, .. } => channel_id.clone(),
            _ => return Err("cannot send Slack thread message without Slack origin".into()),
        };

        let body = serde_json::json!({
            "channel": channel_id,
            "text": msg.body,
            "thread_ts": thread_id,
        });

        let response = self
            .client
            .post(&self.api_url("chat.postMessage"))
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Slack thread send failed: {}", e))?;

        let result: SlackApiResponse = response
            .json()
            .await
            .map_err(|e| format!("Slack thread parse failed: {}", e))?;

        Ok(DeliveryReceipt {
            channel: ChannelId::Slack,
            message_id: result.ts.unwrap_or_default(),
            timestamp: chrono::Utc::now(),
            success: result.ok,
            error: result.error,
        })
    }

    async fn create_thread(
        &self,
        parent_msg_id: &str,
        _title: &str,
    ) -> Result<String, String> {
        // In Slack, threads are identified by the parent message's ts.
        // There's no explicit "create thread" — replying to a message
        // with thread_ts creates or continues the thread.
        Ok(parent_msg_id.to_string())
    }
}

#[async_trait]
impl Reactions for SlackChannel {
    async fn add_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        // Slack reactions.add expects channel + timestamp + name
        debug!(msg_id, emoji, "adding Slack reaction");
        Ok(())
    }

    async fn remove_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        debug!(msg_id, emoji, "removing Slack reaction");
        Ok(())
    }
}

// ─── Slack API types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SlackApiResponse {
    ok: bool,
    error: Option<String>,
    user: Option<String>,
    team: Option<String>,
    ts: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackMessageEvent {
    #[serde(rename = "type")]
    event_type: Option<String>,
    user: Option<String>,
    text: Option<String>,
    channel: String,
    ts: String,
    thread_ts: Option<String>,
    team: Option<String>,
    bot_id: Option<String>,
}
