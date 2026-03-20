//! Telegram Bot API channel implementation.
//!
//! Uses the Telegram Bot API via long-polling (`getUpdates`).
//! Implements `Channel` + `Streaming` + `Reactions`.
//!
//! ## Architecture
//!
//! ```text
//! TelegramChannel
//! ├── poll_loop()     — long-polls getUpdates; spawns as tokio task
//! ├── normalize()     — telegram Update → NormalizedMessage
//! ├── send()          — OutboundMessage → sendMessage API call
//! └── send_streaming()— edit-in-place for streaming responses
//! ```
//!
//! ## Rate limits
//!
//! Telegram enforces:
//! - 30 messages/second to different chats
//! - 1 message/second per chat (soft limit)
//! - 4096 char max for text messages
//! - 20 messages/minute per group
//!
//! The channel respects these via the `ChannelRateLimiter` from
//! `clawdesk-channel::rate_limit`.

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Reactions, StreamHandle, Streaming};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, MediaAttachment, MessageOrigin, NormalizedMessage, OutboundMessage,
    SenderIdentity,
};
use reqwest::Client;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Telegram Bot API channel.
pub struct TelegramChannel {
    client: Client,
    bot_token: String,
    /// Allowed chat IDs. Empty = allow all.
    allowed_chat_ids: Vec<i64>,
    /// Enable group message handling.
    enable_groups: bool,
    /// Last processed update offset for long-polling.
    offset: AtomicI64,
    /// Shutdown signal (shared with poll loop so stop() propagates).
    running: Arc<AtomicBool>,
    /// Shutdown notifier.
    shutdown: Notify,
    /// Auto-discovered chat ID from `getUpdates` probe at startup.
    /// Populated when `allowed_chat_ids` is empty and the bot receives
    /// or has pending messages. Used by `default_origin()` as fallback.
    discovered_chat_id: AtomicI64,
}

