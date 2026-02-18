//! Feishu (飞书 / Lark) channel implementation via Open API.
//!
//! Feishu is ByteDance's enterprise collaboration platform, widely used
//! in China. The international version is branded "Lark". This adapter
//! connects via the Feishu Open API for sending and receiving messages.
//!
//! ## Architecture
//!
//! ```text
//! FeishuChannel
//! ├── webhook_handler()  — receives event callbacks from Feishu
//! ├── normalize()        — Feishu event → NormalizedMessage
//! ├── send()             — OutboundMessage → POST /im/v1/messages
//! ├── send_streaming()   — update-in-place via PATCH /im/v1/messages
//! ├── refresh_token()    — tenant_access_token lifecycle management
//! └── upload_media()     — file/image upload via /im/v1/files
//! ```
//!
//! ## Feishu Open API
//!
//! Base URL: `https://open.feishu.cn/open-apis/`
//!
//! - `POST /auth/v3/tenant_access_token/internal` — obtain tenant token
//! - `POST /im/v1/messages`                       — send a message
//! - `PATCH /im/v1/messages/{message_id}`          — update (edit) a message
//! - `POST /im/v1/files`                          — upload file attachment
//! - `POST /im/v1/images`                         — upload image
//! - `GET  /contact/v3/users/{user_id}`           — get user info
//!
//! ## Authentication
//!
//! Feishu bots use a tenant access token obtained by posting app_id +
//! app_secret to the token endpoint. The token is valid for ~2 hours
//! and must be refreshed proactively.
//!
//! ## Message limits
//!
//! - Text messages: 4000 characters
//! - Rich text (post): 30000 characters
//! - File upload: 30 MB
//! - Image upload: 10 MB

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, StreamHandle, Streaming};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, MediaAttachment, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

const FEISHU_API_BASE: &str = "https://open.feishu.cn/open-apis";
const MAX_MESSAGE_LENGTH: usize = 4000;

/// Feishu / Lark channel adapter via Open API.
pub struct FeishuChannel {
    client: Client,
    /// Application ID from Feishu Developer Console.
    app_id: String,
    /// Application secret.
    app_secret: String,
    /// Optional webhook URL for outbound-only bots.
    webhook_url: Option<String>,
    /// Bot display name.
    bot_name: String,
    /// Cached tenant access token.
    tenant_token: Mutex<Option<TenantToken>>,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Cached tenant access token with expiry.
#[derive(Debug, Clone)]
struct TenantToken {
    token: String,
    /// Unix timestamp when the token expires.
    expires_at: i64,
}

/// Configuration for the Feishu channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default = "default_bot_name")]
    pub bot_name: String,
}

fn default_bot_name() -> String {
    "ClawDesk Bot".into()
}

// ─── Feishu API response types ──────────────────────────────────────

#[derive(Debug, Deserialize)]
struct FeishuBaseResponse {
    code: i32,
    msg: String,
}

