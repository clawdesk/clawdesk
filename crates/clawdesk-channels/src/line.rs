//! Line Messaging API channel implementation.
//!
//! Uses the Line Messaging API v2 with webhook-based push model.
//! Reply tokens expire in 30 seconds (25s safety margin).

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, StreamHandle, Streaming};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, MessageOrigin, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

/// Reply token with TTL tracking.
#[derive(Debug, Clone)]
struct ReplyToken {
    token: String,
    received_at: Instant,
}

impl ReplyToken {
    /// Reply tokens must be used within 1 minute per official docs.
    /// Using 55-second safety margin.
    fn is_valid(&self) -> bool {
        self.received_at.elapsed() < Duration::from_secs(55)
    }
}

/// Line Messaging API channel.
pub struct LineChannel {
    client: Client,
    channel_access_token: String,
    #[allow(dead_code)]
    channel_secret: String,
    running: Arc<AtomicBool>,
    shutdown: Notify,
    reply_tokens: Arc<Mutex<HashMap<String, ReplyToken>>>,
    sink: Arc<Mutex<Option<Arc<dyn MessageSink>>>>,
}

impl LineChannel {
    pub fn new(channel_access_token: String, channel_secret: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            channel_access_token,
            channel_secret,
            running: Arc::new(AtomicBool::new(false)),
            shutdown: Notify::new(),
            reply_tokens: Arc::new(Mutex::new(HashMap::new())),
            sink: Arc::new(Mutex::new(None)),
        }
    }

    fn api_url(path: &str) -> String {
        format!("https://api.line.me/v2/bot{path}")
    }

    async fn cache_reply_token(&self, user_id: &str, token: String) {
        let mut tokens = self.reply_tokens.lock().await;
        tokens.insert(user_id.to_string(), ReplyToken {
            token,
            received_at: Instant::now(),
        });
    }

    async fn get_reply_token(&self, user_id: &str) -> Option<String> {
        let mut tokens = self.reply_tokens.lock().await;
        if let Some(rt) = tokens.get(user_id) {
            if rt.is_valid() {
                return Some(rt.token.clone());
            }
            tokens.remove(user_id);
        }
        None
    }

    fn normalize_event(event: &LineEvent) -> Option<NormalizedMessage> {
        if event.event_type != LineEventType::Message {
            return None;
        }
        let msg = event.message.as_ref()?;
        let text = match msg {
            LineMessage::Text { text, .. } => text.clone(),
            LineMessage::Sticker { package_id, sticker_id, .. } => {
                format!("[sticker: {package_id}/{sticker_id}]")
            }
            LineMessage::Image { .. } => "[image]".to_string(),
            LineMessage::Video { .. } => "[video]".to_string(),
            LineMessage::Audio { .. } => "[audio]".to_string(),
            LineMessage::Location { title, address, .. } => {
                format!("[location: {} - {}]",
                    title.as_deref().unwrap_or(""),
                    address.as_deref().unwrap_or(""))
            }
            LineMessage::File { file_name, .. } => {
                format!("[file: {}]", file_name.as_deref().unwrap_or("unknown"))
            }
        };

        let source_id = event.source.as_ref()
            .and_then(|s| s.user_id.clone())
            .unwrap_or_default();

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key: clawdesk_types::session::SessionKey::new(
                ChannelId::Line,
                &source_id,
            ),
            body: text,
            body_for_agent: None,
            sender: SenderIdentity {
                id: source_id.clone(),
                display_name: String::new(),
                channel: ChannelId::Line,
            },
            media: Vec::new(),
            artifact_refs: Vec::new(),
            reply_context: None,
            origin: MessageOrigin::Line {
                user_id: source_id,
                reply_token: event.reply_token.clone(),
            },
            timestamp: chrono::Utc::now(),
        })
    }

    /// Handle inbound webhook from Line platform.
    pub async fn handle_webhook(&self, body: &str) -> Result<(), String> {
        let payload: LineWebhookPayload =
            serde_json::from_str(body).map_err(|e| format!("Invalid webhook: {e}"))?;
        let sink_guard = self.sink.lock().await;
        let sink = sink_guard.as_ref().ok_or("No sink registered")?;

        for event in &payload.events {
            if let Some(ref token) = event.reply_token {
                if let Some(ref source) = event.source {
                    if let Some(ref uid) = source.user_id {
                        self.cache_reply_token(uid, token.clone()).await;
                    }
                }
            }
            if let Some(msg) = Self::normalize_event(event) {
                sink.on_message(msg).await;
            }
        }
        Ok(())
    }

    async fn reply(&self, reply_token: &str, messages: Vec<serde_json::Value>) -> Result<(), String> {
        let resp = self.client
            .post(&Self::api_url("/message/reply"))
            .bearer_auth(&self.channel_access_token)
            .json(&serde_json::json!({ "replyToken": reply_token, "messages": messages }))
            .send().await
            .map_err(|e| format!("Line reply failed: {e}"))?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Line reply error: {body}"));
        }
        Ok(())
    }

    async fn push(&self, to: &str, messages: Vec<serde_json::Value>) -> Result<(), String> {
        let resp = self.client
            .post(&Self::api_url("/message/push"))
            .bearer_auth(&self.channel_access_token)
            .json(&serde_json::json!({ "to": to, "messages": messages }))
            .send().await
            .map_err(|e| format!("Line push failed: {e}"))?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Line push error: {body}"));
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for LineChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Line
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Line".into(),
            supports_threading: false,
            supports_streaming: true,
            supports_reactions: false,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(5000),
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);
        *self.sink.lock().await = Some(sink);
        info!("Line channel started (webhook mode)");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let text_msg = serde_json::json!({"type": "text", "text": msg.body});
        let messages = vec![text_msg];

        // Try reply token first, then push
        if let MessageOrigin::Line { ref user_id, ref reply_token } = msg.origin {
            if let Some(ref rt) = reply_token {
                if self.get_reply_token(user_id).await.is_some() {
                    self.reply(rt, messages.clone()).await.ok();
                    return Ok(DeliveryReceipt {
                        channel: ChannelId::Line,
                        message_id: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        success: true,
                        error: None,
                    });
                }
            }
            self.push(user_id, messages).await?;
        }

        Ok(DeliveryReceipt {
            channel: ChannelId::Line,
            message_id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Line channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait]
impl Streaming for LineChannel {
    async fn send_streaming(&self, msg: OutboundMessage) -> Result<StreamHandle, String> {
        self.send(msg).await?;
        Ok(StreamHandle {
            message_id: uuid::Uuid::new_v4().to_string(),
            update_fn: Box::new(|_| Ok(())),
        })
    }
}

// ─── Line API Types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LineWebhookPayload {
    events: Vec<LineEvent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineEvent {
    #[serde(rename = "type")]
    event_type: LineEventType,
    reply_token: Option<String>,
    source: Option<LineSource>,
    message: Option<LineMessage>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum LineEventType {
    Message,
    Follow,
    Unfollow,
    Join,
    Leave,
    Postback,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineSource {
    #[serde(rename = "type")]
    source_type: String,
    user_id: Option<String>,
    group_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum LineMessage {
    Text { id: String, text: String },
    Image { id: String },
    Video { id: String },
    Audio { id: String },
    Sticker { id: String, #[serde(rename = "packageId")] package_id: String, #[serde(rename = "stickerId")] sticker_id: String },
    Location { id: String, title: Option<String>, address: Option<String>, latitude: f64, longitude: f64 },
    File { id: String, #[serde(rename = "fileName")] file_name: Option<String> },
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_token_ttl() {
        let rt = ReplyToken { token: "t".into(), received_at: Instant::now() };
        assert!(rt.is_valid());
        let expired = ReplyToken { token: "t".into(), received_at: Instant::now() - Duration::from_secs(56) };
        assert!(!expired.is_valid());
    }

    #[test]
    fn normalize_text_message() {
        let event = LineEvent {
            event_type: LineEventType::Message,
            reply_token: Some("r123".into()),
            source: Some(LineSource { source_type: "user".into(), user_id: Some("U1".into()), group_id: None }),
            message: Some(LineMessage::Text { id: "m1".into(), text: "Hello!".into() }),
        };
        let msg = LineChannel::normalize_event(&event).unwrap();
        assert_eq!(msg.body, "Hello!");
        assert_eq!(msg.sender.id, "U1");
    }

    #[test]
    fn unknown_event_ignored() {
        let event = LineEvent {
            event_type: LineEventType::Follow,
            reply_token: None,
            source: None,
            message: None,
        };
        assert!(LineChannel::normalize_event(&event).is_none());
    }
}