impl TelegramChannel {
    pub fn new(bot_token: String, allowed_chat_ids: Vec<i64>, enable_groups: bool) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("failed to build HTTP client"),
            bot_token,
            allowed_chat_ids,
            enable_groups,
            offset: AtomicI64::new(0),
            running: Arc::new(AtomicBool::new(false)),
            shutdown: Notify::new(),
            discovered_chat_id: AtomicI64::new(0),
        }
    }

    /// Telegram Bot API URL.
    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.bot_token, method)
    }

    /// Check if a chat ID is allowed.
    fn is_allowed(&self, chat_id: i64) -> bool {
        self.allowed_chat_ids.is_empty() || self.allowed_chat_ids.contains(&chat_id)
    }

    /// Long-poll loop: fetches updates and dispatches to the message sink.
    async fn poll_loop(self: Arc<Self>, sink: Arc<dyn MessageSink>) {
        info!("Telegram poll loop started");

        while self.running.load(Ordering::Relaxed) {
            let offset = self.offset.load(Ordering::Relaxed);
            let url = self.api_url("getUpdates");

            let result = self
                .client
                .post(&url)
                .json(&serde_json::json!({
                    "offset": offset,
                    "timeout": 30,
                    "allowed_updates": ["message", "edited_message", "callback_query", "poll_answer", "message_reaction"]
                }))
                .send()
                .await;

            match result {
                Ok(response) => {
                    if let Ok(body) = response.json::<TelegramResponse<Vec<TelegramUpdate>>>().await
                    {
                        if body.ok {
                            for update in body.result.unwrap_or_default() {
                                self.offset
                                    .store(update.update_id + 1, Ordering::Relaxed);

                                if let Some(msg) = update.message {
                                    let chat_id = msg.chat.id;

                                    // Auto-discover: update discovered_chat_id from
                                    // the first message we see (enables default_origin).
                                    if self.discovered_chat_id.load(Ordering::Relaxed) == 0 {
                                        self.discovered_chat_id.store(chat_id, Ordering::Relaxed);
                                        info!(chat_id, "Telegram: auto-discovered chat target from inbound message");
                                    }

                                    // Filter by allowed chats
                                    if !self.is_allowed(chat_id) {
                                        debug!(chat_id, "ignoring message from unallowed chat");
                                        continue;
                                    }

                                    // Filter groups if disabled
                                    let is_group = msg.chat.chat_type == "group"
                                        || msg.chat.chat_type == "supergroup";
                                    if is_group && !self.enable_groups {
                                        continue;
                                    }

                                    // Normalize and dispatch
                                    if let Some(normalized) = self.normalize_update(&msg).await {
                                        sink.on_message(normalized).await;
                                    }
                                }

                                // Handle callback query (inline button presses)
                                if let Some(cb) = update.callback_query {
                                    if let (Some(msg), Some(data)) = (&cb.message, &cb.data) {
                                        let chat_id = msg.chat.id;
                                        if self.is_allowed(chat_id) {
                                            let sender = SenderIdentity {
                                                id: cb.from.id.to_string(),
                                                display_name: cb.from.first_name.clone(),
                                                channel: ChannelId::Telegram,
                                            };
                                            let session_key = clawdesk_types::session::SessionKey::new(
                                                ChannelId::Telegram,
                                                &chat_id.to_string(),
                                            );
                                            let origin = clawdesk_types::message::MessageOrigin::Telegram {
                                                chat_id,
                                                message_id: msg.message_id,
                                                thread_id: msg.message_thread_id,
                                            };
                                            let normalized = NormalizedMessage {
                                                id: uuid::Uuid::new_v4(),
                                                session_key,
                                                body: format!("[callback:{}]", data),
                                                body_for_agent: Some(format!(
                                                    "User pressed inline button with callback data: {}",
                                                    data,
                                                )),
                                                sender,
                                                media: vec![],
                                                artifact_refs: vec![],
                                                reply_context: None,
                                                origin,
                                                timestamp: chrono::Utc::now(),
                                            };
                                            sink.on_message(normalized).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Telegram poll error, retrying in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }

        info!("Telegram poll loop stopped");
    }

    /// Download a file from Telegram using the 2-step getFile → download flow.
    async fn download_telegram_file(&self, file_id: &str) -> Option<Vec<u8>> {
        // Step 1: getFile → file_path
        let url = self.api_url("getFile");
        let resp = self.client
            .post(&url)
            .json(&serde_json::json!({ "file_id": file_id }))
            .send()
            .await
            .ok()?;
        let body: TelegramResponse<TgFileResponse> = resp.json().await.ok()?;
        let file_path = body.result?.file_path?;

        // Step 2: Download bytes from file API
        let download_url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.bot_token, file_path
        );
        let bytes = self.client
            .get(&download_url)
            .send()
            .await
            .ok()?
            .bytes()
            .await
            .ok()?;

        Some(bytes.to_vec())
    }

    /// Normalize a Telegram message to the canonical form.
    /// Downloads attached media (photos, audio, documents) from Telegram's API.
    async fn normalize_update(&self, msg: &TgMessage) -> Option<NormalizedMessage> {
        // Accept messages that have text OR caption (photos use caption, not text)
        let text = msg.text.clone().or_else(|| msg.caption.clone())?;
        let from = msg.from.as_ref()?;

        let sender = SenderIdentity {
            id: from.id.to_string(),
            display_name: from.first_name.clone(),
            channel: ChannelId::Telegram,
        };

        let session_key =
            clawdesk_types::session::SessionKey::new(ChannelId::Telegram, &msg.chat.id.to_string());

        // Process media attachments — download actual bytes from Telegram API
        let mut media = Vec::new();

        // Photos: pick the largest resolution, download via getFile
        if let Some(photos) = &msg.photo {
            if let Some(photo) = photos.last() {
                let data = self.download_telegram_file(&photo.file_id).await;
                if data.is_some() {
                    info!(
                        file_id = %photo.file_id,
                        size = ?data.as_ref().map(|d| d.len()),
                        "Telegram: downloaded photo attachment"
                    );
                }
                media.push(MediaAttachment {
                    media_type: clawdesk_types::message::MediaType::Image,
                    url: None,
                    data,
                    mime_type: "image/jpeg".into(),
                    filename: None,
                    size_bytes: photo.file_size,
                });
            }
        }

        // Voice messages
        if let Some(voice) = &msg.voice {
            let data = self.download_telegram_file(&voice.file_id).await;
            media.push(MediaAttachment {
                media_type: clawdesk_types::message::MediaType::Voice,
                url: None,
                data,
                mime_type: voice.mime_type.clone().unwrap_or_else(|| "audio/ogg".into()),
                filename: voice.file_name.clone(),
                size_bytes: voice.file_size,
            });
        }

        // Audio files
        if let Some(audio) = &msg.audio {
            let data = self.download_telegram_file(&audio.file_id).await;
            media.push(MediaAttachment {
                media_type: clawdesk_types::message::MediaType::Audio,
                url: None,
                data,
                mime_type: audio.mime_type.clone().unwrap_or_else(|| "audio/mpeg".into()),
                filename: audio.file_name.clone(),
                size_bytes: audio.file_size,
            });
        }

        // Documents (PDF, etc.)
        if let Some(doc) = &msg.document {
            let data = self.download_telegram_file(&doc.file_id).await;
            media.push(MediaAttachment {
                media_type: clawdesk_types::message::MediaType::Document,
                url: None,
                data,
                mime_type: doc.mime_type.clone().unwrap_or_else(|| "application/octet-stream".into()),
                filename: doc.file_name.clone(),
                size_bytes: doc.file_size,
            });
        }

        let origin = clawdesk_types::message::MessageOrigin::Telegram {
            chat_id: msg.chat.id,
            message_id: msg.message_id,
            thread_id: msg.message_thread_id,
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: text,
            body_for_agent: None,
            sender,
            media,
            artifact_refs: vec![],
            reply_context: None,
            origin,
            timestamp: chrono::Utc::now(),
        })
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Telegram
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Telegram".into(),
            supports_threading: true,
            supports_streaming: true,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: Some(4096),
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Verify bot token
        let me_url = self.api_url("getMe");
        let resp = self
            .client
            .get(&me_url)
            .send()
            .await
            .map_err(|e| format!("Telegram auth check failed: {}", e))?;

        if !resp.status().is_success() {
            return Err("Telegram bot token is invalid".into());
        }

        let me: TelegramResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| format!("failed to parse getMe: {}", e))?;

        if let Some(result) = me.result {
            info!(
                bot = result.get("username").and_then(|v| v.as_str()).unwrap_or("unknown"),
                "Telegram bot verified"
            );
        }

        // ── Auto-discover default chat target ─────────────────────────
        // When allowed_chat_ids is empty (allow-all mode), probe the
        // Telegram API for any pending or recent update so we have a
        // chat_id to use as the default send target. This eliminates
        // the cold-start problem where "send a message to telegram"
        // fails because no target is known yet.
        if self.allowed_chat_ids.is_empty() {
            let probe_url = self.api_url("getUpdates");
            let probe_result = self.client
                .post(&probe_url)
                .json(&serde_json::json!({
                    "limit": 1,
                    "timeout": 0,
                    "allowed_updates": ["message"]
                }))
                .send()
                .await;

            if let Ok(resp) = probe_result {
                if let Ok(body) = resp.json::<TelegramResponse<Vec<TelegramUpdate>>>().await {
                    if body.ok {
                        if let Some(update) = body.result.as_ref().and_then(|u| u.first()) {
                            if let Some(msg) = &update.message {
                                let chat_id = msg.chat.id;
                                self.discovered_chat_id.store(chat_id, Ordering::Relaxed);
                                info!(
                                    chat_id,
                                    "Telegram: auto-discovered default chat target from pending update"
                                );
                            }
                        }
                    }
                }
            }
        }

        // Spawn the long-polling loop on a background task.
        // Share the running flag so stop() can signal the poll loop.
        let poll_channel = Arc::new(TelegramChannel {
            client: Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("failed to build HTTP client"),
            bot_token: self.bot_token.clone(),
            allowed_chat_ids: self.allowed_chat_ids.clone(),
            enable_groups: self.enable_groups,
            offset: AtomicI64::new(0),
            running: self.running.clone(),
            shutdown: Notify::new(),
            discovered_chat_id: AtomicI64::new(
                self.discovered_chat_id.load(Ordering::Relaxed),
            ),
        });

        tokio::spawn(async move {
            poll_channel.poll_loop(sink).await;
        });

        info!("Telegram channel started — poll loop spawned");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let (chat_id, _reply_to) = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Telegram {
                chat_id,
                message_id,
                ..
            } => (*chat_id, Some(*message_id)),
            _ => return Err("cannot send Telegram message without Telegram origin".into()),
        };

        // Send media attachments first (R5 wiring: screenshots reach users)
        for attachment in &msg.media {
            match attachment.media_type {
                clawdesk_types::message::MediaType::Image => {
                    self.send_photo(chat_id, attachment, msg.reply_to.as_deref()).await?;
                }
                clawdesk_types::message::MediaType::Voice | clawdesk_types::message::MediaType::Audio => {
                    self.send_voice_or_audio(chat_id, attachment, msg.reply_to.as_deref()).await?;
                }
                _ => {
                    self.send_document_attachment(chat_id, attachment, msg.reply_to.as_deref()).await?;
                }
            }
        }

        // Skip text if only media with empty body
        if msg.body.trim().is_empty() && !msg.media.is_empty() {
            return Ok(DeliveryReceipt {
                channel: ChannelId::Telegram,
                message_id: String::new(),
                timestamp: chrono::Utc::now(),
                success: true,
                error: None,
            });
        }

        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": msg.body,
            "parse_mode": "Markdown",
        });

        // Set reply_to for threaded conversations
        if let Some(reply_to_id) = msg.reply_to.as_ref() {
            body["reply_to_message_id"] = serde_json::json!(reply_to_id);
        }

        let url = self.api_url("sendMessage");
        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Telegram send failed: {}", e))?;

        if !response.status().is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(format!("Telegram API error: {}", error_body));
        }

        let result: TelegramResponse<TgMessage> = response
            .json()
            .await
            .map_err(|e| format!("failed to parse send response: {}", e))?;

        let message_id = result
            .result
            .map(|m| m.message_id.to_string())
            .unwrap_or_default();

        Ok(DeliveryReceipt {
            channel: ChannelId::Telegram,
            message_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Telegram channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn default_origin(&self) -> Option<MessageOrigin> {
        // Priority: explicit allowed_chat_ids[0] → auto-discovered chat_id
        let chat_id = self.allowed_chat_ids.first().copied()
            .or_else(|| {
                let discovered = self.discovered_chat_id.load(Ordering::Relaxed);
                if discovered != 0 { Some(discovered) } else { None }
            })?;
        Some(MessageOrigin::Telegram {
            chat_id,
            message_id: 0,
            thread_id: None,
        })
    }
}

#[async_trait]
impl Streaming for TelegramChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        // Extract chat_id before consuming initial in send()
        let chat_id = match &initial.origin {
            clawdesk_types::message::MessageOrigin::Telegram { chat_id, .. } => *chat_id,
            _ => return Err("streaming requires Telegram origin with chat_id".into()),
        };

        // Send initial placeholder message
        let receipt = self.send(initial).await?;
        let msg_id = receipt.message_id.clone();

        let client = self.client.clone();
        let bot_token = self.bot_token.clone();
        let msg_id_for_edit = msg_id.clone();

        Ok(StreamHandle {
            message_id: msg_id,
            update_fn: Box::new(move |text: &str| {
                let url = format!(
                    "https://api.telegram.org/bot{}/editMessageText",
                    bot_token
                );
                let body = serde_json::json!({
                    "chat_id": chat_id,
                    "message_id": msg_id_for_edit.parse::<i64>().unwrap_or(0),
                    "text": text,
                    "parse_mode": "Markdown",
                });
                // Fire-and-forget edit (Telegram rate-limits edits to 1/sec per message;
                // the caller should debounce at 1Hz)
                let client = client.clone();
                tokio::spawn(async move {
                    let _ = client.post(&url).json(&body).send().await;
                });
                Ok(())
            }),
        })
    }
}

#[async_trait]
impl Reactions for TelegramChannel {
    async fn add_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        // msg_id format: "chat_id:message_id"
        let parts: Vec<&str> = msg_id.splitn(2, ':').collect();
        let (chat_id, message_id) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            return Err("msg_id must be chat_id:message_id format".into());
        };

        let url = self.api_url("setMessageReaction");
        let body = serde_json::json!({
            "chat_id": chat_id.parse::<i64>().unwrap_or(0),
            "message_id": message_id.parse::<i64>().unwrap_or(0),
            "reaction": [{ "type": "emoji", "emoji": emoji }],
        });

        let resp = self.client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Telegram setMessageReaction failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Telegram reaction failed ({status}): {err}"));
        }

        debug!(msg_id, emoji, "Telegram reaction added");
        Ok(())
    }

    async fn remove_reaction(&self, msg_id: &str, _emoji: &str) -> Result<(), String> {
        let parts: Vec<&str> = msg_id.splitn(2, ':').collect();
        let (chat_id, message_id) = if parts.len() == 2 {
            (parts[0], parts[1])
        } else {
            return Err("msg_id must be chat_id:message_id format".into());
        };

        let url = self.api_url("setMessageReaction");
        let body = serde_json::json!({
            "chat_id": chat_id.parse::<i64>().unwrap_or(0),
            "message_id": message_id.parse::<i64>().unwrap_or(0),
            "reaction": [],
        });

        let resp = self.client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Telegram removeReaction failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Telegram remove reaction failed ({status}): {err}"));
        }

        debug!(msg_id, "Telegram reaction removed");
        Ok(())
    }
}