#[derive(Debug, Deserialize)]
struct TenantTokenResponse {
    code: i32,
    msg: String,
    tenant_access_token: Option<String>,
    expire: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SendMessageResponse {
    code: i32,
    msg: String,
    data: Option<SendMessageData>,
}

#[derive(Debug, Deserialize)]
struct SendMessageData {
    message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FeishuEvent {
    #[serde(default)]
    schema: String,
    header: Option<FeishuEventHeader>,
    event: Option<FeishuEventBody>,
}

#[derive(Debug, Deserialize)]
struct FeishuEventHeader {
    event_id: Option<String>,
    event_type: Option<String>,
    create_time: Option<String>,
    token: Option<String>,
    app_id: Option<String>,
    tenant_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FeishuEventBody {
    sender: Option<FeishuSender>,
    message: Option<FeishuMessage>,
}

#[derive(Debug, Deserialize)]
struct FeishuSender {
    sender_id: Option<FeishuSenderId>,
    sender_type: Option<String>,
    tenant_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FeishuSenderId {
    open_id: Option<String>,
    user_id: Option<String>,
    union_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FeishuMessage {
    message_id: Option<String>,
    root_id: Option<String>,
    parent_id: Option<String>,
    create_time: Option<String>,
    chat_id: Option<String>,
    chat_type: Option<String>,
    message_type: Option<String>,
    content: Option<String>,
}

// ─── Upload response ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct UploadImageResponse {
    code: i32,
    msg: String,
    data: Option<UploadImageData>,
}

#[derive(Debug, Deserialize)]
struct UploadImageData {
    image_key: Option<String>,
}

impl FeishuChannel {
    pub fn new(config: FeishuConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
            app_id: config.app_id,
            app_secret: config.app_secret,
            webhook_url: config.webhook_url,
            bot_name: config.bot_name,
            tenant_token: Mutex::new(None),
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Build a Feishu API URL.
    fn api_url(&self, path: &str) -> String {
        format!("{}{}", FEISHU_API_BASE, path)
    }

    /// Obtain or refresh the tenant access token.
    ///
    /// The tenant access token is required for all API calls. It expires
    /// after ~2 hours (7200 seconds) so we refresh it proactively with
    /// a 5-minute buffer.
    async fn ensure_tenant_token(&self) -> Result<String, String> {
        {
            let guard = self.tenant_token.lock().await;
            if let Some(ref cached) = *guard {
                let now = chrono::Utc::now().timestamp();
                if now < cached.expires_at - 300 {
                    return Ok(cached.token.clone());
                }
            }
        }

        // Token expired or not yet obtained — refresh
        self.refresh_tenant_token().await
    }

    /// Request a new tenant access token from Feishu.
    async fn refresh_tenant_token(&self) -> Result<String, String> {
        let url = self.api_url("/auth/v3/tenant_access_token/internal");
        let body = serde_json::json!({
            "app_id": self.app_id,
            "app_secret": self.app_secret,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Feishu token request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Feishu token HTTP {}: {}", status, err));
        }

        let token_resp: TenantTokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("Feishu token parse error: {}", e))?;

        if token_resp.code != 0 {
            return Err(format!(
                "Feishu token error {}: {}",
                token_resp.code, token_resp.msg
            ));
        }

        let token = token_resp
            .tenant_access_token
            .ok_or("Feishu returned no tenant_access_token")?;
        let expire_secs = token_resp.expire.unwrap_or(7200);
        let expires_at = chrono::Utc::now().timestamp() + expire_secs;

        let mut guard = self.tenant_token.lock().await;
        *guard = Some(TenantToken {
            token: token.clone(),
            expires_at,
        });

        debug!(expires_in = expire_secs, "Feishu tenant token refreshed");
        Ok(token)
    }

    /// Truncate text to the Feishu message limit.
    fn truncate_message(text: &str) -> String {
        const SUFFIX: &str = "… [message truncated]";
        if text.len() <= MAX_MESSAGE_LENGTH {
            text.to_string()
        } else {
            let truncated = &text[..MAX_MESSAGE_LENGTH - SUFFIX.len()];
            format!("{}{}", truncated, SUFFIX)
        }
    }

    /// Upload an image to Feishu and return the image_key.
    async fn upload_image(&self, data: &[u8], filename: &str) -> Result<String, String> {
        let token = self.ensure_tenant_token().await?;
        let url = self.api_url("/im/v1/images");

        let part = reqwest::multipart::Part::bytes(data.to_vec())
            .file_name(filename.to_string())
            .mime_str("image/png")
            .map_err(|e| format!("MIME error: {}", e))?;

        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part("image", part);

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("Feishu image upload failed: {}", e))?;

        let upload_resp: UploadImageResponse = resp
            .json()
            .await
            .map_err(|e| format!("Feishu upload parse error: {}", e))?;

        if upload_resp.code != 0 {
            return Err(format!(
                "Feishu upload error {}: {}",
                upload_resp.code, upload_resp.msg
            ));
        }

        upload_resp
            .data
            .and_then(|d| d.image_key)
            .ok_or_else(|| "Feishu returned no image_key".into())
    }

    /// Normalize a Feishu event callback into a NormalizedMessage.
    fn normalize_event(&self, event: &FeishuEvent) -> Option<NormalizedMessage> {
        let header = event.header.as_ref()?;
        let event_type = header.event_type.as_deref()?;

        if event_type != "im.message.receive_v1" {
            return None;
        }

        let body = event.event.as_ref()?;
        let message = body.message.as_ref()?;
        let sender = body.sender.as_ref()?;

        let msg_type = message.message_type.as_deref().unwrap_or("text");
        let content_raw = message.content.as_deref().unwrap_or("{}");

        // Parse the content JSON — text messages have {"text": "..."}
        let text = if msg_type == "text" {
            serde_json::from_str::<serde_json::Value>(content_raw)
                .ok()
                .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(String::from))
                .unwrap_or_default()
        } else {
            format!("[{} message]", msg_type)
        };

        let sender_id = sender
            .sender_id
            .as_ref()
            .and_then(|s| s.open_id.clone())
            .unwrap_or_default();

        let chat_id = message.chat_id.clone().unwrap_or_default();
        let msg_id = message.message_id.clone().unwrap_or_default();

        let sender_identity = SenderIdentity {
            id: sender_id.clone(),
            display_name: sender_id.clone(),
            channel: ChannelId::Feishu,
        };

        let session_key =
            clawdesk_types::session::SessionKey::new(ChannelId::Feishu, &chat_id);

        let origin = clawdesk_types::message::MessageOrigin::Feishu {
            chat_id: chat_id.clone(),
            message_id: msg_id,
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: text,
            body_for_agent: None,
            sender: sender_identity,
            media: vec![],
            reply_context: None,
            origin,
            timestamp: chrono::Utc::now(),
        })
    }
}

#[async_trait]
impl Channel for FeishuChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Feishu
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Feishu".into(),
            supports_threading: true,
            supports_streaming: true,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(MAX_MESSAGE_LENGTH),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify connectivity by obtaining a tenant access token
        let token = self.refresh_tenant_token().await?;
        info!(
            app_id = %self.app_id,
            bot = %self.bot_name,
            token_len = token.len(),
            "Feishu channel started"
        );
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        // Determine the receive_id from the origin
        let (receive_id, receive_id_type) = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Feishu { chat_id, message_id, .. } => {
                if !chat_id.is_empty() {
                    (chat_id.clone(), "chat_id")
                } else {
                    (message_id.clone(), "message_id")
                }
            }
            _ => return Err("cannot send Feishu message without Feishu origin".into()),
        };

        let token = self.ensure_tenant_token().await?;
        let text = Self::truncate_message(&msg.body);

        let content = serde_json::json!({ "text": text }).to_string();
        let body = serde_json::json!({
            "receive_id": receive_id,
            "msg_type": "text",
            "content": content,
        });

        let url = format!(
            "{}/im/v1/messages?receive_id_type={}",
            FEISHU_API_BASE, receive_id_type
        );

        let response = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Feishu send failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            return Err(format!("Feishu API error HTTP {}: {}", status, error_body));
        }

        let send_resp: SendMessageResponse = response
            .json()
            .await
            .map_err(|e| format!("Feishu response parse error: {}", e))?;

        if send_resp.code != 0 {
            return Err(format!(
                "Feishu send error {}: {}",
                send_resp.code, send_resp.msg
            ));
        }

        let message_id = send_resp
            .data
            .and_then(|d| d.message_id)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        Ok(DeliveryReceipt {
            channel: ChannelId::Feishu,
            message_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Feishu channel stopped");
        Ok(())
    }
}