// ─── Telegram API types ─────────────────────────────────────────────

// ─── Telegram Rich Actions ──────────────────────────────────────────
//
// Agent-accessible Telegram actions beyond basic sendMessage.
// Each action maps to a Telegram Bot API method.

impl TelegramChannel {
    /// Send a message with inline keyboard buttons.
    ///
    /// `buttons` is a grid: outer Vec = rows, inner Vec = buttons per row.
    /// Each button is `(label, callback_data)`.
    pub async fn send_with_buttons(
        &self,
        chat_id: i64,
        text: &str,
        buttons: Vec<Vec<(String, String)>>,
        reply_to: Option<i64>,
    ) -> Result<String, String> {
        let keyboard: Vec<Vec<serde_json::Value>> = buttons
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|(label, data)| {
                        serde_json::json!({ "text": label, "callback_data": data })
                    })
                    .collect()
            })
            .collect();

        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "Markdown",
            "reply_markup": { "inline_keyboard": keyboard },
        });
        if let Some(r) = reply_to {
            body["reply_to_message_id"] = serde_json::json!(r);
        }

        let resp = self.client
            .post(&self.api_url("sendMessage"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Telegram sendMessage+buttons failed: {e}"))?;
        self.parse_message_id(resp).await
    }

    /// Create a poll.
    pub async fn send_poll(
        &self,
        chat_id: i64,
        question: &str,
        options: Vec<String>,
        allows_multiple: bool,
        is_anonymous: bool,
    ) -> Result<String, String> {
        let body = serde_json::json!({
            "chat_id": chat_id,
            "question": question,
            "options": options,
            "allows_multiple_answers": allows_multiple,
            "is_anonymous": is_anonymous,
        });

        let resp = self.client
            .post(&self.api_url("sendPoll"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Telegram sendPoll failed: {e}"))?;
        self.parse_message_id(resp).await
    }

    /// Send a sticker by file_id.
    pub async fn send_sticker(
        &self,
        chat_id: i64,
        sticker_id: &str,
    ) -> Result<String, String> {
        let body = serde_json::json!({
            "chat_id": chat_id,
            "sticker": sticker_id,
        });

        let resp = self.client
            .post(&self.api_url("sendSticker"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Telegram sendSticker failed: {e}"))?;
        self.parse_message_id(resp).await
    }

    /// Edit a message's text.
    pub async fn edit_message(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
    ) -> Result<(), String> {
        let body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
            "parse_mode": "Markdown",
        });
        self.post_void("editMessageText", &body).await
    }

    /// Delete a message.
    pub async fn delete_message(
        &self,
        chat_id: i64,
        message_id: i64,
    ) -> Result<(), String> {
        let body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
        });
        self.post_void("deleteMessage", &body).await
    }

    /// Pin a message.
    pub async fn pin_message(
        &self,
        chat_id: i64,
        message_id: i64,
        silent: bool,
    ) -> Result<(), String> {
        let body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "disable_notification": silent,
        });
        self.post_void("pinChatMessage", &body).await
    }

    /// Create a forum topic (supergroup with topics enabled).
    pub async fn create_forum_topic(
        &self,
        chat_id: i64,
        name: &str,
        icon_emoji: Option<&str>,
    ) -> Result<i64, String> {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "name": name,
        });
        if let Some(emoji) = icon_emoji {
            body["icon_custom_emoji_id"] = serde_json::json!(emoji);
        }

        let resp = self.client
            .post(&self.api_url("createForumTopic"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Telegram createForumTopic failed: {e}"))?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("createForumTopic failed: {err}"));
        }

        let data: TelegramResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| format!("parse createForumTopic: {e}"))?;

        data.result
            .and_then(|v| v.get("message_thread_id")?.as_i64())
            .ok_or_else(|| "missing thread_id in response".into())
    }

    /// Answer an inline button callback query.
    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> Result<(), String> {
        let mut body = serde_json::json!({
            "callback_query_id": callback_query_id,
            "show_alert": show_alert,
        });
        if let Some(t) = text {
            body["text"] = serde_json::json!(t);
        }
        self.post_void("answerCallbackQuery", &body).await
    }

    // ── Helpers ──────────────────────────────────────────────

    /// POST to a Telegram API method, check success, discard result.
    async fn post_void(&self, method: &str, body: &serde_json::Value) -> Result<(), String> {
        let resp = self.client
            .post(&self.api_url(method))
            .json(body)
            .send()
            .await
            .map_err(|e| format!("Telegram {method} failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Telegram {method} ({status}): {err}"));
        }
        Ok(())
    }

    /// Parse a message_id from a Telegram sendXxx response.
    async fn parse_message_id(&self, resp: reqwest::Response) -> Result<String, String> {
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Telegram API ({status}): {err}"));
        }

        let data: TelegramResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| format!("parse response: {e}"))?;

        Ok(data.result
            .and_then(|v| v.get("message_id")?.as_i64())
            .map(|id| id.to_string())
            .unwrap_or_default())
    }

    /// Send a photo via Telegram Bot API `sendPhoto`.
    /// Reads bytes from `attachment.url` (file path) or uses `attachment.data`.
    async fn send_photo(
        &self,
        chat_id: i64,
        attachment: &MediaAttachment,
        reply_to: Option<&str>,
    ) -> Result<(), String> {
        let url = self.api_url("sendPhoto");

        let bytes = self.read_attachment_bytes(attachment).await?;
        let filename = attachment.filename.clone().unwrap_or_else(|| "photo.jpg".into());

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string());

        if let Some(reply_id) = reply_to {
            if let Ok(id) = reply_id.parse::<i64>() {
                form = form.text("reply_to_message_id", id.to_string());
            }
        }

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str(&attachment.mime_type)
            .map_err(|e| format!("mime: {e}"))?;
        form = form.part("photo", part);

        let resp = self.client.post(&url).multipart(form).send().await
            .map_err(|e| format!("sendPhoto failed: {e}"))?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("sendPhoto error: {err}"));
        }
        Ok(())
    }

    /// Send a document via Telegram Bot API `sendDocument`.
    async fn send_document_attachment(
        &self,
        chat_id: i64,
        attachment: &MediaAttachment,
        reply_to: Option<&str>,
    ) -> Result<(), String> {
        let url = self.api_url("sendDocument");

        let bytes = self.read_attachment_bytes(attachment).await?;
        let filename = attachment.filename.clone().unwrap_or_else(|| "file".into());

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string());

        if let Some(reply_id) = reply_to {
            if let Ok(id) = reply_id.parse::<i64>() {
                form = form.text("reply_to_message_id", id.to_string());
            }
        }

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str(&attachment.mime_type)
            .map_err(|e| format!("mime: {e}"))?;
        form = form.part("document", part);

        let resp = self.client.post(&url).multipart(form).send().await
            .map_err(|e| format!("sendDocument failed: {e}"))?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("sendDocument error: {err}"));
        }
        Ok(())
    }

    /// Send a voice/audio via Telegram Bot API `sendVoice`.
    async fn send_voice_or_audio(
        &self,
        chat_id: i64,
        attachment: &MediaAttachment,
        reply_to: Option<&str>,
    ) -> Result<(), String> {
        let url = self.api_url("sendVoice");

        let bytes = self.read_attachment_bytes(attachment).await?;
        let filename = attachment.filename.clone().unwrap_or_else(|| "audio.ogg".into());

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string());

        if let Some(reply_id) = reply_to {
            if let Ok(id) = reply_id.parse::<i64>() {
                form = form.text("reply_to_message_id", id.to_string());
            }
        }

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str(&attachment.mime_type)
            .map_err(|e| format!("mime: {e}"))?;
        form = form.part("voice", part);

        let resp = self.client.post(&url).multipart(form).send().await
            .map_err(|e| format!("sendVoice failed: {e}"))?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("sendVoice error: {err}"));
        }
        Ok(())
    }

    /// Read bytes from a MediaAttachment — either from `data` or from `url` (file path).
    async fn read_attachment_bytes(&self, attachment: &MediaAttachment) -> Result<Vec<u8>, String> {
        if let Some(ref data) = attachment.data {
            Ok(data.clone())
        } else if let Some(ref path) = attachment.url {
            tokio::fs::read(path).await
                .map_err(|e| format!("read media file {path}: {e}"))
        } else {
            Err("attachment has neither data nor url".into())
        }
    }
}