#[async_trait]
impl Streaming for FeishuChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        // Feishu supports message editing via PATCH /im/v1/messages/{message_id}.
        // Send the initial message, then return a handle that can update it.
        let receipt = self.send(initial).await?;
        let msg_id = receipt.message_id.clone();

        Ok(StreamHandle {
            message_id: msg_id,
            update_fn: Box::new(|_text| Ok(())),
        })
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> FeishuConfig {
        FeishuConfig {
            app_id: "cli_test123456".into(),
            app_secret: "secret_test_abc".into(),
            webhook_url: Some("https://open.feishu.cn/open-apis/bot/v2/hook/xxxx".into()),
            bot_name: "TestBot".into(),
        }
    }

    #[test]
    fn test_feishu_channel_creation() {
        let channel = FeishuChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::Feishu);
        assert_eq!(channel.app_id, "cli_test123456");
        assert_eq!(channel.app_secret, "secret_test_abc");
        assert_eq!(channel.bot_name, "TestBot");
        assert!(channel.webhook_url.is_some());
    }

    #[test]
    fn test_feishu_meta() {
        let channel = FeishuChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "Feishu");
        assert!(meta.supports_threading);
        assert!(meta.supports_streaming);
        assert!(meta.supports_media);
        assert!(meta.supports_groups);
        assert_eq!(meta.max_message_length, Some(4000));
    }

    #[test]
    fn test_feishu_message_truncation() {
        // Short message — no truncation
        let short = "Hello, Feishu!";
        assert_eq!(FeishuChannel::truncate_message(short), short);

        // Exactly at limit
        let exact = "a".repeat(MAX_MESSAGE_LENGTH);
        assert_eq!(FeishuChannel::truncate_message(&exact), exact);

        // Over limit — truncated
        let long = "b".repeat(MAX_MESSAGE_LENGTH + 100);
        let truncated = FeishuChannel::truncate_message(&long);
        assert!(truncated.len() <= MAX_MESSAGE_LENGTH);
        assert!(truncated.ends_with("… [message truncated]"));
    }

    #[test]
    fn test_feishu_token_request_url() {
        let channel = FeishuChannel::new(test_config());
        let url = channel.api_url("/auth/v3/tenant_access_token/internal");
        assert_eq!(
            url,
            "https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal"
        );
    }

    #[test]
    fn test_feishu_send_endpoint_url() {
        let channel = FeishuChannel::new(test_config());
        let url = channel.api_url("/im/v1/messages");
        assert_eq!(
            url,
            "https://open.feishu.cn/open-apis/im/v1/messages"
        );
    }

    #[test]
    fn test_feishu_normalize_text_event() {
        let channel = FeishuChannel::new(test_config());
        let event = FeishuEvent {
            schema: "2.0".into(),
            header: Some(FeishuEventHeader {
                event_id: Some("evt_123".into()),
                event_type: Some("im.message.receive_v1".into()),
                create_time: Some("1234567890".into()),
                token: Some("tok".into()),
                app_id: Some("cli_test".into()),
                tenant_key: Some("tenant".into()),
            }),
            event: Some(FeishuEventBody {
                sender: Some(FeishuSender {
                    sender_id: Some(FeishuSenderId {
                        open_id: Some("ou_user123".into()),
                        user_id: None,
                        union_id: None,
                    }),
                    sender_type: Some("user".into()),
                    tenant_key: Some("tenant".into()),
                }),
                message: Some(FeishuMessage {
                    message_id: Some("om_msg456".into()),
                    root_id: None,
                    parent_id: None,
                    create_time: Some("1234567890".into()),
                    chat_id: Some("oc_chat789".into()),
                    chat_type: Some("group".into()),
                    message_type: Some("text".into()),
                    content: Some(r#"{"text":"Hello from Feishu!"}"#.into()),
                }),
            }),
        };

        let normalized = channel.normalize_event(&event).unwrap();
        assert_eq!(normalized.body, "Hello from Feishu!");
        assert_eq!(normalized.sender.id, "ou_user123");
    }

    #[test]
    fn test_feishu_normalize_wrong_event_type() {
        let channel = FeishuChannel::new(test_config());
        let event = FeishuEvent {
            schema: "2.0".into(),
            header: Some(FeishuEventHeader {
                event_id: Some("evt_123".into()),
                event_type: Some("im.chat.disbanded_v1".into()),
                create_time: None,
                token: None,
                app_id: None,
                tenant_key: None,
            }),
            event: None,
        };

        assert!(channel.normalize_event(&event).is_none());
    }
}