// ─── Telegram API types ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TgMessage>,
    callback_query: Option<TgCallbackQuery>,
}

/// Telegram callback query from an inline button press.
#[derive(Debug, Deserialize)]
struct TgCallbackQuery {
    id: String,
    from: TgUser,
    message: Option<TgMessage>,
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    message_id: i64,
    from: Option<TgUser>,
    chat: TgChat,
    text: Option<String>,
    /// Caption for photos/documents (Telegram puts the text here, not in `text`).
    caption: Option<String>,
    photo: Option<Vec<TgPhotoSize>>,
    /// Voice message.
    voice: Option<TgFileRef>,
    /// Audio file.
    audio: Option<TgFileRef>,
    /// Document (PDF, etc.).
    document: Option<TgFileRef>,
    #[serde(default)]
    message_thread_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TgUser {
    id: i64,
    first_name: String,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: String,
}

#[derive(Debug, Deserialize)]
struct TgPhotoSize {
    file_id: String,
    file_size: Option<u64>,
    width: i32,
    height: i32,
}

/// Generic Telegram file reference (voice, audio, document).
#[derive(Debug, Deserialize)]
struct TgFileRef {
    file_id: String,
    file_size: Option<u64>,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    file_name: Option<String>,
}

/// Response from Telegram `getFile` API.
#[derive(Debug, Deserialize)]
struct TgFileResponse {
    file_path: Option<String>,
}
